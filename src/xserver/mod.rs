//! Servidor X11 propio, en Rust puro.
//!
//! Permite ver las aplicaciones gráficas remotas (reenvío X11 por SSH) dentro
//! de GodRemote sin instalar VcXsrv ni Xming. Funciona en «modo raíz»: una
//! pantalla con todas las ventanas de los clientes dentro, que la interfaz
//! pinta y maneja como cualquier otro escritorio remoto ([`DesktopBackend`]).
//!
//! Implementa el protocolo básico (*core*) de X11 y las extensiones
//! BIG-REQUESTS, RENDER (lo que usan GTK, Qt, Xft y Java para pintar con
//! alfa), SHAPE (ventanas no rectangulares), un RANDR mínimo (para avisar
//! de los cambios de tamaño de la pantalla), XKEYBOARD (el mapa y el estado
//! del teclado) y XInput 2.0 (teclado y ratón como dispositivos): las dos
//! últimas son las que exige GTK 4. No hay XFIXES, MIT-SHM, XTEST ni GLX.
//!
//! Para depurar, con la variable de entorno `GODREMOTE_X11_TRACE` se anotan
//! en la salida de errores las peticiones, los eventos y los errores, y con
//! `GODREMOTE_X11_DISABLE=RENDER,SHAPE,RANDR,XKEYBOARD,XInputExtension` se
//! dejan de anunciar esas extensiones.
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
//!
//! Los clientes X son programas de otros equipos, es decir, entrada no
//! fiable, y el servidor vive dentro del proceso de la aplicación. Por eso:
//!
//! - cada lote de peticiones se procesa dentro de `catch_unwind`: si un
//!   manejador entra en pánico se expulsa a ese cliente, se repara el estado
//!   y el servidor sigue atendiendo a los demás;
//! - los candados toleran el «envenenamiento», de modo que la interfaz nunca
//!   se cae por un pánico ocurrido aquí;
//! - la memoria de imagen tiene un presupuesto global y las reservas de cada
//!   petición están acotadas (error `Alloc` o `Length`, no un aborto);
//! - a quien no lee lo que se le envía se le deja de atender y, si aun así
//!   se le acumula demasiado, se le expulsa.
//!
//! Orden de los candados: primero el del estado y después el del
//! [`SharedFrame`]. Quien use el servidor no debe llamar a sus métodos
//! mientras tenga cogido el candado de la imagen.

mod colors;
mod cursor;
mod draw;
mod font;
mod input;
mod keymap;
mod proto;
mod randr;
mod render;
mod requests;
mod requests_draw;
mod rootless;
mod server;
mod shape;
#[cfg(test)]
mod tests;
mod wm;
mod xinput;
mod xkb;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

use crate::desktop::{
    DesktopBackend, DesktopEvent, DesktopInput, Framebuffer, MouseButton, SharedFrame,
};
use crate::ssh::Waker;
use keymap::{MAX_KEYCODE, MIN_KEYCODE};
use proto::Writer;
pub use rootless::{RootlessInput, RootlessRequest, RootlessShared, RootlessView, RootlessWindow};
use server::{
    ClientId, ClientLink, DEFAULT_COLORMAP, OUTPUT_SOFT_LIMIT, RESOURCE_MASK, ROOT, ROOT_VISUAL,
    Server,
};

/// Nombre del protocolo de autenticación por *cookie*.
const COOKIE_PROTOCOL: &[u8] = b"MIT-MAGIC-COOKIE-1";
/// Campos de autenticación más largos que esto no son de un cliente X.
const MAX_AUTH_FIELD: usize = 1024;
/// Primer número de pantalla que se prueba al buscar uno libre.
const FIRST_FREE_DISPLAY: u16 = 20;

/// Toma un candado aunque un pánico anterior lo dejara «envenenado»: el
/// servidor repara su estado después de un pánico y sigue, así que el dato
/// protegido se puede usar.
pub(crate) fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Tamaño inicial de la pantalla raíz y modo de trabajo.
pub struct XServerConfig {
    pub width: u32,
    pub height: u32,
    /// Ventanas sueltas: cada ventana de primer nivel se entrega por
    /// separado ([`XServer::rootless`]) en lugar de componerlas todas en
    /// la pantalla.
    pub rootless: bool,
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
    /// Modo de ventanas sueltas: lo que se publica para la interfaz y por
    /// dónde llega lo que ella cuenta.
    rootless: Option<RootlessShared>,
    rootless_tx: mpsc::UnboundedSender<RootlessInput>,
    /// Pánicos contenidos hasta ahora (para las pruebas).
    #[cfg(test)]
    panics: std::sync::atomic::AtomicUsize,
}

/// Manejador del servidor X (barato de clonar).
#[derive(Clone)]
pub struct XServer {
    inner: Arc<Inner>,
}

