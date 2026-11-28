//! Visor del baúl: qué contraseñas hay guardadas y de quién, con la opción
//! de verlas (tras repetir la contraseña maestra) y de borrarlas.

use std::collections::BTreeSet;

use uuid::Uuid;

use crate::model::{Node, Tree};
use crate::theme::{error_color, ok_color, warn_color};
use crate::vault::Vault;

#[derive(Default)]
pub struct VaultWindow {
    /// Se ha repetido la contraseña maestra: se pueden ver los valores.
    verified: bool,
    /// Contraseña maestra que se está escribiendo para verlos.
    master: String,
    asking: bool,
    /// El campo de la contraseña ya tiene el foco.
    focused: bool,
    /// Entradas cuyo valor está a la vista.
    revealed: BTreeSet<String>,
    /// Entrada que se ha pedido borrar y falta confirmar.
    deleting: Option<String>,
    message: Option<(String, bool)>,
}

/// A quién pertenece una entrada del baúl y qué guarda.
struct Entry {
    key: String,
    owner: String,
    /// El equipo o la carpeta ya no existe.
    orphan: bool,
    kind: &'static str,
}

fn describe(key: &str, tree: &Tree, paths: &[(Uuid, String)]) -> Entry {
    let (owner_id, what) = key.split_once('/').unwrap_or((key, ""));
    let kind = match what {
        "password" => "Contraseña",
        "passphrase" => "Passphrase de la clave",
        "totp" => "Secreto TOTP",
        _ => "Secreto",
    };
    let (owner, orphan) = match Uuid::parse_str(owner_id).ok() {
        Some(id) => match tree.find(id) {
            Some(Node::Session(_)) => (
                paths
                    .iter()
                    .find(|(pid, _)| *pid == id)
                    .map(|(_, path)| path.clone())
                    .unwrap_or_else(|| id.to_string()),
                false,
            ),
            Some(Node::Folder(folder)) => (format!("Carpeta «{}»", folder.name), false),
            None => ("(equipo o carpeta eliminados)".to_string(), true),
        },
        // Secretos que no son de un equipo (el proxy de salida…).
        None => (
            match owner_id {
                "proxy" => "Proxy de salida".to_string(),
                other => other.to_string(),
            },
            false,
        ),
    };
    Entry {
        key: key.to_string(),
        owner,
        orphan,
        kind,
    }
}

