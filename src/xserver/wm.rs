//! Gestión de ventanas integrada.
//!
//! Normalmente no hay un gestor de ventanas remoto, así que el propio
//! servidor hace de gestor: decora cada ventana de primer nivel (barra de
//! título con su nombre, botones de maximizar y cerrar, y un marco fino del
//! que se puede tirar para cambiar el tamaño), decide dónde aparece, a cuál
//! va el teclado y cuál queda encima. La decoración no es una ventana X: se
//! pinta al componer la pantalla, alrededor de la ventana del cliente, que
//! sigue siendo hija directa de la raíz.
//!
//! Habla lo imprescindible de EWMH (`_NET_WM_STATE`, `_NET_WM_MOVERESIZE`,
//! `_NET_ACTIVE_WINDOW`…) para que las aplicaciones GTK y Qt, incluidas las
//! que pintan su propia barra de título, puedan maximizarse, moverse y
//! cambiar de tamaño.
//!
//! Si un cliente selecciona `SubstructureRedirect` en la raíz (un gestor de
//! ventanas de verdad), todo esto se desactiva.

use std::sync::Arc;

use super::draw::{Clip, Gc, Image, Painter, Rect, blend_pixel};
use super::proto::*;
use super::server::{
    A_ATOM, A_WM_HINTS, A_WM_NAME, A_WM_NORMAL_HINTS, Atoms, Focus, Property, ROOT, SERVER_WINDOW,
    Server, Target, Window,
};

/// Alto de la barra de título.
pub const TITLE_H: i32 = 22;
/// Grosor del marco a los lados y por debajo.
pub const FRAME: i32 = 3;
/// Lado de los botones de la barra.
const BUTTON_SIZE: i32 = 16;
/// Zona de las esquinas que estira en las dos direcciones.
const CORNER: i32 = 18;
/// Bytes del título que se llegan a mirar.
const MAX_TITLE: usize = 512;
/// Milisegundos entre dos clics para que cuenten como doble clic.
const DOUBLE_CLICK_MS: u32 = 400;
/// Átomo predefinido WM_TRANSIENT_FOR.
const A_WM_TRANSIENT_FOR: Xid = 68;
/// Átomo predefinido CARDINAL.
const A_CARDINAL: Xid = 6;
/// Átomo predefinido WINDOW.
const A_WINDOW: Xid = 33;

const ACTIVE_COLOR: u32 = 0x2f6fb5;
const INACTIVE_COLOR: u32 = 0x77808c;
const TITLE_TEXT: u32 = 0xffffff;
const CLOSE_COLOR: u32 = 0xc94f4f;

/// Botones de la barra de título.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WmButton {
    Close,
    Maximize,
}

/// Arrastre en curso de una ventana por su decoración (o pedido por la
/// propia aplicación con `_NET_WM_MOVERESIZE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WmDrag {
    Move {
        window: Xid,
        dx: i32,
        dy: i32,
    },
    Resize {
        window: Xid,
        /// Bordes que se mueven.
        left: bool,
        right: bool,
        top: bool,
        bottom: bool,
        start: (i32, i32),
        geometry: (i32, i32, i32, i32),
    },
    /// Botón de la barra pulsado: actúa si se suelta encima.
    Button {
        window: Xid,
        button: WmButton,
    },
}

impl WmDrag {
    pub fn window(&self) -> Xid {
        match self {
            WmDrag::Move { window, .. }
            | WmDrag::Resize { window, .. }
            | WmDrag::Button { window, .. } => *window,
        }
    }
}

/// Átomos de EWMH que entiende el gestor integrado.
pub struct Ewmh {
    supported: Xid,
    supporting_wm_check: Xid,
    wm_state: Xid,
    maximized_vert: Xid,
    maximized_horz: Xid,
    fullscreen: Xid,
    active_window: Xid,
    close_window: Xid,
    wm_moveresize: Xid,
    moveresize_window: Xid,
    frame_extents: Xid,
    request_frame_extents: Xid,
    window_type: Xid,
    /// Tipos de ventana que no llevan decoración.
    bare_types: Vec<Xid>,
    client_list: Xid,
    workarea: Xid,
    desktop_geometry: Xid,
    number_of_desktops: Xid,
    current_desktop: Xid,
}

impl Ewmh {
    pub fn new(atoms: &mut Atoms) -> Self {
        let mut a = |name: &str| atoms.intern(name);
        Ewmh {
            supported: a("_NET_SUPPORTED"),
            supporting_wm_check: a("_NET_SUPPORTING_WM_CHECK"),
            wm_state: a("_NET_WM_STATE"),
            maximized_vert: a("_NET_WM_STATE_MAXIMIZED_VERT"),
            maximized_horz: a("_NET_WM_STATE_MAXIMIZED_HORZ"),
            fullscreen: a("_NET_WM_STATE_FULLSCREEN"),
            active_window: a("_NET_ACTIVE_WINDOW"),
            close_window: a("_NET_CLOSE_WINDOW"),
            wm_moveresize: a("_NET_WM_MOVERESIZE"),
            moveresize_window: a("_NET_MOVERESIZE_WINDOW"),
            frame_extents: a("_NET_FRAME_EXTENTS"),
            request_frame_extents: a("_NET_REQUEST_FRAME_EXTENTS"),
            window_type: a("_NET_WM_WINDOW_TYPE"),
            bare_types: [
                "DOCK",
                "DESKTOP",
                "SPLASH",
                "NOTIFICATION",
                "TOOLTIP",
                "MENU",
                "POPUP_MENU",
                "DROPDOWN_MENU",
                "COMBO",
                "DND",
            ]
            .iter()
            .map(|t| a(&format!("_NET_WM_WINDOW_TYPE_{t}")))
            .collect(),
            client_list: a("_NET_CLIENT_LIST"),
            workarea: a("_NET_WORKAREA"),
            desktop_geometry: a("_NET_DESKTOP_GEOMETRY"),
            number_of_desktops: a("_NET_NUMBER_OF_DESKTOPS"),
            current_desktop: a("_NET_CURRENT_DESKTOP"),
        }
    }
}

