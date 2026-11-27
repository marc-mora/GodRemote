//! Tipos de letra del núcleo de X, rasterizados aquí mismo.
//!
//! En Windows no hay fuentes de mapa de bits de X, así que los glifos se
//! generan a partir de las dos fuentes TTF que trae egui («Hack»,
//! monoespaciada, y «Ubuntu-Light», proporcional): `skrifa` da los contornos
//! y un rasterizador por acumulación de cobertura los convierte en mapas de
//! 8 bits. Los nombres siguen el convenio XLFD; del patrón pedido se extraen
//! el tamaño, si es monoespaciada, la negrita y la cursiva.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{
    DrawSettings, HintingInstance, HintingOptions, OutlineGlyphCollection, OutlinePen,
};
use skrifa::raw::FontRef;
use skrifa::{GlyphId, MetadataProvider as _};

use super::lock;

/// Memoria que pueden ocupar, entre todas las fuentes, los glifos ya
/// rasterizados. Pasado el tope se siguen dibujando, pero sin guardarlos.
const MAX_GLYPH_CACHE: usize = 48 * 1024 * 1024;
static GLYPH_CACHE_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Fuente TTF de la que salen los glifos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
    Mono,
    Sans,
    /// La fuente «cursor»: glifos vacíos.
    Cursor,
}

/// Juego de caracteres de la fuente X.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    Latin1,
    Latin9,
    /// ISO 10646 (Unicode), dos bytes por carácter.
    Unicode,
    /// La fuente «Symbol» de Adobe (letras griegas y signos matemáticos):
    /// xcalc rotula con ella sus teclas de raíz cuadrada y pi.
    Symbol,
}

/// Unicode de los caracteres de la fuente Symbol, de 0x20 a 0x7E.
const SYMBOL_LOW: &str = " !∀#∃%&∋()∗+,−./0123456789:;<=>?\
≅ΑΒΧΔΕΦΓΗΙϑΚΛΜΝΟΠΘΡΣΤΥςΩΞΨΖ[∴]⊥_\
‾αβχδεφγηιϕκλμνοπθρστυϖωξψζ{|}∼";
/// Y de 0xA0 a 0xF2 (los que no existen, con un espacio).
const SYMBOL_HIGH: &str = "€ϒ′≤⁄∞ƒ♣♦♥♠↔←↑→↓°±″≥×∝∂•÷≠≡≈…  ↵\
ℵℑℜ℘⊗⊕∅∩∪⊃⊇⊄⊂⊆∈∉∠∇®©™∏√⋅¬∧∨⇔⇐⇑⇒⇓\
◊〈®©™∑           〉∫";

/// Último carácter de las fuentes ISO 10646: basta para latín, griego,
/// cirílico, puntuación, flechas, cajas y símbolos sin que `QueryFont`
/// tenga que enviar las 65 536 entradas.
const UNICODE_LAST: u32 = 0x27ff;

impl Encoding {
    fn registry(self) -> &'static str {
        match self {
            Encoding::Latin1 => "iso8859-1",
            Encoding::Latin9 => "iso8859-15",
            Encoding::Unicode => "iso10646-1",
            Encoding::Symbol => "adobe-fontspecific",
        }
    }

    /// Carácter Unicode de un código de la fuente.
    fn unicode(self, code: u32) -> Option<char> {
        let cp = match self {
            Encoding::Latin9 => match code {
                0xA4 => 0x20AC,
                0xA6 => 0x160,
                0xA8 => 0x161,
                0xB4 => 0x17D,
                0xB8 => 0x17E,
                0xBC => 0x152,
                0xBD => 0x153,
                0xBE => 0x178,
                c => c,
            },
            Encoding::Symbol => {
                let (table, first) = if code < 0xA0 {
                    (SYMBOL_LOW, 0x20)
                } else {
                    (SYMBOL_HIGH, 0xA0)
                };
                let c = table.chars().nth(code.checked_sub(first)? as usize)?;
                return (c != ' ' || code == 0x20).then_some(c);
            }
            _ => code,
        };
        if cp < 0x20 || (0x7f..0xa0).contains(&cp) {
            return None;
        }
        char::from_u32(cp)
    }

    fn last(self) -> u32 {
        match self {
            Encoding::Unicode => UNICODE_LAST,
            _ => 0xff,
        }
    }
}

/// Métricas de un carácter, como las pide el protocolo (CHARINFO).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CharInfo {
    pub lbearing: i16,
    pub rbearing: i16,
    pub width: i16,
    pub ascent: i16,
    pub descent: i16,
}

impl CharInfo {
    /// Un carácter con todas las métricas a cero no existe en la fuente.
    pub fn exists(&self) -> bool {
        *self != CharInfo::default()
    }
}

/// Mapa de cobertura de un glifo.
#[derive(Debug, Default)]
pub struct Glyph {
    /// Desplazamiento del mapa respecto al origen del carácter.
    pub left: i32,
    /// Filas por encima de la línea base.
    pub top: i32,
    pub width: usize,
    pub height: usize,
    /// `width * height` valores de cobertura (0–255).
    pub coverage: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CoreKey {
    face: Face,
    pixel: u32,
    bold: bool,
    italic: bool,
    encoding: Encoding,
}

/// Fuente a un tamaño concreto: métricas y glifos (rasterizados al usarse).
pub struct FontCore {
    key: CoreKey,
    /// Tamaño del cuadratín en píxeles.
    em: f32,
    pub ascent: i16,
    pub descent: i16,
    /// Ancho de celda si es monoespaciada.
    pub cell: Option<i16>,
    /// Último código de carácter (el primero es 0).
    pub last: u32,
    pub infos: Vec<CharInfo>,
    pub min_bounds: CharInfo,
    pub max_bounds: CharInfo,
    /// Ajuste a la rejilla con las instrucciones de la propia fuente: deja
    /// los trazos horizontales en píxeles enteros y el texto pequeño se lee
    /// mucho mejor.
    hinter: Option<HintingInstance>,
    glyphs: Mutex<GlyphCache>,
}

/// Glifos ya rasterizados de una fuente y lo que ocupan.
#[derive(Default)]
struct GlyphCache {
    map: HashMap<u32, Option<Arc<Glyph>>>,
    bytes: usize,
}

impl Drop for GlyphCache {
    fn drop(&mut self) {
        GLYPH_CACHE_BYTES.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// Fuente abierta por un cliente: nombre XLFD más la fuente compartida.
#[derive(Clone)]
pub struct Font {
    pub name: String,
    pub core: Arc<FontCore>,
}

// --- Fuentes TTF ------------------------------------------------------------

fn face_data(face: Face) -> Option<&'static [u8]> {
    static DATA: OnceLock<[Option<Arc<egui::FontData>>; 2]> = OnceLock::new();
    let data = DATA.get_or_init(|| {
        let defs = egui::FontDefinitions::default();
        [
            defs.font_data.get("Hack").cloned(),
            defs.font_data.get("Ubuntu-Light").cloned(),
        ]
    });
    let index = match face {
        Face::Mono => 0,
        Face::Sans => 1,
        Face::Cursor => return None,
    };
    data[index].as_ref().map(|d| (*d.font).as_ref())
}

fn face_ref(face: Face) -> Option<FontRef<'static>> {
    FontRef::new(face_data(face)?).ok()
}

// --- Rasterizador -----------------------------------------------------------

/// Contornos de un glifo, ya aplanados a segmentos rectos.
#[derive(Default)]
struct Outline {
    contours: Vec<Vec<(f32, f32)>>,
    /// Inclinación de la cursiva sintética (desplazamiento en x por unidad de y).
    shear: f32,
}

impl Outline {
    fn point(&mut self, x: f32, y: f32) {
        let x = x + y * self.shear;
        if let Some(c) = self.contours.last_mut() {
            c.push((x, y));
        }
    }