impl VaultWindow {
    /// Devuelve si la ventana sigue abierta.
    pub fn show(&mut self, ctx: &egui::Context, vault: &mut Vault, tree: &Tree) -> bool {
        let mut open = true;
        let paths = tree.session_paths();
        let mut entries: Vec<Entry> = vault
            .keys()
            .map(|key| describe(key, tree, &paths))
            .collect();
        entries.sort_by(|a, b| (a.orphan, &a.owner, a.kind).cmp(&(b.orphan, &b.owner, b.kind)));
        let orphans = entries.iter().filter(|e| e.orphan).count();
        let mut remove: Vec<String> = vec![];
        egui::Window::new("Baúl: lo guardado")
            .id(egui::Id::unique("vault_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos([280.0, 80.0])
            .show(ctx, |ui| {
                ui.set_width(640.0);
                ui.horizontal(|ui| {
                    ui.strong(match entries.len() {
                        0 => "El baúl está vacío".to_string(),
                        1 => "1 secreto guardado".to_string(),
                        n => format!("{n} secretos guardados"),
                    });
                    if self.verified {
                        ui.colored_label(ok_color(), "Se pueden ver y copiar");
                    } else if !entries.is_empty()
                        && !self.asking
                        && ui
                            .button("Ver o copiar las contraseñas…")
                            .on_hover_text("Hay que repetir la contraseña maestra")
                            .clicked()
                    {
                        self.asking = true;
                        self.focused = false;
                    }
                });
                if self.asking {
                    ui.horizontal(|ui| {
                        ui.label("Contraseña maestra");
                        let field = ui.add(
                            egui::TextEdit::singleline(&mut self.master)
                                .password(true)
                                .desired_width(220.0),
                        );
                        if !self.focused {
                            field.request_focus();
                            self.focused = true;
                        }
                        let enter =
                            field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("Comprobar").clicked() || enter {
                            // La misma comprobación que al desbloquear.
                            let ok =
                                Vault::unlock(&crate::paths::vault_file(), &self.master).is_ok();
                            self.master.clear();
                            self.asking = false;
                            self.verified = ok;
                            self.message = (!ok)
                                .then(|| ("La contraseña maestra no es correcta".into(), true));
                        }
                        if ui.button("Cancelar").clicked() {
                            self.master.clear();
                            self.asking = false;
                        }
                    });
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(380.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        egui::Grid::new("vault_entries")
                            .num_columns(4)
                            .striped(true)
                            .spacing([12.0, 6.0])
                            .show(ui, |ui| {
                                for entry in &entries {
                                    if entry.orphan {
                                        ui.colored_label(warn_color(), &entry.owner);
                                    } else {
                                        ui.label(&entry.owner);
                                    }
                                    ui.weak(entry.kind);
                                    let shown = self.revealed.contains(&entry.key);
                                    match (shown, vault.get(&entry.key)) {
                                        (true, Some(value)) => {
                                            ui.add(egui::Label::new(
                                                egui::RichText::new(value).monospace(),
                                            ));
                                        }
                                        _ => {
                                            ui.weak("••••••••");
                                        }
                                    }
                                    ui.horizontal(|ui| {
                                        ui.add_enabled_ui(self.verified, |ui| {
                                            let label = if shown { "Ocultar" } else { "Mostrar" };
                                            if ui.small_button(label).clicked() {
                                                if shown {
                                                    self.revealed.remove(&entry.key);
                                                } else {
                                                    self.revealed.insert(entry.key.clone());
                                                }
                                            }
                                            if ui.small_button("Copiar").clicked()
                                                && let Some(value) = vault.get(&entry.key)
                                            {
                                                ui.ctx().copy_text(value.to_string());
                                                self.message = Some((
                                                    "Copiado al portapapeles: bórralo de él \
                                                     cuando termines"
                                                        .into(),
                                                    false,
                                                ));
                                            }
                                        });
                                        if self.deleting.as_deref() == Some(&entry.key) {
                                            if ui.small_button("Sí, borrar").clicked() {
                                                remove.push(entry.key.clone());
                                                self.deleting = None;
                                            }
                                            if ui.small_button("No").clicked() {
                                                self.deleting = None;
                                            }
                                        } else if ui
                                            .small_button("🗑")
                                            .on_hover_text(
                                                "Borrarlo del baúl: se volverá a pedir al conectar",
                                            )
                                            .clicked()
                                        {
                                            self.deleting = Some(entry.key.clone());
                                        }
                                    });
                                    ui.end_row();
                                }
                            });
                    });
                if orphans > 0 {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            warn_color(),
                            format!(
                                "{orphans} de equipos o carpetas que ya no existen (por ejemplo, \
                                 de otra configuración)"
                            ),
                        );
                        if ui.button("Borrarlos").clicked() {
                            remove
                                .extend(entries.iter().filter(|e| e.orphan).map(|e| e.key.clone()));
                        }
                    });
                }
                if let Some((message, is_error)) = &self.message {
                    let color = if *is_error { error_color() } else { ok_color() };
                    ui.colored_label(color, message);
                }
            });
        for key in remove {
            self.revealed.remove(&key);
            match vault.remove(&key) {
                Ok(()) => self.message = Some(("Borrado del baúl".into(), false)),
                Err(e) => self.message = Some((format!("No se pudo borrar: {e:#}"), true)),
            }
        }
        open
    }
}
