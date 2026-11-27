//! Extensión RENDER (versión 0.10): composición con alfa en el servidor.
//!
//! Es lo que usan cairo (GTK), Xft y Qt para pintar texto suavizado, formas
//! y degradados sin tener que traerse la imagen, componerla en el cliente y
//! devolverla. Aquí hay: formatos ARGB32, RGB24, A8 y A1; `Picture` sobre
//! ventanas y pixmaps, colores sólidos y degradados lineales, radiales y
//! cónicos; los operadores de Porter-Duff; máscaras (también con alfa por
//! componente); repetición, transformación afín y filtro bilineal;
//! trapecios y triángulos suavizados; y conjuntos de glifos.
//!
//! Los píxeles con alfa van premultiplicados, como manda el protocolo.

use std::collections::HashMap;

use super::draw::{Image, Rect};
use super::font::Raster;
use super::proto::*;
use super::server::{ClientId, ROOT_VISUAL, Server};

/// Código de operación de RENDER (el mismo que usa X.Org).
pub const RENDER_OPCODE: u8 = 139;
pub const RENDER_FIRST_ERROR: u8 = 142;
const BAD_PICT_FORMAT: u8 = RENDER_FIRST_ERROR;
const BAD_PICTURE: u8 = RENDER_FIRST_ERROR + 1;
const BAD_PICT_OP: u8 = RENDER_FIRST_ERROR + 2;
const BAD_GLYPH_SET: u8 = RENDER_FIRST_ERROR + 3;
const BAD_GLYPH: u8 = RENDER_FIRST_ERROR + 4;

/// Memoria que pueden ocupar, entre todos, los glifos subidos por los
/// clientes.
const MAX_GLYPH_BYTES: usize = 96 * 1024 * 1024;
/// Lado máximo de un glifo.
const MAX_GLYPH_SIDE: usize = 2048;
/// Paradas de color de un degradado.
const MAX_STOPS: usize = 4096;
/// Tope de trabajo (filas × figuras) al rasterizar trapecios y triángulos.
const MAX_SHAPE_WORK: u64 = 20_000_000;
/// Píxeles de máscara que puede llegar a rasterizar una sola petición de
/// trapecios o triángulos.
const MAX_SHAPE_PIXELS: u64 = 64 * 1024 * 1024;
/// Píxeles de una máscara temporal (las mayores se hacen por partes).
const MAX_MASK_PIXELS: i32 = 4 * 1024 * 1024;
/// Coordenada más lejana que se admite en un origen.
const FAR: i32 = 1 << 28;

#[cfg(test)]
const OP_CLEAR: u8 = 0;
const OP_SRC: u8 = 1;
const OP_DST: u8 = 2;
const OP_OVER: u8 = 3;
#[cfg(test)]
const OP_ADD: u8 = 12;
const OP_SATURATE: u8 = 13;

/// Formato de los píxeles de un `Picture`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Argb32,
    Rgb24,
    A8,
    A1,
}

const FORMATS: [Format; 4] = [Format::Argb32, Format::Rgb24, Format::A8, Format::A1];

impl Format {
    fn id(self) -> u32 {
        match self {
            Format::Argb32 => 0x20,
            Format::Rgb24 => 0x21,
            Format::A8 => 0x22,
            Format::A1 => 0x23,
        }
    }

    fn from_id(id: u32) -> Option<Format> {
        FORMATS.into_iter().find(|f| f.id() == id)
    }

    fn depth(self) -> u8 {
        match self {
            Format::Argb32 => 32,
            Format::Rgb24 => 24,
            Format::A8 => 8,
            Format::A1 => 1,
        }
    }

    /// Píxel del búfer → ARGB premultiplicado.
    #[inline]
    fn to_argb(self, px: u32) -> u32 {
        match self {
            Format::Argb32 => px,
            Format::Rgb24 => px | 0xff00_0000,
            Format::A8 => (px & 0xff) << 24,
            Format::A1 => {
                if px & 1 != 0 {
                    0xff00_0000
                } else {
                    0
                }
            }
        }
    }

    /// ARGB premultiplicado → píxel del búfer.
    #[inline]
    fn pack(self, argb: u32) -> u32 {
        match self {
            Format::Argb32 => argb,
            Format::Rgb24 => argb & 0x00ff_ffff,
            Format::A8 => argb >> 24,
            Format::A1 => argb >> 31,
        }
    }
}

/// Degradado: geometría y tabla de colores ya interpolada.
#[derive(Debug, Clone)]
pub struct Gradient {
    kind: GradientKind,
    /// 256 colores ARGB premultiplicados para `t` de 0 a 1.
    table: Vec<u32>,
}

#[derive(Debug, Clone, Copy)]
enum GradientKind {
    Linear {
        p1: (f64, f64),
        p2: (f64, f64),
    },
    Radial {
        c1: (f64, f64),
        r1: f64,
        c2: (f64, f64),
        r2: f64,
    },
    Conical {
        center: (f64, f64),
        angle: f64,
    },
}

impl Gradient {
    /// `stops`: posición (0–1) y color sin premultiplicar (r, g, b, a de 0 a 1).
    fn new(kind: GradientKind, stops: &[(f64, [f64; 4])]) -> Self {
        let mut table = Vec::with_capacity(256);
        for i in 0..256 {
            let t = i as f64 / 255.0;
            let color = match stops {
                [] => [0.0; 4],
                [only] => only.1,
                _ => {
                    let next = stops.iter().position(|s| s.0 >= t);
                    match next {
                        Some(0) => stops[0].1,
                        None => stops[stops.len() - 1].1,
                        Some(n) => {
                            let (a, b) = (&stops[n - 1], &stops[n]);
                            let span = b.0 - a.0;
                            let f = if span > 0.0 { (t - a.0) / span } else { 1.0 };
                            let mut c = [0.0; 4];
                            for (k, slot) in c.iter_mut().enumerate() {
                                *slot = a.1[k] + (b.1[k] - a.1[k]) * f;
                            }
                            c
                        }
                    }
                }
            };
            // Se interpola sin premultiplicar y se premultiplica al final.
            let alpha = color[3].clamp(0.0, 1.0);
            let ch = |v: f64| ((v.clamp(0.0, 1.0) * alpha * 255.0).round() as u32).min(255);
            let a = (alpha * 255.0).round() as u32;
            table.push(a << 24 | ch(color[0]) << 16 | ch(color[1]) << 8 | ch(color[2]));
        }
        Gradient { kind, table }
    }

    /// Color en un punto (ya transformado), según el modo de repetición.
    fn color(&self, x: f64, y: f64, repeat: u8) -> u32 {
        let t = match self.kind {
            GradientKind::Linear { p1, p2 } => {
                let (dx, dy) = (p2.0 - p1.0, p2.1 - p1.1);
                let len = dx * dx + dy * dy;
                if len == 0.0 {
                    return 0;
                }
                ((x - p1.0) * dx + (y - p1.1) * dy) / len
            }
            GradientKind::Radial { c1, r1, c2, r2 } => {
                // El punto está en el círculo de centro c1 + t·(c2 − c1) y
                // radio r1 + t·(r2 − r1): se despeja el mayor t válido.
                let (cdx, cdy, dr) = (c2.0 - c1.0, c2.1 - c1.1, r2 - r1);
                let (pdx, pdy) = (x - c1.0, y - c1.1);
                let a = cdx * cdx + cdy * cdy - dr * dr;
                let b = pdx * cdx + pdy * cdy + r1 * dr;
                let c = pdx * pdx + pdy * pdy - r1 * r1;
                let t = if a.abs() < 1e-9 {
                    if b.abs() < 1e-9 {
                        return 0;
                    }
                    c / (2.0 * b)
                } else {
                    let disc = b * b - a * c;
                    if disc < 0.0 {
                        return 0;
                    }
                    let root = disc.sqrt();
                    let (t1, t2) = ((b + root) / a, (b - root) / a);
                    let (hi, lo) = if t1 > t2 { (t1, t2) } else { (t2, t1) };
                    if r1 + hi * dr >= 0.0 {
                        hi
                    } else if r1 + lo * dr >= 0.0 {
                        lo
                    } else {
                        return 0;
                    }
                };
                if !t.is_finite() {
                    return 0;
                }
                t
            }
            GradientKind::Conical { center, angle } => {
                let a = (y - center.1).atan2(x - center.0) - angle;
                (a / std::f64::consts::TAU).rem_euclid(1.0)
            }
        };
        let t = match repeat {
            1 => t.rem_euclid(1.0),
            2 => t.clamp(0.0, 1.0),
            3 => {
                let m = t.rem_euclid(2.0);
                if m > 1.0 { 2.0 - m } else { m }
            }
            _ => {
                if !(0.0..=1.0).contains(&t) {
                    return 0;
                }
                t
            }
        };
        self.table[((t * 255.0).round() as usize).min(255)]
    }
}

/// De dónde salen los píxeles de un `Picture`.
#[derive(Debug, Clone)]
pub enum Source {
    /// Una ventana o un pixmap (también de destino).
    Drawable(Xid),
    Solid(u32),
    Gradient(Gradient),
}

#[derive(Debug, Clone)]
pub struct Picture {
    pub owner: ClientId,
    pub source: Source,
    pub format: Format,
    /// 0 ninguna, 1 normal, 2 «pad», 3 reflejada.
    repeat: u8,
    clip: Option<Vec<Rect>>,
    clip_origin: (i32, i32),
    component_alpha: bool,
    /// Matriz 3×3 (por filas) de destino a origen; `None` si es la identidad.
    transform: Option<[f64; 9]>,
    bilinear: bool,
    /// Bordes de trapecios y triángulos sin suavizar.
    sharp_edges: bool,
}

