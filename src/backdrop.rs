//! Fondo decorativo del terminal: un texto, arte ASCII o una imagen que se
//! dibuja bajo el texto con la opacidad elegida.
//!
//! Forma parte de los temas (y, si se quiere, de cada sesión), así que se
//! guarda en JSON: la imagen va dentro, en base64, para que un tema exportado
//! se lleve su fondo consigo.

use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use egui::{Align2, Color32, FontId, Pos2, Rect, Vec2};
use serde::{Deserialize, Serialize};

use crate::i18n::tr;
use crate::theme::Rgb;
use crate::trf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BackdropKind {
    #[default]
    #[serde(rename = "ninguno")]
    None,
    #[serde(rename = "texto")]
    Text,
    #[serde(rename = "ascii")]
    Ascii,
    #[serde(rename = "imagen")]
    Image,
}

impl BackdropKind {
    pub const ALL: [BackdropKind; 4] = [
        BackdropKind::None,
        BackdropKind::Text,
        BackdropKind::Ascii,
        BackdropKind::Image,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BackdropKind::None => tr("Ninguno"),
            BackdropKind::Text => tr("Texto"),
            BackdropKind::Ascii => tr("Arte ASCII"),
            BackdropKind::Image => tr("Imagen"),
        }
    }
}

/// Dónde se coloca el fondo dentro del terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Anchor {
    #[default]
    #[serde(rename = "centro")]
    Center,
    #[serde(rename = "arriba")]
    Top,
    #[serde(rename = "abajo")]
    Bottom,
    #[serde(rename = "izquierda")]
    Left,
    #[serde(rename = "derecha")]
    Right,
    #[serde(rename = "arriba_izquierda")]
    TopLeft,
    #[serde(rename = "arriba_derecha")]
    TopRight,
    #[serde(rename = "abajo_izquierda")]
    BottomLeft,
    #[serde(rename = "abajo_derecha")]
    BottomRight,
}

impl Anchor {
    pub const ALL: [Anchor; 9] = [
        Anchor::Center,
        Anchor::Top,
        Anchor::Bottom,
        Anchor::Left,
        Anchor::Right,
        Anchor::TopLeft,
        Anchor::TopRight,
        Anchor::BottomLeft,
        Anchor::BottomRight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Anchor::Center => tr("Centro"),
            Anchor::Top => tr("Arriba"),
            Anchor::Bottom => tr("Abajo"),
            Anchor::Left => tr("Izquierda"),
            Anchor::Right => tr("Derecha"),
            Anchor::TopLeft => tr("Arriba a la izquierda"),
            Anchor::TopRight => tr("Arriba a la derecha"),
            Anchor::BottomLeft => tr("Abajo a la izquierda"),
            Anchor::BottomRight => tr("Abajo a la derecha"),
        }
    }

    fn align(self) -> Align2 {
        match self {
            Anchor::Center => Align2::CENTER_CENTER,
            Anchor::Top => Align2::CENTER_TOP,
            Anchor::Bottom => Align2::CENTER_BOTTOM,
            Anchor::Left => Align2::LEFT_CENTER,
            Anchor::Right => Align2::RIGHT_CENTER,
            Anchor::TopLeft => Align2::LEFT_TOP,
            Anchor::TopRight => Align2::RIGHT_TOP,
            Anchor::BottomLeft => Align2::LEFT_BOTTOM,
            Anchor::BottomRight => Align2::RIGHT_BOTTOM,
        }
    }
}

/// Cómo se adapta el fondo al tamaño del terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Fit {
    /// Tamaño propio (por la escala), colocado según la posición.
    #[serde(rename = "original")]
    Original,
    /// Lo más grande posible sin recortar.
    #[default]
    #[serde(rename = "ajustar")]
    Contain,
    /// Cubre todo el terminal, recortando lo que sobre.
    #[serde(rename = "rellenar")]
    Cover,
    /// Ocupa todo el terminal deformándose.
    #[serde(rename = "estirar")]
    Stretch,
    /// Se repite hasta cubrir el terminal.
    #[serde(rename = "mosaico")]
    Tile,
}

