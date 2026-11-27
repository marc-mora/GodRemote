//! Peticiones de fuentes, pixmaps, contextos gráficos, dibujo, imágenes,
//! color y cursores (códigos 45 a 97).

use std::sync::Arc;

use super::colors;
use super::draw::{
    Clip, Gc, Image, Painter, Rect, decode_bitmap, decode_z32, encode_bitmap, row_bytes,
};
use super::font::{self, CharInfo, Font};
use super::proto::*;
use super::server::{A_FONT, ClientId, DEFAULT_COLORMAP, Pixmap, ROOT_VISUAL, Server};

impl Server {
    pub(super) fn dispatch_draw(
        &mut self,
        c: ClientId,
        opcode: u8,
        data: u8,
        rd: &mut Reader,
    ) -> XResult {
        match opcode {
            45 => {
                let id = rd.u32()?;
                let len = rd.u16()? as usize;
                rd.skip(2)?;
                let name: String = rd.bytes(len)?.iter().map(|b| *b as char).collect();
                self.check_new_id(c, id)?;
                let font = font::open(&name).ok_or(XError::new(BAD_NAME, 0))?;
                self.fonts.insert(id, (c, font));
                Ok(())
            }
            46 => {
                let id = rd.u32()?;
                self.fonts
                    .remove(&id)
                    .map(|_| ())
                    .ok_or(XError::new(BAD_FONT, id))
            }
            47 => {
                let font = self.fontable(rd.u32()?)?;
                let mut w = self.reply(c, 0);
                self.write_font_info(&mut w, &font, None);
                for info in &font.core.infos {
                    write_char_info(&mut w, info);
                }
                self.send_reply(c, w);
                Ok(())
            }
            48 => self.query_text_extents(c, data, rd),
            49 => {
                let (max, len) = (rd.u16()? as usize, rd.u16()? as usize);
                let pattern: String = rd.bytes(len)?.iter().map(|b| *b as char).collect();
                let names = font::list(&pattern, max);
                let mut w = self.reply(c, 0);
                w.u16(names.len() as u16).zeros(22);
                for name in names {
                    let name = &name.as_bytes()[..name.len().min(255)];
                    w.u8(name.len() as u8).bytes(name);
                }
                self.send_reply(c, w);
                Ok(())
            }
            50 => {
                let (max, len) = (rd.u16()? as usize, rd.u16()? as usize);
                let pattern: String = rd.bytes(len)?.iter().map(|b| *b as char).collect();
                // Una respuesta por fuente y una final vacía.
                let names = font::list(&pattern, max.min(200));
                let total = names.len();
                for (i, name) in names.iter().enumerate() {
                    let Some(font) = font::open(name) else {
                        continue;
                    };
                    let name = &name.as_bytes()[..name.len().min(255)];
                    let mut w = self.reply(c, name.len() as u8);
                    self.write_font_info(&mut w, &font, Some((total - i) as u32));
                    w.bytes(name);
                    self.send_reply(c, w);
                }
                let mut w = self.reply(c, 0);
                w.zeros(52);
                self.send_reply(c, w);
                Ok(())
            }
            51 => Ok(()), // SetFontPath
            52 => {
                let mut w = self.reply(c, 0);
                w.u16(0).zeros(22);
                self.send_reply(c, w);
                Ok(())
            }
            53 => {
                let (id, drawable) = (rd.u32()?, rd.u32()?);
                let (w, h) = (rd.u16()? as u32, rd.u16()? as u32);
                self.check_new_id(c, id)?;
                self.drawable_bounds(drawable)?;
                if !matches!(data, 1 | 24 | 32) || w == 0 || h == 0 {
                    return Err(XError::new(BAD_VALUE, data as u32));
                }
                self.pixmaps.insert(
                    id,
                    Pixmap {
                        owner: c,
                        width: w,
                        height: h,
                        image: Arc::new(Image::new(w, h, data)),
                    },
                );
                Ok(())
            }
            54 => {
                let id = rd.u32()?;
                self.pixmaps
                    .remove(&id)
                    .map(|_| ())
                    .ok_or(XError::new(BAD_PIXMAP, id))
            }
            55 => {
                let (id, drawable, mask) = (rd.u32()?, rd.u32()?, rd.u32()?);
                self.check_new_id(c, id)?;
                let mut gc = Gc::new(self.drawable_depth(drawable)?);
                self.apply_gc_values(&mut gc, mask, rd)?;
                self.gcs.insert(id, (c, Arc::new(gc)));
                Ok(())
            }
            56 => {
                let (id, mask) = (rd.u32()?, rd.u32()?);
                let mut gc = (*self.gc(id)?).clone();
                self.apply_gc_values(&mut gc, mask, rd)?;
                self.store_gc(id, gc);
                Ok(())
            }
            57 => {
                let (src, dst, mask) = (rd.u32()?, rd.u32()?, rd.u32()?);
                let src = self.gc(src)?;
                let mut gc = (*self.gc(dst)?).clone();
                if src.depth != gc.depth {
                    return Err(XError::new(BAD_MATCH, dst));
                }
                copy_gc_fields(&src, &mut gc, mask);
                self.store_gc(dst, gc);
                Ok(())
            }
            58 => {
                let id = rd.u32()?;
                let (offset, n) = (rd.u16()?, rd.u16()? as usize);
                let dashes = rd.bytes(n)?.to_vec();
                if dashes.is_empty() || dashes.contains(&0) {
                    return Err(XError::new(BAD_VALUE, 0));
                }
                let mut gc = (*self.gc(id)?).clone();
                (gc.dash_offset, gc.dashes) = (offset, dashes);
                self.store_gc(id, gc);
                Ok(())
            }
            59 => {
                let id = rd.u32()?;
                let (x, y) = (rd.i16()?, rd.i16()?);
                let mut rects = vec![];
                while rd.remaining() >= 8 {
                    let (rx, ry) = (rd.i16()? as i32, rd.i16()? as i32);
                    let (rw, rh) = (rd.u16()? as i32, rd.u16()? as i32);
                    rects.push(Rect::new(rx, ry, rw, rh));
                }
                let mut gc = (*self.gc(id)?).clone();
                (gc.clip_x, gc.clip_y, gc.clip) = (x, y, Clip::Rects(rects));
                self.store_gc(id, gc);
                Ok(())
            }
            60 => {
                let id = rd.u32()?;
                self.gcs
                    .remove(&id)
                    .map(|_| ())
                    .ok_or(XError::new(BAD_GC, id))
            }
            61 => {
                let window = rd.u32()?;
                let (x, y) = (rd.i16()? as i32, rd.i16()? as i32);
                let (w, h) = (rd.u16()? as i32, rd.u16()? as i32);
                let win = self.win(window)?;
                if win.input_only {
                    return Err(XError::new(BAD_MATCH, window));
                }
                // Ancho o alto 0: hasta el borde de la ventana.
                let w = if w == 0 { win.w as i32 - x } else { w };
                let h = if h == 0 { win.h as i32 - y } else { h };
                let r = Rect::new(x, y, w, h);
                if data != 0 {
                    self.expose(window, &[r]);
                } else {
                    self.paint_background(window, r);
                }
                Ok(())
            }
            62 | 63 => self.copy_area(c, opcode, rd),
            64 => {
                let (drawable, gc) = (rd.u32()?, rd.u32()?);
                let points = read_points(rd, data)?;
                self.paint(drawable, gc, |p, _| {
                    for (x, y) in &points {
                        p.pixel(*x, *y);
                    }
                })
            }
            65 => {
                let (drawable, gc) = (rd.u32()?, rd.u32()?);
                let points = read_points(rd, data)?;
                self.paint(drawable, gc, |p, gc| {
                    if let [only] = points[..] {
                        p.line(gc, only.0, only.1, only.0, only.1, true, true);
                    }
                    let closed = points.len() > 2 && points.first() == points.last();
                    for (i, pair) in points.windows(2).enumerate() {
                        // Cada vértice se pinta una sola vez (importa con XOR).
                        let last = i + 2 < points.len() || !closed;
                        let (a, b) = (pair[0], pair[1]);
                        p.line(gc, a.0, a.1, b.0, b.1, i == 0, last);
                    }
                })
            }
            66 => {
                let (drawable, gc) = (rd.u32()?, rd.u32()?);
                let mut segments = vec![];
                while rd.remaining() >= 8 {
                    segments.push((
                        rd.i16()? as i32,
                        rd.i16()? as i32,
                        rd.i16()? as i32,
                        rd.i16()? as i32,
                    ));
                }
                self.paint(drawable, gc, |p, gc| {
                    for (x1, y1, x2, y2) in &segments {
                        p.line(gc, *x1, *y1, *x2, *y2, true, true);
                    }
                })
            }
            67 | 70 => {
                let (drawable, gc) = (rd.u32()?, rd.u32()?);
                let mut rects = vec![];
                while rd.remaining() >= 8 {
                    let (x, y) = (rd.i16()? as i32, rd.i16()? as i32);
                    rects.push((x, y, rd.u16()? as i32, rd.u16()? as i32));
                }
                self.paint(drawable, gc, |p, gc| {
                    for (x, y, w, h) in &rects {
                        if opcode == 70 {
                            p.rect(Rect::new(*x, *y, *w, *h));
                        } else {
                            p.rect_outline(gc, *x, *y, *w, *h);
                        }
                    }
                })
            }
            68 | 71 => {
                let (drawable, gc) = (rd.u32()?, rd.u32()?);
                let mut arcs = vec![];
                while rd.remaining() >= 12 {
                    let (x, y) = (rd.i16()? as i32, rd.i16()? as i32);
                    let (w, h) = (rd.u16()? as i32, rd.u16()? as i32);
                    arcs.push((x, y, w, h, rd.i16()? as i32, rd.i16()? as i32));
                }
                self.paint(drawable, gc, |p, gc| {
                    for (x, y, w, h, a1, a2) in &arcs {
                        p.arc(gc, *x, *y, *w, *h, *a1, *a2, opcode == 71);
                    }
                })
            }
            69 => {
                let (drawable, gc) = (rd.u32()?, rd.u32()?);
                let (_shape, mode) = (rd.u8()?, rd.u8()?);
                rd.skip(2)?;
                let points: Vec<(f64, f64)> = read_points(rd, mode)?
                    .iter()
                    .map(|(x, y)| (*x as f64, *y as f64))
                    .collect();
                self.paint(drawable, gc, |p, gc| p.polygon(&points, gc.fill_rule == 1))
            }
            72 => self.put_image(data, rd),
            73 => self.get_image(c, data, rd),
            74..=77 => self.draw_text(opcode, data, rd),
            78 => {
                let (id, window, visual) = (rd.u32()?, rd.u32()?, rd.u32()?);
                self.check_new_id(c, id)?;
                self.win(window)?;
                if visual != ROOT_VISUAL {
                    return Err(XError::new(BAD_MATCH, visual));
                }
                self.colormaps.insert(id, c);
                Ok(())
            }
            79 => {
                let id = rd.u32()?;
                self.colormaps
                    .remove(&id)
                    .map(|_| ())
                    .ok_or(XError::new(BAD_COLOR, id))
            }
            80 => {
                let (id, src) = (rd.u32()?, rd.u32()?);
                self.check_new_id(c, id)?;
                self.check_colormap(src)?;
                self.colormaps.insert(id, c);
                Ok(())
            }
            81 | 82 => self.check_colormap(rd.u32()?),
            83 => {
                self.win(rd.u32()?)?;
                let mut w = self.reply(c, 0);
                w.u16(1).zeros(22).u32(DEFAULT_COLORMAP);
                self.send_reply(c, w);
                Ok(())
            }
            84 => {
                self.check_colormap(rd.u32()?)?;
                let (r, g, b) = (rd.u16()?, rd.u16()?, rd.u16()?);
                // Mapa estático: el píxel es el propio color a 8 bits.
                let exact = |v: u16| (v >> 8) * 257;
                let mut w = self.reply(c, 0);
                w.u16(exact(r))
                    .u16(exact(g))
                    .u16(exact(b))
                    .u16(0)
                    .u32(colors::pixel(r, g, b));
                self.send_reply(c, w);
                Ok(())
            }
            85 | 92 => {
                self.check_colormap(rd.u32()?)?;
                let len = rd.u16()? as usize;
                rd.skip(2)?;
                let name: String = rd.bytes(len)?.iter().map(|b| *b as char).collect();
                let (r, g, b) = colors::lookup(&name).ok_or(XError::new(BAD_NAME, 0))?;
                let exact = |v: u16| (v >> 8) * 257;
                let mut w = self.reply(c, 0);
                if opcode == 85 {
                    w.u32(colors::pixel(r, g, b));
                }
                w.u16(r).u16(g).u16(b);
                w.u16(exact(r)).u16(exact(g)).u16(exact(b));
                self.send_reply(c, w);
                Ok(())
            }
            // AllocColorCells / AllocColorPlanes: el mapa no es modificable.
            86 | 87 => Err(XError::new(BAD_ALLOC, 0)),
            88..=90 => self.check_colormap(rd.u32()?), // FreeColors, StoreColors…
            91 => {
                self.check_colormap(rd.u32()?)?;
                let mut pixels = vec![];
                while rd.remaining() >= 4 {
                    pixels.push(rd.u32()?);
                }
                let mut w = self.reply(c, 0);
                w.u16(pixels.len() as u16).zeros(22);
                for p in pixels {
                    for shift in [16, 8, 0] {
                        w.u16(((p >> shift) & 0xff) as u16 * 257);
                    }
                    w.u16(0);
                }
                self.send_reply(c, w);
                Ok(())
            }
            93 => {
                let (id, source, mask) = (rd.u32()?, rd.u32()?, rd.u32()?);
                self.check_new_id(c, id)?;
                for pixmap in [source, mask] {
                    if pixmap != 0 && !self.pixmaps.contains_key(&pixmap) {
                        return Err(XError::new(BAD_PIXMAP, pixmap));
                    }
                }
                self.cursors.insert(id, c);
                Ok(())
            }
            94 => {
                let (id, source, mask) = (rd.u32()?, rd.u32()?, rd.u32()?);
                self.check_new_id(c, id)?;
                for font in [source, mask] {
                    if font != 0 && !self.fonts.contains_key(&font) {
                        return Err(XError::new(BAD_FONT, font));
                    }
                }
                self.cursors.insert(id, c);
                Ok(())
            }
            95 => {
                let id = rd.u32()?;
                self.cursors
                    .remove(&id)
                    .map(|_| ())
                    .ok_or(XError::new(BAD_CURSOR, id))
            }
            96 => {
                let id = rd.u32()?;
                if self.cursors.contains_key(&id) {
                    Ok(())
                } else {
                    Err(XError::new(BAD_CURSOR, id))
                }
            }
            97 => {
                self.drawable_bounds(rd.u32()?)?;
                let (w, h) = (rd.u16()?, rd.u16()?);
                let mut out = self.reply(c, 0);
                out.u16(w).u16(h);
                self.send_reply(c, out);
                Ok(())
            }
            _ => Err(XError::new(BAD_IMPLEMENTATION, 0)),
        }
    }

