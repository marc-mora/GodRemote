//! Modo «ventanas sueltas» (sin raíz): cada ventana de primer nivel se
//! entrega a la interfaz por separado, con su propia imagen, para que la
//! muestre como una ventana nativa del sistema en lugar de componerlas todas
//! en una pantalla.
//!
//! El servidor sigue siendo el mismo: hay una pantalla raíz (del tamaño del
//! monitor) y las ventanas tienen en ella la posición que ocupan de verdad
//! en el escritorio, que es lo que necesitan las aplicaciones para colocar
//! sus menús y listas desplegables. Lo que cambia:
//!
//! - no se compone la pantalla: en cada lote se recompone, de cada ventana
//!   de primer nivel, lo que haya cambiado, sobre su propio
//!   [`SharedFrame`], y se publica la lista de ventanas ([`RootlessShared`]);
//! - no hay decoraciones ni colocación del gestor integrado: el marco, mover
//!   y cambiar de tamaño son cosa del sistema, y la interfaz avisa al
//!   servidor de dónde ha quedado cada ventana ([`RootlessInput`]);
//! - lo que las aplicaciones piden al gestor de ventanas (activarse,
//!   maximizarse, minimizarse, que las arrastren por su propia barra) se
//!   apunta para que la interfaz se lo pida al sistema.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::draw::{Rect, Reservation};
use super::proto::Xid;
use super::server::{Focus, ROOT, SERVER_WINDOW, Server};
use crate::desktop::{DesktopInput, Framebuffer, MouseButton, SharedFrame};

/// Ventanas de primer nivel que se llegan a entregar a la interfaz.
const MAX_WINDOWS: usize = 256;
/// Peticiones a la interfaz que se guardan si no las recoge.
const MAX_REQUESTS: usize = 64;
/// Píxel de relleno donde la ventana no pinta nada (fuera de su forma).
const UNPAINTED: u32 = 0xff00_0000;

/// Una ventana de primer nivel tal como la ve la interfaz.
#[derive(Clone)]
pub struct RootlessWindow {
    pub id: u32,
    /// Esquina del interior de la ventana en la pantalla X (que coincide
    /// con el escritorio).
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub title: String,
    /// Menú, lista desplegable, aviso…: sin marco, sin foco y encima de
    /// todo, justo donde lo pone la aplicación.
    pub popup: bool,
    /// Lleva el marco del sistema (las aplicaciones que pintan su propia
    /// barra de título piden que no).
    pub decorated: bool,
    /// Ventana para la que esta es un diálogo.
    pub transient_for: Option<u32>,
    /// Tiene el foco del teclado de X.
    pub active: bool,
    /// Tamaño mínimo y máximo que declara la aplicación.
    pub min_size: Option<(u32, u32)>,
    pub max_size: Option<(u32, u32)>,
    /// La aplicación pidió verla maximizada o a pantalla completa.
    pub maximized: bool,
    pub fullscreen: bool,
    /// Su imagen (RGBA; transparente donde la ventana no pinta).
    pub frame: SharedFrame,
}

/// Lo que una aplicación pide que haga el sistema con una ventana.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootlessRequest {
    /// Traerla al frente y darle el foco.
    Activate(u32),
    Minimize(u32),
    /// Empezar a moverla con el botón que está pulsado (la aplicación
    /// pinta su propia barra de título).
    Move(u32),
    /// Empezar a estirarla por esos bordes: izquierdo, derecho, superior e
    /// inferior.
    Resize(u32, [bool; 4]),
}

/// Lo que el servidor publica para la interfaz.
#[derive(Default)]
pub struct RootlessView {
    /// Ventanas visibles, de la más baja a la más alta.
    pub windows: Vec<RootlessWindow>,
    /// Peticiones pendientes de atender.
    pub requests: Vec<RootlessRequest>,
}

pub type RootlessShared = Arc<Mutex<RootlessView>>;

