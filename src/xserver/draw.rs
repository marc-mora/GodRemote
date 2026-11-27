//! Dibujo sobre los búferes de ventanas y pixmaps.
//!
//! No hay regiones: cada operación se recorta a los límites del destino y
//! al recorte del contexto gráfico (rectángulos o máscara de un bit).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::font::FontCore;

/// Rectángulo semiabierto: `x0 <= x < x1`, `y0 <= y < y1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Rect {
    pub const EMPTY: Rect = Rect {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
    };

    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect {
            x0: x,
            y0: y,
            x1: x.saturating_add(w.max(0)),
            y1: y.saturating_add(h.max(0)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn width(&self) -> i32 {
        (self.x1 - self.x0).max(0)
    }

    pub fn height(&self) -> i32 {
        (self.y1 - self.y0).max(0)
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let r = Rect {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        };
        if r.is_empty() { Rect::EMPTY } else { r }
    }

    /// Menor rectángulo que contiene a los dos (los vacíos no cuentan).
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        Rect {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }

    pub fn translate(&self, dx: i32, dy: i32) -> Rect {
        Rect {
            x0: self.x0 + dx,
            y0: self.y0 + dy,
            x1: self.x1 + dx,
            y1: self.y1 + dy,
        }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    /// Partes de `self` que no cubre `o` (hasta cuatro franjas).
    pub fn subtract(&self, o: &Rect) -> Vec<Rect> {
        let i = self.intersect(o);
        if i.is_empty() {
            return if self.is_empty() { vec![] } else { vec![*self] };
        }
        let mut out = vec![];
        let mut push = |x0, y0, x1, y1| {
            let r = Rect { x0, y0, x1, y1 };
            if !r.is_empty() {
                out.push(r);
            }
        };
        push(self.x0, self.y0, self.x1, i.y0);
        push(self.x0, i.y0, i.x0, i.y1);
        push(i.x1, i.y0, self.x1, i.y1);
        push(self.x0, i.y1, self.x1, self.y1);
        out
    }
}

/// Lado máximo y número máximo de píxeles de un búfer: un cliente puede
/// pedir ventanas de 32767×32767 y no se le van a dar 4 GB.
pub const MAX_SIDE: u32 = 16_384;
pub const MAX_PIXELS: u64 = 48 * 1024 * 1024;
/// Tope de la suma de píxeles de todas las ventanas y pixmaps (≈ 1 GiB).
pub const MAX_TOTAL_PIXELS: u64 = 256 * 1024 * 1024;

/// Presupuesto global de memoria de imagen. Cada búfer que crea un cliente
/// (ventana, pixmap o una copia de estos) se anota aquí al reservarse y se
/// descuenta al liberarse; cuando no cabe, la petición recibe `BadAlloc`.
#[derive(Debug)]
pub struct Budget {
    used: AtomicU64,
    limit: u64,
}

impl Budget {
    pub fn new(limit: u64) -> Arc<Self> {
        Arc::new(Budget {
            used: AtomicU64::new(0),
            limit,
        })
    }

    /// Píxeles reservados ahora mismo.
    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }

    /// Hay sitio para `pixels` más.
    pub fn has_room(&self, pixels: u64) -> bool {
        self.used().saturating_add(pixels) <= self.limit
    }

    fn try_reserve(&self, pixels: u64) -> bool {
        self.used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(pixels)
                    .filter(|total| *total <= self.limit)
            })
            .is_ok()
    }
}

/// Búfer de píxeles de un *drawable*. Cada píxel ocupa un `u32`: 0xRRGGBB
/// con profundidad 24, 0 o 1 con profundidad 1.
#[derive(Debug)]
pub struct Image {
    pub w: i32,
    pub h: i32,
    pub depth: u8,
    /// `w * h` píxeles, fila a fila.
    pub data: Vec<u32>,
    /// Presupuesto al que se cargaron sus píxeles (los búferes internos del
    /// servidor no cuentan).
    budget: Option<Arc<Budget>>,
}

impl Clone for Image {
    fn clone(&self) -> Self {
        // Una copia ocupa lo mismo que el original y también cuenta.
        if let Some(budget) = &self.budget {
            budget
                .used
                .fetch_add(self.data.len() as u64, Ordering::Relaxed);
        }
        Image {
            w: self.w,
            h: self.h,
            depth: self.depth,
            data: self.data.clone(),
            budget: self.budget.clone(),
        }
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        if let Some(budget) = &self.budget {
            let pixels = (self.w.max(0) as u64) * (self.h.max(0) as u64);
            // Nunca por debajo de cero, pase lo que pase.
            let _ = budget
                .used
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                    Some(used.saturating_sub(pixels))
                });
        }
    }
}

impl PartialEq for Image {
    fn eq(&self, other: &Self) -> bool {
        (self.w, self.h, self.depth) == (other.w, other.h, other.depth) && self.data == other.data
    }
}

impl Image {
    /// Tamaño real del búfer para un tamaño pedido (recortado a los topes).
    fn capped(w: u32, h: u32) -> (u32, u32) {
        let w = w.min(MAX_SIDE);
        let mut h = h.min(MAX_SIDE);
        if w as u64 * h as u64 > MAX_PIXELS {
            h = (MAX_PIXELS / w.max(1) as u64) as u32;
        }
        (w, h)
    }

    /// Búfer interno del servidor: no se carga a ningún presupuesto.
    pub fn new(w: u32, h: u32, depth: u8) -> Self {
        let (w, h) = Self::capped(w, h);
        Image {
            w: w as i32,
            h: h as i32,
            depth,
            data: vec![0; w as usize * h as usize],
            budget: None,
        }
    }

    /// Búfer de un cliente, cargado al presupuesto global. `None` si no cabe.
    pub fn budgeted(budget: &Arc<Budget>, w: u32, h: u32, depth: u8) -> Option<Self> {
        let (w, h) = Self::capped(w, h);
        if !budget.try_reserve(w as u64 * h as u64) {
            return None;
        }
        let mut img = Image::new(w, h, depth);
        img.budget = Some(budget.clone());
        Some(img)
    }

    /// Píxeles que ocupa.
    pub fn pixels(&self) -> u64 {
        self.data.len() as u64
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }

    pub fn depth_mask(&self) -> u32 {
        match self.depth {
            32.. => u32::MAX,
            d => (1u32 << d) - 1,
        }
    }