/// Glifo subido por un cliente: sus píxeles van como ARGB (los de solo
/// alfa, con el alfa en el byte alto).
pub struct Glyph {
    w: i32,
    h: i32,
    x: i32,
    y: i32,
    x_off: i32,
    y_off: i32,
    data: Vec<u32>,
}

pub struct GlyphSet {
    pub owner: ClientId,
    format: Format,
    glyphs: HashMap<u32, Glyph>,
    pub bytes: usize,
}

// --- Aritmética de píxeles (ARGB premultiplicado, 8 bits por canal) ---------

/// Multiplica los cuatro canales por `a / 255`.
#[inline]
fn scale(c: u32, a: u32) -> u32 {
    let rb = (c & 0x00ff_00ff) * a + 0x0080_0080;
    let rb = ((rb + ((rb >> 8) & 0x00ff_00ff)) >> 8) & 0x00ff_00ff;
    let ag = ((c >> 8) & 0x00ff_00ff) * a + 0x0080_0080;
    let ag = (ag + ((ag >> 8) & 0x00ff_00ff)) & 0xff00_ff00;
    rb | ag
}

/// Suma canal a canal sin pasarse de 255.
#[inline]
fn add_sat(a: u32, b: u32) -> u32 {
    let mut out = 0;
    for shift in [0, 8, 16, 24] {
        let v = ((a >> shift) & 0xff) + ((b >> shift) & 0xff);
        out |= v.min(255) << shift;
    }
    out
}

/// Multiplica canal a canal (`a_c · b_c / 255`).
#[inline]
fn mul_channels(a: u32, b: u32) -> u32 {
    let mut out = 0;
    for shift in [0, 8, 16, 24] {
        let t = ((a >> shift) & 0xff) * ((b >> shift) & 0xff) + 0x80;
        out |= ((t + (t >> 8)) >> 8) << shift;
    }
    out
}

#[inline]
fn splat(a: u32) -> u32 {
    a * 0x0101_0101
}

/// `origen OVER destino` con alfa escalar.
#[inline]
fn over(s: u32, d: u32) -> u32 {
    match s >> 24 {
        255 => s,
        sa => add_sat(s, scale(d, 255 - sa)),
    }
}

/// Factor de Porter-Duff aplicado canal a canal: `c · f / 255`, donde `f`
/// depende del alfa del otro operando.
#[derive(Clone, Copy, PartialEq)]
enum Factor {
    Zero,
    One,
    /// El alfa del otro.
    Alpha,
    /// Uno menos el alfa del otro.
    InvAlpha,
}

/// Factores `(del origen, del destino)` de cada operador.
fn factors(op: u8) -> Option<(Factor, Factor)> {
    use Factor::*;
    // Las variantes «disjoint» (0x10…) y «conjoint» (0x20…) se tratan como
    // el operador básico: solo difieren cuando las formas se solapan a medias.
    Some(match op & 0x0f {
        0 => (Zero, Zero),
        1 => (One, Zero),
        2 => (Zero, One),
        3 => (One, InvAlpha),
        4 => (InvAlpha, One),
        5 => (Alpha, Zero),
        6 => (Zero, Alpha),
        7 => (InvAlpha, Zero),
        8 => (Zero, InvAlpha),
        9 => (Alpha, InvAlpha),
        10 => (InvAlpha, Alpha),
        11 => (InvAlpha, InvAlpha),
        12 | 13 => (One, One),
        _ => return None,
    })
}

fn valid_op(op: u8) -> bool {
    matches!(op, 0..=13 | 0x10..=0x1b | 0x20..=0x2b)
}

#[inline]
fn apply_factor(c: u32, f: Factor, alpha: u32) -> u32 {
    match f {
        Factor::Zero => 0,
        Factor::One => c,
        Factor::Alpha => mul_channels(c, alpha),
        Factor::InvAlpha => mul_channels(c, !alpha),
    }
}

/// Combina un píxel de origen con uno de destino. `sa` es el alfa del
/// origen por canal (los cuatro bytes iguales salvo con alfa por componente).
#[inline]
fn blend(op: u8, s: u32, sa: u32, d: u32) -> u32 {
    if op == OP_OVER {
        return add_sat(s, mul_channels(d, !sa));
    }
    let Some((fs, fd)) = factors(op) else {
        return d;
    };
    let da = splat(d >> 24);
    if op == OP_SATURATE {
        // El origen se atenúa lo justo para no pasarse de alfa 1.
        let (a, room) = (s >> 24, 255 - (d >> 24));
        let s = if a > room && a > 0 {
            scale(s, room * 255 / a)
        } else {
            s
        };
        return add_sat(s, d);
    }
    add_sat(apply_factor(s, fs, da), apply_factor(d, fd, sa))
}

// --- Muestreo de orígenes y máscaras ----------------------------------------

#[derive(Clone, Copy)]
enum Kind<'a> {
    Solid(u32),
    Pixels {
        data: &'a [u32],
        w: i32,
        h: i32,
        format: Format,
    },
    Gradient(&'a Gradient),
}

/// Origen (o máscara) de una composición, listo para dar píxeles ARGB.
#[derive(Clone, Copy)]
struct Sampler<'a> {
    kind: Kind<'a>,
    repeat: u8,
    transform: Option<[f64; 9]>,
    bilinear: bool,
}

impl<'a> Sampler<'a> {
    fn solid(color: u32) -> Self {
        Sampler {
            kind: Kind::Solid(color),
            repeat: 0,
            transform: None,
            bilinear: false,
        }
    }

    fn pixels(data: &'a [u32], w: i32, h: i32, format: Format) -> Self {
        Sampler {
            kind: Kind::Pixels { data, w, h, format },
            repeat: 0,
            transform: None,
            bilinear: false,
        }
    }

    /// Píxel entero `(x, y)` del origen, con su modo de repetición.
    #[inline]
    fn pixel(&self, x: i32, y: i32) -> u32 {
        let Kind::Pixels { data, w, h, format } = self.kind else {
            return 0;
        };
        if w <= 0 || h <= 0 {
            return 0;
        }
        let wrap = |v: i32, n: i32| -> Option<i32> {
            if v >= 0 && v < n {
                return Some(v);
            }
            match self.repeat {
                1 => Some(v.rem_euclid(n)),
                2 => Some(v.clamp(0, n - 1)),
                3 => {
                    let m = v.rem_euclid(2 * n);
                    Some(if m >= n { 2 * n - 1 - m } else { m })
                }
                _ => None,
            }
        };
        match (wrap(x, w), wrap(y, h)) {
            (Some(x), Some(y)) => data
                .get(y as usize * w as usize + x as usize)
                .map_or(0, |p| format.to_argb(*p)),
            _ => 0,
        }
    }

    /// Color en el píxel de destino `(x, y)` (coordenadas del `Picture`).
    fn at(&self, x: i32, y: i32) -> u32 {
        if let Kind::Solid(c) = self.kind {
            return c;
        }
        // Se muestrea en el centro del píxel.
        let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
        let (fx, fy) = match &self.transform {
            None => (px, py),
            Some(m) => {
                let w = m[6] * px + m[7] * py + m[8];
                if w.abs() < 1e-12 {
                    return 0;
                }
                (
                    (m[0] * px + m[1] * py + m[2]) / w,
                    (m[3] * px + m[4] * py + m[5]) / w,
                )
            }
        };
        if !fx.is_finite() || !fy.is_finite() || fx.abs() > 1e9 || fy.abs() > 1e9 {
            return 0;
        }
        match self.kind {
            Kind::Solid(c) => c,
            Kind::Gradient(g) => g.color(fx, fy, self.repeat),
            Kind::Pixels { .. } if !self.bilinear => {
                self.pixel(fx.floor() as i32, fy.floor() as i32)
            }
            Kind::Pixels { .. } => {
                // Bilineal: los cuatro vecinos, ponderados.
                let (sx, sy) = (fx - 0.5, fy - 0.5);
                let (x0, y0) = (sx.floor(), sy.floor());
                let (ax, ay) = (((sx - x0) * 256.0) as u32, ((sy - y0) * 256.0) as u32);
                let (x0, y0) = (x0 as i32, y0 as i32);
                let corners = [
                    (self.pixel(x0, y0), (256 - ax) * (256 - ay)),
                    (self.pixel(x0 + 1, y0), ax * (256 - ay)),
                    (self.pixel(x0, y0 + 1), (256 - ax) * ay),
                    (self.pixel(x0 + 1, y0 + 1), ax * ay),
                ];
                let mut out = 0;
                for shift in [0, 8, 16, 24] {
                    let sum: u32 = corners
                        .iter()
                        .map(|(c, weight)| ((c >> shift) & 0xff) * weight)
                        .sum();
                    out |= ((sum + 0x8000) >> 16).min(255) << shift;
                }
                out
            }
        }
    }