/// Lo que la interfaz le cuenta al servidor en este modo. Las coordenadas
/// del ratón van en píxeles respecto a la ventana.
#[derive(Debug, Clone, PartialEq)]
pub enum RootlessInput {
    /// Teclado, texto y portapapeles, como en el modo de pestaña.
    Desktop(DesktopInput),
    Pointer {
        window: u32,
        x: i32,
        y: i32,
    },
    Button {
        window: u32,
        x: i32,
        y: i32,
        button: MouseButton,
        pressed: bool,
    },
    Wheel {
        window: u32,
        x: i32,
        y: i32,
        delta: i16,
        horizontal: bool,
    },
    /// La ventana nativa está ahora ahí (esquina de su interior) y mide
    /// eso: la de X la sigue.
    Geometry {
        window: u32,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    /// La ventana nativa tiene el foco.
    Focused(u32),
    /// Ninguna de las ventanas tiene ya el foco.
    Unfocused,
    /// Piden cerrarla (el aspa del marco del sistema).
    Close(u32),
    /// El sistema empezó a arrastrar la ventana: el botón que lo inició ya
    /// no se va a ver soltar.
    DragStarted,
    /// Tamaño que debe tener la pantalla X (el del escritorio).
    Screen {
        width: u32,
        height: u32,
    },
}

/// Ancho y alto.
type Size = (u32, u32);

/// Imagen de una ventana para la interfaz, con lo que ocupa apuntado en el
/// presupuesto de memoria.
struct WindowFrame {
    frame: SharedFrame,
    size: (u32, u32),
    _charge: Reservation,
}

/// Estado del modo de ventanas sueltas.
pub struct Rootless {
    shared: RootlessShared,
    frames: HashMap<Xid, WindowFrame>,
    requests: Vec<RootlessRequest>,
}

impl Rootless {
    /// Estado nuevo que publica en `shared`.
    pub fn new(shared: RootlessShared) -> Rootless {
        Rootless {
            shared,
            frames: HashMap::new(),
            requests: vec![],
        }
    }
}

impl Server {
    /// Hay algo que contar a la interfaz aunque no haya nada que pintar.
    pub fn rootless_pending(&self) -> bool {
        self.rootless
            .as_ref()
            .is_some_and(|r| !r.requests.is_empty())
    }

    /// Apunta una petición para la interfaz (solo en modo sin raíz).
    pub fn rootless_request(&mut self, request: RootlessRequest) {
        if let Some(rootless) = &mut self.rootless
            && rootless.requests.len() < MAX_REQUESTS
            && !rootless.requests.contains(&request)
        {
            rootless.requests.push(request);
        }
    }

    /// Ventanas de primer nivel que se enseñan, de abajo arriba.
    fn rootless_toplevels(&self) -> Vec<Xid> {
        let Some(root) = self.windows.get(&ROOT) else {
            return vec![];
        };
        root.children
            .iter()
            .copied()
            .filter(|id| {
                self.windows.get(id).is_some_and(|w| {
                    w.mapped && !w.input_only && w.owner != 0 && *id != SERVER_WINDOW
                })
            })
            .take(MAX_WINDOWS)
            .collect()
    }

