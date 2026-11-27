//! Servidor X11 propio, en Rust puro.
//!
//! Permite ver las aplicaciones gráficas remotas (reenvío X11 por SSH) dentro
//! de GodRemote sin instalar VcXsrv ni Xming. Funciona en «modo raíz»: una
//! pantalla con todas las ventanas de los clientes dentro, que la interfaz
//! pinta y maneja como cualquier otro escritorio remoto ([`DesktopBackend`]).
//!
//! Implementa el protocolo básico (*core*) de X11 y la extensión
//! BIG-REQUESTS. No hay RENDER, XKB, SHAPE, MIT-SHM, XInput ni GLX: las
//! aplicaciones Xlib/Xt/Athena clásicas se apañan sin ellas.
//!
//! Organización:
//!
//! - un estado central ([`server::Server`]) protegido por un candado, con los
//!   recursos y el árbol de ventanas;
//! - una tarea por cliente que lee peticiones y las procesa por lotes, más
//!   otra que escribe sus respuestas, eventos y errores;
//! - cada ventana y cada pixmap tienen su propio búfer; la pantalla se
//!   **compone** pintando el árbol sobre el [`SharedFrame`], y solo las zonas
//!   que cambiaron en cada lote.

mod colors;
mod draw;
mod font;
mod input;
mod keymap;
mod proto;
mod requests;
mod requests_draw;
mod server;
#[cfg(test)]
mod tests;
mod wm;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, mpsc, watch};

use crate::desktop::{
    DesktopBackend, DesktopEvent, DesktopInput, Framebuffer, MouseButton, SharedFrame,
};
use crate::ssh::Waker;
use keymap::{MAX_KEYCODE, MIN_KEYCODE};
use proto::Writer;
use server::{ClientId, DEFAULT_COLORMAP, RESOURCE_MASK, ROOT, ROOT_VISUAL, Server};

/// Nombre del protocolo de autenticación por *cookie*.
const COOKIE_PROTOCOL: &[u8] = b"MIT-MAGIC-COOKIE-1";
/// Campos de autenticación más largos que esto no son de un cliente X.
const MAX_AUTH_FIELD: usize = 1024;
/// Primer número de pantalla que se prueba al buscar uno libre.
const FIRST_FREE_DISPLAY: u16 = 20;

/// Tamaño inicial de la pantalla raíz.
pub struct XServerConfig {
    pub width: u32,
    pub height: u32,
}

struct Inner {
    state: Mutex<Server>,
    frame: SharedFrame,
    waker: Waker,
    rt: tokio::runtime::Handle,
    events: Mutex<std::sync::mpsc::Sender<DesktopEvent>>,
    /// Pasa a `true` al parar el servidor.
    stop: watch::Sender<bool>,
    /// Búfer de trabajo de la composición.
    scratch: Mutex<Vec<u32>>,
}

/// Manejador del servidor X (barato de clonar).
#[derive(Clone)]
pub struct XServer {
    inner: Arc<Inner>,
}