    /// Rellena `out` con los píxeles de la fila `y` a partir de `x0`.
    fn row(&self, x0: i32, y: i32, out: &mut [u32]) {
        match (self.kind, &self.transform) {
            (Kind::Solid(c), _) => out.fill(c),
            (Kind::Pixels { data, w, h, format }, None) if y >= 0 && y < h && self.repeat == 0 => {
                // Lo habitual: un tramo de una fila, tal cual.
                let start = (-x0).clamp(0, out.len() as i32) as usize;
                let end = (w - x0).clamp(start as i32, out.len() as i32) as usize;
                out[..start].fill(0);
                out[end..].fill(0);
                if start < end {
                    let src = y as usize * w as usize + (x0 + start as i32) as usize;
                    match data.get(src..src + (end - start)) {
                        Some(row) => {
                            for (o, p) in out[start..end].iter_mut().zip(row) {
                                *o = format.to_argb(*p);
                            }
                        }
                        None => out[start..end].fill(0),
                    }
                }
            }
            (Kind::Pixels { .. }, None) => {
                for (i, o) in out.iter_mut().enumerate() {
                    *o = self.pixel(x0 + i as i32, y);
                }
            }
            _ => {
                for (i, o) in out.iter_mut().enumerate() {
                    *o = self.at(x0 + i as i32, y);
                }
            }
        }
    }
}

/// Compone una fila: `dst = (src IN mask) OP dst`.
fn blend_row(op: u8, dst: &mut [u32], format: Format, src: &[u32], mask: Option<(&[u32], bool)>) {
    match (op, mask) {
        (OP_SRC, None) => {
            for (d, s) in dst.iter_mut().zip(src) {
                *d = format.pack(*s);
            }
        }
        (OP_OVER, None) => {
            for (d, s) in dst.iter_mut().zip(src) {
                match *s >> 24 {
                    0 if *s == 0 => {}
                    255 => *d = format.pack(*s),
                    _ => *d = format.pack(over(*s, format.to_argb(*d))),
                }
            }
        }
        (OP_OVER, Some((mask, false))) => {
            // Texto y formas suavizadas: color (o imagen) a través de una
            // máscara de cobertura.
            for ((d, s), m) in dst.iter_mut().zip(src).zip(mask) {
                match *m >> 24 {
                    0 => {}
                    255 if *s >> 24 == 255 => *d = format.pack(*s),
                    a => *d = format.pack(over(scale(*s, a), format.to_argb(*d))),
                }
            }
        }
        (_, None) => {
            for (d, s) in dst.iter_mut().zip(src) {
                *d = format.pack(blend(op, *s, splat(*s >> 24), format.to_argb(*d)));
            }
        }
        (_, Some((mask, component))) => {
            for ((d, s), m) in dst.iter_mut().zip(src).zip(mask) {
                let (s, sa) = if component {
                    (mul_channels(*s, *m), mul_channels(splat(*s >> 24), *m))
                } else {
                    let s = scale(*s, *m >> 24);
                    (s, splat(s >> 24))
                };
                *d = format.pack(blend(op, s, sa, format.to_argb(*d)));
            }
        }
    }
}

/// Compone un rectángulo de `w`×`h` sobre `dst` en `(dx, dy)`, recortado a
/// `clips`. Devuelve la zona modificada.
#[allow(clippy::too_many_arguments)]
fn composite_rect(
    op: u8,
    src: &Sampler,
    mask: Option<(&Sampler, bool)>,
    dst: &mut Image,
    format: Format,
    clips: &[Rect],
    src_at: (i32, i32),
    mask_at: (i32, i32),
    dst_at: (i32, i32),
    size: (i32, i32),
) -> Rect {
    let area = Rect::new(dst_at.0, dst_at.1, size.0, size.1);
    // (Coordenadas de origen disparatadas: se acotan para poder sumarles
    // desplazamientos sin desbordar.)
    let far = |p: (i32, i32)| (p.0.clamp(-FAR, FAR), p.1.clamp(-FAR, FAR));
    let (src_at, mask_at) = (far(src_at), far(mask_at));
    let mut damage = Rect::EMPTY;
    let mut src_row: Vec<u32> = vec![];
    let mut mask_row: Vec<u32> = vec![];
    for clip in clips {
        let r = clip.intersect(&area).intersect(&dst.bounds());
        if r.is_empty() {
            continue;
        }
        let n = r.width() as usize;
        src_row.resize(n, 0);
        mask_row.resize(n, 0);
        let (off_x, off_y) = (r.x0 - dst_at.0, 0);
        for y in r.y0..r.y1 {
            let line = y - dst_at.1 + off_y;
            src.row(src_at.0 + off_x, src_at.1 + line, &mut src_row);
            let mask_part = mask.map(|(m, component)| {
                m.row(mask_at.0 + off_x, mask_at.1 + line, &mut mask_row);
                (&mask_row[..], component)
            });
            blend_row(op, dst.row_mut(y, r.x0, r.x1), format, &src_row, mask_part);
        }
        damage = damage.union(&r);
    }
    damage
}

fn fixed(v: i32) -> f64 {
    v as f64 / 65536.0
}

/// Color de 16 bits por canal (ya premultiplicado) → ARGB de 8 bits.
fn color_argb(r: u16, g: u16, b: u16, a: u16) -> u32 {
    (a as u32 >> 8) << 24 | (r as u32 >> 8) << 16 | (g as u32 >> 8) << 8 | (b as u32 >> 8)
}

/// Máscara de cobertura (A8) de un conjunto de polígonos convexos.
struct ShapeMask {
    bounds: Rect,
    data: Vec<u32>,
}

/// Rectángulo de píxeles que tocan unos polígonos, recortado a `limit`.
fn extent(polygons: &[Vec<(f64, f64)>], limit: Rect) -> Option<Rect> {
    let mut points = polygons
        .iter()
        .flatten()
        .filter(|p| p.0.is_finite() && p.1.is_finite());
    let first = points.next()?;
    let (mut x0, mut y0, mut x1, mut y1) = (first.0, first.1, first.0, first.1);
    for p in points {
        (x0, y0, x1, y1) = (x0.min(p.0), y0.min(p.1), x1.max(p.0), y1.max(p.1));
    }
    let clamp = |v: f64| v.clamp(-1e6, 1e6);
    let bounds = Rect {
        x0: clamp(x0).floor() as i32,
        y0: clamp(y0).floor() as i32,
        x1: clamp(x1).ceil() as i32,
        y1: clamp(y1).ceil() as i32,
    }
    .intersect(&limit);
    (!bounds.is_empty()).then_some(bounds)
}

/// Rasteriza polígonos (en coordenadas del destino) dentro de `limit`.
fn rasterize(polygons: &[Vec<(f64, f64)>], limit: Rect, sharp: bool) -> Option<ShapeMask> {
    let bounds = extent(polygons, limit)?;
    let clamp = |v: f64| v.clamp(-1e6, 1e6);
    let (w, h) = (bounds.width() as usize, bounds.height() as usize);
    let mut raster = Raster::new(w, h);
    for polygon in polygons {
        if polygon.len() < 3 || polygon.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
            continue;
        }
        // Todos en el mismo sentido de giro, para que sus áreas se sumen.
        let area: f64 = (0..polygon.len())
            .map(|i| {
                let (a, b) = (polygon[i], polygon[(i + 1) % polygon.len()]);
                a.0 * b.1 - b.0 * a.1
            })
            .sum();
        let local = |p: (f64, f64)| {
            (
                (clamp(p.0) - bounds.x0 as f64) as f32,
                (clamp(p.1) - bounds.y0 as f64) as f32,
            )
        };
        for i in 0..polygon.len() {
            let (a, b) = (polygon[i], polygon[(i + 1) % polygon.len()]);
            if area >= 0.0 {
                raster.line(local(a), local(b));
            } else {
                raster.line(local(b), local(a));
            }
        }
    }
    let data = raster
        .coverage()
        .into_iter()
        .map(|c| match (sharp, c) {
            (true, c) => u32::from(c >= 128) * 255,
            (false, c) => c as u32,
        })
        .collect();
    Some(ShapeMask { bounds, data })
}

/// Qué hace de origen en una composición.
#[derive(Clone, Copy)]
enum SrcRef {
    Picture(Xid),
    Solid(u32),
}

/// Qué hace de máscara en una composición.
enum MaskRef<'a> {
    None,
    Picture(Xid),
    /// Cobertura A8 ya calculada, colocada en `bounds` del destino.
    Shape(&'a ShapeMask),
    /// Píxeles ARGB de un glifo; `component`: con alfa por componente
    /// (glifos de color o de subpíxel).
    Pixels {
        data: &'a [u32],
        w: i32,
        h: i32,
        component: bool,
    },
}

impl Server {
    fn picture(&self, id: Xid) -> XResult<&Picture> {
        self.pictures.get(&id).ok_or(XError::new(BAD_PICTURE, id))
    }

    fn picture_mut(&mut self, id: Xid) -> XResult<&mut Picture> {
        self.pictures
            .get_mut(&id)
            .ok_or(XError::new(BAD_PICTURE, id))
    }

    /// Búfer del *drawable* de un `Picture` (vivo o ya liberado pero aún
    /// referenciado).
    fn picture_image(&self, drawable: Xid) -> Option<&Image> {
        if let Some(w) = self.windows.get(&drawable) {
            w.image.as_ref()
        } else {
            self.pixmaps
                .get(&drawable)
                .or_else(|| self.zombie_pixmaps.get(&drawable))
                .map(|p| &*p.image)
        }
    }