    /// Recompone lo que haya cambiado de cada ventana de primer nivel sobre
    /// su imagen y publica la lista de ventanas. Hace las veces de la
    /// composición de la pantalla.
    pub fn rootless_publish(&mut self, scratch: &mut Vec<u32>) {
        let Some(mut rootless) = self.rootless.take() else {
            return;
        };
        let damage = std::mem::take(&mut self.damage);
        self.resized = false;
        let tops = self.rootless_toplevels();
        rootless.frames.retain(|id, _| tops.contains(id));
        let focus_top = match self.focus {
            Focus::Window(f) => self.toplevel_of(f),
            _ => None,
        };
        let mut windows = Vec::with_capacity(tops.len());
        for id in tops {
            let Some(w) = self.windows.get(&id) else {
                continue;
            };
            let (x, y) = (w.x + w.bw as i32, w.y + w.bw as i32);
            let size = (w.w, w.h);
            // La imagen de la ventana: nueva, o rehecha si cambió de tamaño.
            let fresh = rootless.frames.get(&id).is_none_or(|f| f.size != size);
            if fresh {
                rootless.frames.remove(&id);
                // (Sin sitio en el presupuesto de memoria, no se enseña.)
                let Some(charge) = self.budget.reserve(size.0 as u64 * size.1 as u64) else {
                    continue;
                };
                rootless.frames.insert(
                    id,
                    WindowFrame {
                        frame: Framebuffer::shared(size.0, size.1),
                        size,
                        _charge: charge,
                    },
                );
            }
            let frame = rootless.frames[&id].frame.clone();
            // Qué hay que recomponer, en coordenadas de la ventana.
            let bounds = Rect::new(0, 0, size.0 as i32, size.1 as i32);
            let parts: Vec<Rect> = if fresh {
                vec![bounds]
            } else {
                damage
                    .iter()
                    .map(|r| r.translate(-x, -y).intersect(&bounds))
                    .filter(|r| !r.is_empty())
                    .collect()
            };
            if !parts.is_empty() {
                let mut fb = super::lock(&frame);
                let stride = fb.width() as usize * 4;
                for r in parts {
                    let (pw, ph) = (r.width() as usize, r.height() as usize);
                    scratch.clear();
                    scratch.resize(pw * ph, UNPAINTED);
                    self.compose_window(id, r, scratch);
                    for row in 0..ph {
                        let start = (r.y0 as usize + row) * stride + r.x0 as usize * 4;
                        let dst = &mut fb.pixels[start..start + pw * 4];
                        for (d, s) in dst.chunks_exact_mut(4).zip(&scratch[row * pw..]) {
                            if *s == UNPAINTED {
                                d.copy_from_slice(&[0, 0, 0, 0]);
                            } else {
                                d[0] = (s >> 16) as u8;
                                d[1] = (s >> 8) as u8;
                                d[2] = *s as u8;
                                d[3] = 255;
                            }
                        }
                    }
                    fb.mark_dirty(r.x0 as u32, r.y0 as u32, pw as u32, ph as u32);
                }
            }
            let hints = self.size_limits(id);
            windows.push(RootlessWindow {
                id,
                x,
                y,
                width: size.0,
                height: size.1,
                title: self.window_title(id),
                popup: w.override_redirect,
                decorated: self.wants_decoration(w),
                transient_for: self.transient_for(id),
                active: focus_top == Some(id),
                min_size: hints.0,
                max_size: hints.1,
                maximized: w.max_h && w.max_v,
                fullscreen: w.fullscreen,
                frame,
            });
        }
        {
            let mut view = super::lock(&rootless.shared);
            view.windows = windows;
            let room = MAX_REQUESTS.saturating_sub(view.requests.len());
            view.requests.extend(rootless.requests.drain(..).take(room));
        }
        rootless.requests.clear();
        self.rootless = Some(rootless);
    }

    /// Sube una ventana de primer nivel en el orden de X, sin más efectos:
    /// así la que tiene el ratón encima es la que encuentra el puntero
    /// aunque en X se solape con otra.
    fn rootless_raise(&mut self, window: Xid) {
        let top = self
            .windows
            .get(&ROOT)
            .and_then(|root| root.children.last().copied());
        if top != Some(window) && self.windows.get(&window).is_some_and(|w| w.parent == ROOT) {
            self.restack(window, None, 0);
        }
    }

    /// Posición en la pantalla X de un punto de una ventana.
    fn rootless_point(&self, window: Xid, x: i32, y: i32) -> Option<(i32, i32)> {
        self.windows.get(&window).filter(|w| w.parent == ROOT)?;
        let (ox, oy) = self.abs_origin(window);
        Some((ox.saturating_add(x), oy.saturating_add(y)))
    }

