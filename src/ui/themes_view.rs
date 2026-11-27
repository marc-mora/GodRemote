//! Ventana de temas: elegir, editar, importar y exportar.

use std::path::PathBuf;

use egui::RichText;

use super::backdrop_view::BackdropEditor;
use crate::theme::{self, Rgb, Settings, Theme, ThemeStore};

/// Tema en edición (copia de trabajo hasta que se guarda).
struct Editing {
    theme: Theme,
    /// Nombre con el que está guardado; `None` si es nuevo.
    original: Option<String>,
}

pub struct ThemesWindow {
    selected: String,
    editing: Option<Editing>,
    /// Ruta escrita a mano para importar o exportar sin diálogo.
    path: String,
    message: Option<(String, bool)>,
    confirm_delete: Option<String>,
    backdrop: BackdropEditor,
}

/// Fila con una medida (tamaño, separación…) y su deslizador.
fn measure(
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

fn color_row(ui: &mut egui::Ui, label: &str, color: &mut Rgb) -> bool {
    let mut changed = false;
    ui.label(label);
    ui.horizontal(|ui| {
        changed |= ui.color_edit_button_srgb(&mut color.0).changed();
        // Mientras se escribe, el texto se guarda aparte: si no, cada tecla
        // se perdería al regenerarse a partir del color.
        let id = ui.make_persistent_id(("hex", label));
        let mut hex = ui
            .data(|d| d.get_temp::<String>(id))
            .unwrap_or_else(|| color.to_hex());
        let resp = ui.add(
            egui::TextEdit::singleline(&mut hex)
                .desired_width(80.0)
                .font(egui::TextStyle::Monospace),
        );
        if resp.changed()
            && hex.trim().trim_start_matches('#').len() == 6
            && let Some(parsed) = Rgb::parse(&hex)
        {
            *color = parsed;
            changed = true;
        }
        if resp.has_focus() {
            ui.data_mut(|d| d.insert_temp(id, hex));
        } else {
            ui.data_mut(|d| d.remove::<String>(id));
        }
    });
    ui.end_row();
    changed
}

impl ThemesWindow {
    pub fn new(active: &str) -> Self {
        Self {
            selected: active.to_string(),
            editing: None,
            path: String::new(),
            message: None,
            confirm_delete: None,
            backdrop: BackdropEditor::default(),
        }
    }

    fn ok(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), false));
    }

    fn fail(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), true));
    }

    /// Activa un tema y lo recuerda en las preferencias.
    fn activate(
        &mut self,
        ctx: &egui::Context,
        store: &ThemeStore,
        settings: &mut Settings,
        name: &str,
    ) {
        if let Some(theme) = store.get(name) {
            theme.apply(ctx);
            self.selected = name.to_string();
            settings.theme = name.to_string();
            if let Err(e) = settings.save(&crate::paths::settings_file()) {
                self.fail(format!("No se pudieron guardar las preferencias: {e:#}"));
            }
        }
    }

    /// Ruta para importar: la escrita a mano o, si está vacía, un diálogo.
    fn import_path(&self) -> Option<PathBuf> {
        if !self.path.trim().is_empty() {
            return Some(crate::paths::expand_tilde(&self.path));
        }
        rfd::FileDialog::new()
            .set_title("Importar tema")
            .add_filter("Tema de GodRemote (JSON)", &["json"])
            .pick_file()
    }

    /// Ruta para exportar: la escrita a mano o, si está vacía, un diálogo.
    fn export_path(&self, suggested: &str) -> Option<PathBuf> {
        if !self.path.trim().is_empty() {
            let mut path = crate::paths::expand_tilde(&self.path);
            // Si se indica una carpeta, se guarda dentro con el nombre sugerido.
            if path.is_dir() {
                path = path.join(suggested);
            }
            return Some(path);
        }
        rfd::FileDialog::new()
            .set_title("Guardar tema")
            .set_directory(theme::default_export_dir())
            .set_file_name(suggested)
            .add_filter("Tema de GodRemote (JSON)", &["json"])
            .save_file()
    }

    fn import(
        &mut self,
        ctx: &egui::Context,
        store: &mut ThemeStore,
        settings: &mut Settings,
        path: PathBuf,
    ) {
        match store.import(&path) {
            Ok(name) => {
                self.editing = None;
                self.activate(ctx, store, settings, &name);
                self.ok(format!("Tema «{name}» importado y aplicado"));
            }
            Err(e) => self.fail(format!("{e:#}")),
        }
    }

    fn export(&mut self, theme: &Theme, suggested: &str) {
        let Some(path) = self.export_path(suggested) else {
            return;
        };
        match theme::export(theme, &path) {
            Ok(()) => self.ok(format!("Tema guardado en {}", path.display())),
            Err(e) => self.fail(format!("{e:#}")),
        }
    }

    /// Devuelve `false` cuando el usuario cierra la ventana.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        store: &mut ThemeStore,
        settings: &mut Settings,
    ) -> bool {
        // Soltar un fichero .json sobre la ventana lo importa.
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        for path in dropped {
            self.import(ctx, store, settings, path);
        }

        let mut open = true;
        egui::Window::new("Temas")
            .id(egui::Id::unique("themes_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos([230.0, 50.0])
            .show(ctx, |ui| {
                ui.set_width(820.0);
                // Alto fijo: el contenido no debe estirar la ventana.
                ui.allocate_ui_with_layout(
                    egui::vec2(820.0, 520.0),
                    egui::Layout::left_to_right(egui::Align::TOP),
                    |ui| {
                        ui.vertical(|ui| {
                            ui.set_width(300.0);
                            self.list(ui, ctx, store, settings);
                        });
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.set_width(490.0);
                            self.editor(ui, ctx, store, settings);
                        });
                    },
                );
                if let Some((msg, is_error)) = &self.message {
                    ui.separator();
                    let color = if *is_error {
                        ui.visuals().error_fg_color
                    } else {
                        theme::ok_color()
                    };
                    ui.colored_label(color, msg);
                }
            });

        if let Some(name) = self.confirm_delete.clone() {
            let mut close = false;
            egui::Modal::new(egui::Id::unique("confirm_delete_theme")).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.heading("Eliminar tema");
                ui.label(format!(
                    "Se eliminará el tema «{name}». No se puede deshacer."
                ));
                ui.horizontal(|ui| {
                    if ui
                        .button(RichText::new("Eliminar").color(ui.visuals().error_fg_color))
                        .clicked()
                    {
                        match store.delete_user(&name) {
                            Ok(()) => {
                                self.editing = None;
                                self.activate(ctx, store, settings, theme::BASE_NAME);
                                self.ok(format!("Tema «{name}» eliminado"));
                            }
                            Err(e) => self.fail(format!("{e:#}")),
                        }
                        close = true;
                    }
                    if ui.button("Cancelar").clicked() {
                        close = true;
                    }
                });
            });
            if close {
                self.confirm_delete = None;
            }
        }

        if !open {
            // Lo que no se guardó se descarta: vuelve el tema activo.
            if self.editing.take().is_some()
                && let Some(active) = store.get(&settings.theme)
            {
                active.apply(ctx);
            }
        }
        open
    }

    fn list(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        store: &mut ThemeStore,
        settings: &mut Settings,
    ) {
        ui.strong("Temas disponibles");
        let mut activate = None;
        egui::ScrollArea::vertical()
            .id_salt("theme_list")
            .max_height(230.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (name, builtin) in store.names() {
                    let label = if builtin {
                        format!("{name}  ·  integrado")
                    } else {
                        name.clone()
                    };
                    if ui.selectable_label(self.selected == name, label).clicked()
                        && self.selected != name
                    {
                        activate = Some(name);
                    }
                }
            });
        if let Some(name) = activate {
            self.editing = None;
            self.activate(ctx, store, settings, &name);
        }

        let builtin = store.is_builtin(&self.selected);
        ui.horizontal(|ui| {
            if ui
                .button("Duplicar y editar")
                .on_hover_text("Crea una copia personalizable del tema seleccionado")
                .clicked()
                && let Some(base) = store.get(&self.selected)
            {
                let mut copy = base.clone();
                copy.name = store.free_name(&format!("{} (mío)", base.name));
                self.editing = Some(Editing {
                    theme: copy,
                    original: None,
                });
            }
            if ui
                .add_enabled(!builtin, egui::Button::new("Editar"))
                .clicked()
                && let Some(theme) = store.get(&self.selected)
            {
                self.editing = Some(Editing {
                    theme: theme.clone(),
                    original: Some(theme.name.clone()),
                });
            }
            if ui
                .add_enabled(!builtin, egui::Button::new("🗑"))
                .on_hover_text("Eliminar el tema seleccionado")
                .clicked()
            {
                self.confirm_delete = Some(self.selected.clone());
            }
        });

        ui.add_space(8.0);
        ui.strong("Compartir");
        if ui
            .button("⬇ Descargar tema base")
            .on_hover_text("Guarda el tema base en un fichero JSON para modificarlo a tu gusto")
            .clicked()
        {
            self.export(&Theme::base(), "godremote-tema-base.json");
        }
        if ui.button("⬇ Exportar el tema seleccionado").clicked()
            && let Some(theme) = store.get(&self.selected).cloned()
        {
            let file = format!(
                "godremote-{}.json",
                theme
                    .name
                    .to_lowercase()
                    .replace(|c: char| !c.is_alphanumeric(), "-")
            );
            self.export(&theme, &file);
        }
        if ui
            .button("⬆ Importar un tema…")
            .on_hover_text("Carga un fichero de tema (también puedes soltarlo sobre la ventana)")
            .clicked()
            && let Some(path) = self.import_path()
        {
            self.import(ctx, store, settings, path);
        }
        ui.add(
            egui::TextEdit::singleline(&mut self.path)
                .hint_text("Ruta del fichero (opcional)")
                .desired_width(f32::INFINITY),
        )
        .on_hover_text(
            "Déjalo vacío para elegir con el explorador de archivos; \
             si escribes una ruta se usa esa al importar o exportar",
        );
        for error in &store.load_errors {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    }

    fn editor(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        store: &mut ThemeStore,
        settings: &mut Settings,
    ) {
        let Some(editing) = &mut self.editing else {
            ui.strong("Personalizar");
            ui.label(
                "Elige un tema de la lista para aplicarlo. Para cambiar colores, tamaños, \
                 separaciones o el fondo del terminal pulsa «Duplicar y editar»: todo se \
                 puede ajustar y los cambios se ven al instante.",
            );
            ui.add_space(6.0);
            ui.label(
                "También puedes descargar el tema base, modificarlo con cualquier editor \
                 de texto y volver a importarlo.",
            );
            ui.add_space(10.0);
            preview(ui);
            return;
        };

        let mut changed = false;
        ui.horizontal(|ui| {
            ui.strong("Nombre");
            ui.add(egui::TextEdit::singleline(&mut editing.theme.name).desired_width(220.0));
            changed |= ui
                .checkbox(&mut editing.theme.dark, "Fondo oscuro")
                .on_hover_text("Ajusta sombras y contrastes para fondos oscuros o claros")
                .changed();
        });
        egui::ScrollArea::vertical()
            .id_salt("theme_editor")
            .max_height(420.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                egui::CollapsingHeader::new("Colores de la interfaz").show(ui, |ui| {
                    egui::Grid::new("theme_ui").num_columns(2).show(ui, |ui| {
                        for (label, color) in editing.theme.ui.fields_mut() {
                            changed |= color_row(ui, label, color);
                        }
                    });
                });
                egui::CollapsingHeader::new("Colores del terminal").show(ui, |ui| {
                    egui::Grid::new("theme_term").num_columns(2).show(ui, |ui| {
                        for (label, color) in editing.theme.terminal.fields_mut() {
                            changed |= color_row(ui, label, color);
                        }
                    });
                });
                egui::CollapsingHeader::new("Colores ANSI del terminal").show(ui, |ui| {
                    egui::Grid::new("theme_ansi").num_columns(2).show(ui, |ui| {
                        for (label, color) in editing.theme.terminal.ansi.fields_mut() {
                            changed |= color_row(ui, label, color);
                        }
                    });
                });
                egui::CollapsingHeader::new("Letra e interlineado").show(ui, |ui| {
                    egui::Grid::new("theme_fonts")
                        .num_columns(2)
                        .show(ui, |ui| {
                            let f = &mut editing.theme.fonts;
                            changed |=
                                measure(ui, "Letra de la interfaz", &mut f.ui, 8.0..=32.0, " pt");
                            changed |= measure(ui, "Títulos", &mut f.heading, 8.0..=48.0, " pt");
                            changed |=
                                measure(ui, "Texto pequeño", &mut f.small, 6.0..=32.0, " pt");
                            changed |= measure(ui, "Botones", &mut f.button, 8.0..=32.0, " pt");
                            changed |= measure(
                                ui,
                                "Interlineado de la interfaz",
                                &mut f.ui_line_spacing,
                                0.0..=12.0,
                                " px",
                            );
                            changed |= measure(
                                ui,
                                "Letra del terminal",
                                &mut f.terminal,
                                6.0..=48.0,
                                " pt",
                            );
                            changed |= measure(
                                ui,
                                "Interlineado del terminal",
                                &mut f.terminal_line_height,
                                0.8..=2.5,
                                "×",
                            );
                            changed |= measure(
                                ui,
                                "Separación entre letras del terminal",
                                &mut f.terminal_letter_spacing,
                                -1.0..=8.0,
                                " px",
                            );
                            changed |= measure(
                                ui,
                                "Barra de información del servidor",
                                &mut f.server_bar,
                                7.0..=28.0,
                                " pt",
                            );
                        });
                });
                egui::CollapsingHeader::new("Separaciones y forma").show(ui, |ui| {
                    egui::Grid::new("theme_shape")
                        .num_columns(2)
                        .show(ui, |ui| {
                            let f = &mut editing.theme.shape;
                            changed |= measure(
                                ui,
                                "Separación horizontal entre elementos",
                                &mut f.spacing,
                                0.0..=30.0,
                                " px",
                            );
                            changed |= measure(
                                ui,
                                "Separación vertical entre elementos",
                                &mut f.spacing_y,
                                0.0..=30.0,
                                " px",
                            );
                            changed |= measure(
                                ui,
                                "Relleno horizontal de los botones",
                                &mut f.button_padding_x,
                                0.0..=40.0,
                                " px",
                            );
                            changed |= measure(
                                ui,
                                "Relleno vertical de los botones",
                                &mut f.button_padding_y,
                                0.0..=24.0,
                                " px",
                            );
                            changed |= measure(
                                ui,
                                "Alto mínimo de filas y botones",
                                &mut f.row_height,
                                10.0..=48.0,
                                " px",
                            );
                            changed |= measure(
                                ui,
                                "Redondeo de esquinas",
                                &mut f.rounding,
                                0.0..=16.0,
                                " px",
                            );
                            changed |= measure(
                                ui,
                                "Grosor de los bordes",
                                &mut f.border,
                                0.0..=4.0,
                                " px",
                            );
                            changed |= measure(
                                ui,
                                "Margen interior de las ventanas",
                                &mut f.window_margin,
                                0.0..=32.0,
                                " px",
                            );
                            changed |= measure(
                                ui,
                                "Margen interior de los menús",
                                &mut f.menu_margin,
                                0.0..=32.0,
                                " px",
                            );
                            changed |= measure(
                                ui,
                                "Sangría del árbol y las secciones",
                                &mut f.indent,
                                4.0..=48.0,
                                " px",
                            );
                            changed |= measure(
                                ui,
                                "Ancho de las barras de desplazamiento",
                                &mut f.scroll_bar,
                                2.0..=24.0,
                                " px",
                            );
                            changed |= measure(
                                ui,
                                "Margen interior del terminal",
                                &mut f.terminal_padding,
                                0.0..=48.0,
                                " px",
                            );
                        });
                });
                egui::CollapsingHeader::new("Fondo del terminal").show(ui, |ui| {
                    ui.weak(
                        "Un texto, un dibujo ASCII o una imagen detrás del texto del terminal. \
                         Cada equipo puede tener además el suyo propio.",
                    );
                    changed |= self.backdrop.show(ui, &mut editing.theme.backdrop, "theme");
                });
            });
        if changed {
            // Vista previa en vivo sobre toda la aplicación.
            editing.theme.apply(ctx);
        }

        ui.add_space(4.0);
        let mut save = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            save = ui.button("Guardar tema").clicked();
            cancel = ui.button("Descartar cambios").clicked();
        });
        if save {
            let theme = editing.theme.clone();
            let original = editing.original.clone();
            match store.save_user(theme.clone(), original.as_deref()) {
                Ok(()) => {
                    let name = theme.name.trim().to_string();
                    self.editing = Some(Editing {
                        theme,
                        original: Some(name.clone()),
                    });
                    self.activate(ctx, store, settings, &name);
                    self.ok(format!("Tema «{name}» guardado"));
                }
                Err(e) => self.fail(format!("{e:#}")),
            }
        } else if cancel {
            self.editing = None;
            if let Some(active) = store.get(&settings.theme) {
                active.apply(ctx);
            }
        }
    }
}