    /// Saca de su sitio el búfer de destino para poder pintar en él mientras
    /// se leen otros. Hay que devolverlo con [`Server::return_image`].
    fn borrow_image(&mut self, drawable: Xid) -> XResult<Image> {
        if let Some(w) = self.windows.get_mut(&drawable) {
            return w.image.take().ok_or(XError::new(BAD_MATCH, drawable));
        }
        let budget = self.budget.clone();
        let p = match self.pixmaps.get_mut(&drawable) {
            Some(p) => p,
            None => self
                .zombie_pixmaps
                .get_mut(&drawable)
                .ok_or(XError::new(BAD_DRAWABLE, drawable))?,
        };
        if std::sync::Arc::strong_count(&p.image) > 1 && !budget.has_room(p.image.pixels()) {
            return Err(XError::new(BAD_ALLOC, drawable));
        }
        let depth = p.image.depth;
        let image = std::sync::Arc::make_mut(&mut p.image);
        Ok(std::mem::replace(image, Image::new(0, 0, depth)))
    }

    fn return_image(&mut self, drawable: Xid, image: Image) {
        if let Some(w) = self.windows.get_mut(&drawable) {
            w.image = Some(image);
        } else if let Some(p) = self
            .pixmaps
            .get_mut(&drawable)
            .or_else(|| self.zombie_pixmaps.get_mut(&drawable))
        {
            *std::sync::Arc::make_mut(&mut p.image) = image;
        }
    }

    /// `FreePixmap`: si algún `Picture` sigue usando el pixmap, su contenido
    /// se conserva (fuera del alcance de las peticiones básicas) hasta que
    /// se libere el último.
    pub(super) fn free_pixmap(&mut self, id: Xid) -> XResult {
        let pixmap = self
            .pixmaps
            .remove(&id)
            .ok_or(XError::new(BAD_PIXMAP, id))?;
        let uses = |p: &Picture| matches!(p.source, Source::Drawable(d) if d == id);
        if self.pictures.values().any(uses) {
            // El identificador queda libre para el cliente: el contenido
            // pasa a uno interno, que ningún cliente puede nombrar.
            let internal = loop {
                self.next_internal = match self.next_internal {
                    0x1000..0x001f_ffff => self.next_internal + 1,
                    _ => 0x1000,
                };
                if !self.zombie_pixmaps.contains_key(&self.next_internal) {
                    break self.next_internal;
                }
            };
            for p in self.pictures.values_mut().filter(|p| uses(p)) {
                p.source = Source::Drawable(internal);
            }
            self.zombie_pixmaps.insert(internal, pixmap);
        }
        Ok(())
    }

    /// Al destruirse una ventana se van con ella sus `Picture`.
    pub(super) fn render_window_destroyed(&mut self, id: Xid) {
        if !self.pictures.is_empty() {
            self.pictures
                .retain(|_, p| !matches!(p.source, Source::Drawable(d) if d == id));
        }
    }

    fn free_picture(&mut self, id: Xid) -> XResult {
        let picture = self
            .pictures
            .remove(&id)
            .ok_or(XError::new(BAD_PICTURE, id))?;
        if let Source::Drawable(d) = picture.source
            && self.zombie_pixmaps.contains_key(&d)
            && !self
                .pictures
                .values()
                .any(|p| matches!(p.source, Source::Drawable(other) if other == d))
        {
            self.zombie_pixmaps.remove(&d);
        }
        Ok(())
    }

    /// Libera lo de RENDER de un cliente que se va.
    pub(super) fn render_remove_client(&mut self, client: ClientId) {
        self.pictures.retain(|_, p| p.owner != client);
        let mut freed = 0;
        self.glyphsets.retain(|_, g| {
            if g.owner == client {
                freed += g.bytes;
            }
            g.owner != client
        });
        self.glyph_bytes = self.glyph_bytes.saturating_sub(freed);
        let used: Vec<Xid> = self
            .pictures
            .values()
            .filter_map(|p| match p.source {
                Source::Drawable(d) => Some(d),
                _ => None,
            })
            .collect();
        if !self.zombie_pixmaps.is_empty() {
            let used: std::collections::HashSet<Xid> = used.into_iter().collect();
            self.zombie_pixmaps.retain(|id, _| used.contains(id));
        }
    }

    /// Composición general: `dst = (src IN mask) OP dst` en un rectángulo.
    #[allow(clippy::too_many_arguments)]
    fn composite(
        &mut self,
        op: u8,
        src: SrcRef,
        mask: MaskRef,
        dst: Xid,
        src_at: (i32, i32),
        mask_at: (i32, i32),
        dst_at: (i32, i32),
        size: (i32, i32),
    ) -> XResult {
        if !valid_op(op) {
            return Err(XError::new(BAD_PICT_OP, op as u32));
        }
        if let SrcRef::Picture(id) = src {
            self.picture(id)?;
        }
        if let MaskRef::Picture(m) = mask {
            self.picture(m)?;
        }
        let target = self.picture(dst)?;
        let Source::Drawable(drawable) = target.source else {
            return Err(XError::new(BAD_MATCH, dst));
        };
        let format = target.format;
        let (clip, origin) = (target.clip.clone(), target.clip_origin);
        if op == OP_DST || size.0 <= 0 || size.1 <= 0 {
            return Ok(());
        }
        // Un origen que es el propio destino se lee de una copia: mientras se
        // pinta, el búfer de destino no está en su sitio.
        let uses_target = |server: &Server, id: Xid| {
            server
                .pictures
                .get(&id)
                .is_some_and(|p| matches!(p.source, Source::Drawable(d) if d == drawable))
        };
        let mask_picture = match mask {
            MaskRef::Picture(m) => Some(m),
            _ => None,
        };
        let aliased = matches!(src, SrcRef::Picture(id) if uses_target(self, id))
            || mask_picture.is_some_and(|m| uses_target(self, m));
        let snapshot: Option<Image> = if aliased {
            let img = self
                .picture_image(drawable)
                .ok_or(XError::new(BAD_DRAWABLE, drawable))?;
            if !self.budget.has_room(img.pixels()) {
                return Err(XError::new(BAD_ALLOC, drawable));
            }
            Some(img.clone())
        } else {
            None
        };
        let mut image = self.borrow_image(drawable)?;
        let bounds = image.bounds();
        let clips: Vec<Rect> = match &clip {
            None => vec![bounds],
            Some(rects) => rects
                .iter()
                .map(|r| r.translate(origin.0, origin.1).intersect(&bounds))
                .filter(|r| !r.is_empty())
                .collect(),
        };
        let damage = {
            let sampler = |id: Xid| -> Option<(Sampler, bool)> {
                let p = self.pictures.get(&id)?;
                let kind = match &p.source {
                    Source::Solid(c) => Kind::Solid(*c),
                    Source::Gradient(g) => Kind::Gradient(g),
                    Source::Drawable(d) => {
                        let img = if *d == drawable {
                            snapshot.as_ref()?
                        } else {
                            self.picture_image(*d)?
                        };
                        Kind::Pixels {
                            data: &img.data,
                            w: img.w,
                            h: img.h,
                            format: p.format,
                        }
                    }
                };
                let sampler = Sampler {
                    kind,
                    repeat: p.repeat,
                    transform: p.transform,
                    bilinear: p.bilinear,
                };
                Some((sampler, p.component_alpha))
            };
            let source = match src {
                SrcRef::Picture(id) => sampler(id),
                SrcRef::Solid(color) => Some((Sampler::solid(color), false)),
            };
            let shape;
            let mask_sampler: Option<Option<(Sampler, bool)>> = match mask {
                MaskRef::None => Some(None),
                MaskRef::Picture(m) => sampler(m).map(|(s, component)| {
                    // El alfa por componente solo tiene sentido si la máscara
                    // tiene canales de color.
                    let has_color = self
                        .pictures
                        .get(&m)
                        .is_some_and(|p| p.format == Format::Argb32);
                    Some((s, component && has_color))
                }),
                MaskRef::Shape(s) => {
                    shape = s;
                    let (w, h) = (shape.bounds.width(), shape.bounds.height());
                    Some(Some((
                        Sampler::pixels(&shape.data, w, h, Format::A8),
                        false,
                    )))
                }
                MaskRef::Pixels {
                    data,
                    w,
                    h,
                    component,
                } => Some(Some((
                    Sampler::pixels(data, w, h, Format::Argb32),
                    component,
                ))),
            };
            match (source, mask_sampler) {
                // (Un origen o una máscara cuyo dibujable ya no existe no
                // pinta nada.)
                (Some((source, _)), Some(mask_sampler)) => composite_rect(
                    op,
                    &source,
                    mask_sampler.as_ref().map(|(s, c)| (s, *c)),
                    &mut image,
                    format,
                    &clips,
                    src_at,
                    mask_at,
                    dst_at,
                    size,
                ),
                _ => Rect::EMPTY,
            }
        };
        self.return_image(drawable, image);
        if !damage.is_empty() && self.windows.contains_key(&drawable) {
            self.damage_window(drawable, damage);
        }
        Ok(())
    }

    /// Zona del destino en la que tiene sentido calcular una máscara.
    fn shape_limit(&self, dst: Xid) -> XResult<Rect> {
        let target = self.picture(dst)?;
        let Source::Drawable(drawable) = target.source else {
            return Err(XError::new(BAD_MATCH, dst));
        };
        let bounds = self
            .picture_image(drawable)
            .map_or(Rect::EMPTY, |img| img.bounds());
        Ok(match &target.clip {
            None => bounds,
            Some(rects) => rects
                .iter()
                .map(|r| r.translate(target.clip_origin.0, target.clip_origin.1))
                .fold(Rect::EMPTY, |acc, r| acc.union(&r))
                .intersect(&bounds),
        })
    }