/// Cómo acabó un lote de peticiones de un cliente.
enum Batch {
    /// Faltan bytes para completar la siguiente petición.
    NeedMore,
    /// El cliente tiene demasiada salida sin leer: se espera a que lea.
    Throttled,
    /// El cliente ya no está dado de alta (se le expulsó).
    Gone,
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
        let (rootless_tx, mut rootless_rx) = mpsc::unbounded_channel::<RootlessInput>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        let frame = Framebuffer::shared(width, height);
        let (stop, _) = watch::channel(false);
        let mut state = Server::new(width, height);
        let rootless = config.rootless.then(|| {
            let shared = RootlessShared::default();
            state.rootless = Some(rootless::Rootless::new(shared.clone()));
            shared
        });
        let server = XServer {
            inner: Arc::new(Inner {
                state: Mutex::new(state),
                frame: frame.clone(),
                waker,
                rt: rt.clone(),
                events: Mutex::new(ev_tx),
                stop,
                scratch: Mutex::new(vec![]),
                rootless,
                rootless_tx,
                #[cfg(test)]
                panics: Default::default(),
            }),
        };
        server.guarded(|st| server.finish(st));
        server.emit(DesktopEvent::Connected);

        // Tarea de entrada: teclado, ratón y portapapeles de la interfaz.
        let task = server.clone();
        let mut stopped = server.inner.stop.subscribe();
        rt.spawn(async move {
            loop {
                // (Con las ventanas sueltas todo llega por su propio canal.)
                let first = tokio::select! {
                    _ = stopped.changed() => break,
                    input = input_rx.recv() => input.map(Incoming::Desktop),
                    Some(input) = rootless_rx.recv() => Some(Incoming::Rootless(input)),
                };
                let Some(first) = first else {
                    break;
                };
                // Todo lo que haya en cola se aplica de una vez y se compone
                // una sola vez.
                let mut close = false;
                task.guarded(|st| {
                    let mut next = Some(first);
                    while let Some(input) = next {
                        match input {
                            Incoming::Desktop(DesktopInput::Close) => {
                                close = true;
                                break;
                            }
                            Incoming::Desktop(input) => apply_input(st, input),
                            Incoming::Rootless(input) => st.rootless_input(input),
                        }
                        next = match input_rx.try_recv() {
                            Ok(input) => Some(Incoming::Desktop(input)),
                            Err(_) => rootless_rx.try_recv().ok().map(Incoming::Rootless),
                        };
                    }
                    task.finish(st);
                });
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

    /// Ejecuta una operación sobre el estado con su candado cogido. Un pánico
    /// dentro no sale de aquí: se repara el estado, se recompone la pantalla
    /// y se devuelve `None`.
    fn guarded<R>(&self, op: impl FnOnce(&mut Server) -> R) -> Option<R> {
        let mut st = lock(&self.inner.state);
        match catch_unwind(AssertUnwindSafe(|| op(&mut st))) {
            Ok(value) => Some(value),
            Err(_) => {
                #[cfg(test)]
                self.inner.panics.fetch_add(1, Ordering::Relaxed);
                // La reparación tampoco puede tumbar a quien llama.
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    // (Si el pánico fue al publicar las ventanas sueltas, su
                    // estado estaba apartado y se ha perdido: se rehace.)
                    if st.rootless.is_none()
                        && let Some(shared) = &self.inner.rootless
                    {
                        st.rootless = Some(rootless::Rootless::new(shared.clone()));
                    }
                    st.repair();
                    self.finish(&mut st);
                }));
                None
            }
        }
    }

    fn emit(&self, event: DesktopEvent) {
        let _ = lock(&self.inner.events).send(event);
        (self.inner.waker)();
    }

    fn stopped(&self) -> bool {
        *self.inner.stop.borrow()
    }