    fn last(&self) -> (f32, f32) {
        self.contours
            .last()
            .and_then(|c| c.last())
            .copied()
            .unwrap_or((0.0, 0.0))
    }

    /// Límites `(x0, y0, x1, y1)` o `None` si no hay tinta.
    fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let mut points = self.contours.iter().flatten();
        let first = points.next()?;
        let mut b = (first.0, first.1, first.0, first.1);
        for (x, y) in points {
            b = (b.0.min(*x), b.1.min(*y), b.2.max(*x), b.3.max(*y));
        }
        Some(b)
    }
}

impl OutlinePen for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.contours.push(vec![]);
        self.point(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.point(x, y);
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        // Los puntos guardados ya llevan la inclinación: se deshace para
        // interpolar en el espacio original.
        let (px, py) = self.last();
        let px = px - py * self.shear;
        let dev = (px - 2.0 * cx + x).powi(2) + (py - 2.0 * cy + y).powi(2);
        let n = if dev < 0.333 {
            1
        } else {
            1 + (3.0 * dev).sqrt().sqrt().floor() as usize
        };
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            self.point(
                u * u * px + 2.0 * u * t * cx + t * t * x,
                u * u * py + 2.0 * u * t * cy + t * t * y,
            );
        }
    }

    fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        let (px, py) = self.last();
        let px = px - py * self.shear;
        const N: usize = 8;
        for i in 1..=N {
            let t = i as f32 / N as f32;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.point(
                a * px + b * c0x + c * c1x + d * x,
                a * py + b * c0y + c * c1y + d * y,
            );
        }
    }

    fn close(&mut self) {
        if let Some(c) = self.contours.last_mut()
            && let Some(first) = c.first().copied()
        {
            c.push(first);
        }
    }
}

/// Acumulador de cobertura con signo (la técnica de font-rs): cada segmento
/// suma el área que deja a su derecha y una suma acumulada por filas da la
/// cobertura de cada píxel.
struct Raster {
    w: usize,
    h: usize,
    acc: Vec<f32>,
}

impl Raster {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            acc: vec![0.0; w * h + 4],
        }
    }

    fn line(&mut self, p0: (f32, f32), p1: (f32, f32)) {
        if (p0.1 - p1.1).abs() < f32::EPSILON {
            return;
        }
        let (dir, p0, p1) = if p0.1 < p1.1 {
            (1.0, p0, p1)
        } else {
            (-1.0, p1, p0)
        };
        let dxdy = (p1.0 - p0.0) / (p1.1 - p0.1);
        let mut x = p0.0;
        if p0.1 < 0.0 {
            x -= p0.1 * dxdy;
        }
        let y0 = p0.1.max(0.0) as usize;
        let y1 = (p1.1.ceil().max(0.0) as usize).min(self.h);
        let max_x = self.w as f32;
        for y in y0..y1 {
            let row = y * self.w;
            let dy = ((y + 1) as f32).min(p1.1) - (y as f32).max(p0.1);
            let xnext = x + dxdy * dy;
            let d = dy * dir;
            let (x0, x1) = if x < xnext { (x, xnext) } else { (xnext, x) };
            let (x0, x1) = (x0.clamp(0.0, max_x), x1.clamp(0.0, max_x));
            let x0floor = x0.floor();
            let x0i = x0floor as usize;
            let x1ceil = x1.ceil();
            let x1i = x1ceil as usize;
            if x1i <= x0i + 1 {
                let xmf = 0.5 * (x0 + x1) - x0floor;
                self.acc[row + x0i] += d - d * xmf;
                self.acc[row + x0i + 1] += d * xmf;
            } else {
                let s = (x1 - x0).recip();
                let x0f = x0 - x0floor;
                let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
                let x1f = x1 - x1ceil + 1.0;
                let am = 0.5 * s * x1f * x1f;
                self.acc[row + x0i] += d * a0;
                if x1i == x0i + 2 {
                    self.acc[row + x0i + 1] += d * (1.0 - a0 - am);
                } else {
                    let a1 = s * (1.5 - x0f);
                    self.acc[row + x0i + 1] += d * (a1 - a0);
                    for xi in x0i + 2..x1i - 1 {
                        self.acc[row + xi] += d * s;
                    }
                    let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                    self.acc[row + x1i - 1] += d * (1.0 - a2 - am);
                }
                self.acc[row + x1i] += d * am;
            }
            x = xnext;
        }
    }

    fn coverage(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.w * self.h);
        let mut sum = 0.0f32;
        for v in &self.acc[..self.w * self.h] {
            sum += v;
            out.push((sum.abs().min(1.0) * 255.0 + 0.5) as u8);
        }
        out
    }
}