    /// Trapecios y triángulos: se rasterizan a una máscara de cobertura y se
    /// compone el origen a través de ella. `anchor` es el punto de la figura
    /// al que se refieren las coordenadas de origen.
    #[allow(clippy::too_many_arguments)]
    fn composite_shapes(
        &mut self,
        op: u8,
        src: Xid,
        dst: Xid,
        mask_format: u32,
        src_at: (i32, i32),
        anchor: (f64, f64),
        polygons: Vec<Vec<(f64, f64)>>,
    ) -> XResult {
        if mask_format != 0 && Format::from_id(mask_format).is_none() {
            return Err(XError::new(BAD_PICT_FORMAT, mask_format));
        }
        let limit = self.shape_limit(dst)?;
        let sharp = self.picture(dst)?.sharp_edges || mask_format == Format::A1.id();
        self.picture(src)?;
        // Sin formato de máscara cada figura se compone por separado; con
        // él, todas juntas (lo que importa cuando se solapan).
        let groups: Vec<Vec<Vec<(f64, f64)>>> = if mask_format == 0 {
            polygons.into_iter().map(|p| vec![p]).collect()
        } else {
            vec![polygons]
        };
        let clamp = |v: f64| v.clamp(-1e6, 1e6);
        let (ax, ay) = (
            clamp(anchor.0).floor() as i32,
            clamp(anchor.1).floor() as i32,
        );
        let (mut work, mut pixels) = (0u64, 0u64);
        for group in groups {
            // Las máscaras grandes se hacen por franjas, para no reservar de
            // golpe una del tamaño del destino.
            let Some(extent) = extent(&group, limit) else {
                continue;
            };
            let rows = (MAX_MASK_PIXELS / extent.width().max(1)).max(1);
            let mut y = extent.y0;
            while y < extent.y1 {
                let band = Rect {
                    y0: y,
                    y1: y.saturating_add(rows).min(extent.y1),
                    ..extent
                };
                y = band.y1;
                work += band.height() as u64 * group.len() as u64;
                pixels += band.height() as u64 * band.width() as u64;
                if work > MAX_SHAPE_WORK || pixels > MAX_SHAPE_PIXELS {
                    return Err(XError::new(BAD_ALLOC, dst));
                }
                let Some(shape) = rasterize(&group, band, sharp) else {
                    continue;
                };
                let b = shape.bounds;
                self.composite(
                    op,
                    SrcRef::Picture(src),
                    MaskRef::Shape(&shape),
                    dst,
                    (
                        src_at.0.saturating_add(b.x0.saturating_sub(ax)),
                        src_at.1.saturating_add(b.y0.saturating_sub(ay)),
                    ),
                    (0, 0),
                    (b.x0, b.y0),
                    (b.width(), b.height()),
                )?;
            }
        }
        Ok(())
    }

    // --- Peticiones -----------------------------------------------------------

    pub(super) fn dispatch_render(&mut self, c: ClientId, minor: u8, rd: &mut Reader) -> XResult {
        match minor {
            // QueryVersion
            0 => {
                let mut w = self.reply(c, 0);
                w.u32(0).u32(10);
                self.send_reply(c, w);
                Ok(())
            }
            1 => self.render_query_formats(c),
            // QueryPictIndexValues: no hay formatos indexados.
            2 => Err(XError::new(BAD_PICT_FORMAT, rd.u32()?)),
            4 => {
                let (id, drawable, format, mask) = (rd.u32()?, rd.u32()?, rd.u32()?, rd.u32()?);
                self.check_new_id(c, id)?;
                let format = Format::from_id(format).ok_or(XError::new(BAD_PICT_FORMAT, format))?;
                let depth = if let Some(w) = self.windows.get(&drawable) {
                    if w.input_only {
                        return Err(XError::new(BAD_MATCH, drawable));
                    }
                    w.depth
                } else if let Some(p) = self.pixmaps.get(&drawable) {
                    p.image.depth
                } else {
                    return Err(XError::new(BAD_DRAWABLE, drawable));
                };
                if depth != format.depth() {
                    return Err(XError::new(BAD_MATCH, drawable));
                }
                let mut picture = Picture {
                    owner: c,
                    source: Source::Drawable(drawable),
                    format,
                    repeat: 0,
                    clip: None,
                    clip_origin: (0, 0),
                    component_alpha: false,
                    transform: None,
                    bilinear: false,
                    sharp_edges: false,
                };
                self.change_picture(&mut picture, mask, rd)?;
                self.pictures.insert(id, picture);
                Ok(())
            }
            5 => {
                let (id, mask) = (rd.u32()?, rd.u32()?);
                let mut picture = self.picture(id)?.clone();
                self.change_picture(&mut picture, mask, rd)?;
                *self.picture_mut(id)? = picture;
                Ok(())
            }
            // SetPictureClipRectangles
            6 => {
                let id = rd.u32()?;
                let origin = (rd.i16()? as i32, rd.i16()? as i32);
                if rd.remaining() / 8 > MAX_CLIP_RECTS {
                    return Err(XError::new(BAD_ALLOC, id));
                }
                let mut rects = vec![];
                while rd.remaining() >= 8 {
                    let (x, y) = (rd.i16()? as i32, rd.i16()? as i32);
                    rects.push(Rect::new(x, y, rd.u16()? as i32, rd.u16()? as i32));
                }
                let p = self.picture_mut(id)?;
                (p.clip, p.clip_origin) = (Some(rects), origin);
                Ok(())
            }
            7 => {
                let id = rd.u32()?;
                self.free_picture(id)
            }
            // Composite
            8 => {
                let op = rd.u8()?;
                rd.skip(3)?;
                let (src, mask, dst) = (rd.u32()?, rd.u32()?, rd.u32()?);
                let src_at = (rd.i16()? as i32, rd.i16()? as i32);
                let mask_at = (rd.i16()? as i32, rd.i16()? as i32);
                let dst_at = (rd.i16()? as i32, rd.i16()? as i32);
                let size = (rd.u16()? as i32, rd.u16()? as i32);
                let mask = if mask == 0 {
                    MaskRef::None
                } else {
                    MaskRef::Picture(mask)
                };
                let src = SrcRef::Picture(src);
                self.composite(op, src, mask, dst, src_at, mask_at, dst_at, size)
            }
            // Trapezoids, Triangles, TriStrip, TriFan
            10..=13 => {
                let op = rd.u8()?;
                rd.skip(3)?;
                let (src, dst, mask_format) = (rd.u32()?, rd.u32()?, rd.u32()?);
                let src_at = (rd.i16()? as i32, rd.i16()? as i32);
                let mut values = Vec::with_capacity(rd.remaining() / 4);
                while rd.remaining() >= 4 {
                    values.push(fixed(rd.i32()?));
                }
                let mut polygons = vec![];
                let mut anchor = (0.0, 0.0);
                if minor == 10 {
                    // top, bottom, y los dos lados como rectas por dos puntos.
                    for (i, t) in values.chunks_exact(10).enumerate() {
                        let (top, bottom) = (t[0], t[1]);
                        if i == 0 {
                            anchor = (t[2], t[3]);
                        }
                        // (Tampoco pasan los que traen coordenadas imposibles.)
                        if bottom.partial_cmp(&top) != Some(std::cmp::Ordering::Greater) {
                            continue;
                        }
                        let at = |l: &[f64], y: f64| {
                            let dy = l[3] - l[1];
                            if dy == 0.0 {
                                l[0]
                            } else {
                                l[0] + (y - l[1]) * (l[2] - l[0]) / dy
                            }
                        };
                        let (left, right) = (&t[2..6], &t[6..10]);
                        polygons.push(vec![
                            (at(left, top), top),
                            (at(right, top), top),
                            (at(right, bottom), bottom),
                            (at(left, bottom), bottom),
                        ]);
                    }
                } else {
                    let points: Vec<(f64, f64)> =
                        values.chunks_exact(2).map(|p| (p[0], p[1])).collect();
                    anchor = points.first().copied().unwrap_or_default();
                    match minor {
                        11 => {
                            for t in points.chunks_exact(3) {
                                polygons.push(t.to_vec());
                            }
                        }
                        12 => {
                            for t in points.windows(3) {
                                polygons.push(t.to_vec());
                            }
                        }
                        _ => {
                            for pair in points.get(1..).unwrap_or_default().windows(2) {
                                polygons.push(vec![points[0], pair[0], pair[1]]);
                            }
                        }
                    }
                }
                self.composite_shapes(op, src, dst, mask_format, src_at, anchor, polygons)
            }
            // CreateGlyphSet
            17 => {
                let (id, format) = (rd.u32()?, rd.u32()?);
                self.check_new_id(c, id)?;
                let format = Format::from_id(format).ok_or(XError::new(BAD_PICT_FORMAT, format))?;
                self.glyphsets.insert(
                    id,
                    GlyphSet {
                        owner: c,
                        format,
                        glyphs: HashMap::new(),
                        bytes: 0,
                    },
                );
                Ok(())
            }
            // FreeGlyphSet
            19 => {
                let id = rd.u32()?;
                let set = self
                    .glyphsets
                    .remove(&id)
                    .ok_or(XError::new(BAD_GLYPH_SET, id))?;
                self.glyph_bytes = self.glyph_bytes.saturating_sub(set.bytes);
                Ok(())
            }
            20 => self.render_add_glyphs(rd),
            // FreeGlyphs
            22 => {
                let id = rd.u32()?;
                let set = self
                    .glyphsets
                    .get_mut(&id)
                    .ok_or(XError::new(BAD_GLYPH_SET, id))?;
                let mut freed = 0;
                while rd.remaining() >= 4 {
                    if let Some(g) = set.glyphs.remove(&rd.u32()?) {
                        freed += g.data.len() * 4 + 64;
                    }
                }
                set.bytes = set.bytes.saturating_sub(freed);
                self.glyph_bytes = self.glyph_bytes.saturating_sub(freed);
                Ok(())
            }
            23..=25 => self.render_composite_glyphs(minor, rd),
            // FillRectangles
            26 => {
                let op = rd.u8()?;
                rd.skip(3)?;
                let dst = rd.u32()?;
                let color = color_argb(rd.u16()?, rd.u16()?, rd.u16()?, rd.u16()?);
                let mut rects = vec![];
                while rd.remaining() >= 8 {
                    let (x, y) = (rd.i16()? as i32, rd.i16()? as i32);
                    rects.push((x, y, rd.u16()? as i32, rd.u16()? as i32));
                }
                self.fill_rectangles(op, dst, color, &rects)
            }
            // CreateCursor (a partir de un Picture) y CreateAnimCursor: los
            // cursores no se dibujan, solo existen.
            27 | 31 => {
                let id = rd.u32()?;
                self.check_new_id(c, id)?;
                if minor == 27 {
                    self.picture(rd.u32()?)?;
                }
                self.cursors.insert(id, c);
                Ok(())
            }
            // SetPictureTransform
            28 => {
                let id = rd.u32()?;
                let mut m = [0.0; 9];
                for v in &mut m {
                    *v = fixed(rd.i32()?);
                }
                let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
                self.picture_mut(id)?.transform = (m != identity).then_some(m);
                Ok(())
            }
            // QueryFilters
            29 => {
                self.drawable_bounds(rd.u32()?)?;
                let names = ["nearest", "bilinear", "fast", "good", "best"];
                // «fast» es «nearest»; «good» y «best», «bilinear».
                let aliases = [0xffffu16, 0xffff, 0, 1, 1];
                let mut w = self.reply(c, 0);
                w.u32(aliases.len() as u32)
                    .u32(names.len() as u32)
                    .zeros(16);
                for alias in aliases {
                    w.u16(alias);
                }
                w.pad();
                for name in names {
                    w.u8(name.len() as u8).bytes(name.as_bytes());
                }
                self.send_reply(c, w);
                Ok(())
            }
            // SetPictureFilter
            30 => {
                let id = rd.u32()?;
                let len = rd.u16()? as usize;
                rd.skip(2)?;
                let name = rd.bytes(len)?;
                let bilinear = match name {
                    b"nearest" | b"fast" => false,
                    b"bilinear" | b"good" | b"best" => true,
                    // Convoluciones y demás: se aceptan como «nearest».
                    _ => false,
                };
                self.picture_mut(id)?.bilinear = bilinear;
                Ok(())
            }
            // CreateSolidFill
            33 => {
                let id = rd.u32()?;
                self.check_new_id(c, id)?;
                let color = color_argb(rd.u16()?, rd.u16()?, rd.u16()?, rd.u16()?);
                self.insert_source(c, id, Source::Solid(color));
                Ok(())
            }
            // CreateLinearGradient, CreateRadialGradient, CreateConicalGradient
            34..=36 => {
                let id = rd.u32()?;
                self.check_new_id(c, id)?;
                let point = |rd: &mut Reader| -> XResult<(f64, f64)> {
                    Ok((fixed(rd.i32()?), fixed(rd.i32()?)))
                };
                let kind = match minor {
                    34 => GradientKind::Linear {
                        p1: point(rd)?,
                        p2: point(rd)?,
                    },
                    35 => {
                        let (c1, c2) = (point(rd)?, point(rd)?);
                        GradientKind::Radial {
                            c1,
                            c2,
                            r1: fixed(rd.i32()?),
                            r2: fixed(rd.i32()?),
                        }
                    }
                    _ => GradientKind::Conical {
                        center: point(rd)?,
                        angle: fixed(rd.i32()?).to_radians(),
                    },
                };
                let n = rd.u32()? as usize;
                // Cada parada ocupa 4 bytes de posición y 8 de color.
                if n > MAX_STOPS || n.saturating_mul(12) > rd.remaining() {
                    return Err(XError::new(BAD_LENGTH, 0));
                }
                let mut positions = Vec::with_capacity(n);
                for _ in 0..n {
                    positions.push(fixed(rd.i32()?));
                }
                let mut stops = Vec::with_capacity(n);
                for position in positions {
                    let mut color = [0.0; 4];
                    for slot in &mut color {
                        *slot = rd.u16()? as f64 / 65535.0;
                    }
                    stops.push((position, color));
                }
                self.insert_source(c, id, Source::Gradient(Gradient::new(kind, &stops)));
                Ok(())
            }
            // Lo demás (glifos a partir de imágenes, AddTraps, referencias a
            // conjuntos de glifos…) no lo usa ninguna librería actual.
            _ => Err(XError::new(BAD_IMPLEMENTATION, 0)),
        }
    }