    fn check_colormap(&self, id: Xid) -> XResult {
        if id == DEFAULT_COLORMAP || self.colormaps.contains_key(&id) {
            Ok(())
        } else {
            Err(XError::new(BAD_COLOR, id))
        }
    }

    // --- Contextos gráficos -------------------------------------------------

    pub(super) fn gc(&self, id: Xid) -> XResult<Arc<Gc>> {
        self.gcs
            .get(&id)
            .map(|(_, gc)| gc.clone())
            .ok_or(XError::new(BAD_GC, id))
    }

    fn store_gc(&mut self, id: Xid, gc: Gc) {
        if let Some(entry) = self.gcs.get_mut(&id) {
            entry.1 = Arc::new(gc);
        }
    }

    fn drawable_depth(&self, id: Xid) -> XResult<u8> {
        if let Some(w) = self.windows.get(&id) {
            Ok(if w.input_only { 24 } else { w.depth })
        } else if let Some(p) = self.pixmaps.get(&id) {
            Ok(p.image.depth)
        } else {
            Err(XError::new(BAD_DRAWABLE, id))
        }
    }

    fn apply_gc_values(&self, gc: &mut Gc, mask: u32, rd: &mut Reader) -> XResult {
        if mask >> 23 != 0 {
            return Err(XError::new(BAD_VALUE, mask));
        }
        let pixmap = |id: Xid, depth: u8| -> XResult<Arc<Image>> {
            let p = self.pixmaps.get(&id).ok_or(XError::new(BAD_PIXMAP, id))?;
            if p.image.depth != depth {
                return Err(XError::new(BAD_MATCH, id));
            }
            Ok(p.image.clone())
        };
        for bit in 0..23 {
            if mask & (1 << bit) == 0 {
                continue;
            }
            let v = rd.u32()?;
            match bit {
                0 => {
                    if v > 15 {
                        return Err(XError::new(BAD_VALUE, v));
                    }
                    gc.function = v as u8;
                }
                1 => gc.plane_mask = v,
                2 => gc.foreground = v,
                3 => gc.background = v,
                4 => gc.line_width = v as u16,
                5 => gc.line_style = v as u8,
                6 => gc.cap_style = v as u8,
                7 => gc.join_style = v as u8,
                8 => {
                    if v > 3 {
                        return Err(XError::new(BAD_VALUE, v));
                    }
                    gc.fill_style = v as u8;
                }
                9 => gc.fill_rule = v as u8,
                10 => gc.tile = Some(pixmap(v, gc.depth)?),
                11 => gc.stipple = Some(pixmap(v, 1)?),
                12 => gc.ts_x = v as i16,
                13 => gc.ts_y = v as i16,
                14 => {
                    if !self.fonts.contains_key(&v) {
                        return Err(XError::new(BAD_FONT, v));
                    }
                    gc.font = v;
                }
                15 => gc.subwindow_mode = v as u8,
                16 => gc.graphics_exposures = v != 0,
                17 => gc.clip_x = v as i16,
                18 => gc.clip_y = v as i16,
                19 => {
                    gc.clip = if v == 0 {
                        Clip::None
                    } else {
                        Clip::Mask(pixmap(v, 1)?)
                    };
                }
                20 => gc.dash_offset = v as u16,
                21 => gc.dashes = vec![v as u8, v as u8],
                _ => gc.arc_mode = v as u8,
            }
        }
        Ok(())
    }