/// Rasteriza un contorno. Devuelve `None` si no tiene tinta.
fn rasterize(outline: &Outline, bold: bool) -> Option<Glyph> {
    let (x0, y0, x1, y1) = outline.bounds()?;
    let left = x0.floor() as i32;
    let top = y1.ceil() as i32;
    let width = (x1.ceil() as i32 - left).max(1) as usize + usize::from(bold);
    let height = (top - y0.floor() as i32).max(1) as usize;
    if width > 512 || height > 512 {
        return None;
    }
    let mut raster = Raster::new(width, height);
    for contour in &outline.contours {
        for pair in contour.windows(2) {
            // El eje y de la fuente apunta hacia arriba; el del mapa, abajo.
            let a = (pair[0].0 - left as f32, top as f32 - pair[0].1);
            let b = (pair[1].0 - left as f32, top as f32 - pair[1].1);
            raster.line(a, b);
        }
    }
    let mut coverage = raster.coverage();
    if bold {
        // Negrita sintética: la tinta se corre un píxel a la derecha.
        for row in coverage.chunks_exact_mut(width) {
            for x in (1..width).rev() {
                row[x] = row[x].max(row[x - 1]);
            }
        }
    }
    Some(Glyph {
        left,
        top,
        width,
        height,
        coverage,
    })
}

// --- Fuente a un tamaño -----------------------------------------------------

impl FontCore {
    fn new(key: CoreKey) -> Self {
        let last = if key.face == Face::Cursor {
            153
        } else {
            key.encoding.last()
        };
        let mut core = FontCore {
            key,
            em: key.pixel as f32,
            ascent: 0,
            descent: 0,
            cell: None,
            last,
            infos: vec![CharInfo::default(); last as usize + 1],
            min_bounds: CharInfo::default(),
            max_bounds: CharInfo::default(),
            hinter: None,
            glyphs: Mutex::new(GlyphCache::default()),
        };
        core.measure();
        core
    }

    fn measure(&mut self) {
        let pixel = self.key.pixel.max(1) as f32;
        let Some((font, em, ascent, descent)) = scale(self.key.face, self.key.pixel) else {
            // Fuente «cursor» (o sin fuentes TTF): cajas vacías.
            let side = if self.key.face == Face::Cursor {
                16
            } else {
                pixel as i16
            };
            let info = CharInfo {
                lbearing: 0,
                rbearing: side,
                width: side,
                ascent: side,
                descent: 0,
            };
            self.ascent = side;
            self.infos.fill(info);
            (self.min_bounds, self.max_bounds) = (info, info);
            return;
        };
        (self.em, self.ascent, self.descent) = (em, ascent, descent);
        let charmap = font.charmap();
        let metrics = font.glyph_metrics(Size::new(self.em), LocationRef::default());
        let advance = |gid: GlyphId| metrics.advance_width(gid).unwrap_or(0.0);
        if self.key.face == Face::Mono {
            let width = charmap.map('M').map(advance).unwrap_or(pixel * 0.6);
            self.cell = Some((width.round() as i16).max(1));
        }
        let outlines = font.outline_glyphs();
        self.hinter = HintingInstance::new(
            &outlines,
            Size::new(self.em),
            LocationRef::default(),
            HintingOptions::default(),
        )
        .ok();
        for code in 0..=self.last {
            let Some(gid) = self.key.encoding.unicode(code).and_then(|c| charmap.map(c)) else {
                continue;
            };
            let info = match self.cell {
                // Fuente de celdas: todos los caracteres miden lo mismo.
                Some(cell) => CharInfo {
                    lbearing: 0,
                    rbearing: cell,
                    width: cell,
                    ascent: self.ascent,
                    descent: self.descent,
                },
                None => {
                    let width = (advance(gid).round() as i16).max(1);
                    match self.outline(&outlines, gid).bounds() {
                        Some((x0, y0, x1, y1)) => CharInfo {
                            lbearing: x0.floor() as i16,
                            rbearing: x1.ceil() as i16 + i16::from(self.key.bold),
                            width,
                            ascent: y1.ceil() as i16,
                            descent: -(y0.floor() as i16),
                        },
                        None => CharInfo {
                            width,
                            ..Default::default()
                        },
                    }
                }
            };
            self.infos[code as usize] = info;
        }
        let mut existing = self.infos.iter().filter(|i| i.exists());
        if let Some(first) = existing.next() {
            let (mut lo, mut hi) = (*first, *first);
            for i in existing {
                lo = CharInfo {
                    lbearing: lo.lbearing.min(i.lbearing),
                    rbearing: lo.rbearing.min(i.rbearing),
                    width: lo.width.min(i.width),
                    ascent: lo.ascent.min(i.ascent),
                    descent: lo.descent.min(i.descent),
                };
                hi = CharInfo {
                    lbearing: hi.lbearing.max(i.lbearing),
                    rbearing: hi.rbearing.max(i.rbearing),
                    width: hi.width.max(i.width),
                    ascent: hi.ascent.max(i.ascent),
                    descent: hi.descent.max(i.descent),
                };
            }
            (self.min_bounds, self.max_bounds) = (lo, hi);
        }
    }

    fn pen(&self) -> Outline {
        Outline {
            contours: vec![],
            shear: if self.key.italic { 0.2 } else { 0.0 },
        }
    }

    pub fn two_byte(&self) -> bool {
        self.last > 0xff
    }

    /// Métricas de un carácter (todo ceros si no existe).
    pub fn info(&self, code: u32) -> CharInfo {
        self.infos.get(code as usize).copied().unwrap_or_default()
    }

    /// Mapa de cobertura de un carácter; `None` si no existe o no tiene tinta.
    pub fn glyph(&self, code: u32) -> Option<Arc<Glyph>> {
        if !self.info(code).exists() {
            return None;
        }
        let mut cache = lock(&self.glyphs);
        if let Some(cached) = cache.map.get(&code) {
            return cached.clone();
        }
        let glyph = self.render(code).map(Arc::new);
        let size = glyph.as_ref().map_or(0, |g| g.coverage.len()) + 64;
        if GLYPH_CACHE_BYTES.load(Ordering::Relaxed) + size <= MAX_GLYPH_CACHE {
            GLYPH_CACHE_BYTES.fetch_add(size, Ordering::Relaxed);
            cache.bytes += size;
            cache.map.insert(code, glyph.clone());
        }
        glyph
    }