impl XServer {
    /// Arranca el servidor. Devuelve el manejador y el [`DesktopBackend`] con
    /// el que la interfaz lo pinta y le manda teclado y ratón. Emite
    /// [`DesktopEvent::Connected`] al arrancar.
    pub fn start(
        rt: &tokio::runtime::Handle,
        config: XServerConfig,
        waker: Waker,
    ) -> (XServer, DesktopBackend) {
        let (width, height) = (
            config.width.clamp(64, 16_384),
            config.height.clamp(64, 16_384),
        );
        let (input_tx, mut input_rx) = mpsc::unbounded_channel::<DesktopInput>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        let frame = Framebuffer::shared(width, height);
        let (stop, _) = watch::channel(false);
        let server = XServer {
            inner: Arc::new(Inner {
                state: Mutex::new(Server::new(width, height)),
                frame: frame.clone(),
                waker,
                rt: rt.clone(),
                events: Mutex::new(ev_tx),
                stop,
                scratch: Mutex::new(vec![]),
            }),
        };
        server.finish(&mut server.inner.state.lock().unwrap());
        server.emit(DesktopEvent::Connected);

        // Tarea de entrada: teclado, ratón y portapapeles de la interfaz.
        let task = server.clone();
        let mut stopped = server.inner.stop.subscribe();
        rt.spawn(async move {
            loop {
                let first = tokio::select! {
                    _ = stopped.changed() => break,
                    input = input_rx.recv() => input,
                };
                let Some(first) = first else {
                    break;
                };
                // Todo lo que haya en cola se aplica de una vez y se compone
                // una sola vez.
                let mut close = false;
                {
                    let mut st = task.inner.state.lock().unwrap();
                    let mut next = Some(first);
                    while let Some(input) = next {
                        if input == DesktopInput::Close {
                            close = true;
                            break;
                        }
                        apply_input(&mut st, input);
                        next = input_rx.try_recv().ok();
                    }
                    task.finish(&mut st);
                }
                if close {
                    break;
                }
            }
            task.stop();
        });
        (
            server,
            DesktopBackend {
                input: input_tx,
                events: ev_rx,
                frame,
            },
        )
    }

    fn emit(&self, event: DesktopEvent) {
        let _ = self.inner.events.lock().unwrap().send(event);
        (self.inner.waker)();
    }

    fn stopped(&self) -> bool {
        *self.inner.stop.borrow()
    }

    /// Cierra un lote de trabajo: envía lo pendiente a los clientes,
    /// recompone las zonas de pantalla que cambiaron y avisa a la interfaz.
    fn finish(&self, st: &mut Server) {
        st.flush();
        let resized = std::mem::take(&mut st.resized);
        if resized || !st.damage.is_empty() {
            st.prepare_decorations();
            let rects = std::mem::take(&mut st.damage);
            let mut scratch = self.inner.scratch.lock().unwrap();
            let mut frame = self.inner.frame.lock().unwrap();
            if resized || (frame.width(), frame.height()) != (st.width, st.height) {
                frame.resize(st.width, st.height);
            }
            let stride = frame.width() as usize * 4;
            for r in rects {
                let (w, h) = (r.width() as usize, r.height() as usize);
                scratch.clear();
                scratch.resize(w * h, 0);
                st.compose(r, &mut scratch);
                for row in 0..h {
                    let start = (r.y0 as usize + row) * stride + r.x0 as usize * 4;
                    let dst = &mut frame.pixels[start..start + w * 4];
                    for (d, s) in dst.chunks_exact_mut(4).zip(&scratch[row * w..]) {
                        d[0] = (s >> 16) as u8;
                        d[1] = (s >> 8) as u8;
                        d[2] = *s as u8;
                        d[3] = 255;
                    }
                }
                frame.mark_dirty(r.x0 as u32, r.y0 as u32, w as u32, h as u32);
            }
            drop(frame);
            (self.inner.waker)();
        }
        for event in st.desktop_events.drain(..) {
            self.emit(event);
        }
    }

    /// Atiende a un cliente X cuyo flujo empieza en la petición de conexión
    /// (byte de orden, etc.). Si `cookie` es `Some`, el cliente debe
    /// presentar esa MIT-MAGIC-COOKIE-1; si es `None` se acepta sin
    /// autenticación. Termina cuando el cliente se va, se le expulsa o se
    /// para el servidor.
    pub async fn serve<S>(&self, stream: S, cookie: Option<[u8; 16]>) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (mut rd, mut wr) = tokio::io::split(stream);