/// Átomos de los mensajes que las aplicaciones mandan al gestor.
struct WmRequests {
    wm_state: Xid,
    maximized_horz: Xid,
    maximized_vert: Xid,
    fullscreen: Xid,
    active_window: Xid,
    close_window: Xid,
    request_frame_extents: Xid,
    wm_moveresize: Xid,
    moveresize_window: Xid,
}

fn cardinals(name: Xid, kind: Xid, values: &[u32]) -> Property {
    Property {
        name,
        kind,
        format: 32,
        data: values.iter().flat_map(|v| v.to_le_bytes()).collect(),
    }
}

impl Server {
    /// Deja en la raíz las propiedades con las que las aplicaciones
    /// reconocen a un gestor de ventanas EWMH.
    pub fn wm_announce(&mut self) {
        let e = &self.ewmh;
        let supported = [
            e.supported,
            e.supporting_wm_check,
            e.wm_state,
            e.maximized_vert,
            e.maximized_horz,
            e.fullscreen,
            e.active_window,
            e.close_window,
            e.wm_moveresize,
            e.moveresize_window,
            e.frame_extents,
            e.request_frame_extents,
            e.window_type,
            e.client_list,
            e.workarea,
            e.desktop_geometry,
            e.number_of_desktops,
            e.current_desktop,
            self.known.net_wm_name,
        ];
        let (w, h) = (self.width, self.height);
        let name = Property {
            name: self.known.net_wm_name,
            kind: self.known.utf8_string,
            format: 8,
            data: b"GodRemote".to_vec(),
        };
        let root = [
            cardinals(e.supported, A_ATOM, &supported),
            cardinals(e.supporting_wm_check, A_WINDOW, &[SERVER_WINDOW]),
            cardinals(e.number_of_desktops, A_CARDINAL, &[1]),
            cardinals(e.current_desktop, A_CARDINAL, &[0]),
            cardinals(e.desktop_geometry, A_CARDINAL, &[w, h]),
            cardinals(e.workarea, A_CARDINAL, &[0, 0, w, h]),
            cardinals(e.active_window, A_WINDOW, &[0]),
            cardinals(e.client_list, A_WINDOW, &[]),
        ];
        let check = cardinals(e.supporting_wm_check, A_WINDOW, &[SERVER_WINDOW]);
        for prop in root {
            self.set_property(ROOT, prop);
        }
        self.set_property(SERVER_WINDOW, check);
        self.set_property(SERVER_WINDOW, name);
    }

    /// Ventana de primer nivel de la que se ocupa el gestor integrado.
    pub fn managed(&self, w: &Window) -> bool {
        self.wm_client.is_none()
            && w.parent == ROOT
            && !w.override_redirect
            && !w.input_only
            && w.owner != 0
            && w.id != SERVER_WINDOW
    }

    /// La ventana lleva la decoración del gestor integrado.
    pub fn decorated(&self, w: &Window) -> bool {
        if !self.managed(w) || w.fullscreen {
            return false;
        }
        // Las aplicaciones pueden pedir que no se las decore (Motif; lo usan
        // GTK y Qt cuando pintan su propia barra de título)…
        let hints = self.property_u32s(w.id, self.known.motif_wm_hints);
        if hints.len() >= 3 && hints[0] & 2 != 0 && hints[2] == 0 {
            return false;
        }
        // …y hay tipos de ventana que nunca llevan marco.
        let kinds = self.property_u32s(w.id, self.ewmh.window_type);
        !kinds
            .first()
            .is_some_and(|k| self.ewmh.bare_types.contains(k))
    }

    /// Zona de pantalla de una ventana de primer nivel con su decoración.
    pub fn frame_rect(&self, w: &Window) -> Rect {
        let outer = w.outer();
        if !self.decorated(w) {
            return outer;
        }
        Rect {
            x0: outer.x0 - FRAME,
            y0: outer.y0 - TITLE_H,
            x1: outer.x1 + FRAME,
            y1: outer.y1 + FRAME,
        }
    }

    fn title_button(frame: Rect, button: WmButton) -> Rect {
        let slot = match button {
            WmButton::Close => 1,
            WmButton::Maximize => 2,
        };
        Rect::new(
            frame.x1 - slot * (BUTTON_SIZE + 4),
            frame.y0 + (TITLE_H - BUTTON_SIZE) / 2,
            BUTTON_SIZE,
            BUTTON_SIZE,
        )
    }

    fn is_active(&self, id: Xid) -> bool {
        match self.focus {
            Focus::Window(f) => f == id || self.toplevel_of(f) == Some(id),
            _ => false,
        }
    }

