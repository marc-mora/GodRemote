//! Controles para elegir el fondo del terminal: texto, arte ASCII o imagen,
//! con su opacidad. Se usan en el editor de temas y en el de sesiones.

use std::path::PathBuf;

use crate::backdrop::{self, Anchor, Backdrop, BackdropKind, Fit, MAX_TEXT_CHARS};
use crate::i18n::tr;
use crate::theme::{self, Rgb};
use crate::trf;

/// Un dibujo de muestra para empezar con el arte ASCII.
const SAMPLE_ASCII: &str = r"        .--.
       |o_o |
       |:_/ |
      //   \ \
     (|     | )
    /'\_   _/`\
    \___)=(___/";

#[derive(Default)]
pub struct BackdropEditor {
    /// Ruta escrita a mano para cargar un fichero sin pasar por el diálogo.
    path: String,
    message: Option<(String, bool)>,
    /// Descripción de la imagen cargada, por identificador de imagen.
    info: Option<(u64, Result<String, String>)>,
}

fn slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
) -> bool {
    ui.label(label);
    let changed = ui
        .add(
            egui::Slider::new(value, range)
                .suffix(suffix)
                .max_decimals(1),
        )
        .changed();
    ui.end_row();
    changed
}

impl BackdropEditor {
    /// Ruta del fichero a cargar: la escrita a mano o, si está vacía, la que
    /// se elija en el diálogo.
    fn pick(&self, title: &str, filter: (&str, &[&str])) -> Option<PathBuf> {
        if !self.path.trim().is_empty() {
            return Some(crate::paths::expand_tilde(&self.path));
        }
        rfd::FileDialog::new()
            .set_title(title)
            .add_filter(filter.0, filter.1)
            .pick_file()
    }