        // --- Petición de conexión -------------------------------------------
        let mut head = [0u8; 12];
        tokio::time::timeout(Duration::from_secs(15), rd.read_exact(&mut head))
            .await
            .map_err(|_| anyhow!("El cliente X no envió la petición de conexión"))??;
        let be = match head[0] {
            0x42 => true,
            0x6c => false,
            _ => bail!("No es un cliente X11"),
        };
        let field = |i: usize| -> usize {
            let b = [head[i], head[i + 1]];
            (if be {
                u16::from_be_bytes(b)
            } else {
                u16::from_le_bytes(b)
            }) as usize
        };
        let (major, name_len, data_len) = (field(2), field(6), field(8));
        if name_len > MAX_AUTH_FIELD || data_len > MAX_AUTH_FIELD {
            bail!("Petición de conexión X11 no válida");
        }
        let mut name = vec![0u8; name_len + proto::pad4(name_len)];
        rd.read_exact(&mut name).await?;
        let mut data = vec![0u8; data_len + proto::pad4(data_len)];
        rd.read_exact(&mut data).await?;
        let refuse = |reason: &str| -> Vec<u8> {
            let mut w = Writer::new(be);
            let padded = reason.len() + proto::pad4(reason.len());
            w.u8(0)
                .u8(reason.len() as u8)
                .u16(11)
                .u16(0)
                .u16((padded / 4) as u16)
                .bytes(reason.as_bytes())
                .pad();
            w.buf
        };
        if major != 11 {
            wr.write_all(&refuse("Protocol version mismatch")).await?;
            bail!("Versión del protocolo X no admitida: {major}");
        }
        if let Some(expected) = cookie
            && (&name[..name_len] != COOKIE_PROTOCOL || data[..data_len] != expected)
        {
            wr.write_all(&refuse("Invalid MIT-MAGIC-COOKIE-1 key"))
                .await?;
            let _ = wr.shutdown().await;
            bail!("El cliente X no presentó la cookie de autorización");
        }

        // --- Alta del cliente -----------------------------------------------
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let kill = Arc::new(Notify::new());
        let id = {
            let mut st = self.inner.state.lock().unwrap();
            let id = if self.stopped() {
                None
            } else {
                st.add_client(be, tx, kill.clone())
            };
            if let Some(id) = id {
                let setup = setup_reply(&st, be, id);
                if let Some(c) = st.clients.get_mut(&id) {
                    c.out = setup;
                }
                st.flush();
            }
            id
        };
        let Some(id) = id else {
            wr.write_all(&refuse("Maximum number of clients reached"))
                .await?;
            bail!("El servidor X no admite más clientes");
        };
        // Tarea de escritura: termina sola cuando el cliente se da de baja.
        let writer = self.inner.rt.spawn(async move {
            while let Some(bytes) = rx.recv().await {
                if wr.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            let _ = wr.shutdown().await;
        });

        let result = self.read_requests(id, be, &mut rd, &kill).await;
        {
            let mut st = self.inner.state.lock().unwrap();
            st.remove_client(id);
            self.finish(&mut st);
        }
        let _ = writer.await;
        result
    }

