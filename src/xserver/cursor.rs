//! Cursores: qué forma de puntero pide la ventana que hay debajo de él.
//!
//! El servidor no dibuja el puntero: le dice a la interfaz cuál toca
//! ([`DesktopEvent::Cursor`](crate::desktop::DesktopEvent)) y ella lo pone.
//! Hay dos clases:
//!
//! - los de la fuente «cursor» de X (`CreateGlyphCursor`: la I de texto, la
//!   mano, las flechas de redimensionar…), que se traducen a una forma
//!   conocida y los dibuja el sistema local;
//! - los de imagen (`CreateCursor` con dos mapas de bits y los de RENDER,
//!   que es como llegan los temas de cursores de GTK y Qt vía libXcursor),
//!   que se entregan como imagen para pintarlos por software.
//!
//! Un cursor sigue vivo mientras alguna ventana lo use, aunque el cliente
//! ya lo haya liberado (lo habitual en Xlib es liberarlo nada más ponerlo).
//! Sus imágenes cuentan en el presupuesto global de memoria de imagen.

use std::sync::Arc;

use super::draw::Reservation;
use super::proto::*;
use super::server::{ClientId, Server};
use super::wm::WmDrag;
use crate::desktop::{CursorFrame, CursorImage, CursorShape, DesktopCursor};

/// Lado máximo de la imagen de un cursor (lo que sobre se recorta).
pub const MAX_CURSOR_SIDE: u32 = 256;
/// Fotogramas de un cursor animado.
const MAX_ANIM_FRAMES: usize = 128;
/// Duración mínima de un fotograma (para no repintar sin descanso).
const MIN_FRAME_MS: u32 = 16;

/// Un cursor creado por un cliente.
#[derive(Debug)]
pub struct CursorData {
    pub cursor: DesktopCursor,
    /// Lo que ocupa su imagen en el presupuesto de memoria.
    _charge: Option<Reservation>,
    /// Cursores de los que salen sus fotogramas (si es animado): mientras
    /// viva este, sus imágenes siguen contando.
    _parts: Vec<Arc<CursorData>>,
}

impl PartialEq for CursorData {
    fn eq(&self, other: &Self) -> bool {
        self.cursor == other.cursor
    }
}

impl Eq for CursorData {}

/// Forma que corresponde a un carácter de la fuente «cursor» de X (los
/// pares; los impares son sus máscaras).
pub fn glyph_shape(glyph: u16) -> CursorShape {
    use CursorShape::*;
    match glyph & !1 {
        152 => Text,                                     // xterm
        58 | 60 => Hand,                                 // hand1, hand2
        150 | 26 => Wait,                                // watch, clock
        30 | 32 | 34 | 36 | 90 | 128 | 130 => Crosshair, // cross, crosshair, plus, tcross…
        52 => Move,                                      // fleur
        108 => ResizeHorizontal,                         // sb_h_double_arrow
        116 | 42 => ResizeVertical,                      // sb_v_double_arrow, double_arrow
        70 | 110 => ResizeW,                             // left_side, sb_left_arrow
        96 | 112 => ResizeE,                             // right_side, sb_right_arrow
        138 | 114 => ResizeN,                            // top_side, sb_up_arrow
        16 | 106 => ResizeS,                             // bottom_side, sb_down_arrow
        134 | 144 => ResizeNw,                           // top_left_corner, ul_angle
        136 | 148 => ResizeNe,                           // top_right_corner, ur_angle
        12 | 76 => ResizeSw,                             // bottom_left_corner, ll_angle
        14 | 78 | 120 => ResizeSe,                       // bottom_right_corner, lr_angle, sizing
        92 => Help,                                      // question_arrow
        0 | 24 | 88 => NotAllowed,                       // X_cursor, circle, pirate
        _ => Default,
    }
}

/// Forma para estirar una ventana por los bordes indicados.
pub fn edge_shape(left: bool, right: bool, top: bool, bottom: bool) -> CursorShape {
    use CursorShape::*;
    match (left, right, top, bottom) {
        (true, _, true, _) => ResizeNw,
        (_, true, true, _) => ResizeNe,
        (true, _, _, true) => ResizeSw,
        (_, true, _, true) => ResizeSe,
        (true, ..) => ResizeW,
        (_, true, ..) => ResizeE,
        (_, _, true, _) => ResizeN,
        (_, _, _, true) => ResizeS,
        _ => Default,
    }
}