    fn render(&self, code: u32) -> Option<Glyph> {
        let font = face_ref(self.key.face)?;
        let gid = font.charmap().map(self.key.encoding.unicode(code)?)?;
        rasterize(&self.outline(&font.outline_glyphs(), gid), self.key.bold)
    }

    /// Contorno de un glifo a este tamaño, ajustado a la rejilla si se puede.
    fn outline(&self, outlines: &OutlineGlyphCollection, gid: GlyphId) -> Outline {
        let mut pen = self.pen();
        let Some(glyph) = outlines.get(gid) else {
            return pen;
        };
        if let Some(hinter) = &self.hinter
            && glyph
                .draw(DrawSettings::hinted(hinter, false), &mut pen)
                .is_ok()
        {
            return pen;
        }
        // Sin instrucciones (o si fallan): el contorno tal cual.
        pen = self.pen();
        let settings = DrawSettings::unhinted(Size::new(self.em), LocationRef::default());
        let _ = glyph.draw(settings, &mut pen);
        pen
    }
}

/// Fuente compartida para una clave. Solo se guardan referencias débiles:
/// una fuente vive mientras alguien la tenga abierta, así que un cliente no
/// puede hacer crecer esta tabla pidiendo tamaños distintos sin parar.
fn core(key: CoreKey) -> Arc<FontCore> {
    static CACHE: OnceLock<Mutex<HashMap<CoreKey, Weak<FontCore>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(found) = lock(cache).get(&key).and_then(Weak::upgrade) {
        return found;
    }
    // Se mide fuera del candado: abrir una fuente Unicode lleva su tiempo.
    let made = Arc::new(FontCore::new(key));
    let mut cache = lock(cache);
    cache.retain(|_, weak| weak.strong_count() > 0);
    // Puede que otra tarea la haya creado entretanto.
    if let Some(found) = cache.get(&key).and_then(Weak::upgrade) {
        return found;
    }
    cache.insert(key, Arc::downgrade(&made));
    made
}

// --- Nombres XLFD -----------------------------------------------------------

/// Familia del catálogo que se anuncia en `ListFonts`.
struct Family {
    foundry: &'static str,
    family: &'static str,
    face: Face,
    setwidth: &'static str,
    /// Letra del campo de inclinación para la cursiva (`i` u `o`).
    italic: char,
    /// Campo de espaciado: `c` (celdas), `m` (monoespaciada) o `p`.
    spacing: char,
}

/// La primera es la que sale cuando el patrón no concreta la familia.
const FAMILIES: &[Family] = &[
    Family {
        foundry: "adobe",
        family: "helvetica",
        face: Face::Sans,
        setwidth: "normal",
        italic: 'o',
        spacing: 'p',
    },
    Family {
        foundry: "misc",
        family: "fixed",
        face: Face::Mono,
        setwidth: "semicondensed",
        italic: 'o',
        spacing: 'c',
    },
    Family {
        foundry: "adobe",
        family: "courier",
        face: Face::Mono,
        setwidth: "normal",
        italic: 'o',
        spacing: 'm',
    },
    Family {
        foundry: "adobe",
        family: "times",
        face: Face::Sans,
        setwidth: "normal",
        italic: 'i',
        spacing: 'p',
    },
    Family {
        foundry: "adobe",
        family: "new century schoolbook",
        face: Face::Sans,
        setwidth: "normal",
        italic: 'i',
        spacing: 'p',
    },
    Family {
        foundry: "b&h",
        family: "lucida",
        face: Face::Sans,
        setwidth: "normal",
        italic: 'i',
        spacing: 'p',
    },
    Family {
        foundry: "b&h",
        family: "lucidatypewriter",
        face: Face::Mono,
        setwidth: "normal",
        italic: 'o',
        spacing: 'm',
    },
    Family {
        foundry: "bitstream",
        family: "charter",
        face: Face::Sans,
        setwidth: "normal",
        italic: 'i',
        spacing: 'p',
    },
    Family {
        foundry: "dec",
        family: "terminal",
        face: Face::Mono,
        setwidth: "normal",
        italic: 'o',
        spacing: 'c',
    },
    Family {
        foundry: "schumacher",
        family: "clean",
        face: Face::Mono,
        setwidth: "normal",
        italic: 'i',
        spacing: 'c',
    },
    Family {
        foundry: "adobe",
        family: "symbol",
        face: Face::Mono,
        setwidth: "normal",
        italic: 'i',
        spacing: 'p',
    },
];

/// Tamaños que se anuncian cuando el patrón no pide ninguno.
const LISTED_SIZES: &[u32] = &[8, 10, 11, 12, 13, 14, 15, 16, 18, 20, 24];
const MAX_PIXEL: u32 = 200;
const DEFAULT_PIXEL: u32 = 13;

/// Coincidencia de patrón al estilo X: `*` vale por cualquier secuencia
/// (también con guiones) y `?` por un carácter; no distingue mayúsculas.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().flat_map(|c| c.to_lowercase()).collect();
    let n: Vec<char> = name.chars().flat_map(|c| c.to_lowercase()).collect();
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

fn number(field: &str) -> Option<u32> {
    (!field.is_empty() && field.bytes().all(|b| b.is_ascii_digit()))
        .then(|| field.parse().ok())
        .flatten()
}

/// Píxeles de un tamaño en décimas de punto a la resolución dada.
fn points_to_pixels(decipoints: u32, resolution: u32) -> u32 {
    ((decipoints as f32 / 10.0) * resolution as f32 / 72.27).round() as u32
}

const MONO_WORDS: &[&str] = &[
    "fixed",
    "courier",
    "terminal",
    "typewriter",
    "mono",
    "clean",
    "console",
    "hack",
];

/// Lo que se saca de un patrón de fuente.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Request {
    /// Campos XLFD concretos que pidió el cliente (los comodines, `None`).
    fields: [Option<String>; 14],
    face: Face,
    bold: bool,
    italic: bool,
    pixel: u32,
    /// El tamaño venía en el patrón (si no, es el de por defecto).
    sized: bool,
    encoding: Encoding,
}