    fn insert_source(&mut self, c: ClientId, id: Xid, source: Source) {
        self.pictures.insert(
            id,
            Picture {
                owner: c,
                source,
                format: Format::Argb32,
                repeat: 0,
                clip: None,
                clip_origin: (0, 0),
                component_alpha: false,
                transform: None,
                bilinear: false,
                sharp_edges: false,
            },
        );
    }

    fn render_query_formats(&mut self, c: ClientId) -> XResult {
        let mut w = self.reply(c, 0);
        // Formatos, pantallas, profundidades, visuales, subpíxeles.
        w.u32(FORMATS.len() as u32)
            .u32(1)
            .u32(1)
            .u32(1)
            .u32(1)
            .zeros(4);
        for f in FORMATS {
            // (desplazamiento, máscara) de rojo, verde, azul y alfa.
            let direct: [(u16, u16); 4] = match f {
                Format::Argb32 => [(16, 0xff), (8, 0xff), (0, 0xff), (24, 0xff)],
                Format::Rgb24 => [(16, 0xff), (8, 0xff), (0, 0xff), (0, 0)],
                Format::A8 => [(0, 0), (0, 0), (0, 0), (0, 0xff)],
                Format::A1 => [(0, 0), (0, 0), (0, 0), (0, 1)],
            };
            w.u32(f.id()).u8(1).u8(f.depth()).u16(0);
            for (shift, mask) in direct {
                w.u16(shift).u16(mask);
            }
            w.u32(0);
        }
        // La única pantalla: una profundidad (24) con el visual de la raíz.
        w.u32(1).u32(Format::Rgb24.id());
        w.u8(24).u8(0).u16(1).u32(0);
        w.u32(ROOT_VISUAL).u32(Format::Rgb24.id());
        w.u32(0); // orden de subpíxeles desconocido
        self.send_reply(c, w);
        Ok(())
    }

    /// Atributos de `CreatePicture` y `ChangePicture`.
    fn change_picture(&self, p: &mut Picture, mask: u32, rd: &mut Reader) -> XResult {
        if mask >> 13 != 0 {
            return Err(XError::new(BAD_VALUE, mask));
        }
        for bit in 0..13 {
            if mask & (1 << bit) == 0 {
                continue;
            }
            let v = rd.u32()?;
            match bit {
                0 => {
                    if v > 3 {
                        return Err(XError::new(BAD_VALUE, v));
                    }
                    p.repeat = v as u8;
                }
                // Mapa de alfa aparte: no se usa.
                1 => {
                    if v != 0 {
                        self.picture(v)?;
                    }
                }
                4 => p.clip_origin.0 = v as i16 as i32,
                5 => p.clip_origin.1 = v as i16 as i32,
                // Máscara de recorte: solo «ninguna» (o un pixmap, que se
                // toma por su rectángulo).
                6 => {
                    p.clip = if v == 0 {
                        None
                    } else {
                        let mask = self.pixmaps.get(&v).ok_or(XError::new(BAD_PIXMAP, v))?;
                        Some(vec![mask.image.bounds()])
                    };
                }
                9 => p.sharp_edges = v == 0,
                12 => p.component_alpha = v != 0,
                _ => {}
            }
        }
        Ok(())
    }

    fn fill_rectangles(
        &mut self,
        op: u8,
        dst: Xid,
        color: u32,
        rects: &[(i32, i32, i32, i32)],
    ) -> XResult {
        if !valid_op(op) {
            return Err(XError::new(BAD_PICT_OP, op as u32));
        }
        self.picture(dst)?;
        let solid = SrcRef::Solid(color);
        for (x, y, w, h) in rects {
            self.composite(
                op,
                solid,
                MaskRef::None,
                dst,
                (0, 0),
                (0, 0),
                (*x, *y),
                (*w, *h),
            )?;
        }
        Ok(())
    }