    /// Bucle de lectura de un cliente: cada vez que llegan datos se procesan
    /// todas las peticiones completas de una tacada.
    async fn read_requests<R: AsyncRead + Unpin>(
        &self,
        id: ClientId,
        be: bool,
        rd: &mut R,
        kill: &Notify,
    ) -> Result<()> {
        let word = |b: &[u8]| -> usize {
            (if be {
                u16::from_be_bytes([b[0], b[1]])
            } else {
                u16::from_le_bytes([b[0], b[1]])
            }) as usize
        };
        let long = |b: &[u8]| -> usize {
            (if be {
                u32::from_be_bytes([b[0], b[1], b[2], b[3]])
            } else {
                u32::from_le_bytes([b[0], b[1], b[2], b[3]])
            }) as usize
        };
        let mut stopped = self.inner.stop.subscribe();
        let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut used = 0;
            {
                let mut st = self.inner.state.lock().unwrap();
                let mut worked = false;
                loop {
                    let Some(client) = st.clients.get(&id) else {
                        return Ok(()); // expulsado
                    };
                    let pending = &buf[used..];
                    if pending.len() < 4 {
                        break;
                    }
                    let (header, total) = match word(&pending[2..4]) {
                        // Longitud 0: petición grande (BIG-REQUESTS), con la
                        // longitud real en los 4 bytes siguientes.
                        0 if client.big_requests => {
                            if pending.len() < 8 {
                                break;
                            }
                            let units = long(&pending[4..8]);
                            if !(2..=requests::MAX_BIG_REQUEST as usize).contains(&units) {
                                bail!("Petición X11 de tamaño no válido");
                            }
                            (8, units * 4)
                        }
                        0 => (4, 4),
                        units => (4, units * 4),
                    };
                    if pending.len() < total {
                        break;
                    }
                    st.request(id, pending[0], pending[1], &pending[header..total]);
                    used += total;
                    worked = true;
                }
                if worked {
                    self.finish(&mut st);
                }
            }
            buf.drain(..used);
            tokio::select! {
                biased;
                _ = kill.notified() => return Ok(()),
                _ = stopped.changed() => return Ok(()),
                n = rd.read(&mut chunk) => {
                    let n = n?;
                    if n == 0 {
                        return Ok(());
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
            }
        }
    }

    /// Escucha en `127.0.0.1:(6000 + display)` para clientes locales. Con
    /// `display = None` busca un número libre. Devuelve el número de pantalla
    /// (`DISPLAY=127.0.0.1:N`).
    pub async fn listen_tcp(&self, display: Option<u16>) -> Result<u16> {
        if self.stopped() {
            bail!("El servidor X está parado");
        }
        let candidates = match display {
            Some(d) => d..=d,
            None => FIRST_FREE_DISPLAY..=FIRST_FREE_DISPLAY + 400,
        };
        let mut last_error = None;
        for number in candidates {
            let Some(port) = 6000u16.checked_add(number) else {
                bail!("Número de pantalla X demasiado alto: {number}");
            };
            let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                Ok(l) => l,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };
            let server = self.clone();
            let mut stopped = self.inner.stop.subscribe();
            self.inner.rt.spawn(async move {
                loop {
                    let accepted = tokio::select! {
                        _ = stopped.changed() => break,
                        accepted = listener.accept() => accepted,
                    };
                    let Ok((socket, _)) = accepted else {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    };
                    let _ = socket.set_nodelay(true);
                    let server = server.clone();
                    tokio::spawn(async move {
                        let _ = server.serve(socket, None).await;
                    });
                }
            });
            return Ok(number);
        }
        match last_error {
            Some(e) => Err(anyhow!("No se pudo abrir el puerto del servidor X: {e}")),
            None => bail!("No queda ningún número de pantalla X libre"),
        }
    }

    /// Clientes X conectados ahora mismo.
    pub fn client_count(&self) -> usize {
        self.inner.state.lock().unwrap().clients.len()
    }

    /// Cambia el tamaño de la pantalla raíz (la pestaña cambió de tamaño).
    /// El [`SharedFrame`] cambia de tamaño (y de `epoch`) a la vez.
    pub fn resize(&self, width: u32, height: u32) {
        let mut st = self.inner.state.lock().unwrap();
        st.resize_screen(width, height);
        self.finish(&mut st);
    }

    /// Elige si lo que se selecciona con el ratón en las aplicaciones
    /// (selección PRIMARY) se envía también como portapapeles a la interfaz.
    /// Por defecto, sí.
    pub fn set_primary_sync(&self, enabled: bool) {
        self.inner.state.lock().unwrap().sync_primary = enabled;
    }

    /// Para el servidor: desconecta a todos los clientes, deja de escuchar
    /// y emite [`DesktopEvent::Closed`].
    pub fn stop(&self) {
        if self.inner.stop.send_replace(true) {
            return;
        }
        {
            let mut st = self.inner.state.lock().unwrap();
            let clients: Vec<ClientId> = st.clients.keys().copied().collect();
            for client in clients {
                st.remove_client(client);
            }
            self.finish(&mut st);
        }
        self.emit(DesktopEvent::Closed(None));
    }
}