    /// Medidas de la imagen cargada (o por qué no se puede leer). Se
    /// descodifica una sola vez por imagen, no en cada fotograma.
    fn image_info(&mut self, b: &Backdrop) -> Result<String, String> {
        // Huella barata (tamaño y extremos): esto se ejecuta en cada
        // fotograma y la imagen puede ocupar varios megas.
        let bytes = b.image.as_bytes();
        let edge = bytes.len().min(512);
        let key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (bytes.len(), &bytes[..edge], &bytes[bytes.len() - edge..]).hash(&mut hasher);
            hasher.finish()
        };
        if self.info.as_ref().is_none_or(|(k, _)| *k != key) {
            let info = b
                .picture()
                .map(|p| {
                    format!(
                        "{}×{} px ({})",
                        p.width,
                        p.height,
                        super::util::format_size((b.image.len() * 3 / 4) as u64)
                    )
                })
                .map_err(|e| format!("{e:#}"));
            self.info = Some((key, info));
        }
        self.info
            .as_ref()
            .map(|(_, i)| i.clone())
            .unwrap_or(Ok(String::new()))
    }

    /// Dibuja los controles. Devuelve `true` si el fondo ha cambiado.
    pub fn show(&mut self, ui: &mut egui::Ui, b: &mut Backdrop, salt: &str) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            for kind in BackdropKind::ALL {
                if ui
                    .selectable_label(b.kind == kind, tr(kind.label()))
                    .clicked()
                    && b.kind != kind
                {
                    b.kind = kind;
                    // Cada tipo empieza con un tamaño de letra razonable.
                    match kind {
                        BackdropKind::Text if b.size < 20.0 => b.size = 48.0,
                        BackdropKind::Ascii if b.size > 24.0 => b.size = 14.0,
                        _ => {}
                    }
                    self.message = None;
                    self.path.clear();
                    changed = true;
                }
            }
        });
        if b.kind == BackdropKind::None {
            ui.weak(tr(
                "El terminal se ve con su color de fondo, sin nada detrás del texto.",
            ));
            return changed;
        }

        let is_text = matches!(b.kind, BackdropKind::Text | BackdropKind::Ascii);
        if is_text {
            let ascii = b.kind == BackdropKind::Ascii;
            let width = ui.available_width();
            // El arte ASCII no se parte en varias líneas: se desplaza.
            let font = egui::TextStyle::Monospace.resolve(ui.style());
            let color = ui.visuals().text_color();
            let mut no_wrap = |ui: &egui::Ui, text: &dyn egui::TextBuffer, _wrap: f32| {
                let job = egui::text::LayoutJob::simple(
                    text.as_str().to_owned(),
                    font.clone(),
                    color,
                    f32::INFINITY,
                );
                ui.fonts_mut(|f| f.layout_job(job))
            };
            let edit = egui::TextEdit::multiline(&mut b.text)
                .id_salt((salt, "backdrop_text"))
                .char_limit(MAX_TEXT_CHARS)
                .desired_width(width)
                .desired_rows(if ascii { 6 } else { 2 })
                .hint_text(if ascii {
                    tr("Pega aquí tu dibujo ASCII")
                } else {
                    tr("Por ejemplo: PRODUCCIÓN")
                });
            let (edit, scroll) = if ascii {
                (edit.layouter(&mut no_wrap), egui::ScrollArea::both())
            } else {
                (edit, egui::ScrollArea::vertical())
            };
            scroll
                .id_salt((salt, "backdrop_text_scroll"))
                .max_height(140.0)
                .show(ui, |ui| changed |= ui.add(edit).changed());
        } else if b.image.is_empty() {
            ui.weak(tr(
                "Aún no hay imagen: elige un fichero PNG, JPEG, GIF, BMP o WebP.",
            ));
        } else {
            match self.image_info(b) {
                Ok(info) => ui.label(trf!("Imagen cargada: {info}", info)),
                Err(e) => ui.colored_label(theme::error_color(), e),
            };
        }

        ui.horizontal(|ui| {
            if is_text {
                if ui
                    .button(tr("📂 Cargar de un fichero…"))
                    .on_hover_text(tr("Lee el texto o el dibujo ASCII de un fichero de texto"))
                    .clicked()
                    && let Some(path) =
                        self.pick(tr("Texto de fondo"), (tr("Texto"), &["txt", "asc", "ans"]))
                {
                    match backdrop::read_text_file(&path) {
                        Ok(text) => {
                            b.text = text;
                            let message = trf!("Cargado {path}", path = path.display());
                            self.message = Some((message, false));
                            changed = true;
                        }
                        Err(e) => self.message = Some((format!("{e:#}"), true)),
                    }
                }
                if b.kind == BackdropKind::Ascii && ui.button(tr("Poner un ejemplo")).clicked() {
                    b.text = SAMPLE_ASCII.to_string();
                    changed = true;
                }
            } else {
                if ui.button(tr("📂 Elegir imagen…")).clicked()
                    && let Some(path) = self.pick(
                        tr("Imagen de fondo"),
                        (
                            tr("Imágenes"),
                            &["png", "jpg", "jpeg", "gif", "bmp", "webp"],
                        ),
                    )
                {
                    match backdrop::encode_image_file(&path) {
                        Ok(image) => {
                            b.image = image;
                            let message = trf!("Cargada {path}", path = path.display());
                            self.message = Some((message, false));
                            changed = true;
                        }
                        Err(e) => self.message = Some((format!("{e:#}"), true)),
                    }
                }
                if !b.image.is_empty() && ui.button(tr("Quitar")).clicked() {
                    b.image.clear();
                    self.message = None;
                    changed = true;
                }
            }
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.path)
                .id_salt((salt, "backdrop_path"))
                .hint_text(tr("Ruta del fichero (opcional)"))
                .desired_width(f32::INFINITY),
        )
        .on_hover_text(tr(
            "Déjalo vacío para elegir con el explorador de archivos; \
             si escribes una ruta se carga esa",
        ));
        if let Some((message, is_error)) = &self.message {
            let color = if *is_error {
                theme::error_color()
            } else {
                theme::ok_color()
            };
            ui.colored_label(color, message);
        }

        egui::Grid::new((salt, "backdrop_grid"))
            .num_columns(2)
            .show(ui, |ui| {
                changed |= slider(ui, tr("Opacidad"), &mut b.opacity, 0.0..=100.0, " %");
                if is_text {
                    changed |= slider(
                        ui,
                        tr("Tamaño de la letra"),
                        &mut b.size,
                        6.0..=300.0,
                        " pt",
                    );
                    ui.label(tr("Color"));
                    ui.horizontal(|ui| {
                        let mut own = b.color.is_some();
                        if ui
                            .checkbox(&mut own, tr("Propio"))
                            .on_hover_text(tr("Sin marcar se usa el color del texto del terminal"))
                            .changed()
                        {
                            b.color = own.then_some(Rgb::hex(0xff5c5c));
                            changed = true;
                        }
                        if let Some(color) = &mut b.color {
                            changed |= ui.color_edit_button_srgb(&mut color.0).changed();
                        }
                    });
                    ui.end_row();
                    changed |= slider(ui, tr("Giro"), &mut b.angle, -90.0..=90.0, "°");
                    ui.label(tr("Repetir"));
                    changed |= ui
                        .checkbox(&mut b.repeat, tr("Por todo el terminal"))
                        .changed();
                    ui.end_row();
                } else {
                    ui.label(tr("Ajuste"));
                    egui::ComboBox::from_id_salt((salt, "backdrop_fit"))
                        .selected_text(tr(b.fit.label()))
                        .width(190.0)
                        .show_ui(ui, |ui| {
                            for fit in Fit::ALL {
                                changed |= ui
                                    .selectable_value(&mut b.fit, fit, tr(fit.label()))
                                    .changed();
                            }
                        });
                    ui.end_row();
                    if matches!(b.fit, Fit::Original | Fit::Tile) {
                        ui.label(tr("Escala"));
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut b.scale, 0.05..=8.0)
                                    .logarithmic(true)
                                    .max_decimals(2)
                                    .suffix("×"),
                            )
                            .changed();
                        ui.end_row();
                    }
                }
                let positioned = if is_text {
                    !b.repeat
                } else {
                    !matches!(b.fit, Fit::Stretch | Fit::Tile)
                };
                if positioned {
                    ui.label(tr("Posición"));
                    egui::ComboBox::from_id_salt((salt, "backdrop_anchor"))
                        .selected_text(tr(b.anchor.label()))
                        .width(190.0)
                        .show_ui(ui, |ui| {
                            for anchor in Anchor::ALL {
                                changed |= ui
                                    .selectable_value(&mut b.anchor, anchor, tr(anchor.label()))
                                    .changed();
                            }
                        });
                    ui.end_row();
                }
                if is_text || matches!(b.fit, Fit::Original | Fit::Contain) {
                    changed |= slider(ui, tr("Margen"), &mut b.margin, 0.0..=300.0, " px");
                }
            });
        changed
    }
}
