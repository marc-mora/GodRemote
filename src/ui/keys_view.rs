//! Ventana de gestión de claves SSH.

use std::path::PathBuf;

use egui::RichText;

use crate::keys::{self, KeyAlgorithm, KeyInfo};

pub struct KeysWindow {
    keys: Vec<KeyInfo>,
    gen_name: String,
    gen_algorithm: KeyAlgorithm,
    gen_comment: String,
    gen_pass: String,
    gen_pass2: String,
    import_path: String,
    message: Option<(String, bool)>,
    confirm_delete: Option<PathBuf>,
}

impl Default for KeysWindow {
    fn default() -> Self {
        Self {
            keys: keys::list(),
            gen_name: "id_ed25519_godremote".into(),
            gen_algorithm: KeyAlgorithm::Ed25519,
            gen_comment: String::new(),
            gen_pass: String::new(),
            gen_pass2: String::new(),
            import_path: String::new(),
            message: None,
            confirm_delete: None,
        }
    }
}

impl KeysWindow {
    fn refresh(&mut self) {
        self.keys = keys::list();
    }

    /// Devuelve `false` cuando el usuario cierra la ventana.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let mut open = true;
        egui::Window::new("Claves SSH")
            .id(egui::Id::unique("keys_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos([290.0, 70.0])
            .show(ctx, |ui| {
                // Ancho fijo: el contenido no debe ensanchar la ventana.
                ui.set_width(700.0);
                self.contents(ui);
            });
        open
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        let own_dir = crate::paths::keys_dir();
        ui.horizontal(|ui| {
            if ui.button("⟳ Actualizar").clicked() {
                self.refresh();
            }
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "Se buscan claves en ~/.ssh y en {}",
                        own_dir.display()
                    ))
                    .weak(),
                )
                .truncate(),
            )
            .on_hover_text(own_dir.display().to_string());
        });
        ui.separator();

        let mut delete_request = None;
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if self.keys.is_empty() {
                    ui.weak("No hay claves todavía. Genera una abajo.");
                }
                for key in &self.keys {
                    ui.horizontal(|ui| {
                        ui.label("🔑");
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.strong(
                                    key.path
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default(),
                                );
                                ui.label(&key.algorithm);
                                if key.encrypted {
                                    ui.label(RichText::new("🔒 con passphrase").weak());
                                }
                                if !key.comment.is_empty() {
                                    ui.weak(&key.comment);
                                }
                            });
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{}   {}",
                                        key.fingerprint,
                                        key.path.display()
                                    ))
                                    .monospace()
                                    .size(11.0)
                                    .weak(),
                                )
                                .truncate(),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // Solo se permiten borrar las claves de la carpeta propia.
                            if key.path.starts_with(&own_dir)
                                && ui
                                    .small_button("🗑")
                                    .on_hover_text("Eliminar clave")
                                    .clicked()
                            {
                                delete_request = Some(key.path.clone());
                            }
                            if ui
                                .add_enabled(
                                    !key.public_openssh.is_empty(),
                                    egui::Button::new("Copiar pública").small(),
                                )
                                .on_hover_text("Copia la clave pública para authorized_keys")
                                .clicked()
                            {
                                ui.ctx().copy_text(key.public_openssh.clone());
                                self.message =
                                    Some(("Clave pública copiada al portapapeles".into(), false));
                            }
                        });
                    });
                    ui.separator();
                }
            });
        if let Some(path) = delete_request {
            self.confirm_delete = Some(path);
        }

        ui.add_space(4.0);
        ui.strong("Generar un par de claves nuevo");
        egui::Grid::new("gen_key")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Nombre de fichero");
                ui.add(egui::TextEdit::singleline(&mut self.gen_name).desired_width(260.0));
                ui.end_row();
                ui.label("Algoritmo");
                egui::ComboBox::from_id_salt("gen_algorithm")
                    .selected_text(self.gen_algorithm.label())
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for a in KeyAlgorithm::ALL {
                            ui.selectable_value(&mut self.gen_algorithm, a, a.label());
                        }
                    });
                ui.end_row();
                ui.label("Comentario");
                ui.add(
                    egui::TextEdit::singleline(&mut self.gen_comment)
                        .hint_text("usuario@equipo (opcional)")
                        .desired_width(260.0),
                );
                ui.end_row();
                ui.label("Passphrase");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.gen_pass)
                            .password(true)
                            .hint_text("opcional")
                            .desired_width(125.0),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.gen_pass2)
                            .password(true)
                            .hint_text("repetir")
                            .desired_width(125.0),
                    );
                });
                ui.end_row();
            });
        if ui.button("Generar clave").clicked() {
            if self.gen_pass != self.gen_pass2 {
                self.message = Some(("Las passphrases no coinciden".into(), true));
            } else {
                let pass = (!self.gen_pass.is_empty()).then_some(self.gen_pass.as_str());
                match keys::generate(
                    &own_dir,
                    self.gen_name.trim(),
                    self.gen_algorithm,
                    self.gen_comment.trim(),
                    pass,
                ) {
                    Ok(info) => {
                        self.message =
                            Some((format!("Clave creada: {}", info.path.display()), false));
                        self.gen_pass.clear();
                        self.gen_pass2.clear();
                        self.refresh();
                    }
                    Err(e) => self.message = Some((format!("{e:#}"), true)),
                }
            }
        }

        ui.add_space(6.0);
        ui.strong("Importar una clave existente");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.import_path)
                    .hint_text("ruta a la clave privada (OpenSSH, PEM o .ppk)")
                    .desired_width(380.0),
            );
            if ui.button("Importar").clicked() {
                let source = crate::paths::expand_tilde(&self.import_path);
                match keys::import(&source, &own_dir) {
                    Ok(info) => {
                        self.message =
                            Some((format!("Clave importada: {}", info.path.display()), false));
                        self.import_path.clear();
                        self.refresh();
                    }
                    Err(e) => self.message = Some((format!("{e:#}"), true)),
                }
            }
        });

        if let Some((msg, is_error)) = &self.message {
            ui.add_space(4.0);
            let color = if *is_error {
                ui.visuals().error_fg_color
            } else {
                crate::theme::ok_color()
            };
            ui.colored_label(color, msg);
        }

        if let Some(path) = self.confirm_delete.clone() {
            let mut close = false;
            egui::Modal::new(egui::Id::unique("confirm_delete_key")).show(ui.ctx(), |ui| {
                ui.set_width(420.0);
                ui.heading("Eliminar clave");
                ui.label(format!(
                    "Se borrará definitivamente {} y su .pub. \
                     No podrás volver a entrar en los servidores que solo acepten esta clave.",
                    path.display()
                ));
                ui.horizontal(|ui| {
                    if ui
                        .button(RichText::new("Eliminar").color(ui.visuals().error_fg_color))
                        .clicked()
                    {
                        match keys::delete(&path) {
                            Ok(()) => self.message = Some(("Clave eliminada".into(), false)),
                            Err(e) => self.message = Some((format!("{e:#}"), true)),
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
                self.refresh();
            }
        }
    }
}
