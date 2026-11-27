//! Ventana para importar equipos de otras herramientas y para guardar o
//! restaurar una copia de seguridad de toda la configuración.

use std::path::PathBuf;

use crate::backup::{self, Backup};
use crate::import::{self, Source};
use crate::model::Folder;
use crate::theme;

/// Lo que la ventana pide a la aplicación.
pub enum TransferRequest {
    None,
    /// Colgar del árbol esta carpeta con los equipos importados.
    Imported(Box<Folder>),
    /// La configuración se ha restaurado: hay que volver a cargarla.
    Restored,
}

pub struct TransferWindow {
    source: Source,
    /// Ruta escrita a mano (vacía = elegir con el explorador de archivos).
    import_path: String,
    backup_path: String,
    include_keys: bool,
    message: Option<(String, bool)>,
    warnings: Vec<String>,
    /// Copia leída, a la espera de confirmar que se restaura.
    confirm: Option<(Backup, PathBuf)>,
}

impl Default for TransferWindow {
    fn default() -> Self {
        Self {
            source: Source::OpenSsh,
            import_path: String::new(),
            backup_path: String::new(),
            include_keys: true,
            message: None,
            warnings: vec![],
            confirm: None,
        }
    }
}

impl TransferWindow {
    fn ok(&mut self, message: impl Into<String>) {
        self.message = Some((message.into(), false));
    }

    fn fail(&mut self, message: impl Into<String>) {
        self.message = Some((message.into(), true));
    }

    /// Fichero del que importar: la ruta escrita o, si no hay, un diálogo.
    fn pick_import(&self) -> Option<PathBuf> {
        if !self.import_path.trim().is_empty() {
            return Some(crate::paths::expand_tilde(&self.import_path));
        }
        if self.source == Source::OpenSsh {
            // Lo habitual: el fichero del propio usuario.
            let config = crate::paths::home_dir().join(".ssh").join("config");
            if config.is_file() {
                return Some(config);
            }
        }
        let mut dialog = rfd::FileDialog::new().set_title("Importar equipos");
        if !self.source.extensions().is_empty() {
            dialog = dialog.add_filter(self.source.label(), self.source.extensions());
        }
        dialog.pick_file()
    }

    fn finish_import(&mut self, result: anyhow::Result<import::Imported>) -> TransferRequest {
        match result {
            Ok(imported) => {
                let count = imported.count();
                self.warnings = imported.warnings.clone();
                let folder = imported.into_folder(self.source.folder_name());
                self.ok(format!(
                    "{count} equipo(s) importados en la carpeta «{}»",
                    folder.name
                ));
                TransferRequest::Imported(Box::new(folder))
            }
            Err(e) => {
                self.warnings.clear();
                self.fail(format!("{e:#}"));
                TransferRequest::None
            }
        }
    }