impl Server {
    /// Cursor con ese identificador, o `BadCursor`.
    pub(super) fn cursor(&self, id: Xid) -> XResult<Arc<CursorData>> {
        self.cursors
            .get(&id)
            .map(|(_, data)| data.clone())
            .ok_or(XError::new(BAD_CURSOR, id))
    }

    /// Cursor opcional de una petición: 0 es «ninguno».
    pub(super) fn optional_cursor(&self, id: Xid) -> XResult<Option<Arc<CursorData>>> {
        if id == 0 {
            Ok(None)
        } else {
            self.cursor(id).map(Some)
        }
    }

    /// Da de alta un cursor con una de las formas conocidas.
    pub(super) fn add_shape_cursor(&mut self, c: ClientId, id: Xid, shape: CursorShape) {
        let data = CursorData {
            cursor: DesktopCursor::Shape(shape),
            _charge: None,
            _parts: vec![],
        };
        self.cursors.insert(id, (c, Arc::new(data)));
    }

    /// Da de alta un cursor de imagen. `pixel(x, y)` devuelve el color ARGB
    /// (con el alfa ya multiplicado) de cada punto de la imagen original, de
    /// `width × height`; lo que pase del lado máximo se recorta.
    pub(super) fn add_image_cursor(
        &mut self,
        c: ClientId,
        id: Xid,
        (width, height): (u32, u32),
        hot: (u32, u32),
        pixel: impl Fn(u32, u32) -> u32,
    ) {
        let (w, h) = (width.min(MAX_CURSOR_SIDE), height.min(MAX_CURSOR_SIDE));
        if w == 0 || h == 0 {
            self.add_shape_cursor(c, id, CursorShape::Hidden);
            return;
        }
        // Sin sitio en el presupuesto de memoria no se contesta con un error
        // (GTK y Qt se cierran ante un error X que no esperan): el cursor
        // existe, pero con la flecha de siempre.
        let Some(charge) = self.budget.reserve(w as u64 * h as u64) else {
            self.add_shape_cursor(c, id, CursorShape::Default);
            return;
        };
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        let mut visible = false;
        for y in 0..h {
            for x in 0..w {
                let argb = pixel(x, y);
                visible |= argb >> 24 != 0;
                rgba.extend_from_slice(&[
                    (argb >> 16) as u8,
                    (argb >> 8) as u8,
                    argb as u8,
                    (argb >> 24) as u8,
                ]);
            }
        }
        // Un cursor en blanco es la forma habitual de esconder el puntero.
        if !visible {
            self.add_shape_cursor(c, id, CursorShape::Hidden);
            return;
        }
        let image = CursorImage {
            width: w,
            height: h,
            hot_x: hot.0.min(w - 1),
            hot_y: hot.1.min(h - 1),
            rgba,
        };
        let frame = CursorFrame {
            image: Arc::new(image),
            delay_ms: 0,
        };
        let data = CursorData {
            cursor: DesktopCursor::Image(Arc::from(vec![frame])),
            _charge: Some(charge),
            _parts: vec![],
        };
        self.cursors.insert(id, (c, Arc::new(data)));
    }

    /// `CreateCursor`: dos mapas de bits (figura y máscara) y dos colores.
    pub(super) fn create_cursor(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let (id, source, mask) = (rd.u32()?, rd.u32()?, rd.u32()?);
        let mut color = || -> XResult<u32> {
            let (r, g, b) = (rd.u16()?, rd.u16()?, rd.u16()?);
            Ok(0xff00_0000 | (r as u32 >> 8) << 16 | (g as u32 >> 8) << 8 | b as u32 >> 8)
        };
        let (fore, back) = (color()?, color()?);
        let hot = (rd.u16()? as u32, rd.u16()? as u32);
        self.check_new_id(c, id)?;
        let bitmap = |id: Xid| {
            let p = self.pixmaps.get(&id).ok_or(XError::new(BAD_PIXMAP, id))?;
            if p.image.depth != 1 {
                return Err(XError::new(BAD_MATCH, id));
            }
            Ok(p.image.clone())
        };
        let source = bitmap(source)?;
        let mask = if mask == 0 { None } else { Some(bitmap(mask)?) };
        // (Como X.Org, se admite el punto caliente justo en el borde: GTK
        // crea su cursor invisible de 1×1 con el punto en (1, 1).)
        if mask
            .as_ref()
            .is_some_and(|m| (m.w, m.h) != (source.w, source.h))
            || hot.0 > source.w as u32
            || hot.1 > source.h as u32
        {
            return Err(XError::new(BAD_MATCH, id));
        }
        let size = (source.w as u32, source.h as u32);
        self.add_image_cursor(c, id, size, hot, |x, y| {
            let (x, y) = (x as i32, y as i32);
            if mask.as_ref().is_some_and(|m| m.get(x, y) & 1 == 0) {
                0
            } else if source.get(x, y) & 1 != 0 {
                fore
            } else {
                back
            }
        });
        Ok(())
    }