    // --- Dibujo -------------------------------------------------------------

    /// Búfer de un *drawable* para pintar en él.
    fn image_mut(&mut self, id: Xid) -> XResult<&mut Image> {
        if let Some(w) = self.windows.get_mut(&id) {
            w.image.as_mut().ok_or(XError::new(BAD_MATCH, id))
        } else if let Some(p) = self.pixmaps.get_mut(&id) {
            Ok(Arc::make_mut(&mut p.image))
        } else {
            Err(XError::new(BAD_DRAWABLE, id))
        }
    }

    /// Ejecuta una operación de dibujo con el GC indicado y anota la zona
    /// modificada si el destino es una ventana visible.
    fn paint_with(
        &mut self,
        drawable: Xid,
        gc: &Gc,
        op: impl FnOnce(&mut Painter, &Gc),
    ) -> XResult {
        let is_window = self.windows.contains_key(&drawable);
        let img = self.image_mut(drawable)?;
        if img.depth != gc.depth {
            return Err(XError::new(BAD_MATCH, drawable));
        }
        let mut painter = Painter::new(img, gc);
        op(&mut painter, gc);
        let damage = painter.damage;
        if is_window && !damage.is_empty() {
            self.damage_window(drawable, damage);
        }
        Ok(())
    }

    fn paint(&mut self, drawable: Xid, gc: Xid, op: impl FnOnce(&mut Painter, &Gc)) -> XResult {
        // Se comprueba primero el destino: es el error que espera el cliente.
        self.drawable_bounds(drawable)?;
        let gc = self.gc(gc)?;
        self.paint_with(drawable, &gc, op)
    }