/// Muestra de cómo queda el tema activo.
fn preview(ui: &mut egui::Ui) {
    ui.strong("Vista previa");
    ui.horizontal(|ui| {
        let _ = ui.button("Botón");
        let _ = ui.selectable_label(true, "Seleccionado");
        ui.label("Texto");
        ui.weak("tenue");
        ui.hyperlink_to("enlace", "https://example.org");
    });
    ui.horizontal(|ui| {
        ui.colored_label(theme::ok_color(), "Correcto");
        ui.colored_label(theme::warn_color(), "Aviso");
        ui.colored_label(ui.visuals().error_fg_color, "Error");
    });
    // Muestra del terminal con la paleta ANSI.
    let palette = theme::terminal_palette();
    let font = egui::FontId::monospace(palette.font_size);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(410.0, 74.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, palette.background);
    painter.text(
        rect.min + egui::vec2(8.0, 6.0),
        egui::Align2::LEFT_TOP,
        "usuario@equipo:~$ ls --color",
        font.clone(),
        palette.foreground,
    );
    let cell = 24.0;
    for (i, color) in palette.ansi.iter().enumerate() {
        let x = rect.min.x + 8.0 + (i % 8) as f32 * (cell + 2.0);
        let y = rect.min.y + 30.0 + (i / 8) as f32 * 20.0;
        painter.rect_filled(
            egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(cell, 16.0)),
            2.0,
            *color,
        );
    }
    painter.rect_filled(
        egui::Rect::from_min_size(rect.min + egui::vec2(240.0, 30.0), egui::vec2(150.0, 16.0)),
        0.0,
        palette.selection_bg,
    );
    painter.text(
        rect.min + egui::vec2(244.0, 30.0),
        egui::Align2::LEFT_TOP,
        "selección",
        font,
        palette.selection_fg,
    );
}