    /// Título de una ventana: `_NET_WM_NAME` o, si no, `WM_NAME`.
    pub fn window_title(&self, id: Xid) -> String {
        self.text_property(id, self.known.net_wm_name, MAX_TITLE)
            .or_else(|| self.text_property(id, A_WM_NAME, MAX_TITLE))
            .unwrap_or_default()
    }

    /// Ventana para la que esta es un diálogo (`WM_TRANSIENT_FOR`).
    fn transient_for(&self, id: Xid) -> Option<Xid> {
        self.property_u32s(id, A_WM_TRANSIENT_FOR)
            .first()
            .copied()
            .filter(|parent| *parent != id && *parent != ROOT && *parent != 0)
    }

    fn render_title(&self, w: &Window, width: i32, active: bool) -> Image {
        let color = if active { ACTIVE_COLOR } else { INACTIVE_COLOR };
        let mut img = Image::new(width.max(1) as u32, TITLE_H as u32, 24);
        img.fill(img.bounds(), color);
        // Una línea clara arriba le da algo de relieve.
        img.fill(Rect::new(0, 0, width, 1), blend_pixel(color, 0xffffff, 70));
        let whole = Rect::new(0, 0, width, TITLE_H);
        let close = Self::title_button(whole, WmButton::Close);
        let maximize = Self::title_button(whole, WmButton::Maximize);
        let mut gc = Gc::new(24);
        gc.foreground = TITLE_TEXT;
        gc.clip = Clip::Rects(vec![Rect::new(6, 0, maximize.x0 - 12, TITLE_H)]);
        let font = &self.title_font.core;
        let codes: Vec<u32> = self
            .window_title(w.id)
            .chars()
            .map(|c| {
                if font.info(c as u32).exists() {
                    c as u32
                } else {
                    '?' as u32
                }
            })
            .collect();
        let baseline = (TITLE_H + font.ascent as i32 - font.descent as i32) / 2;
        Painter::new(&mut img, &gc).text(font, 8, baseline, &codes);
        // Botón de cerrar: un cuadro rojo con un aspa.
        img.fill(close, CLOSE_COLOR);
        let mut gc = Gc::new(24);
        gc.foreground = TITLE_TEXT;
        gc.line_width = 2;
        let mut p = Painter::new(&mut img, &gc);
        let (m, s) = (4, BUTTON_SIZE - 4);
        p.line(
            &gc,
            close.x0 + m,
            close.y0 + m,
            close.x0 + s,
            close.y0 + s,
            true,
            true,
        );
        p.line(
            &gc,
            close.x0 + s,
            close.y0 + m,
            close.x0 + m,
            close.y0 + s,
            true,
            true,
        );
        // Botón de maximizar: un recuadro (dos, solapados, si ya lo está).
        img.fill(maximize, blend_pixel(color, 0xffffff, 40));
        let mut gc = Gc::new(24);
        gc.foreground = TITLE_TEXT;
        let mut p = Painter::new(&mut img, &gc);
        if w.max_h || w.max_v {
            p.rect_outline(&gc, maximize.x0 + 6, maximize.y0 + 3, 7, 6);
            let front = Rect::new(maximize.x0 + 3, maximize.y0 + 6, 8, 7);
            p.solid_copy(blend_pixel(color, 0xffffff, 40));
            p.rect(front);
            p.solid_copy(TITLE_TEXT);
            p.rect_outline(&gc, front.x0, front.y0, 7, 6);
        } else {
            p.rect_outline(&gc, maximize.x0 + 4, maximize.y0 + 4, 8, 8);
            p.rect(Rect::new(maximize.x0 + 4, maximize.y0 + 4, 9, 2));
        }
        img
    }

    /// Dibuja las barras de título que falten o hayan cambiado. Se llama
    /// antes de componer.
    pub fn prepare_decorations(&mut self) {
        let Some(root) = self.windows.get(&ROOT) else {
            return;
        };
        for id in root.children.clone() {
            let Some(w) = self.windows.get(&id) else {
                continue;
            };
            if !w.mapped || !self.decorated(w) {
                continue;
            }
            let width = self.frame_rect(w).width();
            let active = self.is_active(id);
            let fresh = w
                .title_cache
                .as_ref()
                .is_some_and(|(cw, ca, _)| *cw == width && *ca == active);
            if fresh {
                continue;
            }
            let img = Arc::new(self.render_title(w, width, active));
            if let Some(w) = self.windows.get_mut(&id) {
                w.title_cache = Some((width, active, img));
            }
        }
    }

    /// Pinta la decoración de una ventana al componer la pantalla.
    pub fn paint_decoration(&self, w: &Window, clip: Rect, target: &mut Target) {
        let frame = self.frame_rect(w);
        if frame.intersect(&clip).is_empty() {
            return;
        }
        let outer = w.outer();
        let active = w.title_cache.as_ref().is_some_and(|(_, a, _)| *a);
        let color = if active { ACTIVE_COLOR } else { INACTIVE_COLOR };
        let title = Rect {
            y1: outer.y0,
            ..frame
        };
        match &w.title_cache {
            Some((_, _, img)) => target.blit(img, frame.x0, frame.y0, title.intersect(&clip)),
            None => target.fill(title.intersect(&clip), color),
        }
        let body = Rect {
            y0: outer.y0,
            ..frame
        };
        for part in body.subtract(&outer) {
            target.fill(part.intersect(&clip), color);
        }
    }

