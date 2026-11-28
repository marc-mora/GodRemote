//! Ventana de gestión de claves SSH.

use std::path::PathBuf;

use egui::RichText;

use crate::agent::AgentListing;
use crate::i18n::tr;
use crate::keys::{self, CertInfo, KeyAlgorithm, KeyInfo};
use crate::theme::warn_color;
use crate::trf;

/// Lo que se sabe de las claves del agente SSH.
enum AgentState {
    /// Hay que preguntarle.
    Unknown,
    /// Se le está preguntando en otro hilo.
    Asking(std::sync::mpsc::Receiver<Result<AgentListing, String>>),
    Listed(AgentListing),
    Missing(String),
}

pub struct KeysWindow {
    keys: Vec<KeyInfo>,
    agent: AgentState,
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
            agent: AgentState::Unknown,
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
        self.agent = AgentState::Unknown;
    }

    /// Pregunta al agente SSH por sus claves sin parar la interfaz: la
    /// consulta va en un hilo aparte y avisa al terminar.
    fn poll_agent(&mut self, ctx: &egui::Context) {
        if matches!(self.agent, AgentState::Unknown) {
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let listing = crate::agent::list_blocking().map_err(|e| format!("{e:#}"));
                let _ = tx.send(listing);
                ctx.request_repaint();
            });
            self.agent = AgentState::Asking(rx);
        }
        if let AgentState::Asking(rx) = &self.agent {
            match rx.try_recv() {
                Ok(Ok(listing)) => self.agent = AgentState::Listed(listing),
                Ok(Err(e)) => self.agent = AgentState::Missing(e),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.agent =
                        AgentState::Missing(tr("la consulta al agente se interrumpió").into())
                }
            }
        }
    }

    /// Claves cargadas en el agente SSH (el de OpenSSH o Pageant).
    fn agent_section(&mut self, ui: &mut egui::Ui) {
        self.poll_agent(ui.ctx());
        let mut copied = false;
        ui.horizontal(|ui| {
            ui.strong(tr("En el agente SSH"));
            match &self.agent {
                AgentState::Unknown | AgentState::Asking(_) => {
                    ui.spinner();
                    ui.weak(tr("consultando…"));
                }
                AgentState::Listed(listing) => {
                    ui.weak(format!("({})", tr(listing.kind.label())));
                }
                AgentState::Missing(_) => {}
            }
        });
        match &self.agent {
            AgentState::Unknown | AgentState::Asking(_) => {}
            AgentState::Missing(why) => {
                ui.add(
                    egui::Label::new(RichText::new(trf!("No disponible: {why}", why)).weak())
                        .wrap(),
                );
            }
            AgentState::Listed(listing) if listing.keys.is_empty() => {
                ui.weak(tr("No tiene ninguna clave cargada."));
            }
            AgentState::Listed(listing) => {
                egui::ScrollArea::vertical()
                    .id_salt("agent_keys")
                    .max_height(120.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for key in &listing.keys {
                            ui.horizontal(|ui| {
                                ui.label("🔑");
                                ui.vertical(|ui| {
                                    ui.set_max_width((ui.available_width() - 120.0).max(120.0));
                                    ui.horizontal(|ui| {
                                        if key.comment.is_empty() {
                                            ui.strong(tr("(sin nombre)"));
                                        } else {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&key.comment).strong(),
                                                )
                                                .truncate(),
                                            );
                                        }
                                        super::auth_extras::algorithm_label(
                                            ui,
                                            &key.algorithm,
                                            key.security_key,
                                        );
                                    });
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&key.fingerprint)
                                                .monospace()
                                                .size(11.0)
                                                .weak(),
                                        )
                                        .truncate(),
                                    );
                                    if let Some(cert) = &key.certificate {
                                        certificate_line(ui, "", cert);
                                    }
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .add_enabled(
                                                !key.public_openssh.is_empty(),
                                                egui::Button::new(tr("Copiar pública")).small(),
                                            )
                                            .on_hover_text(tr(
                                                "Copia la clave pública para authorized_keys",
                                            ))
                                            .clicked()
                                        {
                                            ui.ctx().copy_text(key.public_openssh.clone());
                                            copied = true;
                                        }
                                    },
                                );
                            });
                            ui.separator();
                        }
                    });
            }
        }
        if copied {
            self.message = Some((tr("Clave pública copiada al portapapeles").into(), false));
        }
    }

    /// Devuelve `false` cuando el usuario cierra la ventana.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let mut open = true;
        egui::Window::new(tr("Claves SSH"))
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
            if ui.button(tr("⟳ Actualizar")).clicked() {
                self.refresh();
            }
            ui.add(
                egui::Label::new(
                    RichText::new(trf!(
                        "Se buscan claves en ~/.ssh y en {dir}",
                        dir = own_dir.display()
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
                    ui.weak(tr("No hay claves todavía. Genera una abajo."));
                }
                for key in &self.keys {
                    ui.horizontal(|ui| {
                        ui.label("🔑");
                        ui.vertical(|ui| {
                            // Sitio para los botones de la derecha.
                            ui.set_max_width((ui.available_width() - 150.0).max(120.0));
                            ui.horizontal(|ui| {
                                ui.strong(
                                    key.path
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default(),
                                );
                                super::auth_extras::algorithm_label(
                                    ui,
                                    &key.algorithm,
                                    key.security_key,
                                );
                                if key.encrypted {
                                    ui.label(RichText::new(tr("🔒 con passphrase")).weak());
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
                            if let Some(cert) = &key.certificate {
                                certificate_line(ui, &key.fingerprint, cert);
                            }
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // Solo se permiten borrar las claves de la carpeta propia.
                            if key.path.starts_with(&own_dir)
                                && ui
                                    .small_button("🗑")
                                    .on_hover_text(tr("Eliminar clave"))
                                    .clicked()
                            {
                                delete_request = Some(key.path.clone());
                            }
                            if ui
                                .add_enabled(
                                    !key.public_openssh.is_empty(),
                                    egui::Button::new(tr("Copiar pública")).small(),
                                )
                                .on_hover_text(tr("Copia la clave pública para authorized_keys"))
                                .clicked()
                            {
                                ui.ctx().copy_text(key.public_openssh.clone());
                                self.message = Some((
                                    tr("Clave pública copiada al portapapeles").into(),
                                    false,
                                ));
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
        self.agent_section(ui);

        ui.add_space(4.0);
        ui.strong(tr("Generar un par de claves nuevo"));
        egui::Grid::new("gen_key")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label(tr("Nombre de fichero"));
                ui.add(egui::TextEdit::singleline(&mut self.gen_name).desired_width(260.0));
                ui.end_row();
                ui.label(tr("Algoritmo"));
                egui::ComboBox::from_id_salt("gen_algorithm")
                    .selected_text(tr(self.gen_algorithm.label()))
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for a in KeyAlgorithm::ALL {
                            ui.selectable_value(&mut self.gen_algorithm, a, tr(a.label()));
                        }
                    });
                ui.end_row();
                ui.label(tr("Comentario"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.gen_comment)
                        .hint_text(tr("usuario@equipo (opcional)"))
                        .desired_width(260.0),
                );
                ui.end_row();
                ui.label(tr("Passphrase"));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.gen_pass)
                            .password(true)
                            .hint_text(tr("opcional"))
                            .desired_width(125.0),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.gen_pass2)
                            .password(true)
                            .hint_text(tr("repetir"))
                            .desired_width(125.0),
                    );
                });
                ui.end_row();
            });
        if ui.button(tr("Generar clave")).clicked() {
            if self.gen_pass != self.gen_pass2 {
                self.message = Some((tr("Las passphrases no coinciden").into(), true));
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
                        let message = trf!("Clave creada: {path}", path = info.path.display());
                        self.message = Some((message, false));
                        self.gen_pass.clear();
                        self.gen_pass2.clear();
                        self.refresh();
                    }
                    Err(e) => self.message = Some((format!("{e:#}"), true)),
                }
            }
        }

        ui.add_space(6.0);
        ui.strong(tr("Importar una clave existente"));
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.import_path)
                    .hint_text(tr("ruta a la clave privada (OpenSSH, PEM o .ppk)"))
                    .desired_width(380.0),
            );
            if ui.button(tr("Importar")).clicked() {
                let source = crate::paths::expand_tilde(&self.import_path);
                match keys::import(&source, &own_dir) {
                    Ok(info) => {
                        let message = trf!("Clave importada: {path}", path = info.path.display());
                        self.message = Some((message, false));
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
                ui.heading(tr("Eliminar clave"));
                ui.label(trf!(
                    "Se borrará definitivamente {path}, con su .pub y su certificado si lo tiene. \
                     No podrás volver a entrar en los servidores que solo acepten esta clave.",
                    path = path.display()
                ));
                ui.horizontal(|ui| {
                    if ui
                        .button(RichText::new(tr("Eliminar")).color(ui.visuals().error_fg_color))
                        .clicked()
                    {
                        match keys::delete(&path) {
                            Ok(()) => self.message = Some((tr("Clave eliminada").into(), false)),
                            Err(e) => self.message = Some((format!("{e:#}"), true)),
                        }
                        close = true;
                    }
                    if ui.button(tr("Cancelar")).clicked() {
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

/// Resumen del certificado que acompaña a una clave: de quién es, para qué
/// usuarios vale, hasta cuándo y qué autoridad lo firmó. `key_fingerprint`
/// es la huella de la clave junto a la que está (vacía si no se conoce).
fn certificate_line(ui: &mut egui::Ui, key_fingerprint: &str, cert: &CertInfo) {
    let (validity, color) = super::auth_extras::validity_label(cert);
    let mismatch = !key_fingerprint.is_empty() && key_fingerprint != cert.key_fingerprint;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let small = |text: String| RichText::new(text).size(11.5);
        ui.label(small(trf!("📜 Certificado «{id}»", id = cert.key_id)));
        if mismatch {
            ui.label(small(tr("⚠ no es de esta clave").into()).color(warn_color()));
        } else if cert.host {
            ui.label(small(tr("⚠ de host, no de usuario").into()).color(warn_color()));
        }
        ui.label(small(validity).color(color));
        ui.add(
            egui::Label::new(
                small(trf!(
                    "para {principals} · CA {ca}",
                    principals = cert.principals_text(),
                    ca = cert.ca_fingerprint
                ))
                .weak(),
            )
            .truncate(),
        );
    })
    .response
    .on_hover_text(super::auth_extras::cert_details(cert));
}