impl Fit {
    pub const ALL: [Fit; 5] = [
        Fit::Original,
        Fit::Contain,
        Fit::Cover,
        Fit::Stretch,
        Fit::Tile,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Fit::Original => tr("Tamaño original"),
            Fit::Contain => tr("Ajustar (sin recortar)"),
            Fit::Cover => tr("Rellenar (recortando)"),
            Fit::Stretch => tr("Estirar"),
            Fit::Tile => tr("Mosaico (repetir)"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Backdrop {
    #[serde(rename = "tipo")]
    pub kind: BackdropKind,
    /// Texto o arte ASCII (puede tener varias líneas).
    #[serde(rename = "texto")]
    pub text: String,
    /// Color del texto; `null` usa el color del texto del terminal.
    pub color: Option<Rgb>,
    /// Tamaño de la letra del texto, en puntos.
    #[serde(rename = "tamano")]
    pub size: f32,
    /// Giro del texto, en grados (positivo = sentido horario).
    #[serde(rename = "giro")]
    pub angle: f32,
    /// Repetir el texto hasta cubrir el terminal.
    #[serde(rename = "repetir")]
    pub repeat: bool,
    /// Imagen (PNG, JPEG, GIF, BMP o WebP) en base64.
    #[serde(rename = "imagen")]
    pub image: String,
    #[serde(rename = "ajuste")]
    pub fit: Fit,
    /// Escala de la imagen con «original» y «mosaico» (1 = tamaño real).
    #[serde(rename = "escala")]
    pub scale: f32,
    #[serde(rename = "posicion")]
    pub anchor: Anchor,
    /// Opacidad, de 0 (invisible) a 100 (opaco).
    #[serde(rename = "opacidad")]
    pub opacity: f32,
    /// Distancia a los bordes del terminal.
    #[serde(rename = "margen")]
    pub margin: f32,
}

impl Default for Backdrop {
    fn default() -> Self {
        Self {
            kind: BackdropKind::None,
            text: String::new(),
            color: None,
            size: 48.0,
            angle: 0.0,
            repeat: false,
            image: String::new(),
            fit: Fit::Contain,
            scale: 1.0,
            anchor: Anchor::Center,
            opacity: 15.0,
            margin: 16.0,
        }
    }
}

/// Tamaño máximo del texto de fondo.
pub const MAX_TEXT_CHARS: usize = 20_000;
/// Tamaño máximo de la imagen ya codificada en base64.
pub const MAX_IMAGE_BASE64: usize = 12 * 1024 * 1024;
/// Tamaño máximo del fichero de imagen que se acepta al cargarlo.
const MAX_IMAGE_FILE: u64 = 40 * 1024 * 1024;
/// Lado máximo con el que se guarda una imagen (las mayores se reducen).
const MAX_STORED_SIDE: u32 = 1920;
/// Lado máximo de la textura que se dibuja.
const MAX_TEXTURE_SIDE: u32 = 2048;
/// Tope de repeticiones en los mosaicos.
const MAX_TILES: usize = 600;

/// Imagen descodificada, lista para subirla a la tarjeta gráfica.
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

fn decode_image(bytes: &[u8]) -> Result<image::DynamicImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .context(tr("No se pudo leer la imagen"))?;
    // Una imagen ajena (un tema importado) no debe poder agotar la memoria.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().map_err(|e| {
        anyhow!(
            "{}",
            trf!(
                "La imagen no se pudo interpretar (se admiten PNG, JPEG, GIF, BMP y WebP): {e}",
                e
            )
        )
    })
}

fn could_not_read(path: &Path) -> String {
    trf!("No se pudo leer {path}", path = path.display())
}

fn not_a_file(path: &Path) -> String {
    trf!("{path} no es un fichero", path = path.display())
}

fn fit_within(img: image::DynamicImage, side: u32) -> (image::DynamicImage, bool) {
    if img.width().max(img.height()) > side {
        (
            img.resize(side, side, image::imageops::FilterType::Triangle),
            true,
        )
    } else {
        (img, false)
    }
}

/// Lee un fichero de imagen y lo devuelve en base64, listo para guardarlo en
/// un tema. Las imágenes muy grandes se reducen para no engordar el fichero.
pub fn encode_image_file(path: &Path) -> Result<String> {
    let meta = std::fs::metadata(path).with_context(|| could_not_read(path))?;
    if !meta.is_file() {
        bail!("{}", not_a_file(path));
    }
    if meta.len() > MAX_IMAGE_FILE {
        bail!(
            "{}",
            trf!(
                "{path} es demasiado grande (máximo {max} MB)",
                path = path.display(),
                max = MAX_IMAGE_FILE / (1024 * 1024)
            )
        );
    }
    let original = std::fs::read(path).with_context(|| could_not_read(path))?;
    let (img, resized) = fit_within(decode_image(&original)?, MAX_STORED_SIDE);
    let bytes = if resized || original.len() > 1_500_000 {
        let mut out = std::io::Cursor::new(Vec::new());
        let transparent = img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p.0[3] != 255);
        if transparent {
            img.write_to(&mut out, image::ImageFormat::Png)
        } else {
            image::DynamicImage::ImageRgb8(img.to_rgb8()).write_with_encoder(
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85),
            )
        }
        .context(tr("No se pudo preparar la imagen"))?;
        out.into_inner()
    } else {
        original
    };
    let encoded = B64.encode(bytes);
    if encoded.len() > MAX_IMAGE_BASE64 {
        bail!(
            "{}",
            tr("La imagen sigue siendo demasiado grande después de reducirla")
        );
    }
    Ok(encoded)
}