    fn render_add_glyphs(&mut self, rd: &mut Reader) -> XResult {
        let (id, n) = (rd.u32()?, rd.u32()? as usize);
        let format = self
            .glyphsets
            .get(&id)
            .ok_or(XError::new(BAD_GLYPH_SET, id))?
            .format;
        // Identificadores (4 bytes) e información (12 bytes) de cada glifo.
        if n.saturating_mul(16) > rd.remaining() {
            return Err(XError::new(BAD_LENGTH, 0));
        }
        let mut ids = Vec::with_capacity(n);
        for _ in 0..n {
            ids.push(rd.u32()?);
        }
        let mut infos = Vec::with_capacity(n);
        for _ in 0..n {
            let (w, h) = (rd.u16()? as usize, rd.u16()? as usize);
            let (x, y) = (rd.i16()? as i32, rd.i16()? as i32);
            infos.push((w, h, x, y, rd.i16()? as i32, rd.i16()? as i32));
        }
        let mut added = vec![];
        let mut bytes = 0usize;
        for (gid, (w, h, x, y, x_off, y_off)) in ids.into_iter().zip(infos) {
            if w > MAX_GLYPH_SIDE || h > MAX_GLYPH_SIDE {
                return Err(XError::new(BAD_ALLOC, id));
            }
            // Filas rellenas a 4 bytes; con A1, bits de menos a más peso.
            let stride = match format {
                Format::Argb32 | Format::Rgb24 => w * 4,
                Format::A8 => w.div_ceil(4) * 4,
                Format::A1 => w.div_ceil(32) * 4,
            };
            let raw = rd.bytes(stride * h)?;
            let mut data = Vec::with_capacity(w * h);
            for row in raw.chunks_exact(stride.max(1)).take(h) {
                for col in 0..w {
                    data.push(match format {
                        Format::Argb32 | Format::Rgb24 => {
                            let p = &row[col * 4..col * 4 + 4];
                            format.to_argb(u32::from_le_bytes([p[0], p[1], p[2], p[3]]))
                        }
                        Format::A8 => (row[col] as u32) << 24,
                        Format::A1 => u32::from(row[col / 8] >> (col % 8) & 1) * 0xff00_0000,
                    });
                }
            }
            data.resize(w * h, 0);
            bytes += data.len() * 4 + 64;
            added.push((
                gid,
                Glyph {
                    w: w as i32,
                    h: h as i32,
                    x,
                    y,
                    x_off,
                    y_off,
                    data,
                },
            ));
        }
        if self.glyph_bytes + bytes > MAX_GLYPH_BYTES {
            return Err(XError::new(BAD_ALLOC, id));
        }
        let set = self
            .glyphsets
            .get_mut(&id)
            .ok_or(XError::new(BAD_GLYPH_SET, id))?;
        for (gid, glyph) in added {
            if let Some(old) = set.glyphs.insert(gid, glyph) {
                let freed = old.data.len() * 4 + 64;
                set.bytes = set.bytes.saturating_sub(freed);
                self.glyph_bytes = self.glyph_bytes.saturating_sub(freed);
            }
        }
        set.bytes += bytes;
        self.glyph_bytes += bytes;
        Ok(())
    }

    /// `CompositeGlyphs8/16/32`: texto.
    fn render_composite_glyphs(&mut self, minor: u8, rd: &mut Reader) -> XResult {
        let op = rd.u8()?;
        rd.skip(3)?;
        let (src, dst, mask_format, mut set_id) = (rd.u32()?, rd.u32()?, rd.u32()?, rd.u32()?);
        let src_at = (rd.i16()? as i32, rd.i16()? as i32);
        if !valid_op(op) {
            return Err(XError::new(BAD_PICT_OP, op as u32));
        }
        if mask_format != 0 && Format::from_id(mask_format).is_none() {
            return Err(XError::new(BAD_PICT_FORMAT, mask_format));
        }
        self.picture(src)?;
        let limit = self.shape_limit(dst)?;
        if !self.glyphsets.contains_key(&set_id) {
            return Err(XError::new(BAD_GLYPH_SET, set_id));
        }
        let size = match minor {
            23 => 1,
            24 => 2,
            _ => 4,
        };
        // Glifos colocados: (conjunto, glifo, x, y) con la esquina de su imagen.
        let mut placed: Vec<(Xid, u32, i32, i32)> = vec![];
        let (mut pen_x, mut pen_y) = (0i32, 0i32);
        let mut origin = None;
        while rd.remaining() >= 8 {
            let len = rd.u8()? as usize;
            rd.skip(3)?;
            let (dx, dy) = (rd.i16()? as i32, rd.i16()? as i32);
            if len == 255 {
                // Cambio de conjunto de glifos.
                set_id = rd.u32()?;
                if !self.glyphsets.contains_key(&set_id) {
                    return Err(XError::new(BAD_GLYPH_SET, set_id));
                }
                continue;
            }
            pen_x = pen_x.saturating_add(dx);
            pen_y = pen_y.saturating_add(dy);
            origin.get_or_insert((pen_x, pen_y));
            let set = self
                .glyphsets
                .get(&set_id)
                .ok_or(XError::new(BAD_GLYPH_SET, set_id))?;
            let ids = rd.bytes(len * size)?;
            for raw in ids.chunks_exact(size) {
                let gid = match size {
                    1 => raw[0] as u32,
                    2 => u16_of(rd.be, raw) as u32,
                    _ => u32_of(rd.be, raw),
                };
                let glyph = set.glyphs.get(&gid).ok_or(XError::new(BAD_GLYPH, gid))?;
                // Los que caen fuera del destino no hace falta ni apuntarlos.
                let (x, y) = (pen_x.saturating_sub(glyph.x), pen_y.saturating_sub(glyph.y));
                if !Rect::new(x, y, glyph.w, glyph.h)
                    .intersect(&limit)
                    .is_empty()
                {
                    placed.push((set_id, gid, x, y));
                }
                pen_x = pen_x.saturating_add(glyph.x_off);
                pen_y = pen_y.saturating_add(glyph.y_off);
            }
            let pad = pad4(len * size).min(rd.remaining());
            rd.skip(pad)?;
        }
        let Some(origin) = origin else {
            return Ok(());
        };
        let mut bounds = Rect::EMPTY;
        for (set, gid, x, y) in &placed {
            if let Some((_, g)) = glyph(self, *set, *gid) {
                bounds = bounds.union(&Rect::new(*x, *y, g.w, g.h));
            }
        }
        let bounds = bounds.intersect(&limit);
        // Los glifos de cobertura se juntan en una sola máscara (salvo que
        // saliera desmesurada); los de color, o con alfa por componente, van
        // de uno en uno.
        let joint = bounds.width() as i64 * bounds.height() as i64 <= MAX_MASK_PIXELS as i64;
        let source_at = |x: i32, y: i32| {
            (
                src_at.0.saturating_add(x.saturating_sub(origin.0)),
                src_at.1.saturating_add(y.saturating_sub(origin.1)),
            )
        };
        let src = SrcRef::Picture(src);
        let mut coverage: Option<ShapeMask> = None;
        for (set, gid, x, y) in &placed {
            let Some((format, g)) = glyph(self, *set, *gid) else {
                continue;
            };
            if g.w <= 0 || g.h <= 0 {
                continue;
            }
            if format == Format::Argb32 || !joint {
                let data = g.data.clone();
                let (w, h) = (g.w, g.h);
                let mask = MaskRef::Pixels {
                    data: &data,
                    w,
                    h,
                    component: format == Format::Argb32,
                };
                self.composite(
                    op,
                    src,
                    mask,
                    dst,
                    source_at(*x, *y),
                    (0, 0),
                    (*x, *y),
                    (w, h),
                )?;
                continue;
            }
            let mask = coverage.get_or_insert_with(|| ShapeMask {
                bounds,
                data: vec![0; bounds.width() as usize * bounds.height() as usize],
            });
            let r = Rect::new(*x, *y, g.w, g.h).intersect(&bounds);
            let stride = bounds.width() as usize;
            for py in r.y0..r.y1 {
                let from = (py - y) as usize * g.w as usize + (r.x0 - x) as usize;
                let to = (py - bounds.y0) as usize * stride + (r.x0 - bounds.x0) as usize;
                let n = r.width() as usize;
                for (m, a) in mask.data[to..to + n]
                    .iter_mut()
                    .zip(&g.data[from..from + n])
                {
                    *m = (*m + (a >> 24)).min(255);
                }
            }
        }
        if let Some(mask) = coverage {
            let b = mask.bounds;
            self.composite(
                op,
                src,
                MaskRef::Shape(&mask),
                dst,
                source_at(b.x0, b.y0),
                (0, 0),
                (b.x0, b.y0),
                (b.width(), b.height()),
            )?;
        }
        Ok(())
    }
}

/// Un glifo de un conjunto, con el formato de este.
fn glyph(server: &Server, set: Xid, gid: u32) -> Option<(Format, &Glyph)> {
    let s = server.glyphsets.get(&set)?;
    Some((s.format, s.glyphs.get(&gid)?))
}

/// Rectángulos de recorte de un `Picture`.
const MAX_CLIP_RECTS: usize = 8192;

fn u16_of(be: bool, b: &[u8]) -> u16 {
    if be {
        u16::from_be_bytes([b[0], b[1]])
    } else {
        u16::from_le_bytes([b[0], b[1]])
    }
}