    /// Ventana bajo el puntero para los eventos: sobre una decoración el
    /// puntero está, a efectos de los clientes, en la raíz.
    pub fn pointer_target(&self) -> Xid {
        let (x, y) = self.pointer;
        if self.wm_client.is_none()
            && let Some(root) = self.windows.get(&ROOT)
        {
            for child in root.children.iter().rev() {
                let Some(c) = self.windows.get(child) else {
                    continue;
                };
                if !c.mapped {
                    continue;
                }
                if c.outer().contains(x, y) {
                    break;
                }
                if self.frame_rect(c).contains(x, y) {
                    return ROOT;
                }
            }
        }
        self.window_at(x, y)
    }

    // --- Propiedades EWMH que mantiene el gestor ----------------------------

    /// Publica en la ventana su estado (`_NET_WM_STATE`).
    fn net_publish_state(&mut self, id: Xid) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        let e = &self.ewmh;
        let mut atoms = vec![];
        if w.max_v {
            atoms.push(e.maximized_vert);
        }
        if w.max_h {
            atoms.push(e.maximized_horz);
        }
        if w.fullscreen {
            atoms.push(e.fullscreen);
        }
        let prop = cardinals(e.wm_state, A_ATOM, &atoms);
        if self.windows.get(&id).and_then(|w| w.prop(prop.name)) != Some(&prop) {
            self.set_property(id, prop);
        }
    }

    /// Publica el grosor de la decoración (`_NET_FRAME_EXTENTS`).
    fn net_publish_extents(&mut self, id: Xid) {
        // Siempre cero, aunque la ventana lleve marco: el marco lo pinta el
        // servidor por fuera, sin meter la ventana dentro de otra, y Java
        // (AWT) da por hecho que unas extensiones distintas de cero vienen
        // con esa otra ventana: descuadra su contenido lo que mida la barra
        // de título. GTK y Qt solo las usan para afinar posiciones.
        let prop = cardinals(self.ewmh.frame_extents, A_CARDINAL, &[0; 4]);
        if self.windows.get(&id).and_then(|w| w.prop(prop.name)) != Some(&prop) {
            self.set_property(id, prop);
        }
    }

    fn net_publish_clients(&mut self) {
        let clients: Vec<u32> = self
            .windows
            .get(&ROOT)
            .map(|root| {
                root.children
                    .iter()
                    .copied()
                    .filter(|c| {
                        self.windows
                            .get(c)
                            .is_some_and(|w| w.mapped && self.managed(w))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let prop = cardinals(self.ewmh.client_list, A_WINDOW, &clients);
        if self.windows.get(&ROOT).and_then(|w| w.prop(prop.name)) != Some(&prop) {
            self.set_property(ROOT, prop);
        }
    }

    // --- Colocación, foco y cierre ------------------------------------------

    /// Prepara una ventana de primer nivel que se va a mostrar: posición
    /// inicial, tamaño que quepa en pantalla y estado inicial.
    pub fn wm_before_map(&mut self, id: Xid) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        if w.placed || !self.managed(w) {
            return;
        }
        let decorated = self.decorated(w);
        let (x, y, bw) = (w.x, w.y, w.bw);
        let (mut ww, mut wh) = (w.w as i32, w.h as i32);
        let original = (ww, wh);
        let hints = self.property_u32s(id, A_WM_NORMAL_HINTS);
        let flags = hints.first().copied().unwrap_or(0);
        let (sw, sh) = (self.width as i32, self.height as i32);
        let (side, top) = if decorated { (FRAME, TITLE_H) } else { (0, 0) };
        // Una ventana más grande que la pantalla (que aquí es una pestaña)
        // se encoge hasta caber, sin bajar de su tamaño mínimo.
        let room = (
            sw - 2 * side - 2 * bw as i32,
            sh - top - side - 2 * bw as i32,
        );
        if ww > room.0 || wh > room.1 {
            (ww, wh) = self.constrain_size(id, ww.min(room.0), wh.min(room.1));
        }
        // USPosition, o una posición distinta del origen: se respeta. Un
        // diálogo sin posición se centra sobre su ventana; el resto, en
        // cascada.
        let asked = flags & 1 != 0 || x != 0 || y != 0;
        let parent = self
            .transient_for(id)
            .and_then(|p| self.windows.get(&p))
            .filter(|p| p.mapped)
            .map(|p| p.outer());
        let (mut nx, mut ny) = if asked {
            (x, y)
        } else if let Some(p) = parent {
            (p.x0 + (p.width() - ww) / 2, p.y0 + (p.height() - wh) / 2)
        } else {
            let step = (self.cascade % 8) as i32 * 28;
            self.cascade += 1;
            (24 + step + side, 24 + step + top)
        };
        // Que quepa entera si puede y, en todo caso, con la barra a la vista.
        nx = nx.min(sw - ww - side - 2 * bw as i32).max(side);
        ny = ny.min(sh - wh - side - 2 * bw as i32).max(top);
        if let Some(w) = self.windows.get_mut(&id) {
            w.placed = true;
        }
        if (nx, ny, ww, wh) != (x, y, original.0, original.1) {
            self.move_resize(id, nx, ny, ww as u32, wh as u32, bw);
            self.notify_configure(id);
        }
        // Estado inicial pedido por la aplicación antes de mostrarse.
        let state = self.property_u32s(id, self.ewmh.wm_state);
        let wants = |atom: Xid| state.contains(&atom);
        let (v, h) = (
            wants(self.ewmh.maximized_vert),
            wants(self.ewmh.maximized_horz),
        );
        let full = wants(self.ewmh.fullscreen);
        if v || h || full {
            self.wm_set_state(id, h, v, full);
        }
    }

    /// Una ventana de primer nivel acaba de mostrarse: arriba y con el foco.
    pub fn wm_after_map(&mut self, id: Xid) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        if !self.managed(w) {
            // Menús, listas desplegables y avisos van por encima de todo.
            if w.parent == ROOT && w.override_redirect && self.wm_client.is_none() {
                self.restack(id, None, 0);
            }
            return;
        }
        // Como cualquier gestor de ventanas (ICCCM): la ventana gestionada
        // lleva WM_STATE = «normal». Herramientas como xdotool o xprop lo
        // usan para distinguir la ventana de la aplicación de sus hijas.
        let wm_state = self.known.wm_state;
        self.set_property(id, cardinals(wm_state, wm_state, &[1, 0]));
        self.net_publish_extents(id);
        self.net_publish_state(id);
        self.net_publish_clients();
        self.wm_activate(id);
    }

    /// Una ventana de primer nivel se ha ocultado: deja de estar gestionada.
    pub fn wm_after_unmap(&mut self, id: Xid) {
        let managed = self
            .windows
            .get(&id)
            .is_some_and(|w| w.parent == ROOT && !w.override_redirect);
        if managed && self.wm_client.is_none() {
            self.delete_property(id, self.known.wm_state);
            self.net_publish_clients();
        }
    }

    /// Sube una ventana (con sus diálogos encima, y los menús abiertos por
    /// encima de todo) y le da el foco del teclado.
    pub fn wm_activate(&mut self, id: Xid) {
        self.wm_raise(id);
        // WM_HINTS: si la aplicación dice que no quiere teclado, no se le da
        // el foco (pero sí el aviso WM_TAKE_FOCUS si lo pide).
        let hints = self.property_u32s(id, A_WM_HINTS);
        let wants_input = !(hints.len() >= 2 && hints[0] & 1 != 0 && hints[1] == 0);
        if wants_input {
            self.set_focus(Focus::Window(id));
        }
        let protocols = self.property_u32s(id, self.known.wm_protocols);
        if protocols.contains(&self.known.wm_take_focus) {
            self.send_wm_protocol(id, self.known.wm_take_focus);
        }
        let active = cardinals(self.ewmh.active_window, A_WINDOW, &[id]);
        if self.windows.get(&ROOT).and_then(|w| w.prop(active.name)) != Some(&active) {
            self.set_property(ROOT, active);
        }
    }

    /// Sube una ventana respetando el orden que deben guardar las demás.
    fn wm_raise(&mut self, id: Xid) {
        let Some(siblings) = self.windows.get(&ROOT).map(|r| r.children.clone()) else {
            return;
        };
        // La ventana, luego sus diálogos (y los de estos) y, por último, lo
        // que no pasa por el gestor (menús, listas…), que siempre va encima.
        let mut order = vec![id];
        let mut i = 0;
        while i < order.len() && order.len() < 64 {
            let parent = order[i];
            for s in &siblings {
                let dialog = self.windows.get(s).is_some_and(|w| w.mapped)
                    && self.transient_for(*s) == Some(parent)
                    && !order.contains(s);
                if dialog {
                    order.push(*s);
                }
            }
            i += 1;
        }
        for s in &siblings {
            let popup = self
                .windows
                .get(s)
                .is_some_and(|w| w.mapped && w.override_redirect);
            if popup && !order.contains(s) {
                order.push(*s);
            }
        }
        for window in order {
            self.restack(window, None, 0);
        }
    }

    fn send_wm_protocol(&mut self, id: Xid, protocol: Xid) {
        let Some(owner) = self.windows.get(&id).map(|w| w.owner) else {
            return;
        };
        let ev = Ev::new(CLIENT_MESSAGE, 32)
            .u32(id)
            .u32(self.known.wm_protocols)
            .u32(protocol)
            .u32(self.now())
            .done();
        self.send(owner, ev);
    }

    /// Cierra una ventana: con `WM_DELETE_WINDOW` si la aplicación lo
    /// admite; si no, cortando la conexión de su cliente.
    pub fn wm_close(&mut self, id: Xid) {
        let Some(owner) = self.windows.get(&id).map(|w| w.owner) else {
            return;
        };
        let protocols = self.property_u32s(id, self.known.wm_protocols);
        if protocols.contains(&self.known.wm_delete_window) {
            self.send_wm_protocol(id, self.known.wm_delete_window);
        } else {
            self.remove_client(owner);
        }
    }

    /// La ventana con el foco ya no se ve: pasa a la de primer nivel más
    /// alta que quede.
    pub fn wm_focus_lost(&mut self, gone: Xid) {
        let next = self.windows.get(&ROOT).and_then(|root| {
            root.children.iter().rev().copied().find(|c| {
                self.windows.get(c).is_some_and(|w| {
                    w.mapped && *c != gone && !w.override_redirect && !w.input_only && w.owner != 0
                })
            })
        });
        match next {
            Some(top) if self.wm_client.is_none() => self.set_focus(Focus::Window(top)),
            _ => self.set_focus(Focus::PointerRoot),
        }
    }

    /// La pantalla ha cambiado de tamaño: las ventanas maximizadas la siguen
    /// y las que hayan quedado fuera vuelven a la vista.
    pub fn wm_keep_on_screen(&mut self) {
        let (sw, sh) = (self.width, self.height);
        let geometry = cardinals(self.ewmh.desktop_geometry, A_CARDINAL, &[sw, sh]);
        let workarea = cardinals(self.ewmh.workarea, A_CARDINAL, &[0, 0, sw, sh]);
        self.set_property(ROOT, geometry);
        self.set_property(ROOT, workarea);
        let Some(root) = self.windows.get(&ROOT) else {
            return;
        };
        let (sw, sh) = (sw as i32, sh as i32);
        for id in root.children.clone() {
            let Some(w) = self.windows.get(&id) else {
                continue;
            };
            if !w.mapped || !self.managed(w) {
                continue;
            }
            if w.max_h || w.max_v || w.fullscreen {
                self.wm_apply_state(id);
                continue;
            }
            let top = if self.decorated(w) { TITLE_H } else { 0 };
            let (x, y, ww, wh, bw) = (w.x, w.y, w.w, w.h, w.bw);
            let (nx, ny) = (x.min(sw - 48).max(0), y.min(sh - 8).max(top));
            if (nx, ny) != (x, y) {
                self.move_resize(id, nx, ny, ww, wh, bw);
                self.notify_configure(id);
            }
        }
    }

    // --- Maximizar y pantalla completa --------------------------------------

    /// Fija el estado de una ventana (maximizada a lo ancho, a lo alto, a
    /// pantalla completa) y ajusta su geometría.
    pub fn wm_set_state(&mut self, id: Xid, horizontal: bool, vertical: bool, fullscreen: bool) {
        // Una ventana de tamaño fijo (mínimo = máximo en WM_NORMAL_HINTS,
        // como tantos diálogos) no se maximiza.
        let hints = self.property_u32s(id, A_WM_NORMAL_HINTS);
        let fixed = hints.len() >= 9
            && hints[0] & 16 != 0
            && hints[0] & 32 != 0
            && (hints[5], hints[6]) == (hints[7], hints[8]);
        let (horizontal, vertical) = (horizontal && !fixed, vertical && !fixed);
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        if (w.max_h, w.max_v, w.fullscreen) == (horizontal, vertical, fullscreen) {
            return;
        }
        let was_plain = !(w.max_h || w.max_v || w.fullscreen);
        if was_plain {
            w.saved = Some((w.x, w.y, w.w, w.h));
        }
        (w.max_h, w.max_v, w.fullscreen) = (horizontal, vertical, fullscreen);
        w.title_cache = None;
        // Entrar o salir de pantalla completa pone o quita la decoración.
        self.damage_screen(self.screen_rect());
        self.wm_apply_state(id);
        self.net_publish_extents(id);
        self.net_publish_state(id);
    }

    /// Coloca una ventana según su estado (o la devuelve a donde estaba).
    fn wm_apply_state(&mut self, id: Xid) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        let (sw, sh) = (self.width as i32, self.height as i32);
        let bw = w.bw as i32;
        let (mut x, mut y, mut ww, mut wh) = w.saved.unwrap_or((w.x, w.y, w.w, w.h));
        if w.fullscreen {
            (x, y, ww, wh) = (-bw, -bw, sw as u32, sh as u32);
        } else {
            let (side, top) = if self.decorated(w) {
                (FRAME, TITLE_H)
            } else {
                (0, 0)
            };
            if w.max_h {
                x = side;
                ww = (sw - 2 * side - 2 * bw).max(1) as u32;
            }
            if w.max_v {
                y = top;
                wh = (sh - top - side - 2 * bw).max(1) as u32;
            }
        }
        let plain = !(w.max_h || w.max_v || w.fullscreen);
        let bw = w.bw;
        self.move_resize(id, x, y, ww, wh, bw);
        self.notify_configure(id);
        if plain && let Some(w) = self.windows.get_mut(&id) {
            w.saved = None;
        }
        self.refresh_pointer_window();
    }

    fn wm_toggle_maximize(&mut self, id: Xid) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        let on = !(w.max_h && w.max_v);
        let fullscreen = w.fullscreen;
        self.wm_set_state(id, on, on, fullscreen);
    }

    // --- Peticiones de las aplicaciones (EWMH) -------------------------------

    /// Mensaje de cliente dirigido a la raíz: lo que una aplicación le pide
    /// al gestor de ventanas.
    pub fn wm_client_message(&mut self, ev: &Event) {
        let (window, kind) = (ev.u32_at(4), ev.u32_at(8));
        let l = |i: usize| ev.u32_at(12 + 4 * i);
        let Some(w) = self.windows.get(&window) else {
            return;
        };
        if ev.0[1] != 32 || !self.managed(w) {
            return;
        }
        if super::requests::tracing() {
            eprintln!(
                "[x11] al gestor de ventanas: {} para {window:#x}: {:?}",
                self.atoms.name(kind).unwrap_or("?"),
                (0..5).map(l).collect::<Vec<_>>()
            );
        }
        let (max_h, max_v, fullscreen, mapped) = (w.max_h, w.max_v, w.fullscreen, w.mapped);
        let e = WmRequests {
            wm_state: self.ewmh.wm_state,
            maximized_horz: self.ewmh.maximized_horz,
            maximized_vert: self.ewmh.maximized_vert,
            fullscreen: self.ewmh.fullscreen,
            active_window: self.ewmh.active_window,
            close_window: self.ewmh.close_window,
            request_frame_extents: self.ewmh.request_frame_extents,
            wm_moveresize: self.ewmh.wm_moveresize,
            moveresize_window: self.ewmh.moveresize_window,
        };
        if kind == e.wm_state {
            // l[0]: 0 quitar, 1 poner, 2 alternar; l[1] y l[2]: qué estados.
            let apply = |current: bool, atom: Xid| -> bool {
                if l(1) != atom && l(2) != atom {
                    return current;
                }
                match l(0) {
                    0 => false,
                    1 => true,
                    _ => !current,
                }
            };
            let h = apply(max_h, e.maximized_horz);
            let v = apply(max_v, e.maximized_vert);
            let full = apply(fullscreen, e.fullscreen);
            if mapped {
                self.wm_set_state(window, h, v, full);
            }
        } else if kind == e.active_window {
            if mapped {
                self.wm_activate(window);
            }
        } else if kind == e.close_window {
            self.wm_close(window);
        } else if kind == e.request_frame_extents {
            self.net_publish_extents(window);
        } else if kind == e.wm_moveresize {
            if mapped {
                self.wm_start_moveresize(window, l(2));
            }
        } else if kind == e.moveresize_window {
            // l[0]: bits 8–11 dicen cuáles de x, y, ancho y alto vienen.
            let Some(w) = self.windows.get(&window) else {
                return;
            };
            let flags = l(0);
            let pick = |bit: u32, value: u32, current: i32| {
                if flags & (1 << bit) != 0 {
                    (value as i32).clamp(-32_768, 32_767)
                } else {
                    current
                }
            };
            let (x, y) = (pick(8, l(1), w.x), pick(9, l(2), w.y));
            let (ww, wh) = (pick(10, l(3), w.w as i32), pick(11, l(4), w.h as i32));
            let bw = w.bw;
            let (ww, wh) = self.constrain_size(window, ww, wh);
            self.move_resize(window, x, y, ww as u32, wh as u32, bw);
            self.notify_configure(window);
        }
    }

    /// `_NET_WM_MOVERESIZE`: la aplicación (que pinta su propia barra) pide
    /// al gestor que mueva o estire la ventana con el botón que está pulsado.
    fn wm_start_moveresize(&mut self, id: Xid, direction: u32) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        if self.buttons == 0 || w.max_h || w.max_v || w.fullscreen {
            return;
        }
        let (x, y) = self.pointer;
        let geometry = (w.x, w.y, w.w as i32, w.h as i32);
        let drag = match direction {
            // 0 arriba-izquierda y sigue en el sentido de las agujas.
            0..=7 => WmDrag::Resize {
                window: id,
                left: matches!(direction, 0 | 6 | 7),
                right: matches!(direction, 2..=4),
                top: matches!(direction, 0..=2),
                bottom: matches!(direction, 4..=6),
                start: (x, y),
                geometry,
            },
            8 => WmDrag::Move {
                window: id,
                dx: x - geometry.0,
                dy: y - geometry.1,
            },
            _ => return,
        };
        // La aplicación suelta su captura al pedirlo; por si acaso.
        self.end_grab();
        self.wm_drag = Some(drag);
    }

    // --- Ratón sobre las decoraciones ---------------------------------------

    /// Pulsación de un botón. Devuelve `true` si se la queda el gestor (cayó
    /// en una decoración); un clic dentro de una ventana la activa y sigue
    /// su camino hacia el cliente.
    pub fn wm_button_press(&mut self, button: u8) -> bool {
        if self.wm_client.is_some() {
            return false;
        }
        let (x, y) = self.pointer;
        let Some(root) = self.windows.get(&ROOT) else {
            return false;
        };
        for id in root.children.iter().rev().copied().collect::<Vec<_>>() {
            let Some(c) = self.windows.get(&id) else {
                continue;
            };
            if !c.mapped {
                continue;
            }
            let outer = c.outer();
            if outer.contains(x, y) {
                if self.managed(c) {
                    if self.is_active(id) {
                        self.wm_raise(id);
                    } else {
                        self.wm_activate(id);
                    }
                }
                return false;
            }
            let frame = self.frame_rect(c);
            if !frame.contains(x, y) {
                continue;
            }
            let geometry = (c.x, c.y, c.w as i32, c.h as i32);
            let maximized = c.max_h || c.max_v;
            self.wm_activate(id);
            if button != 1 {
                return true;
            }
            // Los bordes (y la franja de arriba de la barra) estiran; las
            // esquinas, en las dos direcciones.
            let top = y < frame.y0 + FRAME;
            let (left, right, bottom) = (x < outer.x0, x >= outer.x1, y >= outer.y1);
            if (top || left || right || bottom) && !maximized {
                let near_top = y < frame.y0 + CORNER;
                let near_bottom = y >= frame.y1 - CORNER;
                let near_left = x < frame.x0 + CORNER;
                let near_right = x >= frame.x1 - CORNER;
                self.wm_drag = Some(WmDrag::Resize {
                    window: id,
                    left: left || ((top || bottom) && near_left),
                    right: right || ((top || bottom) && near_right),
                    top: top || ((left || right) && near_top),
                    bottom: bottom || ((left || right) && near_bottom),
                    start: (x, y),
                    geometry,
                });
                return true;
            }
            for which in [WmButton::Close, WmButton::Maximize] {
                if Self::title_button(frame, which).contains(x, y) {
                    self.wm_drag = Some(WmDrag::Button {
                        window: id,
                        button: which,
                    });
                    return true;
                }
            }
            if y >= outer.y0 {
                return true; // marco de una ventana maximizada
            }
            // Doble clic en el título: maximizar o restaurar.
            let now = self.now();
            let double = self.last_title_click.is_some_and(|(w, time, pos)| {
                w == id
                    && now.wrapping_sub(time) <= DOUBLE_CLICK_MS
                    && (pos.0 - x).abs() <= 4
                    && (pos.1 - y).abs() <= 4
            });
            if double {
                self.last_title_click = None;
                self.wm_toggle_maximize(id);
                return true;
            }
            self.last_title_click = Some((id, now, (x, y)));
            if !maximized {
                self.wm_drag = Some(WmDrag::Move {
                    window: id,
                    dx: x - geometry.0,
                    dy: y - geometry.1,
                });
            }
            return true;
        }
        false
    }

    /// Movimiento del puntero durante un arrastre del gestor.
    pub fn wm_drag_motion(&mut self) -> bool {
        let Some(drag) = self.wm_drag else {
            return false;
        };
        let (x, y) = self.pointer;
        let (sw, sh) = (self.width as i32, self.height as i32);
        let Some(w) = self.windows.get(&drag.window()) else {
            self.wm_drag = None;
            return true;
        };
        let (ww, wh, bw) = (w.w, w.h, w.bw);
        let top = if self.decorated(w) { TITLE_H } else { 0 };
        match drag {
            WmDrag::Button { .. } => {}
            WmDrag::Move { window, dx, dy } => {
                // La barra de título no puede salirse de la pantalla.
                let nx = (x - dx).clamp(48 - ww as i32, sw - 48);
                let ny = (y - dy).clamp(top, sh - 8);
                self.move_resize(window, nx, ny, ww, wh, bw);
                self.notify_configure(window);
            }
            WmDrag::Resize {
                window,
                left,
                right,
                top: from_top,
                bottom,
                start,
                geometry: (gx, gy, gw, gh),
            } => {
                let (dx, dy) = (x - start.0, y - start.1);
                let mut nw = gw;
                if right {
                    nw += dx;
                } else if left {
                    nw -= dx;
                }
                let mut nh = gh;
                if bottom {
                    nh += dy;
                } else if from_top {
                    nh -= dy;
                }
                let (nw, nh) = self.constrain_size(window, nw, nh);
                // Al tirar de un borde, el contrario se queda quieto.
                let nx = if left { gx + gw - nw } else { gx };
                let ny = if from_top {
                    (gy + gh - nh).max(top)
                } else {
                    gy
                };
                self.move_resize(window, nx, ny, nw as u32, nh as u32, bw);
                self.notify_configure(window);
            }
        }
        true
    }

    /// Ajusta un tamaño a lo que la aplicación declara en `WM_NORMAL_HINTS`
    /// (mínimo, máximo e incrementos, como las celdas de un terminal).
    pub fn constrain_size(&self, id: Xid, w: i32, h: i32) -> (i32, i32) {
        let hints = self.property_u32s(id, A_WM_NORMAL_HINTS);
        let flags = hints.first().copied().unwrap_or(0);
        // Los valores los pone el cliente: se acotan antes de hacer cuentas.
        let get = |i: usize| hints.get(i).copied().unwrap_or(0).min(32_767) as i32;
        let (mut min_w, mut min_h) = (1, 1);
        if flags & 16 != 0 {
            (min_w, min_h) = (get(5).max(1), get(6).max(1));
        }
        let (base_w, base_h) = if flags & 256 != 0 {
            (get(15), get(16))
        } else if flags & 16 != 0 {
            (min_w, min_h)
        } else {
            (0, 0)
        };
        let (mut w, mut h) = (w.max(min_w).max(24), h.max(min_h).max(8));
        if flags & 32 != 0 {
            if get(7) > 0 {
                w = w.min(get(7).max(min_w));
            }
            if get(8) > 0 {
                h = h.min(get(8).max(min_h));
            }
        }
        if flags & 64 != 0 {
            let (iw, ih) = (get(9).max(1), get(10).max(1));
            w = base_w + (w - base_w).max(0) / iw * iw;
            h = base_h + (h - base_h).max(0) / ih * ih;
        }
        (w.clamp(1, 32_767), h.clamp(1, 32_767))
    }

    /// Suelta de un botón: termina el arrastre en curso, si lo hay.
    pub fn wm_button_release(&mut self, _button: u8) -> bool {
        let Some(drag) = self.wm_drag.take() else {
            return false;
        };
        if let WmDrag::Button { window, button } = drag {
            let over = self.windows.get(&window).is_some_and(|w| {
                Self::title_button(self.frame_rect(w), button)
                    .contains(self.pointer.0, self.pointer.1)
            });
            match button {
                WmButton::Close if over => self.wm_close(window),
                WmButton::Maximize if over => self.wm_toggle_maximize(window),
                _ => {}
            }
        }
        self.refresh_pointer_window();
        true
    }
}