    /// Píxel en `(x, y)`; 0 fuera de la imagen.
    pub fn get(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return 0;
        }
        self.data[y as usize * self.w as usize + x as usize]
    }

    pub fn row(&self, y: i32, x0: i32, x1: i32) -> &[u32] {
        let start = y as usize * self.w as usize;
        &self.data[start + x0 as usize..start + x1 as usize]
    }

    pub fn row_mut(&mut self, y: i32, x0: i32, x1: i32) -> &mut [u32] {
        let start = y as usize * self.w as usize;
        &mut self.data[start + x0 as usize..start + x1 as usize]
    }

    /// Rellena un rectángulo (recortado a la imagen) con un valor.
    pub fn fill(&mut self, r: Rect, pixel: u32) {
        let r = r.intersect(&self.bounds());
        let pixel = pixel & self.depth_mask();
        for y in r.y0..r.y1 {
            self.row_mut(y, r.x0, r.x1).fill(pixel);
        }
    }

    /// Rellena un rectángulo con un mosaico cuyo origen está en `(ox, oy)`.
    pub fn fill_tiled(&mut self, r: Rect, tile: &Image, ox: i32, oy: i32) {
        let r = r.intersect(&self.bounds());
        if tile.w <= 0 || tile.h <= 0 {
            return;
        }
        for y in r.y0..r.y1 {
            let ty = (y - oy).rem_euclid(tile.h);
            for x in r.x0..r.x1 {
                let tx = (x - ox).rem_euclid(tile.w);
                self.data[y as usize * self.w as usize + x as usize] = tile.get(tx, ty);
            }
        }
    }

    /// Copia de una zona; lo que cae fuera de la imagen queda a 0.
    pub fn extract(&self, r: Rect) -> Vec<u32> {
        let (w, h) = (r.width() as usize, r.height() as usize);
        let mut out = vec![0u32; w * h];
        let inside = r.intersect(&self.bounds());
        for y in inside.y0..inside.y1 {
            let dst = (y - r.y0) as usize * w + (inside.x0 - r.x0) as usize;
            let src = self.row(y, inside.x0, inside.x1);
            out[dst..dst + src.len()].copy_from_slice(src);
        }
        out
    }

    /// Nueva imagen de otro tamaño que conserva el contenido desplazado.
    /// `None` si no cabe en el presupuesto de memoria.
    pub fn resized(&self, w: u32, h: u32, dx: i32, dy: i32) -> Option<Image> {
        let mut out = match &self.budget {
            Some(budget) => Image::budgeted(budget, w, h, self.depth)?,
            None => Image::new(w, h, self.depth),
        };
        let dst = self.bounds().translate(dx, dy).intersect(&out.bounds());
        for y in dst.y0..dst.y1 {
            let src = self.row(y - dy, dst.x0 - dx, dst.x1 - dx);
            out.row_mut(y, dst.x0, dst.x1).copy_from_slice(src);
        }
        Some(out)
    }
}

/// Tope de trabajo (filas × vértices) al rellenar un polígono.
const MAX_POLYGON_WORK: u64 = 200_000_000;

// Valores de los atributos del GC.
pub const GX_COPY: u8 = 3;
#[cfg(test)]
pub const GX_XOR: u8 = 6;
pub const FILL_SOLID: u8 = 0;
pub const FILL_TILED: u8 = 1;
pub const FILL_STIPPLED: u8 = 2;
pub const FILL_OPAQUE_STIPPLED: u8 = 3;
pub const CAP_NOT_LAST: u8 = 0;

/// Recorte de un contexto gráfico.
#[derive(Debug, Clone, PartialEq)]
pub enum Clip {
    None,
    /// Rectángulos relativos al origen de recorte.
    Rects(Vec<Rect>),
    /// Máscara de profundidad 1 colocada en el origen de recorte.
    Mask(Arc<Image>),
}

/// Contexto gráfico.
#[derive(Debug, Clone)]
pub struct Gc {
    pub depth: u8,
    pub function: u8,
    pub plane_mask: u32,
    pub foreground: u32,
    pub background: u32,
    pub line_width: u16,
    pub line_style: u8,
    pub cap_style: u8,
    pub join_style: u8,
    pub fill_style: u8,
    pub fill_rule: u8,
    pub tile: Option<Arc<Image>>,
    pub stipple: Option<Arc<Image>>,
    pub ts_x: i16,
    pub ts_y: i16,
    /// Identificador de la fuente (0 = la de por defecto).
    pub font: u32,
    pub subwindow_mode: u8,
    pub graphics_exposures: bool,
    pub clip_x: i16,
    pub clip_y: i16,
    pub clip: Clip,
    pub dash_offset: u16,
    pub dashes: Vec<u8>,
    pub arc_mode: u8,
}

impl Gc {
    pub fn new(depth: u8) -> Self {
        Gc {
            depth,
            function: GX_COPY,
            plane_mask: u32::MAX,
            foreground: 0,
            background: 1,
            line_width: 0,
            line_style: 0,
            cap_style: 1,
            join_style: 0,
            fill_style: FILL_SOLID,
            fill_rule: 0,
            tile: None,
            stipple: None,
            ts_x: 0,
            ts_y: 0,
            font: 0,
            subwindow_mode: 0,
            graphics_exposures: true,
            clip_x: 0,
            clip_y: 0,
            clip: Clip::None,
            dash_offset: 0,
            dashes: vec![4, 4],
            arc_mode: 1,
        }
    }
}

/// Las 16 funciones lógicas de X entre origen y destino.
pub fn raster_op(function: u8, s: u32, d: u32) -> u32 {
    match function {
        0 => 0,
        1 => s & d,
        2 => s & !d,
        3 => s,
        4 => !s & d,
        5 => d,
        6 => s ^ d,
        7 => s | d,
        8 => !s & !d,
        9 => !s ^ d,
        10 => !d,
        11 => s | !d,
        12 => !s,
        13 => !s | d,
        14 => !s | !d,
        _ => u32::MAX,
    }
}