/// Traduce los alias clásicos a un patrón XLFD.
fn alias(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let fixed = |pixel: u32, weight: &str| {
        format!("-misc-fixed-{weight}-r-semicondensed--{pixel}-*-75-75-c-*-iso8859-1")
    };
    match lower.as_str() {
        "fixed" => {
            return Some("-misc-fixed-medium-r-semicondensed--13-120-75-75-c-*-iso8859-1".into());
        }
        "variable" => {
            return Some("-adobe-helvetica-bold-r-normal--12-*-75-75-p-*-iso8859-1".into());
        }
        _ => {}
    }
    // «6x13», «9x15bold», «10x20»…
    let (base, weight) = match lower.strip_suffix("bold") {
        Some(base) => (base, "bold"),
        None => (lower.as_str(), "medium"),
    };
    if let Some((w, h)) = base.split_once('x')
        && number(w).is_some()
        && let Some(h) = number(h)
    {
        return Some(fixed(h.clamp(4, MAX_PIXEL), weight));
    }
    // «lucidasans-12», «lucidasanstypewriter-bold-14», «courier12»…
    let digits = lower.trim_end_matches(|c: char| c.is_ascii_digit());
    let size = number(&lower[digits.len()..])?;
    let family = digits.trim_end_matches('-');
    let (family, weight) = match family.strip_suffix("-bold") {
        Some(f) => (f, "bold"),
        None => (family, "medium"),
    };
    if family.is_empty() || !family.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let known = FAMILIES.iter().any(|f| f.family == family)
        || family.starts_with("lucida")
        || MONO_WORDS.contains(&family);
    known.then(|| format!("-*-{family}-{weight}-r-*-*-{size}-*-*-*-*-*-iso8859-1"))
}

/// Interpreta un patrón XLFD. `None` si pide un juego de caracteres que no
/// hay (KOI8, JIS…) o no es un patrón XLFD.
fn parse(pattern: &str) -> Option<Request> {
    // «*-courier-*»: el primer comodín hace de fundición.
    let owned;
    let pattern = if pattern.starts_with('*') {
        owned = format!("-{pattern}");
        owned.as_str()
    } else {
        pattern
    };
    let body = pattern.strip_prefix('-')?;
    let parts: Vec<&str> = body.split('-').collect();
    if parts.len() > 14 {
        return None;
    }

    // Colocación de los campos: exacta si están los 14. Si faltan, cada «*»
    // vale por un campo salvo el último, que absorbe los que falten.
    let mut placed: [Option<&str>; 14] = [None; 14];
    if parts.len() == 14 {
        for (slot, part) in placed.iter_mut().zip(&parts) {
            *slot = Some(part);
        }
    } else {
        // Sin ningún comodín no puede coincidir con un nombre completo.
        let star = parts.iter().rposition(|p| *p == "*")?;
        let right = &parts[star + 1..];
        for (slot, part) in placed.iter_mut().zip(&parts[..star]) {
            *slot = Some(part);
        }
        for (slot, part) in placed[14 - right.len()..].iter_mut().zip(right) {
            *slot = Some(part);
        }
    }
    let lower = |i: usize| placed[i].map(str::to_ascii_lowercase);
    let wild = |i: usize| placed[i].is_none_or(|f| f.contains(['*', '?']));

    // Juego de caracteres.
    let registry = lower(12).unwrap_or_default();
    let enc = lower(13).unwrap_or_default();
    let encoding = if wild(12) {
        match enc.as_str() {
            "15" => Encoding::Latin9,
            _ if wild(13) || enc == "1" => Encoding::Latin1,
            _ => return None,
        }
    } else if registry.starts_with("iso10646") && (wild(13) || enc == "1") {
        Encoding::Unicode
    } else if registry.starts_with("iso8859") && enc == "15" {
        Encoding::Latin9
    } else if (registry.starts_with("iso8859") && (wild(13) || enc == "1"))
        || registry.starts_with("iso646")
    {
        Encoding::Latin1
    } else if registry == "adobe" && enc == "fontspecific" {
        Encoding::Symbol
    } else {
        return None;
    };
    // La familia «symbol» lleva su propio juego de caracteres aunque el
    // patrón no lo concrete.
    let symbol = lower(1).is_some_and(|f| f == "symbol");
    let encoding = match encoding {
        Encoding::Latin1 if symbol && wild(12) && wild(13) => Encoding::Symbol,
        other => other,
    };

    // Campos concretos. Un «0» en los de tamaño es «cualquiera» (fuente
    // escalable); el de estilo añadido puede ir vacío a propósito.
    let mut fields: [Option<String>; 14] = Default::default();
    for (i, slot) in fields.iter_mut().enumerate() {
        let Some(part) = placed[i] else { continue };
        let scalable = matches!(i, 6..=9 | 11) && part == "0";
        let empty = part.is_empty() && i != 5;
        if !wild(i) && !scalable && !empty {
            *slot = Some(part.to_string());
        }
    }

    // Tamaño: en píxeles o, si no, en décimas de punto.
    let field_number = |i: usize| fields[i].as_deref().and_then(number).filter(|v| *v > 0);
    let resolution = field_number(9)
        .filter(|r| (50..=300).contains(r))
        .unwrap_or(75);
    let (pixel, sized) = if let Some(px) = field_number(6) {
        (px, true)
    } else if let Some(pt) = field_number(7) {
        (points_to_pixels(pt, resolution), true)
    } else {
        (DEFAULT_PIXEL, false)
    };
    let pixel = pixel.clamp(4, MAX_PIXEL);

    let family = lower(1).unwrap_or_default();
    let spacing = lower(10).unwrap_or_default();
    let mono = matches!(spacing.as_str(), "c" | "m")
        || (spacing != "p" && MONO_WORDS.iter().any(|m| family.contains(m)));
    let weight = lower(2).unwrap_or_default();
    // (Los símbolos salen de la fuente monoespaciada, que es la que tiene
    // letras griegas y signos matemáticos.)
    let mono = mono || encoding == Encoding::Symbol;
    Some(Request {
        face: if mono { Face::Mono } else { Face::Sans },
        bold: ["bold", "demi", "black", "heavy"]
            .iter()
            .any(|w| weight.contains(w)),
        italic: matches!(lower(3).as_deref(), Some("i" | "o")),
        pixel,
        sized,
        encoding,
        fields,
    })
}