    /// Píxeles de una zona de un *drawable* (0 fuera de sus límites). Las
    /// ventanas con hijas visibles se leen ya compuestas.
    fn read_pixels(&self, id: Xid, r: Rect, with_children: bool) -> XResult<Vec<u32>> {
        if let Some(w) = self.windows.get(&id) {
            let img = w.image.as_ref().ok_or(XError::new(BAD_MATCH, id))?;
            let has_children = w
                .children
                .iter()
                .any(|c| self.windows.get(c).is_some_and(|c| c.mapped));
            if with_children && has_children {
                let mut out = vec![0u32; r.width() as usize * r.height() as usize];
                self.compose_window(id, r, &mut out);
                return Ok(out);
            }
            Ok(img.extract(r))
        } else if let Some(p) = self.pixmaps.get(&id) {
            Ok(p.image.extract(r))
        } else {
            Err(XError::new(BAD_DRAWABLE, id))
        }
    }

    /// `CopyArea` (62) y `CopyPlane` (63).
    fn copy_area(&mut self, c: ClientId, opcode: u8, rd: &mut Reader) -> XResult {
        let (src, dst, gc) = (rd.u32()?, rd.u32()?, rd.u32()?);
        let (sx, sy) = (rd.i16()? as i32, rd.i16()? as i32);
        let (dx, dy) = (rd.i16()? as i32, rd.i16()? as i32);
        let (w, h) = (rd.u16()? as i32, rd.u16()? as i32);
        let plane = if opcode == 63 { rd.u32()? } else { 0 };
        let src_bounds = self.drawable_bounds(src)?;
        let dst_bounds = self.drawable_bounds(dst)?;
        let gc = self.gc(gc)?;
        let (src_depth, dst_depth) = (self.drawable_depth(src)?, self.drawable_depth(dst)?);
        if opcode == 62 && src_depth != dst_depth {
            return Err(XError::new(BAD_MATCH, src));
        }
        if opcode == 63 && (plane == 0 || !plane.is_power_of_two()) {
            return Err(XError::new(BAD_VALUE, plane));
        }
        let area = Rect::new(sx, sy, w, h);
        let mut block = self.read_pixels(src, area, gc.subwindow_mode == 1)?;
        if opcode == 63 {
            for p in &mut block {
                *p = if *p & plane != 0 {
                    gc.foreground
                } else {
                    gc.background
                };
            }
        }
        // Parte del origen que existe de verdad, en coordenadas del bloque.
        let valid = area.intersect(&src_bounds).translate(-sx, -sy);
        self.paint_with(dst, &gc, |p, _| p.blit(dx, dy, w, &block, valid))?;
        if !gc.graphics_exposures {
            return Ok(());
        }
        // Lo que no se pudo copiar porque caía fuera del origen: se avisa
        // con GraphicsExpose; si se copió todo, con NoExpose.
        let missing: Vec<Rect> = Rect::new(0, 0, w, h)
            .subtract(&valid)
            .iter()
            .map(|r| r.translate(dx, dy).intersect(&dst_bounds))
            .filter(|r| !r.is_empty())
            .collect();
        if missing.is_empty() {
            let ev = Ev::new(NO_EXPOSE, 0).u32(dst).u16(0).u8(opcode).done();
            self.send(c, ev);
            return Ok(());
        }
        let is_window = self.windows.contains_key(&dst);
        for (i, r) in missing.iter().enumerate() {
            if is_window {
                self.paint_background(dst, *r);
            }
            let ev = Ev::new(GRAPHICS_EXPOSE, 0)
                .u32(dst)
                .u16(r.x0 as u16)
                .u16(r.y0 as u16)
                .u16(r.width() as u16)
                .u16(r.height() as u16)
                .u16(0)
                .u16((missing.len() - 1 - i) as u16)
                .u8(opcode)
                .done();
            self.send(c, ev);
        }
        Ok(())
    }