fn u32_of(be: bool, b: &[u8]) -> u32 {
    if be {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    } else {
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_arithmetic() {
        assert_eq!(scale(0xffff_ffff, 255), 0xffff_ffff);
        assert_eq!(scale(0xffff_ffff, 0), 0);
        assert_eq!(scale(0xff80_4020, 128), 0x8040_2010);
        assert_eq!(add_sat(0xf0f0_1010, 0x2020_2020), 0xffff_3030);
        assert_eq!(mul_channels(0xff80_ff00, 0x80ff_00ff), 0x8080_0000);
        // Opaco sobre lo que sea: el origen. Transparente: el destino.
        assert_eq!(over(0xff11_2233, 0xff99_9999), 0xff11_2233);
        assert_eq!(over(0, 0xff99_9999), 0xff99_9999);
        // Blanco al 50 % sobre negro opaco: gris medio.
        assert_eq!(over(0x8080_8080, 0xff00_0000), 0xff80_8080);
    }

    #[test]
    fn porter_duff_operators() {
        let (s, d) = (0x8000_0080u32, 0xffff_0000u32);
        let sa = splat(s >> 24);
        assert_eq!(blend(OP_CLEAR, s, sa, d), 0);
        assert_eq!(blend(OP_SRC, s, sa, d), s);
        assert_eq!(blend(OP_DST, s, sa, d), d);
        assert_eq!(blend(OP_OVER, s, sa, d), 0xff7f_0080);
        // In: el origen donde hay destino (aquí, opaco): el origen entero.
        assert_eq!(blend(5, s, sa, d), s);
        // Out: el origen donde no hay destino: nada.
        assert_eq!(blend(7, s, sa, d), 0);
        // OutReverse: el destino donde no hay origen.
        assert_eq!(blend(8, s, sa, d), 0x7f7f_0000);
        // Add satura.
        assert_eq!(
            blend(OP_ADD, 0xc0c0_c0c0, splat(0xc0), 0x8080_8080),
            0xffff_ffff
        );
        // Saturate no pasa de alfa 1.
        assert_eq!(
            blend(OP_SATURATE, 0xff00_00ff, splat(255), 0x8000_0000) >> 24,
            0xff
        );
        assert!(!valid_op(14) && valid_op(0x13) && !valid_op(0x30));
    }

    #[test]
    fn formats_convert_to_and_pack() {
        assert_eq!(Format::Rgb24.to_argb(0x0012_3456), 0xff12_3456);
        assert_eq!(Format::Rgb24.pack(0x8012_3456), 0x0012_3456);
        assert_eq!(Format::A8.to_argb(0x7f), 0x7f00_0000);
        assert_eq!(Format::A8.pack(0x7f12_3456), 0x7f);
        assert_eq!(Format::A1.to_argb(1), 0xff00_0000);
        assert_eq!(
            (Format::A1.pack(0x8000_0000), Format::A1.pack(0x7f00_0000)),
            (1, 0)
        );
        for f in FORMATS {
            assert_eq!(Format::from_id(f.id()), Some(f));
        }
        assert_eq!(Format::from_id(0x99), None);
    }

    #[test]
    fn samplers_repeat_and_transform() {
        let data = [0xff00_0001u32, 0xff00_0002, 0xff00_0003];
        let mut s = Sampler::pixels(&data, 3, 1, Format::Argb32);
        assert_eq!(
            (s.pixel(1, 0), s.pixel(3, 0), s.pixel(0, 1)),
            (0xff00_0002, 0, 0)
        );
        s.repeat = 1;
        assert_eq!((s.pixel(3, 0), s.pixel(-1, 5)), (0xff00_0001, 0xff00_0003));
        s.repeat = 2;
        assert_eq!((s.pixel(9, 0), s.pixel(-9, -3)), (0xff00_0003, 0xff00_0001));
        s.repeat = 3;
        assert_eq!((s.pixel(3, 0), s.pixel(5, 0)), (0xff00_0003, 0xff00_0001));
        // Fila sin repetición: transparente fuera.
        s.repeat = 0;
        let mut row = [9u32; 5];
        s.row(-1, 0, &mut row);
        assert_eq!(row, [0, 0xff00_0001, 0xff00_0002, 0xff00_0003, 0]);
        // Traslación de un píxel a la derecha en el origen.
        s.transform = Some([1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        assert_eq!(s.at(0, 0), 0xff00_0002);
        // Escala ×2 con filtro bilineal: mezcla de vecinos.
        let grey = [0xff00_0000u32, 0xffff_ffff];
        let mut s = Sampler::pixels(&grey, 2, 1, Format::Argb32);
        s.transform = Some([0.5, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        s.bilinear = true;
        s.repeat = 2;
        let mid = s.at(1, 0) & 0xff;
        assert!(mid > 0x20 && mid < 0xe0, "{mid:#x}");
    }

    #[test]
    fn gradients() {
        let stops = [(0.0, [1.0, 0.0, 0.0, 1.0]), (1.0, [0.0, 0.0, 1.0, 1.0])];
        let linear = Gradient::new(
            GradientKind::Linear {
                p1: (0.0, 0.0),
                p2: (10.0, 0.0),
            },
            &stops,
        );
        assert_eq!(linear.color(0.0, 3.0, 0), 0xffff_0000);
        assert_eq!(linear.color(10.0, 3.0, 0), 0xff00_00ff);
        let mid = linear.color(5.0, 0.0, 0);
        assert!((mid >> 16 & 0xff) > 0x70 && (mid & 0xff) > 0x70, "{mid:#x}");
        // Fuera: transparente sin repetición, el extremo con «pad».
        assert_eq!(linear.color(20.0, 0.0, 0), 0);
        assert_eq!(linear.color(20.0, 0.0, 2), 0xff00_00ff);
        assert_eq!(linear.color(15.0, 0.0, 3), linear.color(5.0, 0.0, 0));
        let radial = Gradient::new(
            GradientKind::Radial {
                c1: (0.0, 0.0),
                r1: 0.0,
                c2: (0.0, 0.0),
                r2: 10.0,
            },
            &stops,
        );
        assert_eq!(radial.color(0.0, 0.0, 0), 0xffff_0000);
        assert_eq!(radial.color(0.0, 10.0, 0), 0xff00_00ff);
        assert_eq!(radial.color(30.0, 0.0, 0), 0);
        // El alfa se premultiplica después de interpolar.
        let fading = Gradient::new(
            GradientKind::Linear {
                p1: (0.0, 0.0),
                p2: (1.0, 0.0),
            },
            &[(0.0, [1.0, 1.0, 1.0, 0.0]), (1.0, [1.0, 1.0, 1.0, 1.0])],
        );
        let half = fading.color(0.5, 0.0, 0);
        assert!((half >> 24) > 0x70 && (half >> 24) < 0x90 && half & 0xff == half >> 24);
    }

    #[test]
    fn rows_compose_with_masks() {
        // Negro opaco a través de una máscara al 0, 50 y 100 % sobre blanco.
        let mut dst = [0x00ff_ffffu32; 3];
        let src = [0xff00_0000u32; 3];
        let mask = [0u32, 0x8000_0000, 0xff00_0000];
        blend_row(OP_OVER, &mut dst, Format::Rgb24, &src, Some((&mask, false)));
        assert_eq!(dst, [0x00ff_ffff, 0x007f_7f7f, 0]);
        // Alfa por componente: solo deja pasar el canal rojo de la máscara.
        let mut dst = [0xff00_0000u32];
        blend_row(
            OP_OVER,
            &mut dst,
            Format::Argb32,
            &[0xffff_ffff],
            Some((&[0xffff_0000], true)),
        );
        assert_eq!(dst, [0xffff_0000]);
        // Sobre una máscara A8: Add acumula cobertura.
        let mut dst = [0x40u32];
        blend_row(OP_ADD, &mut dst, Format::A8, &[0x8000_0000], None);
        assert_eq!(dst, [0xc0]);
    }

    #[test]
    fn shapes_rasterize_with_coverage() {
        // Un rectángulo de (1.5, 1) a (4, 3): borde izquierdo al 50 %.
        let square = vec![vec![(1.5, 1.0), (4.0, 1.0), (4.0, 3.0), (1.5, 3.0)]];
        let limit = Rect::new(0, 0, 10, 10);
        let mask = rasterize(&square, limit, false).unwrap();
        assert_eq!(mask.bounds, Rect::new(1, 1, 3, 2));
        let row: Vec<u32> = mask.data[..3].to_vec();
        assert!(
            (120..=135).contains(&row[0]) && row[1] == 255 && row[2] == 255,
            "{row:?}"
        );
        // Sin suavizar: o todo o nada.
        let sharp = rasterize(&square, limit, true).unwrap();
        assert!(sharp.data.iter().all(|c| *c == 0 || *c == 255));
        // Dos triángulos en sentidos opuestos que forman un cuadrado no se
        // anulan entre sí.
        let two = vec![
            vec![(0.0, 0.0), (4.0, 0.0), (0.0, 4.0)],
            vec![(4.0, 0.0), (0.0, 4.0), (4.0, 4.0)],
        ];
        let mask = rasterize(&two, limit, false).unwrap();
        let covered: u32 = mask.data.iter().sum();
        assert!((covered as i32 - 16 * 255).abs() < 64, "{covered}");
        // Recortado al límite, y nada si queda fuera.
        let clipped = rasterize(&square, Rect::new(0, 0, 3, 2), false).unwrap();
        assert_eq!(clipped.bounds, Rect::new(1, 1, 2, 1));
        assert!(rasterize(&square, Rect::new(20, 20, 5, 5), false).is_none());
        // Un polígono con coordenadas imposibles no pinta nada.
        let broken = rasterize(
            &[vec![(f64::NAN, 0.0), (1.0, 1.0), (2.0, 0.0)]],
            limit,
            false,
        );
        assert!(broken.is_none_or(|m| m.data.iter().all(|c| *c == 0)));
    }
}
