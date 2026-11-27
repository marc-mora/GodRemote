//! Diálogo de creación / edición de sesiones.

use uuid::Uuid;

use crate::keys::KeyInfo;
use crate::model::{AuthMethod, Protocol, Session, Tree};
use crate::vault::Vault;

pub struct SessionEditor {
    pub session: Session,
    pub is_new: bool,
    /// Carpeta destino (`None` = raíz).
    pub parent: Option<Uuid>,
    original_parent: Option<Uuid>,
    password: String,
    passphrase: String,
    save_password: bool,
    forget_password: bool,
    forget_passphrase: bool,
    error: Option<String>,
    keys: Vec<KeyInfo>,
}

pub enum EditorResult {
    Open,
    Cancelled,
    /// Guardada; `true` si además hay que conectar.
    Saved {
        connect: bool,
    },
}

impl SessionEditor {
    pub fn new(parent: Option<Uuid>) -> Self {
        Self::build(Session::default(), true, parent)
    }

    pub fn edit(session: Session, parent: Option<Uuid>) -> Self {
        Self::build(session, false, parent)
    }

    fn build(session: Session, is_new: bool, parent: Option<Uuid>) -> Self {
        Self {
            session,
            is_new,
            parent,
            original_parent: parent,
            password: String::new(),
            passphrase: String::new(),
            save_password: true,
            forget_password: false,
            forget_passphrase: false,
            error: None,
            keys: crate::keys::list(),
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        tree: &mut Tree,
        vault: &mut Option<Vault>,
    ) -> EditorResult {
        let mut result = EditorResult::Open;
        let mut open = true;
        let title = if self.is_new {
            "Nueva sesión"
        } else {
            "Editar sesión"
        };
        egui::Window::new(title)
            .id(egui::Id::unique("session_editor"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                self.form(ui, tree, vault.as_ref());
                ui.add_space(6.0);
                if let Some(err) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let mut save = None;
                    if ui.button("Guardar").clicked() {
                        save = Some(false);
                    }
                    if ui.button("Guardar y conectar").clicked() {
                        save = Some(true);
                    }
                    if ui.button("Cancelar").clicked() {
                        result = EditorResult::Cancelled;
                    }
                    if let Some(connect) = save {
                        match self.commit(tree, vault) {
                            Ok(()) => result = EditorResult::Saved { connect },
                            Err(e) => self.error = Some(e),
                        }
                    }
                });
            });
        if !open {
            result = EditorResult::Cancelled;
        }
        result
    }

    fn form(&mut self, ui: &mut egui::Ui, tree: &Tree, vault: Option<&Vault>) {
        let s = &mut self.session;
        egui::Grid::new("session_form")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Nombre");
                ui.add(
                    egui::TextEdit::singleline(&mut s.name)
                        .hint_text("p. ej. web-01 (opcional)")
                        .desired_width(300.0),
                );
                ui.end_row();

                ui.label("Protocolo");
                ui.horizontal(|ui| {
                    for p in Protocol::ALL {
                        if ui.selectable_label(s.protocol == p, p.label()).clicked()
                            && s.protocol != p
                        {
                            // Si el puerto era el predeterminado, se adapta.
                            if s.port == s.protocol.default_port() {
                                s.port = p.default_port();
                            }
                            s.protocol = p;
                        }
                    }
                });
                ui.end_row();

                ui.label("Host");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut s.host)
                            .hint_text("nombre o IP")
                            .desired_width(210.0),
                    );
                    ui.label("Puerto");
                    ui.add(egui::DragValue::new(&mut s.port).range(1..=65535).speed(1));
                });
                ui.end_row();

                ui.label("Usuario");
                ui.add(egui::TextEdit::singleline(&mut s.username).desired_width(300.0));
                ui.end_row();

                if s.protocol == Protocol::Rdp {
                    ui.label("Dominio");
                    ui.add(
                        egui::TextEdit::singleline(&mut s.rdp.domain)
                            .hint_text("opcional")
                            .desired_width(300.0),
                    );
                    ui.end_row();
                } else {
                    ui.label("Autenticación");
                    ui.horizontal(|ui| {
                        for m in AuthMethod::ALL {
                            ui.selectable_value(&mut s.auth, m, m.label());
                        }
                    });
                    ui.end_row();
                }

                let uses_password = s.protocol == Protocol::Rdp || s.auth == AuthMethod::Password;
                if uses_password {
                    ui.label("Contraseña");
                    ui.vertical(|ui| {
                        let stored = vault.is_some_and(|v| v.contains(&s.password_key()));
                        let hint = if stored && !self.forget_password {
                            "guardada en el baúl (escribe para cambiarla)"
                        } else {
                            "se pedirá al conectar si se deja vacía"
                        };
                        ui.add(
                            egui::TextEdit::singleline(&mut self.password)
                                .password(true)
                                .hint_text(hint)
                                .desired_width(300.0),
                        );
                        if vault.is_some() {
                            ui.checkbox(&mut self.save_password, "Guardar en el baúl cifrado");
                            if stored {
                                ui.checkbox(
                                    &mut self.forget_password,
                                    "Olvidar la contraseña guardada",
                                );
                            }
                        } else {
                            ui.weak("Baúl bloqueado: la contraseña no se guardará.");
                        }
                    });
                    ui.end_row();
                } else if s.auth == AuthMethod::Key {
                    ui.label("Clave privada");
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut s.key_path)
                                    .hint_text("~/.ssh/id_ed25519")
                                    .desired_width(270.0),
                            );
                            egui::ComboBox::from_id_salt("key_picker")
                                .selected_text("…")
                                .width(24.0)
                                .show_ui(ui, |ui| {
                                    if self.keys.is_empty() {
                                        ui.weak("No se han encontrado claves");
                                    }
                                    for k in &self.keys {
                                        let label = format!(
                                            "{}  ({}{})",
                                            k.path.display(),
                                            k.algorithm,
                                            if k.encrypted { ", cifrada" } else { "" }
                                        );
                                        if ui.selectable_label(false, label).clicked() {
                                            s.key_path = k.path.display().to_string();
                                        }
                                    }
                                });
                        });
                        let stored = vault.is_some_and(|v| v.contains(&s.passphrase_key()));
                        let hint = if stored && !self.forget_passphrase {
                            "passphrase guardada en el baúl"
                        } else {
                            "passphrase (si la clave está cifrada)"
                        };
                        ui.add(
                            egui::TextEdit::singleline(&mut self.passphrase)
                                .password(true)
                                .hint_text(hint)
                                .desired_width(300.0),
                        );
                        if vault.is_some() {
                            if stored {
                                ui.checkbox(
                                    &mut self.forget_passphrase,
                                    "Olvidar la passphrase guardada",
                                );
                            }
                        } else {
                            ui.weak("Baúl bloqueado: la passphrase no se guardará.");
                        }
                    });
                    ui.end_row();
                } else {
                    ui.label("");
                    ui.weak("Se usarán las claves cargadas en ssh-agent / Pageant.");
                    ui.end_row();
                }

                if s.protocol == Protocol::Rdp {
                    ui.label("Pantalla");
                    ui.vertical(|ui| {
                        ui.checkbox(&mut s.rdp.fullscreen, "Pantalla completa");
                        ui.add_enabled_ui(!s.rdp.fullscreen, |ui| {
                            ui.horizontal(|ui| {
                                ui.add(egui::DragValue::new(&mut s.rdp.width).range(640..=7680));
                                ui.label("×");
                                ui.add(egui::DragValue::new(&mut s.rdp.height).range(480..=4320));
                            });
                        });
                        ui.checkbox(&mut s.rdp.clipboard, "Compartir portapapeles");
                    });
                    ui.end_row();
                }

                ui.label("Carpeta");
                let folders = tree.folder_paths();
                let current = self
                    .parent
                    .and_then(|id| folders.iter().find(|(fid, _)| *fid == id))
                    .map(|(_, path)| path.clone())
                    .unwrap_or_else(|| "(raíz)".into());
                egui::ComboBox::from_id_salt("folder_picker")
                    .selected_text(current)
                    .width(300.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.parent, None, "(raíz)");
                        for (id, path) in &folders {
                            ui.selectable_value(&mut self.parent, Some(*id), path);
                        }
                    });
                ui.end_row();

                ui.label("Notas");
                ui.add(
                    egui::TextEdit::multiline(&mut s.notes)
                        .desired_rows(2)
                        .desired_width(300.0),
                );
                ui.end_row();
            });
    }

    /// Valida y guarda la sesión (y sus secretos en el baúl).
    fn commit(&mut self, tree: &mut Tree, vault: &mut Option<Vault>) -> Result<(), String> {
        let mut s = self.session.clone();
        s.name = s.name.trim().to_string();
        s.host = s.host.trim().to_string();
        s.username = s.username.trim().to_string();
        s.key_path = s.key_path.trim().to_string();
        s.validate()?;

        if let Some(vault) = vault {
            let uses_password = s.protocol == Protocol::Rdp || s.auth == AuthMethod::Password;
            let res = (|| -> anyhow::Result<()> {
                if uses_password && !self.password.is_empty() && self.save_password {
                    vault.set(&s.password_key(), &self.password)?;
                } else if self.forget_password || !uses_password {
                    vault.remove(&s.password_key())?;
                }
                let uses_key = s.protocol != Protocol::Rdp && s.auth == AuthMethod::Key;
                if uses_key && !self.passphrase.is_empty() {
                    vault.set(&s.passphrase_key(), &self.passphrase)?;
                } else if self.forget_passphrase || !uses_key {
                    vault.remove(&s.passphrase_key())?;
                }
                Ok(())
            })();
            res.map_err(|e| format!("No se pudo actualizar el baúl: {e:#}"))?;
        }

        let id = s.id;
        tree.upsert_session(self.parent, s.clone());
        if !self.is_new && self.parent != self.original_parent {
            tree.move_node(id, self.parent);
        }
        self.session = s;
        Ok(())
    }
}