/// Tamaño del cuadratín y ascenso/descenso en píxeles para un «tamaño de
/// píxel» de fuente X, que es su altura total y no el cuadratín.
fn scale(face: Face, pixel: u32) -> Option<(FontRef<'static>, f32, i16, i16)> {
    let font = face_ref(face)?;
    let pixel = pixel.max(1) as f32;
    let unit = skrifa::metrics::Metrics::new(&font, Size::new(1.0), LocationRef::default());
    let total = (unit.ascent - unit.descent).max(0.5);
    let em = pixel / total;
    let ascent = ((unit.ascent * em).round() as i16).clamp(1, pixel as i16);
    Some((font, em, ascent, pixel as i16 - ascent))
}

/// Ancho medio en décimas de píxel (campo AVERAGE_WIDTH del nombre).
fn average_width(face: Face, pixel: u32) -> u32 {
    static CACHE: OnceLock<Mutex<HashMap<(Face, u32), u32>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(found) = lock(cache).get(&(face, pixel)) {
        return *found;
    }
    let width = match scale(face, pixel) {
        Some((font, em, ..)) => {
            let charmap = font.charmap();
            let metrics = font.glyph_metrics(Size::new(em), LocationRef::default());
            let advance = |c: char| {
                charmap
                    .map(c)
                    .and_then(|g| metrics.advance_width(g))
                    .map(|a| a.round().max(1.0) as u32)
            };
            if face == Face::Mono {
                advance('M').unwrap_or(pixel / 2) * 10
            } else {
                let widths: Vec<u32> = (' '..='~').filter_map(advance).collect();
                widths.iter().sum::<u32>() * 10 / widths.len().max(1) as u32
            }
        }
        None => pixel * 10,
    };
    lock(cache).insert((face, pixel), width);
    width
}

impl Request {
    fn key(&self) -> CoreKey {
        CoreKey {
            face: self.face,
            pixel: self.pixel,
            bold: self.bold,
            italic: self.italic,
            encoding: self.encoding,
        }
    }

    /// Nombre XLFD completo: los campos que concretó el cliente tal cual y
    /// el resto con los valores de la fuente que se le da.
    fn name(&self) -> String {
        let family = self.fields[1]
            .as_deref()
            .and_then(|f| FAMILIES.iter().find(|c| c.family.eq_ignore_ascii_case(f)))
            .or_else(|| FAMILIES.iter().find(|c| c.face == self.face))
            .unwrap_or(&FAMILIES[0]);
        let registry = self.encoding.registry();
        let (registry, encoding) = registry.rsplit_once('-').unwrap_or((registry, "1"));
        let defaults: [String; 14] = [
            family.foundry.into(),
            family.family.into(),
            if self.bold { "bold" } else { "medium" }.into(),
            if self.italic {
                family.italic.to_string()
            } else {
                "r".into()
            },
            family.setwidth.into(),
            String::new(),
            self.pixel.to_string(),
            (self.pixel * 10).to_string(),
            "75".into(),
            "75".into(),
            if self.face == Face::Mono && family.face != Face::Mono {
                "m".into()
            } else if self.face == Face::Mono {
                family.spacing.to_string()
            } else {
                "p".into()
            },
            average_width(self.face, self.pixel).to_string(),
            registry.into(),
            encoding.into(),
        ];
        let mut name = String::new();
        for (asked, default) in self.fields.iter().zip(defaults) {
            name.push('-');
            name.push_str(asked.as_deref().unwrap_or(&default));
        }
        name
    }
}

/// Abre la fuente que mejor responde a un nombre o patrón.
pub fn open(pattern: &str) -> Option<Font> {
    let pattern = pattern.trim();
    if pattern.eq_ignore_ascii_case("cursor") {
        return Some(Font {
            name: "cursor".into(),
            core: core(CoreKey {
                face: Face::Cursor,
                pixel: 16,
                bold: false,
                italic: false,
                encoding: Encoding::Latin1,
            }),
        });
    }
    let aliased = alias(pattern);
    let request = parse(aliased.as_deref().unwrap_or(pattern))?;
    Some(Font {
        name: request.name(),
        core: core(request.key()),
    })
}