    fn put_image(&mut self, format: u8, rd: &mut Reader) -> XResult {
        let (drawable, gc) = (rd.u32()?, rd.u32()?);
        let (w, h) = (rd.u16()? as usize, rd.u16()? as usize);
        let (x, y) = (rd.i16()? as i32, rd.i16()? as i32);
        let (left_pad, depth) = (rd.u8()? as usize, rd.u8()?);
        rd.skip(2)?;
        let data = rd.rest();
        let dst_depth = self.drawable_depth(drawable)?;
        let gc = self.gc(gc)?;
        let short = XError::new(BAD_LENGTH, 0);
        let block = match format {
            // XYBitmap: un plano que se pinta con primer plano y fondo.
            0 => {
                if depth != 1 {
                    return Err(XError::new(BAD_MATCH, drawable));
                }
                let mut bits = decode_bitmap(data, w, h, left_pad).ok_or(short)?;
                for p in &mut bits {
                    *p = if *p != 0 {
                        gc.foreground
                    } else {
                        gc.background
                    };
                }
                bits
            }
            // XYPixmap: un mapa de bits por plano, el más significativo primero.
            1 => {
                if depth != dst_depth {
                    return Err(XError::new(BAD_MATCH, drawable));
                }
                let plane_bytes = row_bytes(w + left_pad, 1) * h;
                let mut block = vec![0u32; w * h];
                for plane in 0..depth as usize {
                    let chunk = data.get(plane * plane_bytes..).ok_or(short)?;
                    let bits = decode_bitmap(chunk, w, h, left_pad).ok_or(short)?;
                    let bit = 1u32 << (depth as usize - 1 - plane);
                    for (p, b) in block.iter_mut().zip(bits) {
                        if b != 0 {
                            *p |= bit;
                        }
                    }
                }
                block
            }
            2 => {
                if depth != dst_depth {
                    return Err(XError::new(BAD_MATCH, drawable));
                }
                if depth == 1 {
                    decode_bitmap(data, w, h, 0).ok_or(short)?
                } else {
                    decode_z32(data, w, h).ok_or(short)?
                }
            }
            other => return Err(XError::new(BAD_VALUE, other as u32)),
        };
        let valid = Rect::new(0, 0, w as i32, h as i32);
        self.paint_with(drawable, &gc, |p, _| p.blit(x, y, w as i32, &block, valid))
    }

