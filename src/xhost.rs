//! El servidor X integrado al servicio de la aplicación.
//!
//! Se arranca solo cuando hace falta (la primera aplicación gráfica que
//! llega por un reenvío X11) y entrega su pantalla a la interfaz, que la
//! muestra en una pestaña. Las conexiones SSH lo encuentran a través de
//! [`active`].

use std::sync::{Arc, Mutex, RwLock};

use crate::desktop::DesktopBackend;
use crate::ssh::Waker;
use crate::xserver::{XServer, XServerConfig};

/// Tamaño de la pantalla hasta que la interfaz diga cuánto sitio hay.
const DEFAULT_SIZE: (u32, u32) = (1024, 700);

struct State {
    /// Usar el servidor integrado para los reenvíos X11.
    enabled: bool,
    /// Aceptar también programas de este equipo (`DISPLAY=127.0.0.1:N`).
    listen_local: bool,
    /// Cada aplicación en su propia ventana del sistema, y no todas en la
    /// pantalla de una pestaña. Vale para el siguiente arranque.
    windows: bool,
    size: (u32, u32),
    server: Option<XServer>,
    /// Pantalla recién arrancada que la interfaz aún no ha recogido.
    backend: Option<DesktopBackend>,
    /// Ya se ha pedido escuchar para programas locales.
    listening: bool,
    /// Número de pantalla en el que escucha para programas locales.
    local_display: Option<u16>,
    /// Por qué no se pudo escuchar para programas locales.
    local_error: Option<String>,
}

pub struct XHost {
    rt: tokio::runtime::Handle,
    waker: Waker,
    state: Mutex<State>,
}

impl XHost {
    pub fn new(rt: tokio::runtime::Handle, waker: Waker) -> Arc<Self> {
        Arc::new(Self {
            rt,
            waker,
            state: Mutex::new(State {
                enabled: false,
                listen_local: false,
                windows: false,
                size: DEFAULT_SIZE,
                server: None,
                backend: None,
                listening: false,
                local_display: None,
                local_error: None,
            }),
        })
    }

    /// Elige si la próxima vez que arranque el servidor cada aplicación irá
    /// en su propia ventana del sistema (no cambia uno ya en marcha).
    pub fn set_windows(&self, windows: bool) {
        self.state.lock().unwrap().windows = windows;
    }

    /// Aplica los ajustes. Al desactivarlo se para el servidor (y con él
    /// las aplicaciones que tuviera abiertas).
    pub fn configure(self: &Arc<Self>, enabled: bool, listen_local: bool) {
        let stopped = {
            let mut st = self.state.lock().unwrap();
            let changed = st.listen_local != listen_local;
            st.enabled = enabled;
            st.listen_local = listen_local;
            // Dejar de escuchar exige parar: los programas locales ya
            // conectados se cerrarían de todos modos.
            if !enabled || (changed && !listen_local && st.listening) {
                st.reset()
            } else {
                None
            }
        };
        if let Some(server) = stopped {
            server.stop();
        }
        // Si está en marcha y ahora debe escuchar, empieza a hacerlo.
        if enabled && listen_local && self.running().is_some() {
            self.listen_if_wanted();
        }
    }

    pub fn enabled(&self) -> bool {
        self.state.lock().unwrap().enabled
    }

    /// Tamaño con el que arrancará la pantalla (el de la zona de pestañas).
    pub fn set_initial_size(&self, width: u32, height: u32) {
        let mut st = self.state.lock().unwrap();
        if st.server.is_none() {
            st.size = (width.max(320), height.max(240));
        }
    }

    /// El servidor, si está en marcha.
    pub fn running(&self) -> Option<XServer> {
        self.state.lock().unwrap().server.clone()
    }

    /// El servidor en marcha; lo arranca si hace falta.
    pub fn server(self: &Arc<Self>) -> XServer {
        let (server, started) = {
            let mut st = self.state.lock().unwrap();
            match &st.server {
                Some(server) => (server.clone(), false),
                None => {
                    let (server, backend) = XServer::start(
                        &self.rt,
                        XServerConfig {
                            width: st.size.0,
                            height: st.size.1,
                            rootless: st.windows,
                        },
                        self.waker.clone(),
                    );
                    st.server = Some(server.clone());
                    st.backend = Some(backend);
                    (server, true)
                }
            }
        };
        if started {
            self.listen_if_wanted();
            // La interfaz tiene una pantalla nueva que mostrar.
            (self.waker)();
        }
        server
    }

    /// Empieza a escuchar para programas de este equipo, si se ha pedido.
    fn listen_if_wanted(self: &Arc<Self>) {
        let server = {
            let mut st = self.state.lock().unwrap();
            if !st.listen_local || st.listening {
                return;
            }
            let Some(server) = st.server.clone() else {
                return;
            };
            st.listening = true;
            server
        };
        let host = self.clone();
        self.rt.spawn(async move {
            let result = server.listen_tcp(None).await;
            {
                let mut st = host.state.lock().unwrap();
                match result {
                    Ok(display) => {
                        st.local_display = Some(display);
                        st.local_error = None;
                    }
                    Err(e) => st.local_error = Some(format!("{e:#}")),
                }
            }
            (host.waker)();
        });
    }

    /// Pantalla recién arrancada, para abrirla en una pestaña. Solo se
    /// entrega una vez.
    pub fn take_backend(&self) -> Option<DesktopBackend> {
        self.state.lock().unwrap().backend.take()
    }

    /// `DISPLAY` para los programas de este equipo, si se está escuchando.
    pub fn local_display(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .local_display
            .map(|number| format!("127.0.0.1:{number}"))
    }

    pub fn local_error(&self) -> Option<String> {
        self.state.lock().unwrap().local_error.clone()
    }

    /// Para el servidor: se cierran las aplicaciones que tuviera abiertas.
    /// Volverá a arrancar cuando haga falta.
    pub fn stop(&self) {
        let server = self.state.lock().unwrap().reset();
        if let Some(server) = server {
            server.stop();
        }
    }
}

impl State {
    /// Olvida el servidor en marcha y lo devuelve para pararlo.
    fn reset(&mut self) -> Option<XServer> {
        self.backend = None;
        self.listening = false;
        self.local_display = None;
        self.local_error = None;
        self.server.take()
    }
}

static HOST: RwLock<Option<Arc<XHost>>> = RwLock::new(None);

/// Registra el servidor integrado de la aplicación.
pub fn install(host: Arc<XHost>) {
    *HOST.write().unwrap_or_else(|e| e.into_inner()) = Some(host);
}

/// El servidor integrado, si la aplicación lo tiene y está activado en los
/// ajustes.
pub fn active() -> Option<Arc<XHost>> {
    HOST.read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .filter(|host| host.enabled())
}