/// De dónde sale el color de cada píxel que se pinta.
#[derive(Clone, Copy)]
enum Source<'a> {
    Solid(u32),
    Tiled(&'a Image),
    /// Solo se pinta donde la trama vale 1.
    Stippled(&'a Image, u32),
    /// Trama con primer plano y fondo.
    Opaque(&'a Image, u32, u32),
}

/// Pinta en una imagen con las reglas de un contexto gráfico y lleva la
/// cuenta de la zona modificada.
pub struct Painter<'a> {
    img: &'a mut Image,
    function: u8,
    plane_mask: u32,
    depth_mask: u32,
    source: Source<'a>,
    ts: (i32, i32),
    clips: Vec<Rect>,
    mask: Option<(&'a Image, i32, i32)>,
    /// Copia directa de un color sólido: el caso habitual, sin cuentas.
    fast: bool,
    /// Caja que contiene todo lo pintado.
    pub damage: Rect,
}

impl<'a> Painter<'a> {
    pub fn new(img: &'a mut Image, gc: &'a Gc) -> Self {
        let bounds = img.bounds();
        let origin = (gc.clip_x as i32, gc.clip_y as i32);
        let (clips, mask) = match &gc.clip {
            Clip::None => (vec![bounds], None),
            Clip::Rects(rects) => (
                rects
                    .iter()
                    .map(|r| r.translate(origin.0, origin.1).intersect(&bounds))
                    .filter(|r| !r.is_empty())
                    .collect(),
                None,
            ),
            Clip::Mask(m) => (
                vec![m.bounds().translate(origin.0, origin.1).intersect(&bounds)],
                Some((m.as_ref(), origin.0, origin.1)),
            ),
        };
        let source = match (gc.fill_style, &gc.tile, &gc.stipple) {
            (FILL_TILED, Some(tile), _) if tile.w > 0 && tile.h > 0 => Source::Tiled(tile),
            (FILL_STIPPLED, _, Some(s)) if s.w > 0 && s.h > 0 => Source::Stippled(s, gc.foreground),
            (FILL_OPAQUE_STIPPLED, _, Some(s)) if s.w > 0 && s.h > 0 => {
                Source::Opaque(s, gc.foreground, gc.background)
            }
            _ => Source::Solid(gc.foreground),
        };
        let depth_mask = img.depth_mask();
        let mut painter = Painter {
            img,
            function: gc.function,
            plane_mask: gc.plane_mask & depth_mask,
            depth_mask,
            source,
            ts: (gc.ts_x as i32, gc.ts_y as i32),
            clips,
            mask,
            fast: false,
            damage: Rect::EMPTY,
        };
        painter.update_fast();
        painter
    }

    fn update_fast(&mut self) {
        self.fast = self.function == GX_COPY
            && self.plane_mask == self.depth_mask
            && self.mask.is_none()
            && matches!(self.source, Source::Solid(_));
    }

    /// Pasa a pintar un color sólido con la función «copiar» (lo que piden
    /// `ImageText` y `ClearArea`), conservando recorte y máscara de planos.
    pub fn solid_copy(&mut self, color: u32) {
        self.source = Source::Solid(color);
        self.function = GX_COPY;
        self.update_fast();
    }

    /// Límites verticales de lo que el recorte deja pintar.
    fn clip_rows(&self) -> (i32, i32) {
        let y0 = self.clips.iter().map(|c| c.y0).min().unwrap_or(0);
        let y1 = self.clips.iter().map(|c| c.y1).max().unwrap_or(0);
        (y0, y1)
    }

    fn clip_cols(&self) -> (i32, i32) {
        let x0 = self.clips.iter().map(|c| c.x0).min().unwrap_or(0);
        let x1 = self.clips.iter().map(|c| c.x1).max().unwrap_or(0);
        (x0, x1)
    }

    fn mix(&self, dst: u32, src: u32) -> u32 {
        let out = raster_op(self.function, src, dst);
        ((dst & !self.plane_mask) | (out & self.plane_mask)) & self.depth_mask
    }

    fn source_at(&self, x: i32, y: i32) -> Option<u32> {
        let at = |img: &Image| {
            img.get(
                (x - self.ts.0).rem_euclid(img.w),
                (y - self.ts.1).rem_euclid(img.h),
            )
        };
        match self.source {
            Source::Solid(c) => Some(c),
            Source::Tiled(t) => Some(at(t)),
            Source::Stippled(s, fg) => (at(s) & 1 != 0).then_some(fg),
            Source::Opaque(s, fg, bg) => Some(if at(s) & 1 != 0 { fg } else { bg }),
        }
    }

    fn mask_allows(&self, x: i32, y: i32) -> bool {
        match self.mask {
            Some((m, ox, oy)) => m.get(x - ox, y - oy) & 1 != 0,
            None => true,
        }
    }

    fn touch(&mut self, r: Rect) {
        self.damage = self.damage.union(&r);
    }

    /// Pinta el tramo `[x0, x1)` de la fila `y`.
    pub fn span(&mut self, y: i32, x0: i32, x1: i32) {
        for i in 0..self.clips.len() {
            let c = self.clips[i];
            if y < c.y0 || y >= c.y1 {
                continue;
            }
            let (a, b) = (x0.max(c.x0), x1.min(c.x1));
            if a >= b {
                continue;
            }
            if self.fast {
                let Source::Solid(color) = self.source else {
                    continue;
                };
                let color = color & self.depth_mask;
                self.img.row_mut(y, a, b).fill(color);
            } else {
                for x in a..b {
                    if !self.mask_allows(x, y) {
                        continue;
                    }
                    if let Some(src) = self.source_at(x, y) {
                        let i = y as usize * self.img.w as usize + x as usize;
                        self.img.data[i] = self.mix(self.img.data[i], src);
                    }
                }
            }
            self.touch(Rect {
                x0: a,
                y0: y,
                x1: b,
                y1: y + 1,
            });
        }
    }

    pub fn pixel(&mut self, x: i32, y: i32) {
        self.span(y, x, x + 1);
    }

    /// Rellena un rectángulo.
    pub fn rect(&mut self, r: Rect) {
        let (y0, y1) = self.clip_rows();
        for y in r.y0.max(y0)..r.y1.min(y1) {
            self.span(y, r.x0, r.x1);
        }
    }

    /// Copia un bloque de píxeles (`w` de ancho) en `(x, y)` con la función
    /// y la máscara de planos del GC. `valid` limita la parte del bloque
    /// que tiene datos.
    pub fn blit(&mut self, x: i32, y: i32, w: i32, block: &[u32], valid: Rect) {
        let direct =
            self.function == GX_COPY && self.plane_mask == self.depth_mask && self.mask.is_none();
        let dst = valid.translate(x, y);
        for i in 0..self.clips.len() {
            let r = self.clips[i].intersect(&dst);
            if r.is_empty() {
                continue;
            }
            for py in r.y0..r.y1 {
                let src = (py - y) as usize * w as usize + (r.x0 - x) as usize;
                let src = &block[src..src + r.width() as usize];
                if direct {
                    let mask = self.depth_mask;
                    let row = self.img.row_mut(py, r.x0, r.x1);
                    if mask == u32::MAX {
                        row.copy_from_slice(src);
                    } else {
                        for (d, s) in row.iter_mut().zip(src) {
                            *d = s & mask;
                        }
                    }
                } else {
                    for (k, s) in src.iter().enumerate() {
                        let px = r.x0 + k as i32;
                        if self.mask_allows(px, py) {
                            let i = py as usize * self.img.w as usize + px as usize;
                            self.img.data[i] = self.mix(self.img.data[i], *s);
                        }
                    }
                }
            }
            self.touch(r);
        }
    }

    /// Línea fina (de un píxel) por Bresenham. `first`/`last` indican si se
    /// pintan los extremos (en una polilínea los vértices solo van una vez).
    pub fn thin_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, first: bool, last: bool) {
        if y0 == y1 {
            let (mut a, mut b) = (x0.min(x1), x0.max(x1) + 1);
            let (skip_low, skip_high) = if x0 <= x1 {
                (!first, !last)
            } else {
                (!last, !first)
            };
            if skip_low {
                a += 1;
            }
            if skip_high {
                b -= 1;
            }
            self.span(y0, a, b);
            return;
        }
        // Recorte burdo en vertical para no recorrer líneas kilométricas.
        let (cy0, cy1) = self.clip_rows();
        let (cx0, cx1) = self.clip_cols();
        if y0.max(y1) < cy0 || y0.min(y1) >= cy1 || x0.max(x1) < cx0 || x0.min(x1) >= cx1 {
            return;
        }
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let mut err = dx + dy;
        let (mut x, mut y) = (x0, y0);
        loop {
            let end = x == x1 && y == y1;
            let start = x == x0 && y == y0;
            if (!start || first) && (!end || last) {
                self.span(y, x, x + 1);
            }
            if end {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// Segmento con el grosor del GC.
    #[allow(clippy::too_many_arguments)]
    pub fn line(&mut self, gc: &Gc, x0: i32, y0: i32, x1: i32, y1: i32, first: bool, last: bool) {
        let width = gc.line_width as i32;
        if width <= 1 {
            let last = last && !(gc.cap_style == CAP_NOT_LAST && width == 0);
            self.thin_line(x0, y0, x1, y1, first, last);
            return;
        }
        let (fx0, fy0, fx1, fy1) = (x0 as f64, y0 as f64, x1 as f64, y1 as f64);
        let (dx, dy) = (fx1 - fx0, fy1 - fy0);
        let len = (dx * dx + dy * dy).sqrt();
        let half = width as f64 / 2.0;
        if len == 0.0 {
            // Un punto: un cuadradito salvo con extremos romos.
            if gc.cap_style != 1 {
                self.rect(Rect::new(x0 - width / 2, y0 - width / 2, width, width));
            }
            return;
        }
        let (ux, uy) = (dx / len, dy / len);
        // Los extremos proyectados (o redondos) alargan media anchura.
        let ext = if gc.cap_style >= 2 { half } else { 0.0 };
        let (ax, ay) = (fx0 - ux * ext, fy0 - uy * ext);
        let (bx, by) = (fx1 + ux * ext, fy1 + uy * ext);
        let (nx, ny) = (-uy * half, ux * half);
        self.polygon(
            &[
                (ax + nx, ay + ny),
                (bx + nx, by + ny),
                (bx - nx, by - ny),
                (ax - nx, ay - ny),
            ],
            true,
        );
    }

    /// Relleno de polígono por líneas de barrido: se pinta el píxel cuyo
    /// centro queda dentro (borde izquierdo y superior incluidos).
    pub fn polygon(&mut self, pts: &[(f64, f64)], winding: bool) {
        if pts.len() < 3 {
            return;
        }
        let (cy0, cy1) = self.clip_rows();
        let ymin = pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
        let ymax = pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
        let y_start = (ymin.ceil() as i32).max(cy0);
        let y_end = (ymax.ceil() as i32).min(cy1);
        // Un polígono de millones de vértices sobre miles de filas tendría
        // al servidor ocupado minutos: no es un dibujo, es un abuso.
        let rows = (y_end as i64 - y_start as i64).max(0) as u64;
        if rows * pts.len() as u64 > MAX_POLYGON_WORK {
            return;
        }
        let mut crossings: Vec<(f64, i32)> = Vec::with_capacity(8);
        for y in y_start..y_end {
            let fy = y as f64;
            crossings.clear();
            for i in 0..pts.len() {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                if a.1 == b.1 {
                    continue;
                }
                let (lo, hi, dir) = if a.1 < b.1 { (a, b, 1) } else { (b, a, -1) };
                if fy >= lo.1 && fy < hi.1 {
                    let x = lo.0 + (fy - lo.1) * (hi.0 - lo.0) / (hi.1 - lo.1);
                    crossings.push((x, dir));
                }
            }
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
            if winding {
                let mut count = 0;
                let mut start = 0.0;
                for (x, dir) in &crossings {
                    if count == 0 {
                        start = *x;
                    }
                    count += dir;
                    if count == 0 {
                        self.span(y, start.ceil() as i32, x.ceil() as i32);
                    }
                }
            } else {
                for pair in crossings.chunks_exact(2) {
                    self.span(y, pair[0].0.ceil() as i32, pair[1].0.ceil() as i32);
                }
            }
        }
    }

    /// Contorno de un rectángulo de `w`×`h` (cubre `w + 1`×`h + 1` píxeles).
    pub fn rect_outline(&mut self, gc: &Gc, x: i32, y: i32, w: i32, h: i32) {
        let lw = (gc.line_width as i32).max(1);
        let (a, b) = (lw / 2, lw - lw / 2);
        // Barras superior e inferior completas; las laterales, entre ellas.
        self.rect(Rect {
            x0: x - a,
            y0: y - a,
            x1: x + w + b,
            y1: y + b,
        });
        if h > 0 {
            self.rect(Rect {
                x0: x - a,
                y0: y + h - a,
                x1: x + w + b,
                y1: y + h + b,
            });
        }
        let (top, bottom) = (y + b, y + h - a);
        if bottom > top {
            self.rect(Rect {
                x0: x - a,
                y0: top,
                x1: x + b,
                y1: bottom,
            });
            if w > 0 {
                self.rect(Rect {
                    x0: x + w - a,
                    y0: top,
                    x1: x + w + b,
                    y1: bottom,
                });
            }
        }
    }

    /// Arco de elipse inscrito en el rectángulo dado. Los ángulos van en
    /// 64.ᵃˢ de grado, en sentido antihorario desde las tres en punto.
    #[allow(clippy::too_many_arguments)]
    pub fn arc(&mut self, gc: &Gc, x: i32, y: i32, w: i32, h: i32, a1: i32, a2: i32, fill: bool) {
        if w < 0 || h < 0 {
            return;
        }
        let (start, extent) = normalize_angles(a1, a2);
        if extent == 0.0 {
            return;
        }
        let full = extent >= 360.0;
        let inside_angle = |dx: f64, dy: f64, rx: f64, ry: f64| -> bool {
            if full {
                return true;
            }
            let angle = (-dy / ry.max(0.5)).atan2(dx / rx.max(0.5)).to_degrees();
            let rel = (angle - start).rem_euclid(360.0);
            rel <= extent
        };
        let (cy0, cy1) = self.clip_rows();
        // (Los bucles por píxel se limitan a las columnas que se ven: un
        // arco de radio enorme no debe costar más que la zona visible.)
        let (cx0, cx1) = self.clip_cols();
        if fill {
            // Relleno: celdas de píxel dentro de la elipse de w×h.
            let (rx, ry) = (w as f64 / 2.0, h as f64 / 2.0);
            if rx == 0.0 || ry == 0.0 {
                return;
            }
            let (cx, cy) = (x as f64 + rx, y as f64 + ry);
            // Cuerda: el lado de la recta entre extremos donde cae el arco.
            let chord = (!full && gc.arc_mode == 0).then(|| {
                let p = |deg: f64| {
                    let t = deg.to_radians();
                    (cx + rx * t.cos(), cy - ry * t.sin())
                };
                let (p1, p2, mid) = (p(start), p(start + extent), p(start + extent / 2.0));
                let side =
                    |q: (f64, f64)| (p2.0 - p1.0) * (q.1 - p1.1) - (p2.1 - p1.1) * (q.0 - p1.0);
                (p1, p2, side(mid) >= 0.0)
            });
            for py in y.max(cy0)..(y + h).min(cy1) {
                let dy = py as f64 + 0.5 - cy;
                let t = 1.0 - (dy / ry) * (dy / ry);
                if t < 0.0 {
                    continue;
                }
                let half = rx * t.sqrt();
                let xl = ((cx - half - 0.5).ceil() as i32).max(cx0);
                let xr = ((cx + half - 0.5).floor() as i32).min(cx1);
                if full {
                    self.span(py, xl, xr + 1);
                    continue;
                }
                let mut run: Option<i32> = None;
                for px in xl..=xr + 1 {
                    let dx = px as f64 + 0.5 - cx;
                    let inside = px <= xr
                        && match &chord {
                            Some((p1, p2, positive)) => {
                                let s = (p2.0 - p1.0) * (py as f64 + 0.5 - p1.1)
                                    - (p2.1 - p1.1) * (px as f64 + 0.5 - p1.0);
                                (s >= 0.0) == *positive
                            }
                            None => inside_angle(dx, dy, rx, ry),
                        };
                    match (inside, run) {
                        (true, None) => run = Some(px),
                        (false, Some(start_x)) => {
                            self.span(py, start_x, px);
                            run = None;
                        }
                        _ => {}
                    }
                }
            }
            return;
        }
        let lw = gc.line_width as i32;
        let (rx, ry) = (w as f64 / 2.0, h as f64 / 2.0);
        let (cx, cy) = (x as f64 + rx, y as f64 + ry);
        if lw <= 1 {
            // Trazo fino: puntos de la curva a menos de un píxel entre sí.
            let steps = ((extent.to_radians() * rx.max(ry).max(1.0) * 2.0).ceil() as usize).max(1);
            let mut prev = None;
            let mut first = None;
            for i in 0..=steps {
                let t = (start + extent * i as f64 / steps as f64).to_radians();
                let p = (
                    (cx + rx * t.cos()).round() as i32,
                    (cy - ry * t.sin()).round() as i32,
                );
                if prev == Some(p) || (i == steps && first == Some(p) && steps > 1) {
                    continue;
                }
                first.get_or_insert(p);
                prev = Some(p);
                self.span(p.1, p.0, p.0 + 1);
            }
            return;
        }
        // Trazo grueso: anillo entre dos elipses.
        let half = lw as f64 / 2.0;
        let (orx, ory) = (rx + half, ry + half);
        let (irx, iry) = (rx - half, ry - half);
        let y_top = (cy - ory).floor() as i32;
        let y_bottom = (cy + ory).ceil() as i32;
        for py in y_top.max(cy0)..=y_bottom.min(cy1 - 1) {
            let dy = py as f64 - cy;
            let t = 1.0 - (dy / ory) * (dy / ory);
            if t < 0.0 {
                continue;
            }
            let outer = orx * t.sqrt();
            let inner = if irx > 0.0 && iry > 0.0 {
                let ti = 1.0 - (dy / iry) * (dy / iry);
                (ti > 0.0).then(|| irx * ti.sqrt())
            } else {
                None
            };
            let xl = ((cx - outer).ceil() as i32).max(cx0);
            let xr = ((cx + outer).floor() as i32).min(cx1);
            let mut run: Option<i32> = None;
            for px in xl..=xr + 1 {
                let dx = px as f64 - cx;
                let inside =
                    px <= xr && inner.is_none_or(|i| dx.abs() >= i) && inside_angle(dx, dy, rx, ry);
                match (inside, run) {
                    (true, None) => run = Some(px),
                    (false, Some(start_x)) => {
                        self.span(py, start_x, px);
                        run = None;
                    }
                    _ => {}
                }
            }
        }
    }

    /// Dibuja texto con el origen de la línea base en `(x, y)`. Devuelve el
    /// avance total. Con un color sólido copiado tal cual, los bordes se
    /// suavizan mezclando el primer plano con lo que hubiera debajo.
    pub fn text(&mut self, font: &FontCore, x: i32, y: i32, codes: &[u32]) -> i32 {
        let mut pen = x;
        let blend = self.fast && self.img.depth >= 24;
        for code in codes {
            let info = font.info(*code);
            if !info.exists() {
                continue;
            }
            if let Some(glyph) = font.glyph(*code) {
                let (gx, gy) = (pen.saturating_add(glyph.left), y - glyph.top);
                let cell = Rect::new(gx, gy, glyph.width as i32, glyph.height as i32);
                for i in 0..self.clips.len() {
                    let r = self.clips[i].intersect(&cell);
                    if r.is_empty() {
                        continue;
                    }
                    if blend {
                        let Source::Solid(fg) = self.source else {
                            continue;
                        };
                        for py in r.y0..r.y1 {
                            let cov = &glyph.coverage
                                [(py - gy) as usize * glyph.width + (r.x0 - gx) as usize..];
                            let row = self.img.row_mut(py, r.x0, r.x1);
                            for (d, c) in row.iter_mut().zip(cov) {
                                match *c {
                                    0 => {}
                                    255 => *d = fg & 0x00ff_ffff,
                                    c => *d = blend_pixel(*d, fg, c),
                                }
                            }
                        }
                        self.touch(r);
                    } else {
                        for py in r.y0..r.y1 {
                            for px in r.x0..r.x1 {
                                let c = glyph.coverage
                                    [(py - gy) as usize * glyph.width + (px - gx) as usize];
                                if c >= 128 {
                                    self.span(py, px, px + 1);
                                }
                            }
                        }
                    }
                }
            }
            pen = pen.saturating_add(info.width as i32);
        }
        pen.saturating_sub(x)
    }
}

/// Mezcla `fg` sobre `dst` con una cobertura de 0 a 255 (píxeles 0xRRGGBB).
pub fn blend_pixel(dst: u32, fg: u32, coverage: u8) -> u32 {
    let a = coverage as u32;
    let mix = |shift: u32| {
        let (d, f) = ((dst >> shift) & 0xff, (fg >> shift) & 0xff);
        ((d * (255 - a) + f * a + 127) / 255) << shift
    };
    mix(16) | mix(8) | mix(0)
}

/// Ángulo inicial en grados `[0, 360)` y extensión positiva (máximo 360).
fn normalize_angles(a1: i32, a2: i32) -> (f64, f64) {
    let (mut start, mut extent) = (a1 as f64 / 64.0, a2 as f64 / 64.0);
    if extent < 0.0 {
        start += extent;
        extent = -extent;
    }
    (start.rem_euclid(360.0), extent.min(360.0))
}

/// Bytes por fila de una imagen de `width` píxeles con `bpp` bits por píxel
/// (filas rellenas a 32 bits).
pub fn row_bytes(width: usize, bpp: usize) -> usize {
    (width * bpp).div_ceil(32) * 4
}

/// Decodifica una imagen ZPixmap de 32 bits por píxel (orden LSB).
pub fn decode_z32(data: &[u8], w: usize, h: usize) -> Option<Vec<u32>> {
    let need = w.checked_mul(h)?.checked_mul(4)?;
    let data = data.get(..need)?;
    Some(
        data.chunks_exact(4)
            .map(|p| u32::from_le_bytes([p[0], p[1], p[2], p[3]]))
            .collect(),
    )
}

/// Decodifica un mapa de bits (un plano): filas rellenas a 32 bits, bit
/// menos significativo primero, con `left_pad` bits iniciales sin usar.
pub fn decode_bitmap(data: &[u8], w: usize, h: usize, left_pad: usize) -> Option<Vec<u32>> {
    let stride = row_bytes(w + left_pad, 1);
    let data = data.get(..stride.checked_mul(h)?)?;
    let mut out = Vec::with_capacity(w * h);
    for row in data.chunks_exact(stride.max(1)).take(h) {
        for x in left_pad..left_pad + w {
            out.push(((row[x / 8] >> (x % 8)) & 1) as u32);
        }
    }
    (out.len() == w * h).then_some(out)
}

/// Codifica un plano de bits de un bloque de píxeles (para `GetImage`).
pub fn encode_bitmap(block: &[u32], w: usize, h: usize, plane: u32, out: &mut Vec<u8>) {
    let stride = row_bytes(w, 1);
    for y in 0..h {
        let start = out.len();
        out.resize(start + stride, 0);
        for x in 0..w {
            if block[y * w + x] & plane != 0 {
                out[start + x / 8] |= 1 << (x % 8);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(w: u32, h: u32) -> Image {
        Image::new(w, h, 24)
    }

    fn count(img: &Image, pixel: u32) -> usize {
        img.data.iter().filter(|p| **p == pixel).count()
    }

    #[test]
    fn rect_arithmetic() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(&b), Rect::new(5, 5, 5, 5));
        assert_eq!(a.union(&b), Rect::new(0, 0, 15, 15));
        assert!(a.intersect(&Rect::new(20, 0, 5, 5)).is_empty());
        assert_eq!(a.union(&Rect::EMPTY), a);
        assert!(a.contains(9, 9) && !a.contains(10, 9));
        let parts = a.subtract(&b);
        let area: i32 = parts.iter().map(|r| r.width() * r.height()).sum();
        assert_eq!(area, 75);
        assert_eq!(a.subtract(&Rect::new(50, 50, 1, 1)), vec![a]);
        assert!(a.subtract(&a).is_empty());
    }

    #[test]
    fn huge_drawables_are_capped() {
        let img = Image::new(32_767, 32_767, 24);
        assert!(img.data.len() as u64 <= MAX_PIXELS);
        assert!(img.w > 0 && img.h > 0);
        // Pintar fuera del búfer recortado no rompe nada.
        let mut img = img;
        let gc = Gc::new(24);
        let mut p = Painter::new(&mut img, &gc);
        p.rect(Rect::new(30_000, 30_000, 100, 100));
        p.thin_line(-5, -5, 40_000, 3, true, true);
    }

    #[test]
    fn fill_respects_bounds_and_clip_rectangles() {
        let mut img = canvas(20, 10);
        let mut gc = Gc::new(24);
        gc.foreground = 0x112233;
        let mut p = Painter::new(&mut img, &gc);
        p.rect(Rect::new(-5, -5, 10, 10));
        assert_eq!(p.damage, Rect::new(0, 0, 5, 5));
        assert_eq!(count(&img, 0x112233), 25);

        gc.clip = Clip::Rects(vec![Rect::new(0, 0, 2, 2), Rect::new(10, 0, 3, 1)]);
        gc.clip_x = 1;
        gc.clip_y = 1;
        gc.foreground = 0xff0000;
        let mut p = Painter::new(&mut img, &gc);
        p.rect(Rect::new(0, 0, 20, 10));
        assert_eq!(count(&img, 0xff0000), 4 + 3);
        assert_eq!(img.get(1, 1), 0xff0000);
        assert_eq!(img.get(11, 1), 0xff0000);
        assert_eq!(img.get(0, 0), 0x112233);
    }

    #[test]
    fn functions_and_plane_mask() {
        assert_eq!(raster_op(0, 0xf0, 0x3c), 0);
        assert_eq!(raster_op(1, 0xf0, 0x3c), 0x30);
        assert_eq!(raster_op(GX_COPY, 0xf0, 0x3c), 0xf0);
        assert_eq!(raster_op(GX_XOR, 0xf0, 0x3c), 0xcc);
        assert_eq!(raster_op(7, 0xf0, 0x3c), 0xfc);
        assert_eq!(raster_op(10, 0xf0, 0x3c) & 0xff, 0xc3);
        assert_eq!(raster_op(12, 0xf0, 0x3c) & 0xff, 0x0f);
        assert_eq!(raster_op(15, 0xf0, 0x3c), u32::MAX);

        let mut img = canvas(4, 1);
        img.fill(img.bounds(), 0x00ff00);
        let mut gc = Gc::new(24);
        gc.function = GX_XOR;
        gc.foreground = 0xffffff;
        let mut p = Painter::new(&mut img, &gc);
        p.span(0, 0, 2);
        assert_eq!(img.data, vec![0xff00ff, 0xff00ff, 0x00ff00, 0x00ff00]);
        // Dos veces XOR deja lo que había.
        let mut p = Painter::new(&mut img, &gc);
        p.span(0, 0, 2);
        assert_eq!(count(&img, 0x00ff00), 4);
        // La máscara de planos protege los bits que no incluye.
        gc.function = GX_COPY;
        gc.plane_mask = 0x0000ff;
        gc.foreground = 0x123456;
        let mut p = Painter::new(&mut img, &gc);
        p.span(0, 0, 1);
        assert_eq!(img.get(0, 0), 0x00ff56);
        // En profundidad 1 solo existe el bit bajo.
        let mut bitmap = Image::new(2, 1, 1);
        let mut gc = Gc::new(1);
        gc.foreground = 0xffff_ffff;
        Painter::new(&mut bitmap, &gc).span(0, 0, 1);
        assert_eq!(bitmap.data, vec![1, 0]);
    }

    #[test]
    fn tiles_stipples_and_clip_masks() {
        let mut tile = canvas(2, 2);
        tile.data = vec![1, 2, 3, 4];
        let mut stipple = Image::new(2, 1, 1);
        stipple.data = vec![1, 0];

        let mut img = canvas(4, 2);
        let mut gc = Gc::new(24);
        gc.fill_style = FILL_TILED;
        gc.tile = Some(Arc::new(tile));
        gc.ts_x = 1;
        Painter::new(&mut img, &gc).rect(Rect::new(0, 0, 4, 2));
        assert_eq!(img.data, vec![2, 1, 2, 1, 4, 3, 4, 3]);

        gc.fill_style = FILL_STIPPLED;
        gc.stipple = Some(Arc::new(stipple));
        gc.ts_x = 0;
        gc.foreground = 9;
        Painter::new(&mut img, &gc).rect(Rect::new(0, 0, 4, 1));
        assert_eq!(&img.data[..4], &[9, 1, 9, 1]);

        gc.fill_style = FILL_OPAQUE_STIPPLED;
        gc.background = 7;
        Painter::new(&mut img, &gc).rect(Rect::new(0, 1, 4, 1));
        assert_eq!(&img.data[4..], &[9, 7, 9, 7]);

        // Máscara de recorte de 2×1 colocada en (1, 0): solo deja el (1, 0).
        let mut mask = Image::new(2, 1, 1);
        mask.data = vec![1, 0];
        let mut gc = Gc::new(24);
        gc.clip = Clip::Mask(Arc::new(mask));
        gc.clip_x = 1;
        gc.foreground = 0xabcdef;
        let mut p = Painter::new(&mut img, &gc);
        p.rect(Rect::new(0, 0, 4, 2));
        assert_eq!(p.damage, Rect::new(1, 0, 2, 1));
        assert_eq!(count(&img, 0xabcdef), 1);
        assert_eq!(img.get(1, 0), 0xabcdef);
    }

    #[test]
    fn lines_thin_and_wide() {
        let mut img = canvas(10, 10);
        let mut gc = Gc::new(24);
        gc.foreground = 1;
        gc.cap_style = 1;
        let mut p = Painter::new(&mut img, &gc);
        p.line(&gc, 0, 0, 9, 9, true, true);
        assert_eq!(count(&img, 1), 10);
        assert!((0..10).all(|i| img.get(i, i) == 1));
        // Horizontal al revés y sin el último punto.
        let mut img = canvas(10, 10);
        let mut p = Painter::new(&mut img, &gc);
        p.thin_line(8, 2, 3, 2, true, false);
        assert_eq!(count(&img, 1), 5);
        assert_eq!((img.get(8, 2), img.get(4, 2), img.get(3, 2)), (1, 1, 0));
        // Gruesa horizontal: tres filas centradas en la línea.
        let mut img = canvas(10, 10);
        gc.line_width = 3;
        let mut p = Painter::new(&mut img, &gc);
        p.line(&gc, 2, 5, 8, 5, true, true);
        assert_eq!(count(&img, 1), 6 * 3);
        assert_eq!((img.get(2, 4), img.get(2, 6), img.get(2, 3)), (1, 1, 0));
        // Gruesa vertical de anchura par.
        let mut img = canvas(10, 10);
        gc.line_width = 2;
        let mut p = Painter::new(&mut img, &gc);
        p.line(&gc, 5, 1, 5, 9, true, true);
        assert_eq!(count(&img, 1), 8 * 2);
    }

    #[test]
    fn rectangles_outlines() {
        let mut img = canvas(10, 10);
        let mut gc = Gc::new(24);
        gc.foreground = 1;
        gc.function = GX_XOR;
        // Con XOR cada píxel del contorno debe pintarse una sola vez.
        Painter::new(&mut img, &gc).rect_outline(&gc, 1, 1, 5, 4);
        assert_eq!(count(&img, 1), 2 * 6 + 2 * 3);
        assert_eq!(img.get(1, 1), 1);
        assert_eq!(img.get(6, 5), 1);
        assert_eq!(img.get(2, 2), 0);
    }

    #[test]
    fn polygons() {
        // Triángulo rectángulo: mitad del cuadrado.
        let mut img = canvas(10, 10);
        let mut gc = Gc::new(24);
        gc.foreground = 1;
        Painter::new(&mut img, &gc).polygon(&[(0.0, 0.0), (8.0, 0.0), (0.0, 8.0)], false);
        let n = count(&img, 1);
        assert!((28..=40).contains(&n), "{n}");
        assert_eq!(img.get(0, 0), 1);
        assert_eq!(img.get(7, 7), 0);
        // Rectángulo: borde derecho e inferior excluidos.
        let mut img = canvas(10, 10);
        Painter::new(&mut img, &gc)
            .polygon(&[(2.0, 2.0), (6.0, 2.0), (6.0, 5.0), (2.0, 5.0)], false);
        assert_eq!(count(&img, 1), 4 * 3);
        // Estrella que se cruza: par-impar deja hueco, «winding» no.
        let star = [(5.0, 0.0), (8.0, 9.0), (0.0, 3.0), (10.0, 3.0), (2.0, 9.0)];
        let mut even = canvas(11, 10);
        Painter::new(&mut even, &gc).polygon(&star, false);
        let mut wind = canvas(11, 10);
        Painter::new(&mut wind, &gc).polygon(&star, true);
        assert!(count(&wind, 1) > count(&even, 1));
        assert_eq!((even.get(5, 4), wind.get(5, 4)), (0, 1));
    }

    #[test]
    fn arcs() {
        // Círculo relleno de 20 de diámetro: área ≈ π·10².
        let mut img = canvas(30, 30);
        let mut gc = Gc::new(24);
        gc.foreground = 1;
        Painter::new(&mut img, &gc).arc(&gc, 5, 5, 20, 20, 0, 360 * 64, true);
        let n = count(&img, 1) as f64;
        assert!((n - 314.0).abs() < 16.0, "{n}");
        assert_eq!(img.get(15, 15), 1);
        assert_eq!(img.get(5, 5), 0);
        // No se sale de su caja de 20×20.
        assert!(img.data.iter().enumerate().all(|(i, p)| {
            let (x, y) = (i as i32 % 30, i as i32 / 30);
            *p == 0 || (5..25).contains(&x) && (5..25).contains(&y)
        }));
        // Cuarto de tarta (de 0° a 90°): arriba a la derecha.
        let mut img = canvas(30, 30);
        Painter::new(&mut img, &gc).arc(&gc, 5, 5, 20, 20, 0, 90 * 64, true);
        let n = count(&img, 1) as f64;
        assert!((n - 78.5).abs() < 14.0, "{n}");
        assert_eq!(img.get(20, 10), 1);
        assert_eq!(img.get(10, 20), 0);
        assert_eq!(img.get(10, 10), 0);
        // Contorno fino: toca los cuatro extremos y deja el centro vacío.
        let mut img = canvas(30, 30);
        Painter::new(&mut img, &gc).arc(&gc, 5, 5, 20, 20, 0, 360 * 64, false);
        assert_eq!(
            (
                img.get(25, 15),
                img.get(5, 15),
                img.get(15, 5),
                img.get(15, 25)
            ),
            (1, 1, 1, 1)
        );
        assert_eq!(img.get(15, 15), 0);
        let n = count(&img, 1);
        assert!((50..=80).contains(&n), "{n}");
        // Contorno grueso: un anillo.
        let mut img = canvas(30, 30);
        gc.line_width = 4;
        Painter::new(&mut img, &gc).arc(&gc, 5, 5, 20, 20, 0, 360 * 64, false);
        assert_eq!(img.get(15, 15), 0);
        assert_eq!(
            (img.get(25, 15), img.get(24, 15), img.get(26, 15)),
            (1, 1, 1)
        );
        let n = count(&img, 1) as f64;
        assert!((n - 251.0).abs() < 40.0, "{n}");
        // Ángulo negativo: sentido horario desde las 3, es decir, abajo.
        let mut img = canvas(30, 30);
        gc.line_width = 0;
        Painter::new(&mut img, &gc).arc(&gc, 5, 5, 20, 20, 0, -90 * 64, true);
        assert_eq!(img.get(20, 20), 1);
        assert_eq!(img.get(20, 10), 0);
    }

    #[test]
    fn blits_copy_and_clip() {
        let mut img = canvas(4, 4);
        let gc = Gc::new(24);
        let block: Vec<u32> = (1..=9).collect();
        let mut p = Painter::new(&mut img, &gc);
        // Bloque de 3×3 en (2, 2): solo cabe la esquina de 2×2.
        p.blit(2, 2, 3, &block, Rect::new(0, 0, 3, 3));
        assert_eq!(p.damage, Rect::new(2, 2, 2, 2));
        assert_eq!(
            (img.get(2, 2), img.get(3, 2), img.get(2, 3), img.get(3, 3)),
            (1, 2, 4, 5)
        );
        // Solo una parte del bloque es válida.
        let mut img = canvas(4, 4);
        Painter::new(&mut img, &gc).blit(0, 0, 3, &block, Rect::new(1, 1, 2, 2));
        assert_eq!(count(&img, 0), 12);
        assert_eq!((img.get(1, 1), img.get(2, 2)), (5, 9));
    }

    #[test]
    fn image_helpers() {
        let mut img = canvas(4, 4);
        img.fill(Rect::new(1, 1, 2, 2), 0xffaabbcc);
        assert_eq!(img.get(1, 1), 0xaabbcc);
        let block = img.extract(Rect::new(-1, 0, 3, 2));
        assert_eq!(block, vec![0, 0, 0, 0, 0, 0xaabbcc]);
        let bigger = img.resized(6, 6, 1, 1).unwrap();
        assert_eq!((bigger.w, bigger.h), (6, 6));
        assert_eq!(bigger.get(2, 2), 0xaabbcc);
        assert_eq!(bigger.get(1, 1), 0);
        let smaller = img.resized(2, 2, 0, 0).unwrap();
        assert_eq!(smaller.data, vec![0, 0, 0, 0xaabbcc]);
    }

    #[test]
    fn budget_counts_every_buffer() {
        let budget = Budget::new(100);
        let a = Image::budgeted(&budget, 5, 10, 24).unwrap();
        assert_eq!(budget.used(), 50);
        // La copia también cuenta (aunque se pase del tope: ya está hecha).
        let b = a.clone();
        assert_eq!(budget.used(), 100);
        assert!(Image::budgeted(&budget, 1, 1, 24).is_none());
        assert!(a.resized(6, 10, 0, 0).is_none(), "no cabe otra más");
        drop(b);
        assert_eq!(budget.used(), 50);
        assert!(budget.has_room(50) && !budget.has_room(51));
        let c = a.resized(5, 4, 0, 0).unwrap();
        assert_eq!(budget.used(), 70);
        drop((a, c));
        assert_eq!(budget.used(), 0);
        // Los búferes internos no cuentan, y los tamaños se recortan antes
        // de cargarlos.
        let _internal = Image::new(100, 100, 24);
        assert_eq!(budget.used(), 0);
        let big = Budget::new(u64::MAX);
        let huge = Image::budgeted(&big, 65_535, 65_535, 24).unwrap();
        assert_eq!(big.used(), huge.pixels());
        assert!(huge.pixels() <= MAX_PIXELS);
    }

    #[test]
    fn image_wire_formats() {
        assert_eq!(row_bytes(1, 1), 4);
        assert_eq!(row_bytes(33, 1), 8);
        assert_eq!(row_bytes(3, 32), 12);
        assert_eq!(
            decode_z32(&[1, 2, 3, 0, 4, 5, 6, 0], 2, 1),
            Some(vec![0x030201, 0x060504])
        );
        assert_eq!(decode_z32(&[1, 2, 3], 1, 1), None);
        // 10 píxeles de ancho: una palabra de 32 bits por fila.
        let bits = decode_bitmap(&[0b0000_0101, 0b0000_0010, 0, 0], 10, 1, 0).unwrap();
        assert_eq!(bits, vec![1, 0, 1, 0, 0, 0, 0, 0, 0, 1]);
        let padded = decode_bitmap(&[0b0000_0110, 0, 0, 0], 2, 1, 1).unwrap();
        assert_eq!(padded, vec![1, 1]);
        let mut out = vec![];
        encode_bitmap(&bits, 10, 1, 1, &mut out);
        assert_eq!(out, vec![0b0000_0101, 0b0000_0010, 0, 0]);
        assert_eq!(decode_bitmap(&[0; 3], 10, 1, 0), None);
    }

    #[test]
    fn text_is_blended_and_clipped() {
        let font = crate::xserver::font::open("fixed").unwrap().core;
        let mut img = canvas(40, 16);
        img.fill(img.bounds(), 0xffffff);
        let mut gc = Gc::new(24);
        gc.foreground = 0x000000;
        let codes: Vec<u32> = "Hi".bytes().map(|b| b as u32).collect();
        let mut p = Painter::new(&mut img, &gc);
        let advance = p.text(&font, 2, 12, &codes);
        assert_eq!(advance, 2 * font.cell.unwrap() as i32);
        let damage = p.damage;
        assert!(!damage.is_empty() && damage.x0 >= 2 && damage.y1 <= 13);
        let dark = img.data.iter().filter(|p| **p & 0xff < 0xa0).count();
        let grey = img
            .data
            .iter()
            .filter(|p| (0x10..0xf0).contains(&(**p & 0xff)))
            .count();
        assert!(dark > 8, "tinta: {dark}");
        assert!(grey > 0, "sin suavizado");
        // Con recorte no se pinta nada fuera.
        let mut img = canvas(40, 16);
        gc.clip = Clip::Rects(vec![Rect::new(0, 0, 5, 16)]);
        gc.foreground = 0xffffff;
        Painter::new(&mut img, &gc).text(&font, 2, 12, &codes);
        assert!(
            img.data
                .iter()
                .enumerate()
                .all(|(i, p)| *p == 0 || i % 40 < 5)
        );
        assert!(img.data.iter().any(|p| *p != 0));
        // En un mapa de bits el texto va sin suavizar: solo 0 y 1.
        let mut bitmap = Image::new(40, 16, 1);
        let mut gc = Gc::new(1);
        gc.foreground = 1;
        Painter::new(&mut bitmap, &gc).text(&font, 2, 12, &codes);
        assert!(bitmap.data.iter().all(|p| *p <= 1));
        assert!(bitmap.data.iter().filter(|p| **p == 1).count() > 8);
    }

    #[test]
    fn blending() {
        assert_eq!(blend_pixel(0x000000, 0xffffff, 255), 0xffffff);
        assert_eq!(blend_pixel(0x123456, 0xffffff, 0), 0x123456);
        assert_eq!(blend_pixel(0x000000, 0xff8000, 128), 0x804000);
    }
}