    /// `CreateGlyphCursor`: un carácter de una fuente; de la fuente «cursor»
    /// salen las formas de siempre.
    pub(super) fn create_glyph_cursor(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let (id, source, mask) = (rd.u32()?, rd.u32()?, rd.u32()?);
        let (glyph, _mask_glyph) = (rd.u16()?, rd.u16()?);
        self.check_new_id(c, id)?;
        for font in [source, mask] {
            if font != 0 && !self.fonts.contains_key(&font) {
                return Err(XError::new(BAD_FONT, font));
            }
        }
        let standard = self
            .fonts
            .get(&source)
            .is_some_and(|(_, font)| font.name == "cursor");
        let shape = if standard {
            glyph_shape(glyph)
        } else {
            CursorShape::Default
        };
        self.add_shape_cursor(c, id, shape);
        Ok(())
    }

    /// RENDER `CreateAnimCursor`: una lista de cursores con su duración.
    pub(super) fn create_anim_cursor(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let id = rd.u32()?;
        self.check_new_id(c, id)?;
        if rd.remaining() / 8 > MAX_ANIM_FRAMES {
            return Err(XError::new(BAD_ALLOC, id));
        }
        let mut parts = vec![];
        let mut frames = vec![];
        let mut first = None;
        while rd.remaining() >= 8 {
            let (cursor, delay) = (rd.u32()?, rd.u32()?);
            let part = self.cursor(cursor)?;
            if let DesktopCursor::Image(images) = &part.cursor {
                frames.extend(images.iter().map(|f| CursorFrame {
                    image: f.image.clone(),
                    delay_ms: delay.clamp(MIN_FRAME_MS, 10_000),
                }));
            }
            first.get_or_insert_with(|| part.cursor.clone());
            parts.push(part);
        }
        // Sin fotogramas de imagen (o sin ninguno) vale el primero tal cual.
        let cursor = if frames.is_empty() {
            first.ok_or(XError::new(BAD_MATCH, id))?
        } else {
            DesktopCursor::Image(Arc::from(frames))
        };
        let data = CursorData {
            cursor,
            _charge: None,
            _parts: parts,
        };
        self.cursors.insert(id, (c, Arc::new(data)));
        Ok(())
    }

    /// Cursor que toca ahora mismo: el de un arrastre del gestor de
    /// ventanas, el de la captura del puntero o el de la ventana que está
    /// debajo (o, si no tiene, el de su antecesora más cercana que tenga).
    pub fn current_cursor(&self) -> DesktopCursor {
        let start = match (&self.wm_drag, &self.grab) {
            (Some(drag), _) => return DesktopCursor::Shape(drag_shape(drag)),
            (None, Some(grab)) => {
                if let Some(cursor) = &grab.cursor {
                    return cursor.cursor.clone();
                }
                let inside = self.pointer_window == grab.window
                    || self.is_ancestor(grab.window, self.pointer_window);
                if inside {
                    self.pointer_window
                } else {
                    grab.window
                }
            }
            (None, None) => {
                if let Some(shape) = self.wm_hover_shape() {
                    return DesktopCursor::Shape(shape);
                }
                self.pointer_window
            }
        };
        self.ancestry(start)
            .iter()
            .find_map(|id| self.windows.get(id)?.cursor.as_ref())
            .map_or(DesktopCursor::Shape(CursorShape::Default), |data| {
                data.cursor.clone()
            })
    }
}

/// Forma del puntero durante un arrastre del gestor de ventanas.
fn drag_shape(drag: &WmDrag) -> CursorShape {
    match *drag {
        WmDrag::Move { .. } => CursorShape::Move,
        WmDrag::Resize {
            left,
            right,
            top,
            bottom,
            ..
        } => edge_shape(left, right, top, bottom),
        WmDrag::Button { .. } | WmDrag::Chip { .. } => CursorShape::Default,
    }
}
