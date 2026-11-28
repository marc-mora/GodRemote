//! Visor del baúl: qué contraseñas hay guardadas y de quién, con la opción
//! de verlas (tras repetir la contraseña maestra) y de borrarlas.

use std::collections::BTreeSet;

use uuid::Uuid;

use crate::i18n::tr;
use crate::model::{Node, Tree};
use crate::theme::{error_color, ok_color, warn_color};
use crate::vault::Vault;
use crate::{mark, trf};

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
    /// Qué es, sin traducir (también ordena la lista): se traduce al pintar.
    kind: &'static str,
}

fn describe(key: &str, tree: &Tree, paths: &[(Uuid, String)]) -> Entry {
    let (owner_id, what) = key.split_once('/').unwrap_or((key, ""));
    let kind = match what {
        "password" => mark!("Contraseña"),
        "passphrase" => mark!("Passphrase de la clave"),
        "totp" => mark!("Secreto TOTP"),
        _ => mark!("Secreto"),
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
            Some(Node::Folder(folder)) => (trf!("Carpeta «{name}»", name = folder.name), false),
            None => (tr("(equipo o carpeta eliminados)").to_string(), true),
        },
        // Secretos que no son de un equipo (el proxy de salida…).
        None => (
            match owner_id {
                "proxy" => tr("Proxy de salida").to_string(),
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
        egui::Window::new(tr("Baúl: lo guardado"))
            .id(egui::Id::unique("vault_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos([280.0, 80.0])
            .show(ctx, |ui| {
                ui.set_width(640.0);
                ui.horizontal(|ui| {
                    ui.strong(match entries.len() {
                        0 => tr("El baúl está vacío").to_string(),
                        1 => tr("1 secreto guardado").to_string(),
                        n => trf!("{n} secretos guardados", n),
                    });
                    if self.verified {
                        ui.colored_label(ok_color(), tr("Se pueden ver y copiar"));
                    } else if !entries.is_empty()
                        && !self.asking
                        && ui
                            .button(tr("Ver o copiar las contraseñas…"))
                            .on_hover_text(tr("Hay que repetir la contraseña maestra"))
                            .clicked()
                    {
                        self.asking = true;
                        self.focused = false;
                    }
                });
                if self.asking {
                    ui.horizontal(|ui| {
                        ui.label(tr("Contraseña maestra"));
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
                        if ui.button(tr("Comprobar")).clicked() || enter {
                            // La misma comprobación que al desbloquear.
                            let ok =
                                Vault::unlock(&crate::paths::vault_file(), &self.master).is_ok();
                            self.master.clear();
                            self.asking = false;
                            self.verified = ok;
                            self.message = (!ok)
                                .then(|| (tr("La contraseña maestra no es correcta").into(), true));
                        }
                        if ui.button(tr("Cancelar")).clicked() {
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
                                    ui.weak(tr(entry.kind));
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
                                            let label =
                                                if shown { tr("Ocultar") } else { tr("Mostrar") };
                                            if ui.small_button(label).clicked() {
                                                if shown {
                                                    self.revealed.remove(&entry.key);
                                                } else {
                                                    self.revealed.insert(entry.key.clone());
                                                }
                                            }
                                            if ui.small_button(tr("Copiar")).clicked()
                                                && let Some(value) = vault.get(&entry.key)
                                            {
                                                ui.ctx().copy_text(value.to_string());
                                                let message =
                                                    tr("Copiado al portapapeles: bórralo de él \
                                                     cuando termines");
                                                self.message = Some((message.into(), false));
                                            }
                                        });
                                        if self.deleting.as_deref() == Some(&entry.key) {
                                            if ui.small_button(tr("Sí, borrar")).clicked() {
                                                remove.push(entry.key.clone());
                                                self.deleting = None;
                                            }
                                            if ui.small_button(tr("No")).clicked() {
                                                self.deleting = None;
                                            }
                                        } else if ui
                                            .small_button("🗑")
                                            .on_hover_text(tr(
                                                "Borrarlo del baúl: se volverá a pedir al conectar",
                                            ))
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
                            trf!(
                                "{orphans} de equipos o carpetas que ya no existen (por ejemplo, \
                                 de otra configuración)",
                                orphans
                            ),
                        );
                        if ui.button(tr("Borrarlos")).clicked() {
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
                Ok(()) => self.message = Some((tr("Borrado del baúl").into(), false)),
                Err(e) => {
                    let message = trf!("No se pudo borrar: {error}", error = format!("{e:#}"));
                    self.message = Some((message, true));
                }
            }
        }
        open
    }
}