    fn get_image(&mut self, c: ClientId, format: u8, rd: &mut Reader) -> XResult {
        let drawable = rd.u32()?;
        let (x, y) = (rd.i16()? as i32, rd.i16()? as i32);
        let (w, h) = (rd.u16()? as usize, rd.u16()? as usize);
        let plane_mask = rd.u32()?;
        let depth = self.drawable_depth(drawable)?;
        let is_window = self.windows.contains_key(&drawable);
        if !matches!(format, 1 | 2) {
            return Err(XError::new(BAD_VALUE, format as u32));
        }
        let area = Rect::new(x, y, w as i32, h as i32);
        if area.intersect(&self.drawable_bounds(drawable)?) != area && !is_window {
            return Err(XError::new(BAD_MATCH, drawable));
        }
        let block = self.read_pixels(drawable, area, true)?;
        let mut data = Vec::new();
        if format == 2 && depth > 1 {
            data.reserve(block.len() * 4);
            for p in &block {
                data.extend_from_slice(&(p & plane_mask).to_le_bytes());
            }
        } else if format == 2 {
            encode_bitmap(&block, w, h, 1 & plane_mask, &mut data);
        } else {
            for plane in (0..depth as u32).rev() {
                if plane_mask & (1 << plane) != 0 {
                    encode_bitmap(&block, w, h, 1 << plane, &mut data);
                }
            }
        }
        let mut out = self.reply(c, depth);
        out.u32(if is_window { ROOT_VISUAL } else { 0 })
            .zeros(20)
            .bytes(&data);
        self.send_reply(c, out);
        Ok(())
    }