/// Nombres de fuente que encajan con un patrón (`ListFonts`).
pub fn list(pattern: &str, max: usize) -> Vec<String> {
    let pattern = pattern.trim();
    let mut out: Vec<String> = vec![];
    if max == 0 {
        return out;
    }
    // Los alias se listan con su propio nombre.
    for name in [
        "fixed", "variable", "cursor", "6x13", "7x14", "8x13", "9x15", "10x20",
    ] {
        if glob_match(pattern, name) {
            out.push(name.into());
        }
    }
    if !pattern.starts_with('-') && !pattern.starts_with('*') {
        if out.is_empty() && alias(pattern).is_some() {
            out.push(pattern.to_ascii_lowercase());
        }
        out.truncate(max);
        return out;
    }
    // Catálogo: familias × pesos × inclinaciones × tamaños × juegos.
    let request = parse(pattern);
    let sizes: Vec<u32> = match &request {
        Some(r) if r.sized => vec![r.pixel],
        _ => LISTED_SIZES.to_vec(),
    };
    'all: for family in FAMILIES {
        for bold in [false, true] {
            for italic in [false, true] {
                for pixel in &sizes {
                    let encodings: &[Encoding] = if family.family == "symbol" {
                        &[Encoding::Symbol]
                    } else {
                        &[Encoding::Latin1, Encoding::Latin9, Encoding::Unicode]
                    };
                    for encoding in encodings.iter().copied() {
                        let mut candidate = Request {
                            fields: Default::default(),
                            face: family.face,
                            bold,
                            italic,
                            pixel: *pixel,
                            sized: true,
                            encoding,
                        };
                        candidate.fields[0] = Some(family.foundry.into());
                        candidate.fields[1] = Some(family.family.into());
                        // Si se pidió en puntos, el nombre lleva esos puntos.
                        if let Some(r) = &request {
                            for i in [7, 8, 9] {
                                candidate.fields[i] = r.fields[i].clone();
                            }
                        }
                        let name = candidate.name();
                        if glob_match(pattern, &name) {
                            out.push(name);
                            if out.len() >= max {
                                break 'all;
                            }
                        }
                    }
                }
            }
        }
    }
    // Nada del catálogo encaja, pero el patrón se puede servir: se anuncia
    // la fuente que se abriría.
    if out.is_empty()
        && let Some(r) = request
    {
        out.push(r.name());
    }
    out.truncate(max);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matching() {
        assert!(glob_match("*", "cualquiera"));
        assert!(glob_match(
            "-*-fixed-*",
            "-misc-fixed-medium-r-normal--13-120"
        ));
        assert!(glob_match("-*-FIXED-medium-?-*", "-misc-fixed-medium-r-x"));
        assert!(!glob_match("-*-courier-*", "-misc-fixed-medium-r-normal"));
        assert!(glob_match("fix??", "fixed"));
        assert!(!glob_match("fix?", "fixed"));
        assert!(glob_match("*-13-*-iso8859-1", "-a-b-13-120-iso8859-1"));
        assert!(!glob_match("", "x"));
        assert!(glob_match("", ""));
    }

    #[test]
    fn patterns_give_size_spacing_and_weight() {
        let r = parse("-*-fixed-medium-r-*-*-14-*").unwrap();
        assert_eq!(
            (r.face, r.pixel, r.bold, r.italic),
            (Face::Mono, 14, false, false)
        );
        let r = parse("-*-helvetica-bold-r-normal--12-*").unwrap();
        assert_eq!((r.face, r.pixel, r.bold), (Face::Sans, 12, true));
        let r = parse("-adobe-courier-medium-o-normal--0-180-75-75-m-0-iso8859-1").unwrap();
        // 18 puntos a 75 ppp son 19 píxeles.
        assert_eq!((r.face, r.pixel, r.italic), (Face::Mono, 19, true));
        let r =
            parse("-b&h-lucidatypewriter-medium-r-normal-sans-24-*-*-*-*-*-iso10646-1").unwrap();
        assert_eq!(
            (r.face, r.pixel, r.encoding),
            (Face::Mono, 24, Encoding::Unicode)
        );
        // El patrón por defecto de Xt: 12 puntos, cualquier familia.
        let r = parse("-*-*-*-R-*-*-*-120-*-*-*-*-ISO8859-*").unwrap();
        assert_eq!(
            (r.face, r.pixel, r.encoding),
            (Face::Sans, 12, Encoding::Latin1)
        );
        // El espaciado manda aunque la familia no diga nada.
        let r = parse("-*-*-medium-r-*-*-16-*-*-*-m-*-iso8859-15").unwrap();
        assert_eq!(
            (r.face, r.pixel, r.encoding),
            (Face::Mono, 16, Encoding::Latin9)
        );
        // La fuente de símbolos de xcalc: pi y raíz cuadrada.
        let r = parse("-adobe-symbol-*-*-*-*-*-120-*-*-*-*-*-*").unwrap();
        assert_eq!((r.pixel, r.encoding), (12, Encoding::Symbol));
        let symbol = open("-adobe-symbol-*-*-*-*-*-120-*-*-*-*-*-*").unwrap();
        assert!(
            symbol.name.ends_with("-adobe-fontspecific"),
            "{}",
            symbol.name
        );
        assert_eq!(Encoding::Symbol.unicode(0x70), Some('π'));
        assert_eq!(Encoding::Symbol.unicode(0xD6), Some('√'));
        assert_eq!(Encoding::Symbol.unicode(0xB8), Some('÷'));
        assert_eq!(Encoding::Symbol.unicode(0x31), Some('1'));
        assert_eq!(Encoding::Symbol.unicode(0xF1), Some('〉'));
        assert_eq!(Encoding::Symbol.unicode(0xF2), Some('∫'));
        assert_eq!(Encoding::Symbol.unicode(0xF3), None);
        assert_eq!(Encoding::Symbol.unicode(0x7E), Some('∼'));
        assert_eq!(Encoding::Symbol.unicode(0xBD), None);
        assert_eq!(Encoding::Symbol.unicode(0x90), None);
        assert!(symbol.core.glyph(0x70).is_some() && symbol.core.glyph(0xD6).is_some());
        // Juegos de caracteres que no hay.
        assert_eq!(
            parse("-*-fixed-medium-r-normal--16-*-*-*-*-*-jisx0208.1983-0"),
            None
        );
        assert_eq!(parse("-*-*-*-*-*-*-*-*-*-*-*-*-koi8-r"), None);
        assert_eq!(parse("sin-guion"), None);
    }

    #[test]
    fn aliases() {
        for (name, pixel, face) in [
            ("fixed", 13, Face::Mono),
            ("6x13", 13, Face::Mono),
            ("9x15", 15, Face::Mono),
            ("9x15bold", 15, Face::Mono),
            ("10x20", 20, Face::Mono),
            ("variable", 12, Face::Sans),
            ("lucidasans-14", 14, Face::Sans),
            ("lucidasanstypewriter-12", 12, Face::Mono),
        ] {
            let font = open(name).unwrap_or_else(|| panic!("no abre {name}"));
            assert_eq!(font.core.key.pixel, pixel, "{name}");
            assert_eq!(font.core.key.face, face, "{name}");
            assert_eq!(font.core.ascent + font.core.descent, pixel as i16, "{name}");
        }
        assert!(open("9x15bold").unwrap().core.key.bold);
        assert!(open("cursor").is_some());
        assert!(open("no-existe").is_none());
        assert!(open("").is_none());
    }

    #[test]
    fn opened_names_are_full_xlfd_and_reopen_as_themselves() {
        for pattern in [
            "fixed",
            "-*-fixed-medium-r-*-*-14-*",
            "-*-helvetica-bold-r-normal--12-*",
            "-misc-fixed-medium-r-semicondensed--13-120-75-75-c-60-iso8859-1",
            "-*-*-*-R-*-*-*-120-*-*-*-*-ISO8859-*",
            "-*-dejavu sans mono-medium-r-*-*-17-*-*-*-*-*-iso10646-1",
        ] {
            let font = open(pattern).unwrap_or_else(|| panic!("no abre {pattern}"));
            assert_eq!(font.name.matches('-').count(), 14, "{}", font.name);
            assert!(!font.name.contains('*'), "{}", font.name);
            let again = open(&font.name).unwrap();
            assert_eq!(again.name, font.name);
            assert!(Arc::ptr_eq(&again.core, &font.core), "{pattern}");
            if pattern.starts_with('-') {
                assert!(glob_match(pattern, &font.name), "{pattern} / {}", font.name);
            }
        }
        // Un nombre completo se respeta tal cual.
        let exact = "-misc-fixed-medium-r-semicondensed--13-120-75-75-c-60-iso8859-1";
        assert_eq!(open(exact).unwrap().name, exact);
    }

    #[test]
    fn listing_matches_the_pattern() {
        let all = list("-*-*-*-*-*-*-*-*-*-*-*-*-*-*", 10_000);
        assert!(all.len() > 500, "{}", all.len());
        assert!(all.iter().all(|n| n.matches('-').count() == 14));
        let fixed = list("-misc-fixed-*", 10_000);
        assert!(!fixed.is_empty());
        assert!(fixed.iter().all(|n| n.starts_with("-misc-fixed-")));
        // Con tamaño: solo ese tamaño (y cualquier tamaño vale).
        let sized = list("-*-courier-bold-r-*-*-17-*-*-*-*-*-iso8859-1", 100);
        assert_eq!(sized.len(), 1, "{sized:?}");
        assert!(sized[0].contains("-courier-bold-r-normal--17-170-75-75-m-"));
        // El límite se respeta.
        assert_eq!(list("*", 7).len(), 7);
        assert_eq!(list("fixed", 10), vec!["fixed".to_string()]);
        assert_eq!(list("9x15", 10), vec!["9x15".to_string()]);
        assert!(list("-*-*-*-*-*-*-*-*-*-*-*-*-koi8-r", 10).is_empty());
        // Todo lo listado se puede abrir con su nombre.
        for name in all.iter().step_by(37) {
            assert_eq!(&open(name).unwrap().name, name);
        }
    }

    #[test]
    fn monospace_metrics_are_a_cell() {
        let font = open("fixed").unwrap().core;
        let cell = font.cell.expect("celda");
        assert!((6..=8).contains(&cell), "{cell}");
        assert_eq!((font.ascent + font.descent), 13);
        assert!(font.ascent >= 9 && font.descent >= 2);
        for c in [b' ', b'a', b'M', b'~', 0xf1] {
            let info = font.info(c as u32);
            assert_eq!((info.width, info.lbearing, info.rbearing), (cell, 0, cell));
        }
        // Los de control no existen: xterm dibuja él mismo los gráficos.
        assert!(!font.info(0x01).exists());
        assert!(!font.info(0x7f).exists());
        assert_eq!(font.min_bounds, font.max_bounds);
        assert!(!font.two_byte());
        let unicode = open("-misc-fixed-medium-r-normal--13-*-*-*-c-*-iso10646-1")
            .unwrap()
            .core;
        assert!(unicode.two_byte());
        assert!(unicode.info(0x2500).exists(), "cajas");
        assert!(unicode.info(0x20ac).exists(), "euro");
    }

    #[test]
    fn glyphs_have_ink_where_expected() {
        let font = open("-*-fixed-medium-r-*-*-20-*").unwrap().core;
        let glyph = font.glyph(b'M' as u32).expect("glifo de M");
        let ink: u32 = glyph.coverage.iter().map(|c| *c as u32).sum();
        assert!(ink > 255 * 20, "poca tinta: {ink}");
        assert!(glyph.coverage.iter().any(|c| *c > 200));
        // Dentro de la celda y sobre la línea base.
        let cell = font.cell.unwrap() as i32;
        assert!(glyph.left >= -1 && glyph.left + glyph.width as i32 <= cell + 1);
        assert!(glyph.top > 8 && glyph.top <= font.ascent as i32);
        assert!(
            glyph.top - glyph.height as i32 >= -1,
            "M no baja de la línea base"
        );
        // La «g» sí baja; el espacio no tiene tinta; el punto es pequeño.
        let g = font.glyph(b'g' as u32).unwrap();
        assert!(g.top - (g.height as i32) < -1);
        assert!(font.glyph(b' ' as u32).is_none());
        let dot = font.glyph(b'.' as u32).unwrap();
        assert!(dot.height < glyph.height / 2);
        // La negrita tiene más tinta que la normal.
        let bold = open("-*-fixed-bold-r-*-*-20-*").unwrap().core;
        let bold_ink: u32 = bold
            .glyph(b'M' as u32)
            .unwrap()
            .coverage
            .iter()
            .map(|c| *c as u32)
            .sum();
        assert!(bold_ink > ink);
    }

    #[test]
    fn proportional_metrics_vary() {
        let font = open("-*-helvetica-medium-r-*-*-14-*").unwrap().core;
        assert_eq!(font.cell, None);
        let (i, m) = (font.info(b'i' as u32), font.info(b'M' as u32));
        assert!(i.width < m.width, "{i:?} {m:?}");
        assert!(m.ascent > 5 && m.descent <= 1);
        assert!(m.rbearing > m.lbearing);
        assert!(font.info(b' ' as u32).exists());
        assert!(font.max_bounds.width >= m.width);
        assert!(font.min_bounds.width <= i.width);
        // Las métricas cubren el mapa de bits.
        let glyph = font.glyph(b'M' as u32).unwrap();
        assert!(glyph.left >= m.lbearing as i32);
        assert!(glyph.left + glyph.width as i32 <= m.rbearing as i32);
        assert!(glyph.top <= m.ascent as i32);
    }

    #[test]
    fn rasterizer_fills_a_square() {
        let mut outline = Outline::default();
        outline.move_to(1.0, 1.0);
        outline.line_to(5.0, 1.0);
        outline.line_to(5.0, 4.0);
        outline.line_to(1.0, 4.0);
        outline.close();
        let glyph = rasterize(&outline, false).unwrap();
        assert_eq!(
            (glyph.left, glyph.top, glyph.width, glyph.height),
            (1, 4, 4, 3)
        );
        assert!(
            glyph.coverage.iter().all(|c| *c == 255),
            "{:?}",
            glyph.coverage
        );
        // Medio píxel de desplazamiento: bordes al 50 %.
        let mut outline = Outline::default();
        outline.move_to(0.5, 0.0);
        outline.line_to(2.5, 0.0);
        outline.line_to(2.5, 1.0);
        outline.line_to(0.5, 1.0);
        outline.close();
        let glyph = rasterize(&outline, false).unwrap();
        assert_eq!(glyph.width, 3);
        let c = &glyph.coverage;
        assert!(
            (120..=135).contains(&c[0]) && c[1] == 255 && (120..=135).contains(&c[2]),
            "{c:?}"
        );
    }
}