/// Aplica una orden de la interfaz al estado del servidor.
fn apply_input(st: &mut Server, input: DesktopInput) {
    match input {
        DesktopInput::Key { key, pressed } => st.input_key(key.keysym(), pressed),
        DesktopInput::Text(text) => st.input_text(&text),
        DesktopInput::PointerMove { x, y } => st.pointer_motion(x as i32, y as i32),
        DesktopInput::PointerButton {
            button,
            pressed,
            x,
            y,
        } => {
            st.pointer_motion(x as i32, y as i32);
            let button = match button {
                MouseButton::Left => 1,
                MouseButton::Middle => 2,
                MouseButton::Right => 3,
            };
            st.pointer_button(button, pressed);
        }
        DesktopInput::Wheel {
            x,
            y,
            delta,
            horizontal,
        } => {
            st.pointer_motion(x as i32, y as i32);
            st.input_wheel(delta, horizontal);
        }
        DesktopInput::ClipboardText(text) => st.offer_clipboard(text),
        DesktopInput::Close => {}
    }
}

/// Respuesta de conexión aceptada: describe la pantalla, el visual y los
/// formatos de imagen.
fn setup_reply(st: &Server, be: bool, client: ClientId) -> Vec<u8> {
    const VENDOR: &[u8] = b"GodRemote";
    // (profundidad, bits por píxel, relleno de fila)
    const FORMATS: [(u8, u8, u8); 3] = [(1, 1, 32), (24, 32, 32), (32, 32, 32)];
    let mm = |px: u32| (px as f32 * 25.4 / 96.0).round() as u16;
    let root_masks = st.windows.get(&ROOT).map_or(0, |w| w.all_masks());
    let mut body = Writer::new(be);
    body.u32(12_101_000) // número de versión, al estilo de X.Org
        .u32(Server::resource_base(client))
        .u32(RESOURCE_MASK)
        .u32(256)
        .u16(VENDOR.len() as u16)
        .u16(0xffff)
        .u8(1)
        .u8(FORMATS.len() as u8)
        .u8(0) // imágenes: byte menos significativo primero
        .u8(0) // mapas de bits: bit menos significativo primero
        .u8(32)
        .u8(32)
        .u8(MIN_KEYCODE)
        .u8(MAX_KEYCODE)
        .u32(0)
        .bytes(VENDOR)
        .pad();
    for (depth, bpp, pad) in FORMATS {
        body.u8(depth).u8(bpp).u8(pad).zeros(5);
    }
    // La única pantalla.
    body.u32(ROOT)
        .u32(DEFAULT_COLORMAP)
        .u32(0x00ff_ffff)
        .u32(0)
        .u32(root_masks)
        .u16(st.width as u16)
        .u16(st.height as u16)
        .u16(mm(st.width))
        .u16(mm(st.height))
        .u16(1)
        .u16(1)
        .u32(ROOT_VISUAL)
        .u8(0) // sin «backing store» declarado
        .u8(0)
        .u8(24)
        .u8(FORMATS.len() as u8);
    // Profundidad 24 con su visual TrueColor; 1 y 32 solo para pixmaps.
    body.u8(24).u8(0).u16(1).u32(0);
    body.u32(ROOT_VISUAL)
        .u8(4)
        .u8(8)
        .u16(256)
        .u32(0x00ff_0000)
        .u32(0x0000_ff00)
        .u32(0x0000_00ff)
        .u32(0);
    body.u8(1).u8(0).u16(0).u32(0);
    body.u8(32).u8(0).u16(0).u32(0);
    let mut w = Writer::new(be);
    w.u8(1)
        .u8(0)
        .u16(11)
        .u16(0)
        .u16((body.buf.len() / 4) as u16)
        .bytes(&body.buf);
    w.buf
}