    // --- Texto --------------------------------------------------------------

    /// Fuente de un identificador de fuente o de GC.
    fn fontable(&self, id: Xid) -> XResult<Font> {
        if let Some((_, font)) = self.fonts.get(&id) {
            return Ok(font.clone());
        }
        let (_, gc) = self.gcs.get(&id).ok_or(XError::new(BAD_FONT, id))?;
        Ok(self.gc_font(gc))
    }

    fn gc_font(&self, gc: &Gc) -> Font {
        self.fonts
            .get(&gc.font)
            .map(|(_, f)| f.clone())
            .unwrap_or_else(|| self.default_font.clone())
    }

    /// Cabecera común de `QueryFont` y `ListFontsWithInfo`.
    fn write_font_info(&mut self, w: &mut Writer, font: &Font, replies_hint: Option<u32>) {
        let core = &font.core;
        let x_height = core.info('x' as u32).ascent.max(1) as u32;
        let cap_height = core.info('X' as u32).ascent.max(1) as u32;
        let pixel = (core.ascent + core.descent) as u32;
        let props = [
            (A_FONT, self.atoms.intern(&font.name)),
            (self.atoms.intern("PIXEL_SIZE"), pixel),
            (59, pixel * 10),                       // POINT_SIZE
            (57, core.max_bounds.width as u32),     // QUAD_WIDTH
            (56, x_height),                         // X_HEIGHT
            (66, cap_height),                       // CAP_HEIGHT
            (51, (core.descent as u32 / 2).max(1)), // UNDERLINE_POSITION
            (52, 1),                                // UNDERLINE_THICKNESS
        ];
        let (max_byte1, max_char) = if core.two_byte() {
            ((core.last >> 8) as u8, 255)
        } else {
            (0, core.last as u16)
        };
        write_char_info(w, &core.min_bounds);
        w.zeros(4);
        write_char_info(w, &core.max_bounds);
        w.zeros(4);
        w.u16(0)
            .u16(max_char)
            .u16(0)
            .u16(props.len() as u16)
            .u8(0)
            .u8(0)
            .u8(max_byte1)
            .u8(0)
            .i16(core.ascent)
            .i16(core.descent)
            .u32(replies_hint.unwrap_or(core.infos.len() as u32));
        for (atom, value) in props {
            w.u32(atom).u32(value);
        }
    }

    fn query_text_extents(&mut self, c: ClientId, odd: u8, rd: &mut Reader) -> XResult {
        let font = self.fontable(rd.u32()?)?;
        let bytes = rd.rest();
        let count = bytes.len() / 2 - usize::from(odd != 0 && bytes.len() >= 2);
        let core = &font.core;
        let (mut ascent, mut descent, mut width) = (0i32, 0i32, 0i32);
        let (mut left, mut right) = (0i32, 0i32);
        let mut first = true;
        for pair in bytes.chunks_exact(2).take(count) {
            let info = core.info((pair[0] as u32) << 8 | pair[1] as u32);
            if !info.exists() {
                continue;
            }
            let (l, r) = (width + info.lbearing as i32, width + info.rbearing as i32);
            if first {
                (left, right, first) = (l, r, false);
            }
            left = left.min(l);
            right = right.max(r);
            ascent = ascent.max(info.ascent as i32);
            descent = descent.max(info.descent as i32);
            width += info.width as i32;
        }
        let mut w = self.reply(c, 0);
        w.i16(core.ascent)
            .i16(core.descent)
            .i16(ascent as i16)
            .i16(descent as i16)
            .u32(width as u32)
            .u32(left as u32)
            .u32(right as u32);
        self.send_reply(c, w);
        Ok(())
    }