/// Lee un fichero de texto (arte ASCII) para usarlo de fondo.
pub fn read_text_file(path: &Path) -> Result<String> {
    let meta = std::fs::metadata(path).with_context(|| could_not_read(path))?;
    if !meta.is_file() {
        bail!("{}", not_a_file(path));
    }
    if meta.len() > 4 * MAX_TEXT_CHARS as u64 {
        bail!(
            "{}",
            trf!(
                "{path} es demasiado grande para un fondo de texto",
                path = path.display()
            )
        );
    }
    let bytes = std::fs::read(path).with_context(|| could_not_read(path))?;
    let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
    Ok(clean_text(&text))
}

/// Quita caracteres de control y limita el tamaño del texto.
fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .take(MAX_TEXT_CHARS)
        .collect()
}

impl Backdrop {
    /// Ajusta los valores a rangos razonables.
    pub fn sanitize(&mut self) {
        let fix = |v: f32, min: f32, max: f32, default: f32| {
            if v.is_finite() {
                v.clamp(min, max)
            } else {
                default
            }
        };
        self.size = fix(self.size, 6.0, 300.0, 48.0);
        self.angle = fix(self.angle, -180.0, 180.0, 0.0);
        self.scale = fix(self.scale, 0.05, 8.0, 1.0);
        self.opacity = fix(self.opacity, 0.0, 100.0, 15.0);
        self.margin = fix(self.margin, 0.0, 300.0, 16.0);
        if self.text.chars().count() > MAX_TEXT_CHARS
            || self
                .text
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            self.text = clean_text(&self.text);
        }
        if self.image.len() > MAX_IMAGE_BASE64 {
            self.image.clear();
        }
    }

    /// `true` si hay algo que dibujar.
    pub fn is_visible(&self) -> bool {
        self.opacity > 0.0
            && match self.kind {
                BackdropKind::None => false,
                BackdropKind::Text | BackdropKind::Ascii => !self.text.trim().is_empty(),
                BackdropKind::Image => !self.image.is_empty(),
            }
    }

    /// Identificador de la imagen, para no descodificarla en cada fotograma.
    /// Se calcula una vez, al aplicar el tema.
    pub fn image_key(&self) -> u64 {
        if self.image.is_empty() {
            return 0;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.image.hash(&mut hasher);
        hasher.finish() | 1
    }

    /// Descodifica la imagen del fondo.
    pub fn picture(&self) -> Result<Picture> {
        // Se admite también una URL `data:image/png;base64,…`.
        let data = match self.image.split_once("base64,") {
            Some((prefix, rest)) if prefix.starts_with("data:") => rest,
            _ => self.image.as_str(),
        };
        let compact: String = data.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = B64
            .decode(compact.trim_end_matches('='))
            .or_else(|_| B64.decode(&compact))
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&compact))
            .map_err(|_| anyhow!("{}", tr("La imagen del fondo no está en base64")))?;
        let (img, _) = fit_within(decode_image(&bytes)?, MAX_TEXTURE_SIDE);
        let rgba = img.to_rgba8();
        Ok(Picture {
            width: rgba.width() as usize,
            height: rgba.height() as usize,
            rgba: rgba.into_raw(),
        })
    }

    /// Dibuja el fondo dentro de `rect`. `key` es `image_key()` (calculado
    /// de antemano) y `text_color` el color por defecto del texto.
    pub fn paint(&self, painter: &egui::Painter, rect: Rect, key: u64, text_color: Color32) {
        if !self.is_visible() {
            return;
        }
        let opacity = self.opacity / 100.0;
        let area = rect.shrink(self.margin.min(rect.width().min(rect.height()) / 3.0));
        match self.kind {
            BackdropKind::None => {}
            BackdropKind::Text | BackdropKind::Ascii => {
                let font = if self.kind == BackdropKind::Ascii {
                    FontId::monospace(self.size)
                } else {
                    FontId::proportional(self.size)
                };
                let color = self
                    .color
                    .map(Rgb::color32)
                    .unwrap_or(text_color)
                    .gamma_multiply(opacity);
                let galley = painter.layout_no_wrap(self.text.clone(), font, color);
                let size = galley.size();
                if size.x < 1.0 || size.y < 1.0 {
                    return;
                }
                let angle = self.angle.to_radians();
                // La posición de un texto es su esquina superior izquierda;
                // el giro se hace alrededor de su centro.
                let draw = |center: Pos2| {
                    painter.add(
                        egui::epaint::TextShape::new(center - size / 2.0, galley.clone(), color)
                            .with_angle_and_anchor(angle, Align2::CENTER_CENTER),
                    );
                };
                if self.repeat {
                    // Rejilla con las filas alternas desplazadas, como una
                    // marca de agua. Se gira entera alrededor del centro
                    // para que los textos girados no se pisen entre sí.
                    let gap = self.margin.max(8.0) * 2.0;
                    let mut step = size + Vec2::splat(gap);
                    let reach = rect.size().length() / 2.0 + size.length();
                    // Con una letra diminuta harían falta miles de copias:
                    // se separan más en lugar de dejar zonas sin cubrir.
                    let wanted = (2.0 * reach / step.x + 1.0) * (2.0 * reach / step.y + 1.0);
                    if wanted > MAX_TILES as f32 {
                        step *= (wanted / MAX_TILES as f32).sqrt();
                    }
                    let cols = (reach / step.x).ceil() as i32;
                    let rows = (reach / step.y).ceil() as i32;
                    let rotation = egui::emath::Rot2::from_angle(angle);
                    let visible = rect.expand(size.length() / 2.0);
                    for row in -rows..=rows {
                        let shift = if row.rem_euclid(2) == 1 {
                            step.x / 2.0
                        } else {
                            0.0
                        };
                        for col in -cols..=cols {
                            let offset =
                                Vec2::new(col as f32 * step.x + shift, row as f32 * step.y);
                            let center = rect.center() + rotation * offset;
                            if visible.contains(center) {
                                draw(center);
                            }
                        }
                    }
                } else {
                    let place = self.anchor.align().align_size_within_rect(size, area);
                    draw(place.center());
                }
            }
            BackdropKind::Image => {
                let Some(texture) = texture(painter.ctx(), self, key) else {
                    return;
                };
                let tint = Color32::from_white_alpha((opacity * 255.0).round() as u8);
                let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                let image_size = texture.size_vec2();
                if image_size.x < 1.0 || image_size.y < 1.0 {
                    return;
                }
                let align = self.anchor.align();
                match self.fit {
                    Fit::Stretch => {
                        painter.image(texture.id(), rect, uv, tint);
                    }
                    Fit::Contain | Fit::Cover => {
                        let (frame, ratio) = if self.fit == Fit::Contain {
                            let r = (area.width() / image_size.x).min(area.height() / image_size.y);
                            (area, r)
                        } else {
                            let r = (rect.width() / image_size.x).max(rect.height() / image_size.y);
                            (rect, r)
                        };
                        let place = align.align_size_within_rect(image_size * ratio, frame);
                        painter.image(texture.id(), place, uv, tint);
                    }
                    Fit::Original => {
                        let place = align.align_size_within_rect(image_size * self.scale, area);
                        painter.image(texture.id(), place, uv, tint);
                    }
                    Fit::Tile => {
                        let tile = (image_size * self.scale).max(Vec2::splat(4.0));
                        let rows = (rect.height() / tile.y).ceil() as usize;
                        let cols = (rect.width() / tile.x).ceil() as usize;
                        let cols = cols.min(MAX_TILES / rows.clamp(1, MAX_TILES)).max(1);
                        for row in 0..rows {
                            for col in 0..cols {
                                let min =
                                    rect.min + Vec2::new(col as f32 * tile.x, row as f32 * tile.y);
                                painter.image(
                                    texture.id(),
                                    Rect::from_min_size(min, tile),
                                    uv,
                                    tint,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Texturas ya subidas, por identificador de imagen. `None` recuerda que una
/// imagen no se pudo descodificar, para no reintentarlo en cada fotograma.
#[derive(Clone, Default)]
struct Textures(Arc<Mutex<Vec<CachedTexture>>>);

type CachedTexture = (u64, Option<egui::TextureHandle>);

fn texture(ctx: &egui::Context, backdrop: &Backdrop, key: u64) -> Option<egui::TextureHandle> {
    let cache: Textures = ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Textures>(egui::Id::unique("godremote_backdrops"))
            .clone()
    });
    let mut list = cache.0.lock().unwrap();
    if let Some((_, found)) = list.iter().find(|(k, _)| *k == key) {
        return found.clone();
    }
    let loaded = backdrop.picture().ok().map(|p| {
        ctx.load_texture(
            format!("fondo-{key:016x}"),
            egui::ColorImage::from_rgba_unmultiplied([p.width, p.height], &p.rgba),
            egui::TextureOptions::LINEAR,
        )
    });
    if list.len() >= 6 {
        list.remove(0);
    }
    list.push((key, loaded.clone()));
    loaded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, alpha: u8) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 200, 30, alpha]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn json_is_readable_and_tolerant() {
        let json = serde_json::to_string(&Backdrop::default()).unwrap();
        assert!(json.contains("\"tipo\":\"ninguno\""), "{json}");
        assert!(json.contains("\"opacidad\":15.0"), "{json}");
        // Lo que falta se toma de los valores por defecto.
        let b: Backdrop =
            serde_json::from_str(r##"{"tipo":"texto","texto":"PRODUCCIÓN","color":"#ff0000"}"##)
                .unwrap();
        assert_eq!(b.kind, BackdropKind::Text);
        assert_eq!(b.color, Some(Rgb([255, 0, 0])));
        assert_eq!(b.anchor, Anchor::Center);
        assert!(b.is_visible());
        // Claves desconocidas y tipos inventados se rechazan.
        assert!(serde_json::from_str::<Backdrop>(r#"{"tipoo":"texto"}"#).is_err());
        assert!(serde_json::from_str::<Backdrop>(r#"{"tipo":"video"}"#).is_err());
    }

    #[test]
    fn sanitize_limits_values() {
        let mut b = Backdrop {
            kind: BackdropKind::Ascii,
            text: format!("a\u{7}b\x1b[31m\n{}", "x".repeat(MAX_TEXT_CHARS)),
            size: 9999.0,
            opacity: -5.0,
            scale: f32::NAN,
            ..Default::default()
        };
        b.sanitize();
        assert_eq!(b.size, 300.0);
        assert_eq!(b.opacity, 0.0);
        assert_eq!(b.scale, 1.0);
        assert!(b.text.starts_with("ab[31m\n"));
        assert_eq!(b.text.chars().count(), MAX_TEXT_CHARS);
        // Con opacidad 0 no hay nada que dibujar.
        assert!(!b.is_visible());
    }

    #[test]
    fn image_files_are_encoded_and_decoded() {
        let dir = tempfile::tempdir().unwrap();
        // Una imagen pequeña se guarda tal cual.
        let small = dir.path().join("logo.png");
        std::fs::write(&small, png(40, 20, 255)).unwrap();
        let encoded = encode_image_file(&small).unwrap();
        let b = Backdrop {
            kind: BackdropKind::Image,
            image: encoded.clone(),
            ..Default::default()
        };
        let p = b.picture().unwrap();
        assert_eq!((p.width, p.height), (40, 20));
        assert_eq!(&p.rgba[..4], &[10, 200, 30, 255]);
        assert!(b.is_visible() && b.image_key() != 0);
        assert_eq!(Backdrop::default().image_key(), 0);
        // También como URL `data:` y con saltos de línea.
        let wrapped = Backdrop {
            image: format!("data:image/png;base64,{encoded}\n"),
            ..b.clone()
        };
        assert_eq!(wrapped.picture().unwrap().width, 40);

        // Una imagen enorme se reduce al guardarla, conservando la proporción
        // y la transparencia.
        let big = dir.path().join("grande.png");
        std::fs::write(&big, png(4000, 1000, 128)).unwrap();
        let b = Backdrop {
            image: encode_image_file(&big).unwrap(),
            ..b
        };
        let p = b.picture().unwrap();
        assert_eq!((p.width, p.height), (1920, 480));
        assert_eq!(p.rgba[3], 128);

        // Errores comprensibles.
        let bad = dir.path().join("no-es-imagen.png");
        std::fs::write(&bad, b"hola").unwrap();
        assert!(format!("{:#}", encode_image_file(&bad).unwrap_err()).contains("imagen"));
        assert!(encode_image_file(&dir.path().join("no-existe.png")).is_err());
        let broken = Backdrop {
            image: "esto no es base64 !!".into(),
            ..Default::default()
        };
        assert!(broken.picture().is_err());
    }

    #[test]
    fn ascii_files_are_cleaned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("arte.txt");
        std::fs::write(&path, "  /\\_/\\\r\n ( o.o )\x07\r\n").unwrap();
        assert_eq!(read_text_file(&path).unwrap(), "  /\\_/\\\n ( o.o )\n");
    }
}