    /// Aplica lo que cuenta la interfaz en el modo de ventanas sueltas.
    pub fn rootless_input(&mut self, input: RootlessInput) {
        if self.rootless.is_none() {
            return;
        }
        match input {
            RootlessInput::Desktop(input) => super::apply_input(self, input),
            RootlessInput::Pointer { window, x, y } => {
                if let Some((x, y)) = self.rootless_point(window, x, y) {
                    self.rootless_raise(window);
                    self.pointer_motion(x, y);
                }
            }
            RootlessInput::Button {
                window,
                x,
                y,
                button,
                pressed,
            } => {
                if let Some((x, y)) = self.rootless_point(window, x, y) {
                    self.rootless_raise(window);
                    self.pointer_motion(x, y);
                    let button = match button {
                        MouseButton::Left => 1,
                        MouseButton::Middle => 2,
                        MouseButton::Right => 3,
                    };
                    self.pointer_button(button, pressed);
                }
            }
            RootlessInput::Wheel {
                window,
                x,
                y,
                delta,
                horizontal,
            } => {
                if let Some((x, y)) = self.rootless_point(window, x, y) {
                    self.rootless_raise(window);
                    self.pointer_motion(x, y);
                    self.input_wheel(delta, horizontal);
                }
            }
            RootlessInput::Geometry {
                window,
                x,
                y,
                width,
                height,
            } => {
                let Some(w) = self.windows.get(&window) else {
                    return;
                };
                if w.parent != ROOT || w.override_redirect {
                    return;
                }
                let bw = w.bw;
                let (x, y) = (x.clamp(-32_768, 32_767), y.clamp(-32_768, 32_767));
                let (x, y) = (x - bw as i32, y - bw as i32);
                self.move_resize(window, x, y, width.max(1), height.max(1), bw);
                self.notify_configure(window);
                self.refresh_pointer_window();
            }
            RootlessInput::Focused(window) => {
                let managed = self
                    .windows
                    .get(&window)
                    .is_some_and(|w| w.mapped && self.managed(w));
                if managed && !self.rootless_is_active(window) {
                    self.wm_activate(window);
                }
            }
            RootlessInput::Unfocused => {
                self.rootless_dismiss_popups();
                self.set_focus(Focus::None);
            }
            RootlessInput::Close(window) => {
                if self.windows.get(&window).is_some_and(|w| self.managed(w)) {
                    self.wm_close(window);
                }
            }
            RootlessInput::DragStarted => {
                // El sistema se queda el ratón hasta que se suelte: aquí se
                // dan por soltados los botones.
                for button in 1..=3 {
                    if self.buttons & (crate::xserver::proto::S_BUTTON1 << (button - 1)) != 0 {
                        self.pointer_button(button, false);
                    }
                }
            }
            RootlessInput::Screen { width, height } => self.resize_screen(width, height),
        }
    }

    /// El usuario se ha ido a otra aplicación con un menú (o una lista
    /// desplegable) abierto. La aplicación lo tiene con el ratón capturado y
    /// espera una pulsación fuera para cerrarlo, pero esa pulsación ha sido
    /// en una ventana ajena y no va a llegar: se le da una donde no hay
    /// ninguna ventana, para que el menú no se quede flotando encima de todo.
    fn rootless_dismiss_popups(&mut self) {
        let menu_open = self.grab.as_ref().is_some_and(|g| !g.by_button);
        if !menu_open || self.buttons != 0 {
            return;
        }
        let (w, h) = (self.width as i32, self.height as i32);
        let candidates = [
            (0, 0),
            (w - 1, 0),
            (0, h - 1),
            (w - 1, h - 1),
            (w / 2, 0),
            (w / 2, h - 1),
            (0, h / 2),
            (w - 1, h / 2),
        ];
        let Some((x, y)) = candidates
            .into_iter()
            .find(|&(x, y)| self.window_at(x, y) == ROOT)
        else {
            return;
        };
        self.pointer_motion(x, y);
        self.pointer_button(1, true);
        self.pointer_button(1, false);
    }

    fn rootless_is_active(&self, window: Xid) -> bool {
        matches!(self.focus, Focus::Window(f) if self.toplevel_of(f) == Some(window))
    }

    /// Tamaños mínimo y máximo de `WM_NORMAL_HINTS`.
    fn size_limits(&self, id: Xid) -> (Option<Size>, Option<Size>) {
        let hints = self.property_u32s(id, super::server::A_WM_NORMAL_HINTS);
        let flags = hints.first().copied().unwrap_or(0);
        let pair = |i: usize| {
            let get = |i: usize| hints.get(i).copied().unwrap_or(0).min(32_767);
            (get(i) > 0 && get(i + 1) > 0).then(|| (get(i), get(i + 1)))
        };
        (
            if flags & 16 != 0 { pair(5) } else { None },
            if flags & 32 != 0 { pair(7) } else { None },
        )
    }
}