    /// `PolyText8/16` (74, 75) e `ImageText8/16` (76, 77).
    fn draw_text(&mut self, opcode: u8, data: u8, rd: &mut Reader) -> XResult {
        let (drawable, gc_id) = (rd.u32()?, rd.u32()?);
        let (x, y) = (rd.i16()? as i32, rd.i16()? as i32);
        self.drawable_bounds(drawable)?;
        let mut gc = self.gc(gc_id)?;
        let wide = matches!(opcode, 75 | 77);
        let decode = |bytes: &[u8]| -> Vec<u32> {
            if wide {
                bytes
                    .chunks_exact(2)
                    .map(|p| (p[0] as u32) << 8 | p[1] as u32)
                    .collect()
            } else {
                bytes.iter().map(|b| *b as u32).collect()
            }
        };
        let unit = if wide { 2 } else { 1 };
        if opcode >= 76 {
            // ImageText: rectángulo de fondo y, encima, el texto.
            let codes = decode(rd.bytes(data as usize * unit)?);
            let font = self.gc_font(&gc);
            let core = font.core.clone();
            let width: i32 = codes.iter().map(|c| core.info(*c).width as i32).sum();
            let (fg, bg) = (gc.foreground, gc.background);
            return self.paint_with(drawable, &gc, |p, _| {
                p.solid_copy(bg);
                p.rect(Rect::new(
                    x,
                    y - core.ascent as i32,
                    width,
                    (core.ascent + core.descent) as i32,
                ));
                p.solid_copy(fg);
                p.text(&core, x, y, &codes);
            });
        }
        // PolyText: una lista de tramos con desplazamiento y cambios de fuente.
        let mut runs: Vec<(Font, i32, Vec<u32>)> = vec![];
        let mut font = self.gc_font(&gc);
        while rd.remaining() >= 2 {
            let len = rd.u8()? as usize;
            if len == 255 {
                // Cambio de fuente: su identificador, siempre en big-endian.
                let b = rd.bytes(4)?;
                let id = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                let (_, new) = self.fonts.get(&id).ok_or(XError::new(BAD_FONT, id))?;
                font = new.clone();
                // El cambio queda guardado en el GC.
                let mut changed = (*gc).clone();
                changed.font = id;
                self.store_gc(gc_id, changed.clone());
                gc = Arc::new(changed);
                continue;
            }
            let delta = rd.u8()? as i8 as i32;
            let codes = decode(rd.bytes(len * unit)?);
            runs.push((font.clone(), delta, codes));
        }
        self.paint_with(drawable, &gc, |p, _| {
            let mut pen = x;
            for (font, delta, codes) in &runs {
                pen += delta;
                pen += p.text(&font.core, pen, y, codes);
            }
        })
    }
}

fn write_char_info(w: &mut Writer, info: &CharInfo) {
    w.i16(info.lbearing)
        .i16(info.rbearing)
        .i16(info.width)
        .i16(info.ascent)
        .i16(info.descent)
        .u16(0);
}

/// Lista de puntos; con `mode` 1 cada uno es relativo al anterior.
fn read_points(rd: &mut Reader, mode: u8) -> XResult<Vec<(i32, i32)>> {
    let mut points: Vec<(i32, i32)> = Vec::with_capacity(rd.remaining() / 4);
    while rd.remaining() >= 4 {
        let (mut x, mut y) = (rd.i16()? as i32, rd.i16()? as i32);
        if mode == 1
            && let Some(prev) = points.last()
        {
            x += prev.0;
            y += prev.1;
        }
        points.push((x, y));
    }
    Ok(points)
}

/// Copia de un GC a otro los campos que indica la máscara (`CopyGC`).
fn copy_gc_fields(src: &Gc, dst: &mut Gc, mask: u32) {
    let has = |bit: u32| mask & (1 << bit) != 0;
    if has(0) {
        dst.function = src.function;
    }
    if has(1) {
        dst.plane_mask = src.plane_mask;
    }
    if has(2) {
        dst.foreground = src.foreground;
    }
    if has(3) {
        dst.background = src.background;
    }
    if has(4) {
        dst.line_width = src.line_width;
    }
    if has(5) {
        dst.line_style = src.line_style;
    }
    if has(6) {
        dst.cap_style = src.cap_style;
    }
    if has(7) {
        dst.join_style = src.join_style;
    }
    if has(8) {
        dst.fill_style = src.fill_style;
    }
    if has(9) {
        dst.fill_rule = src.fill_rule;
    }
    if has(10) {
        dst.tile = src.tile.clone();
    }
    if has(11) {
        dst.stipple = src.stipple.clone();
    }
    if has(12) {
        dst.ts_x = src.ts_x;
    }
    if has(13) {
        dst.ts_y = src.ts_y;
    }
    if has(14) {
        dst.font = src.font;
    }
    if has(15) {
        dst.subwindow_mode = src.subwindow_mode;
    }
    if has(16) {
        dst.graphics_exposures = src.graphics_exposures;
    }
    if has(17) {
        dst.clip_x = src.clip_x;
    }
    if has(18) {
        dst.clip_y = src.clip_y;
    }
    if has(19) {
        dst.clip = src.clip.clone();
    }
    if has(20) {
        dst.dash_offset = src.dash_offset;
    }
    if has(21) {
        dst.dashes = src.dashes.clone();
    }
    if has(22) {
        dst.arc_mode = src.arc_mode;
    }
}
