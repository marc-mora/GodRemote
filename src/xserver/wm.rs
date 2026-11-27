//! Gestión de ventanas integrada.
//!
//! Normalmente no hay un gestor de ventanas remoto, así que el propio
//! servidor decora cada ventana de primer nivel: barra de título con su
//! nombre y un botón de cerrar, y un marco fino del que se puede tirar para
//! cambiar el tamaño. La decoración no es una ventana X: se pinta al
//! componer la pantalla, alrededor de la ventana del cliente, que sigue
//! siendo hija directa de la raíz.
//!
//! Si un cliente selecciona `SubstructureRedirect` en la raíz (un gestor de
//! ventanas de verdad), todo esto se desactiva.

use std::sync::Arc;

use super::draw::{Clip, Gc, Image, Painter, Rect};
use super::proto::*;
use super::server::{
    A_WM_HINTS, A_WM_NAME, A_WM_NORMAL_HINTS, Focus, Property, ROOT, SERVER_WINDOW, Server, Target,
    Window, WmDrag,
};

/// Alto de la barra de título.
pub const TITLE_H: i32 = 22;
/// Grosor del marco a los lados y por debajo.
pub const FRAME: i32 = 3;
/// Lado del botón de cerrar.
const CLOSE_SIZE: i32 = 16;
/// Zona de las esquinas inferiores que estira en las dos direcciones.
const CORNER: i32 = 18;
/// Bytes del título que se llegan a mirar.
const MAX_TITLE: usize = 512;

const ACTIVE_COLOR: u32 = 0x2f6fb5;
const INACTIVE_COLOR: u32 = 0x77808c;
const TITLE_TEXT: u32 = 0xffffff;
const CLOSE_COLOR: u32 = 0xc94f4f;