    /// Cierra un lote de trabajo: envía lo pendiente a los clientes,
    /// recompone las zonas de pantalla que cambiaron y avisa a la interfaz.
    fn finish(&self, st: &mut Server) {
        st.flush();
        // Con las ventanas sueltas no se compone la pantalla: cada ventana
        // de primer nivel va a su propia imagen.
        if st.rootless.is_some() {
            if st.resized || !st.damage.is_empty() || st.rootless_pending() {
                st.rootless_publish(&mut lock(&self.inner.scratch));
                (self.inner.waker)();
            }
            self.finish_events(st);
            return;
        }
        let resized = std::mem::take(&mut st.resized);
        if resized || !st.damage.is_empty() {
            st.prepare_decorations();
            let rects = std::mem::take(&mut st.damage);
            let mut scratch = lock(&self.inner.scratch);
            let mut frame = lock(&self.inner.frame);
            if resized || (frame.width(), frame.height()) != (st.width, st.height) {
                frame.resize(st.width, st.height);
            }
            let bounds = st.screen_rect();
            let stride = frame.width() as usize * 4;
            for r in rects {
                let r = r.intersect(&bounds);
                let (w, h) = (r.width() as usize, r.height() as usize);
                if w == 0 || h == 0 {
                    continue;
                }
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
        self.finish_events(st);
    }

    /// Avisos para la interfaz al cerrar un lote de trabajo.
    fn finish_events(&self, st: &mut Server) {
        // El puntero pasó a otra ventana, o la de debajo cambió de cursor.
        let cursor = st.current_cursor();
        if cursor != st.cursor_sent {
            st.cursor_sent = cursor.clone();
            st.desktop_events.push(DesktopEvent::Cursor(cursor));
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
        if self.stopped() {
            bail!("El servidor X está parado");
        }
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
        let link = ClientLink::default();
        let id = self
            .guarded(|st| {
                if self.stopped() {
                    return None;
                }
                let id = st.add_client(be, tx, link.clone())?;
                let setup = setup_reply(st, be, id);
                if let Some(c) = st.clients.get_mut(&id) {
                    c.out = setup;
                }
                st.flush();
                Some(id)
            })
            .flatten();
        let Some(id) = id else {
            wr.write_all(&refuse("Maximum number of clients reached"))
                .await?;
            bail!("El servidor X no admite más clientes");
        };
        // Tarea de escritura: termina sola cuando el cliente se da de baja.
        let writer_link = link.clone();
        let writer = self.inner.rt.spawn(async move {
            while let Some(bytes) = rx.recv().await {
                let written = wr.write_all(&bytes).await.is_ok();
                writer_link.queued.fetch_sub(bytes.len(), Ordering::Relaxed);
                writer_link.drained.notify_one();
                if !written {
                    // Sin poder escribirle no tiene sentido seguir leyéndole.
                    writer_link.kill.notify_one();
                    break;
                }
            }
            let _ = wr.shutdown().await;
        });

        let result = self.read_requests(id, be, &mut rd, &link).await;
        self.expel(id);
        // La tarea de escritura acaba al vaciar lo pendiente; si el cliente
        // no lee, no se la espera para siempre: se corta la conexión.
        let mut writer = writer;
        if tokio::time::timeout(Duration::from_secs(5), &mut writer)
            .await
            .is_err()
        {
            writer.abort();
        }
        result
    }

    /// Da de baja a un cliente y libera lo suyo.
    fn expel(&self, id: ClientId) {
        self.guarded(|st| {
            st.remove_client(id);
            self.finish(st);
        });
        // Si la propia baja falló a medias, al menos que no siga constando.
        self.guarded(|st| st.clients.remove(&id).is_some());
    }

    /// Procesa todas las peticiones completas que haya en `buf`. Devuelve
    /// cuántos bytes consumió y por qué paró.
    fn process(
        &self,
        st: &mut Server,
        id: ClientId,
        be: bool,
        buf: &[u8],
    ) -> Result<(usize, Batch)> {
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
        let mut used = 0;
        let outcome = loop {
            let Some(client) = st.clients.get(&id) else {
                break Batch::Gone;
            };
            // No se generan más respuestas mientras no lea las que tiene.
            let pending_out = client.out.len() + client.link.queued.load(Ordering::Relaxed);
            if pending_out > OUTPUT_SOFT_LIMIT {
                break Batch::Throttled;
            }
            let pending = &buf[used..];
            if pending.len() < 4 {
                break Batch::NeedMore;
            }
            let (header, total) = match word(&pending[2..4]) {
                // Longitud 0: petición grande (BIG-REQUESTS), con la longitud
                // real en los 4 bytes siguientes.
                0 if client.big_requests => {
                    if pending.len() < 8 {
                        break Batch::NeedMore;
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
                break Batch::NeedMore;
            }
            st.request(id, pending[0], pending[1], &pending[header..total]);
            used += total;
        };
        // (Al frenar se envía siempre lo pendiente: es lo que hay que esperar
        // a que el cliente lea.)
        if used > 0 || matches!(outcome, Batch::Throttled) {
            self.finish(st);
        }
        Ok((used, outcome))
    }

    /// Bucle de lectura de un cliente: cada vez que llegan datos se procesan
    /// todas las peticiones completas de una tacada.
    async fn read_requests<R: AsyncRead + Unpin>(
        &self,
        id: ClientId,
        be: bool,
        rd: &mut R,
        link: &ClientLink,
    ) -> Result<()> {
        let mut stopped = self.inner.stop.subscribe();
        let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            // Con demasiada salida pendiente, se espera a que el cliente lea.
            while link.queued.load(Ordering::Relaxed) > OUTPUT_SOFT_LIMIT {
                tokio::select! {
                    biased;
                    _ = link.kill.notified() => return Ok(()),
                    _ = stopped.changed() => return Ok(()),
                    _ = link.drained.notified() => {}
                }
            }
            let Some(batch) = self.guarded(|st| self.process(st, id, be, &buf)) else {
                // Un manejador entró en pánico: fuera este cliente; los demás
                // siguen (el estado ya se ha reparado).
                self.expel(id);
                bail!("El servidor X expulsó a un cliente por un error interno al atenderlo");
            };
            let (used, outcome) = batch?;
            buf.drain(..used);
            match outcome {
                Batch::Gone => return Ok(()),
                Batch::Throttled => continue,
                Batch::NeedMore => {}
            }
            tokio::select! {
                biased;
                _ = link.kill.notified() => return Ok(()),
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
        // (Estos errores sí los ve el usuario, en la pestaña del servidor X.)
        if self.stopped() {
            bail!("{}", crate::i18n::tr("El servidor X está parado"));
        }
        let candidates = match display {
            Some(d) => d..=d,
            None => FIRST_FREE_DISPLAY..=FIRST_FREE_DISPLAY + 400,
        };
        let mut last_error = None;
        for number in candidates {
            let Some(port) = 6000u16.checked_add(number) else {
                bail!(
                    "{}",
                    crate::trf!("Número de pantalla X demasiado alto: {number}", number)
                );
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
            Some(e) => Err(anyhow!(
                "{}",
                crate::trf!("No se pudo abrir el puerto del servidor X: {e}", e)
            )),
            None => bail!(
                "{}",
                crate::i18n::tr("No queda ningún número de pantalla X libre")
            ),
        }
    }

    /// En modo de ventanas sueltas, lo que el servidor publica: las
    /// ventanas de primer nivel con su imagen y lo que piden. `None` en el
    /// modo normal (todas en una pantalla).
    pub fn rootless(&self) -> Option<RootlessShared> {
        self.inner.rootless.clone()
    }

    /// Lo que la interfaz cuenta al servidor en modo de ventanas sueltas
    /// (teclado, ratón, dónde queda cada ventana…). No bloquea.
    pub fn rootless_send(&self, input: RootlessInput) {
        let _ = self.inner.rootless_tx.send(input);
    }

    /// Clientes X conectados ahora mismo.
    pub fn client_count(&self) -> usize {
        lock(&self.inner.state).clients.len()
    }

    /// Cambia el tamaño de la pantalla raíz (la pestaña cambió de tamaño).
    /// El [`SharedFrame`] cambia de tamaño (y de `epoch`) a la vez.
    pub fn resize(&self, width: u32, height: u32) {
        self.guarded(|st| {
            st.resize_screen(width, height);
            self.finish(st);
        });
    }

    /// Elige si lo que se selecciona con el ratón en las aplicaciones
    /// (selección PRIMARY) se envía también como portapapeles a la interfaz.
    /// Por defecto, sí.
    pub fn set_primary_sync(&self, enabled: bool) {
        lock(&self.inner.state).sync_primary = enabled;
    }

    /// Para el servidor: desconecta a todos los clientes, deja de escuchar
    /// y emite [`DesktopEvent::Closed`].
    pub fn stop(&self) {
        if self.inner.stop.send_replace(true) {
            return;
        }
        self.guarded(|st| {
            let clients: Vec<ClientId> = st.clients.keys().copied().collect();
            for client in clients {
                st.remove_client(client);
            }
            self.finish(st);
        });
        // Pase lo que pase arriba, nadie sigue dado de alta.
        self.guarded(|st| st.clients.clear());
        self.emit(DesktopEvent::Closed(None));
    }
}

/// Lo que llega de la interfaz, por uno u otro canal.
enum Incoming {
    Desktop(DesktopInput),
    Rootless(RootlessInput),
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
    // (La 4 está solo porque libXrender exige poder crear pixmaps de 1, 4,
    // 8, 24 y 32 antes de dar RENDER por bueno.)
    const FORMATS: [(u8, u8, u8); 5] = [
        (1, 1, 32),
        (4, 8, 32),
        (8, 8, 32),
        (24, 32, 32),
        (32, 32, 32),
    ];
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
    // Profundidad 24 con su visual TrueColor; las demás, solo para pixmaps.
    body.u8(24).u8(0).u16(1).u32(0);
    body.u32(ROOT_VISUAL)
        .u8(4)
        .u8(8)
        .u16(256)
        .u32(0x00ff_0000)
        .u32(0x0000_ff00)
        .u32(0x0000_00ff)
        .u32(0);
    for depth in [1, 4, 8, 32] {
        body.u8(depth).u8(0).u16(0).u32(0);
    }
    let mut w = Writer::new(be);
    w.u8(1)
        .u8(0)
        .u16(11)
        .u16(0)
        .u16((body.buf.len() / 4) as u16)
        .bytes(&body.buf);
    w.buf
}