    /// Devuelve si la ventana sigue abierta y lo que haya que hacer.
    pub fn show(&mut self, ctx: &egui::Context) -> (bool, TransferRequest) {
        let mut open = true;
        let mut request = TransferRequest::None;
        egui::Window::new("Importar y copias de seguridad")
            .id(egui::Id::unique("transfer_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos([300.0, 70.0])
            .show(ctx, |ui| {
                ui.set_width(540.0);
                ui.strong("Importar equipos de otra herramienta");
                for source in Source::ALL {
                    ui.radio_value(&mut self.source, source, source.label());
                }
                ui.horizontal(|ui| {
                    let hint = match self.source {
                        Source::OpenSsh => "vacío = ~/.ssh/config",
                        _ => "ruta del fichero (vacío = elegirlo)",
                    };
                    ui.add(
                        egui::TextEdit::singleline(&mut self.import_path)
                            .hint_text(hint)
                            .desired_width(330.0),
                    );
                    if ui.button("Importar").clicked()
                        && let Some(path) = self.pick_import()
                    {
                        request = self.finish_import(import::import_file(self.source, &path));
                    }
                    if cfg!(windows)
                        && self.source == Source::Putty
                        && ui
                            .button("Las de este equipo")
                            .on_hover_text("Lee las sesiones de PuTTY guardadas en este Windows")
                            .clicked()
                    {
                        request = self.finish_import(import::putty_from_registry());
                    }
                });
                ui.weak(match self.source {
                    Source::OpenSsh => {
                        "Se crea un equipo por cada «Host» con su HostName, User, Port, \
                         IdentityFile y ProxyJump (como saltos)."
                    }
                    Source::Putty => {
                        "En PuTTY las sesiones están en el registro de Windows: expórtalas con \
                         «reg export HKCU\\Software\\SimonTatham\\PuTTY\\Sessions putty.reg»."
                    }
                    Source::MobaXterm => {
                        "En MobaXterm: clic derecho sobre «User sessions» → «Export all \
                         sessions to file». Se importan SSH, SFTP, Telnet, RDP y VNC, con sus \
                         carpetas."
                    }
                });
                ui.weak("Las contraseñas no se importan: se piden al conectar.");

                ui.separator();
                ui.strong("Copia de seguridad");
                ui.weak(
                    "Un solo fichero con los equipos, el baúl (cifrado, tal cual), los ajustes, \
                     los snippets y los temas.",
                );
                ui.checkbox(
                    &mut self.include_keys,
                    "Incluir las claves privadas guardadas en la carpeta de GodRemote",
                )
                .on_hover_text(
                    "Las que generaste o importaste desde «Claves». Las de ~/.ssh no se copian",
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.backup_path)
                        .hint_text("ruta del fichero (vacío = elegirlo)")
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal(|ui| {
                    if ui.button("⬇ Guardar copia…").clicked() {
                        self.save_backup();
                    }
                    if ui.button("⬆ Restaurar copia…").clicked() {
                        self.load_backup();
                    }
                });

                if let Some((message, is_error)) = &self.message {
                    ui.separator();
                    let color = if *is_error {
                        theme::error_color()
                    } else {
                        theme::ok_color()
                    };
                    ui.colored_label(color, message);
                }
                if !self.warnings.is_empty() {
                    egui::ScrollArea::vertical()
                        .id_salt("transfer_warnings")
                        .max_height(120.0)
                        .show(ui, |ui| {
                            for warning in &self.warnings {
                                ui.colored_label(theme::warn_color(), format!("• {warning}"));
                            }
                        });
                }
            });

        if let Some((backup, path)) = &self.confirm {
            let mut restore = false;
            let mut cancel = false;
            egui::Modal::new(egui::Id::unique("confirm_restore")).show(ctx, |ui| {
                ui.set_width(460.0);
                ui.heading("Restaurar copia de seguridad");
                ui.label(backup.summary());
                ui.label(
                    "Se sustituirá la configuración actual por la de la copia. Antes se guarda \
                     la actual en la carpeta de GodRemote, por si hay que volver atrás. Las \
                     pestañas abiertas siguen como están.",
                );
                if backup.has_keys() {
                    ui.weak("La copia incluye claves privadas.");
                }
                ui.weak(path.display().to_string());
                ui.horizontal(|ui| {
                    if ui.button("Restaurar").clicked() {
                        restore = true;
                    }
                    if ui.button("Cancelar").clicked() {
                        cancel = true;
                    }
                });
            });
            if restore && let Some((backup, _)) = self.confirm.take() {
                match restore_with_safety_copy(&backup) {
                    Ok((written, safety)) => {
                        self.ok(format!(
                            "Copia restaurada ({written} ficheros). La configuración anterior \
                             está en {}",
                            safety.display()
                        ));
                        request = TransferRequest::Restored;
                    }
                    Err(e) => self.fail(format!("{e:#}")),
                }
            } else if cancel {
                self.confirm = None;
            }
        }
        (open, request)
    }

    fn save_backup(&mut self) {
        let path = if self.backup_path.trim().is_empty() {
            rfd::FileDialog::new()
                .set_title("Guardar copia de seguridad")
                .set_directory(theme::default_export_dir())
                .set_file_name(backup::suggested_name())
                .add_filter("Copia de GodRemote (JSON)", &["json"])
                .save_file()
        } else {
            let mut path = crate::paths::expand_tilde(&self.backup_path);
            if path.is_dir() {
                path = path.join(backup::suggested_name());
            }
            Some(path)
        };
        let Some(path) = path else {
            return;
        };
        let result = Backup::create(&crate::paths::data_dir(), self.include_keys)
            .and_then(|backup| backup.write(&path).map(|()| backup));
        self.warnings.clear();
        match result {
            Ok(backup) => self.ok(format!(
                "{} — guardada en {}",
                backup.summary(),
                path.display()
            )),
            Err(e) => self.fail(format!("{e:#}")),
        }
    }

    fn load_backup(&mut self) {
        let path = if self.backup_path.trim().is_empty() {
            rfd::FileDialog::new()
                .set_title("Restaurar copia de seguridad")
                .add_filter("Copia de GodRemote (JSON)", &["json"])
                .pick_file()
        } else {
            Some(crate::paths::expand_tilde(&self.backup_path))
        };
        let Some(path) = path else {
            return;
        };
        self.warnings.clear();
        match Backup::read(&path) {
            Ok(backup) => {
                self.message = None;
                self.confirm = Some((backup, path));
            }
            Err(e) => self.fail(format!("{e:#}")),
        }
    }
}

/// Guarda la configuración actual en la carpeta de datos y, después,
/// restaura la copia. Devuelve cuántos ficheros se escribieron y dónde
/// quedó la configuración anterior.
fn restore_with_safety_copy(backup: &Backup) -> anyhow::Result<(usize, PathBuf)> {
    let data_dir = crate::paths::data_dir();
    let safety = data_dir.join(format!(
        "antes-de-restaurar-{}.json",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    // Si aún no hay nada que guardar, no hay copia previa que hacer.
    if let Ok(current) = Backup::create(&data_dir, true) {
        current.write(&safety)?;
    }
    let written = backup.restore(&data_dir)?;
    Ok((written, safety))
}