impl Server {
    /// La ventana lleva la decoración del gestor integrado.
    pub fn decorated(&self, w: &Window) -> bool {
        if self.wm_client.is_some()
            || w.parent != ROOT
            || w.override_redirect
            || w.input_only
            || w.owner == 0
            || w.id == SERVER_WINDOW
        {
            return false;
        }
        // Las aplicaciones Motif pueden pedir que no se decore.
        let hints = self.property_u32s(w.id, self.known.motif_wm_hints);
        !(hints.len() >= 3 && hints[0] & 2 != 0 && hints[2] == 0)
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

    fn close_button(frame: Rect) -> Rect {
        Rect::new(
            frame.x1 - CLOSE_SIZE - 4,
            frame.y0 + (TITLE_H - CLOSE_SIZE) / 2,
            CLOSE_SIZE,
            CLOSE_SIZE,
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

    fn render_title(&self, w: &Window, width: i32, active: bool) -> Image {
        let color = if active { ACTIVE_COLOR } else { INACTIVE_COLOR };
        let mut img = Image::new(width.max(1) as u32, TITLE_H as u32, 24);
        img.fill(img.bounds(), color);
        // Una línea clara arriba le da algo de relieve.
        img.fill(
            Rect::new(0, 0, width, 1),
            super::draw::blend_pixel(color, 0xffffff, 70),
        );
        let close = Self::close_button(Rect::new(0, 0, width, TITLE_H));
        let mut gc = Gc::new(24);
        gc.foreground = TITLE_TEXT;
        gc.clip = Clip::Rects(vec![Rect::new(6, 0, close.x0 - 12, TITLE_H)]);
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
        let (m, s) = (4, CLOSE_SIZE - 4);
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

    // --- Colocación, foco y cierre ------------------------------------------

    /// Elige la posición inicial de una ventana de primer nivel que se va a
    /// mostrar por primera vez.
    pub fn wm_before_map(&mut self, id: Xid) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        if w.placed || !self.decorated(w) {
            return;
        }
        let (x, y, ww, wh, bw) = (w.x, w.y, w.w, w.h, w.bw);
        let hints = self.property_u32s(id, A_WM_NORMAL_HINTS);
        let flags = hints.first().copied().unwrap_or(0);
        // USPosition, o una posición distinta del origen: se respeta.
        let asked = flags & 1 != 0 || x != 0 || y != 0;
        let (mut nx, mut ny) = if asked {
            (x, y)
        } else {
            let step = (self.cascade % 8) as i32 * 28;
            self.cascade += 1;
            (24 + step + FRAME, 24 + step + TITLE_H)
        };
        // La barra de título debe quedar a la vista.
        let (sw, sh) = (self.width as i32, self.height as i32);
        nx = nx.min(sw - 48).max(FRAME);
        ny = ny.min(sh - 8).max(TITLE_H);
        if let Some(w) = self.windows.get_mut(&id) {
            w.placed = true;
        }
        if (nx, ny) != (x, y) {
            self.move_resize(id, nx, ny, ww, wh, bw);
            self.notify_configure(id);
        }
    }

    /// Una ventana de primer nivel acaba de mostrarse: arriba y con el foco.
    pub fn wm_after_map(&mut self, id: Xid) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        if self.wm_client.is_some() || w.override_redirect || w.owner == 0 || w.input_only {
            return;
        }
        // Como cualquier gestor de ventanas (ICCCM): la ventana gestionada
        // lleva WM_STATE = «normal». Herramientas como xdotool o xprop lo
        // usan para distinguir la ventana de la aplicación de sus hijas.
        let wm_state = self.known.wm_state;
        self.set_property(
            id,
            Property {
                name: wm_state,
                kind: wm_state,
                format: 32,
                data: [1u32, 0].iter().flat_map(|v| v.to_le_bytes()).collect(),
            },
        );
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
        }
    }

    /// Sube una ventana de primer nivel y le da el foco del teclado.
    pub fn wm_activate(&mut self, id: Xid) {
        self.restack(id, None, 0);
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

    /// Tras encoger la pantalla, recoloca las ventanas cuya barra de título
    /// se haya quedado fuera.
    pub fn wm_keep_on_screen(&mut self) {
        let Some(root) = self.windows.get(&ROOT) else {
            return;
        };
        let (sw, sh) = (self.width as i32, self.height as i32);
        for id in root.children.clone() {
            let Some(w) = self.windows.get(&id) else {
                continue;
            };
            if !w.mapped || !self.decorated(w) {
                continue;
            }
            let (x, y, ww, wh, bw) = (w.x, w.y, w.w, w.h, w.bw);
            let (nx, ny) = (x.min(sw - 48).max(FRAME), y.min(sh - 8).max(TITLE_H));
            if (nx, ny) != (x, y) {
                self.move_resize(id, nx, ny, ww, wh, bw);
                self.notify_configure(id);
            }
        }
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
                if !c.override_redirect && c.owner != 0 && !c.input_only && !self.is_active(id) {
                    self.wm_activate(id);
                } else if !c.override_redirect && c.owner != 0 {
                    self.restack(id, None, 0);
                }
                return false;
            }
            let frame = self.frame_rect(c);
            if !frame.contains(x, y) {
                continue;
            }
            let geometry = (c.x, c.y, c.w as i32, c.h as i32);
            self.wm_activate(id);
            if button != 1 {
                return true;
            }
            self.wm_drag = Some(if y < outer.y0 {
                if Self::close_button(frame).contains(x, y) {
                    WmDrag::Close { window: id }
                } else {
                    WmDrag::Move {
                        window: id,
                        dx: x - geometry.0,
                        dy: y - geometry.1,
                    }
                }
            } else {
                let low = y >= outer.y1 - CORNER;
                WmDrag::Resize {
                    window: id,
                    left: x < outer.x0 || (y >= outer.y1 && x < outer.x0 + CORNER),
                    right: x >= outer.x1 || (y >= outer.y1 && x >= outer.x1 - CORNER),
                    bottom: y >= outer.y1 || low,
                    start: (x, y),
                    geometry,
                }
            });
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
        match drag {
            WmDrag::Close { .. } => {}
            WmDrag::Move { window, dx, dy } => {
                let Some(w) = self.windows.get(&window) else {
                    self.wm_drag = None;
                    return true;
                };
                let (ww, wh, bw) = (w.w, w.h, w.bw);
                // La barra de título no puede salirse de la pantalla.
                let nx = (x - dx).clamp(48 - ww as i32, sw - 48);
                let ny = (y - dy).clamp(TITLE_H, sh - 8);
                self.move_resize(window, nx, ny, ww, wh, bw);
                self.notify_configure(window);
            }
            WmDrag::Resize {
                window,
                left,
                right,
                bottom,
                start,
                geometry: (gx, gy, gw, gh),
            } => {
                let Some(bw) = self.windows.get(&window).map(|w| w.bw) else {
                    self.wm_drag = None;
                    return true;
                };
                let (dx, dy) = (x - start.0, y - start.1);
                let mut nw = gw;
                if right {
                    nw += dx;
                } else if left {
                    nw -= dx;
                }
                let nh = if bottom { gh + dy } else { gh };
                let (nw, nh) = self.constrain_size(window, nw, nh);
                // Al tirar del borde izquierdo el derecho se queda quieto.
                let nx = if left { gx + gw - nw } else { gx };
                self.move_resize(window, nx, gy, nw as u32, nh as u32, bw);
                self.notify_configure(window);
            }
        }
        true
    }

    /// Ajusta un tamaño a lo que la aplicación declara en `WM_NORMAL_HINTS`
    /// (mínimo, máximo e incrementos, como las celdas de un terminal).
    fn constrain_size(&self, id: Xid, w: i32, h: i32) -> (i32, i32) {
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
                w = w.min(get(7));
            }
            if get(8) > 0 {
                h = h.min(get(8));
            }
        }
        if flags & 64 != 0 {
            let (iw, ih) = (get(9).max(1), get(10).max(1));
            w = base_w + (w - base_w).max(0) / iw * iw;
            h = base_h + (h - base_h).max(0) / ih * ih;
        }
        (w.max(1), h.max(1))
    }

    /// Suelta de un botón: termina el arrastre en curso, si lo hay.
    pub fn wm_button_release(&mut self, button: u8) -> bool {
        if button != 1 {
            return false;
        }
        let Some(drag) = self.wm_drag.take() else {
            return false;
        };
        if let WmDrag::Close { window } = drag {
            let over = self.windows.get(&window).is_some_and(|w| {
                Self::close_button(self.frame_rect(w)).contains(self.pointer.0, self.pointer.1)
            });
            if over {
                self.wm_close(window);
            }
        }
        self.refresh_pointer_window();
        true
    }
}
