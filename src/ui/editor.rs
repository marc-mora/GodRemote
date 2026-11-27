//! Diálogo de creación / edición de sesiones.

use uuid::Uuid;

use crate::keys::KeyInfo;
use crate::model::{
    AuthMethod, Protocol, SerialFlow, SerialParity, Session, Tree, Tunnel, TunnelKind,
};
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
    serial_ports: Vec<String>,
}

pub enum EditorResult {
    Open,
    Cancelled,
    /// Guardada; `true` si además hay que conectar.
    Saved {
        connect: bool,
    },
}

const FIELD_WIDTH: f32 = 320.0;
const BAUD_RATES: [u32; 9] = [
    1200, 2400, 4800, 9600, 19_200, 38_400, 57_600, 115_200, 230_400,
];

/// `true` si la sesión se autentica con una contraseña que puede ir al baúl.
fn uses_password(s: &Session) -> bool {
    match s.protocol {
        Protocol::Rdp | Protocol::Vnc => true,
        Protocol::Ssh | Protocol::Sftp => s.auth == AuthMethod::Password,
        Protocol::Telnet | Protocol::Serial => false,
    }
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
            serial_ports: crate::serial::available_ports(),
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
        let max_height = (ctx.content_rect().height() - 150.0).max(240.0);
        egui::Window::new(title)
            .id(egui::Id::unique("session_editor"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .show(ui, |ui| {
                        self.form(ui, tree, vault.as_ref());
                        if self.session.protocol.is_ssh() {
                            self.advanced_ssh(ui, tree);
                        }
                    });
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
                        .desired_width(FIELD_WIDTH),
                );
                ui.end_row();

                ui.label("Protocolo");
                ui.horizontal(|ui| {
                    for p in Protocol::ALL {
                        if ui.selectable_label(s.protocol == p, p.label()).clicked()
                            && s.protocol != p
                        {
                            // Si el puerto era el predeterminado, se adapta.
                            if s.port == s.protocol.default_port() || s.port == 0 {
                                s.port = p.default_port();
                            }
                            s.protocol = p;
                        }
                    }
                });
                ui.end_row();

                if s.protocol.uses_network() {
                    ui.label("Host");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut s.host)
                                .hint_text("nombre o IP")
                                .desired_width(FIELD_WIDTH - 110.0),
                        );
                        ui.label("Puerto");
                        ui.add(egui::DragValue::new(&mut s.port).range(1..=65535).speed(1));
                    });
                    ui.end_row();
                }

                match s.protocol {
                    Protocol::Ssh | Protocol::Sftp => {
                        ui.label("Usuario");
                        ui.add(
                            egui::TextEdit::singleline(&mut s.username).desired_width(FIELD_WIDTH),
                        );
                        ui.end_row();
                        ui.label("Autenticación");
                        ui.horizontal(|ui| {
                            for m in AuthMethod::ALL {
                                ui.selectable_value(&mut s.auth, m, m.label());
                            }
                        });
                        ui.end_row();
                    }
                    Protocol::Rdp => {
                        ui.label("Usuario");
                        ui.add(
                            egui::TextEdit::singleline(&mut s.username).desired_width(FIELD_WIDTH),
                        );
                        ui.end_row();
                        ui.label("Dominio");
                        ui.add(
                            egui::TextEdit::singleline(&mut s.rdp.domain)
                                .hint_text("opcional")
                                .desired_width(FIELD_WIDTH),
                        );
                        ui.end_row();
                    }
                    Protocol::Telnet => {
                        ui.label("");
                        ui.colored_label(
                            crate::theme::warn_color(),
                            "Telnet no cifra nada: úsalo solo en redes de confianza.",
                        );
                        ui.end_row();
                    }
                    Protocol::Vnc | Protocol::Serial => {}
                }

                if uses_password(s) {
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
                                .desired_width(FIELD_WIDTH),
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
                        if s.protocol == Protocol::Vnc {
                            ui.weak(
                                "VNC no cifra la sesión y solo usa 8 caracteres de contraseña.",
                            );
                        }
                    });
                    ui.end_row();
                } else if s.protocol.is_ssh() && s.auth == AuthMethod::Key {
                    ui.label("Clave privada");
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut s.key_path)
                                    .hint_text("~/.ssh/id_ed25519")
                                    .desired_width(FIELD_WIDTH - 86.0),
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
                            if ui
                                .button("📂")
                                .on_hover_text("Elegir el fichero de la clave")
                                .clicked()
                                && let Some(path) = rfd::FileDialog::new()
                                    .set_title("Clave privada SSH")
                                    .pick_file()
                            {
                                s.key_path = path.display().to_string();
                            }
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
                                .desired_width(FIELD_WIDTH),
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
                } else if s.protocol.is_ssh() {
                    ui.label("");
                    ui.weak("Se usarán las claves cargadas en ssh-agent / Pageant.");
                    ui.end_row();
                }

                if s.protocol == Protocol::Rdp {
                    ui.label("Cliente");
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut s.rdp.embedded, true, "Integrado (pestaña)");
                            ui.selectable_value(&mut s.rdp.embedded, false, "Externo");
                        });
                        if !s.rdp.embedded {
                            ui.weak(format!("Se usará {}", crate::rdp::client_description()));
                        }
                    });
                    ui.end_row();

                    ui.label("Pantalla");
                    ui.vertical(|ui| {
                        let fixed_size = if s.rdp.embedded {
                            ui.checkbox(&mut s.rdp.fit_window, "Usar el tamaño de la pestaña");
                            !s.rdp.fit_window
                        } else {
                            ui.checkbox(&mut s.rdp.fullscreen, "Pantalla completa");
                            !s.rdp.fullscreen
                        };
                        ui.add_enabled_ui(fixed_size, |ui| {
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

                if s.protocol == Protocol::Serial {
                    ui.label("Dispositivo");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut s.serial.device)
                                .hint_text(if cfg!(windows) {
                                    "COM3"
                                } else {
                                    "/dev/ttyUSB0"
                                })
                                .desired_width(FIELD_WIDTH - 60.0),
                        );
                        egui::ComboBox::from_id_salt("serial_ports")
                            .selected_text("…")
                            .width(24.0)
                            .show_ui(ui, |ui| {
                                if self.serial_ports.is_empty() {
                                    ui.weak("No se han detectado puertos serie");
                                }
                                for port in &self.serial_ports {
                                    if ui.selectable_label(false, port).clicked() {
                                        s.serial.device = port.clone();
                                    }
                                }
                            });
                    });
                    ui.end_row();

                    ui.label("Velocidad");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("serial_baud")
                            .selected_text(format!("{} baudios", s.serial.baud))
                            .show_ui(ui, |ui| {
                                for baud in BAUD_RATES {
                                    ui.selectable_value(&mut s.serial.baud, baud, baud.to_string());
                                }
                            });
                        ui.add(
                            egui::DragValue::new(&mut s.serial.baud)
                                .range(50..=4_000_000)
                                .speed(100),
                        );
                    });
                    ui.end_row();

                    ui.label("Formato");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("serial_data")
                            .selected_text(format!("{} bits", s.serial.data_bits))
                            .width(70.0)
                            .show_ui(ui, |ui| {
                                for bits in [5u8, 6, 7, 8] {
                                    ui.selectable_value(
                                        &mut s.serial.data_bits,
                                        bits,
                                        format!("{bits} bits"),
                                    );
                                }
                            });
                        let parity = |p: SerialParity| match p {
                            SerialParity::None => "Sin paridad",
                            SerialParity::Odd => "Impar",
                            SerialParity::Even => "Par",
                        };
                        egui::ComboBox::from_id_salt("serial_parity")
                            .selected_text(parity(s.serial.parity))
                            .width(100.0)
                            .show_ui(ui, |ui| {
                                for p in [SerialParity::None, SerialParity::Odd, SerialParity::Even]
                                {
                                    ui.selectable_value(&mut s.serial.parity, p, parity(p));
                                }
                            });
                        egui::ComboBox::from_id_salt("serial_stop")
                            .selected_text(format!("{} de parada", s.serial.stop_bits))
                            .width(100.0)
                            .show_ui(ui, |ui| {
                                for bits in [1u8, 2] {
                                    ui.selectable_value(
                                        &mut s.serial.stop_bits,
                                        bits,
                                        format!("{bits} de parada"),
                                    );
                                }
                            });
                    });
                    ui.end_row();

                    ui.label("Control de flujo");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut s.serial.flow, SerialFlow::None, "Ninguno");
                        ui.selectable_value(&mut s.serial.flow, SerialFlow::Software, "XON/XOFF");
                        ui.selectable_value(&mut s.serial.flow, SerialFlow::Hardware, "RTS/CTS");
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
                    .width(FIELD_WIDTH)
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
                        .desired_width(FIELD_WIDTH),
                );
                ui.end_row();
            });
    }

    /// Salto por bastión, reenvío X11 y túneles.
    fn advanced_ssh(&mut self, ui: &mut egui::Ui, tree: &Tree) {
        let s = &mut self.session;
        let configured = s.jump.is_some() || s.x11_forwarding || !s.tunnels.is_empty();
        egui::CollapsingHeader::new("Avanzado: salto, X11 y túneles")
            .id_salt("ssh_advanced")
            .default_open(configured)
            .show(ui, |ui| {
                egui::Grid::new("ssh_advanced_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Saltar por").on_hover_text(
                            "Bastión: se conecta primero a esa sesión y desde ella al destino",
                        );
                        let candidates: Vec<(Uuid, String)> = tree
                            .ssh_sessions()
                            .into_iter()
                            .filter(|(id, _)| *id != s.id)
                            .collect();
                        let current = match s.jump {
                            None => "(conexión directa)".to_string(),
                            Some(id) => candidates
                                .iter()
                                .find(|(cid, _)| *cid == id)
                                .map(|(_, name)| name.clone())
                                .unwrap_or_else(|| "(sesión eliminada)".into()),
                        };
                        egui::ComboBox::from_id_salt("jump_picker")
                            .selected_text(current)
                            .width(FIELD_WIDTH)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut s.jump, None, "(conexión directa)");
                                for (id, name) in &candidates {
                                    ui.selectable_value(&mut s.jump, Some(*id), name);
                                }
                            });
                        ui.end_row();

                        if s.protocol == Protocol::Ssh {
                            ui.label("X11");
                            ui.checkbox(
                                &mut s.x11_forwarding,
                                "Reenviar aplicaciones gráficas (X11)",
                            )
                            .on_hover_text(
                                "Necesita un servidor X local: el escritorio en Linux, \
                                     VcXsrv/Xming en Windows o XQuartz en macOS",
                            );
                            ui.end_row();
                        }
                    });

                if s.protocol == Protocol::Ssh {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.strong("Túneles");
                        if ui.small_button("➕ Añadir").clicked() {
                            s.tunnels.push(Tunnel::default());
                        }
                    });
                    let mut remove = None;
                    for (i, t) in s.tunnels.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt(("tunnel_kind", i))
                                .selected_text(t.kind.label())
                                .width(150.0)
                                .show_ui(ui, |ui| {
                                    for kind in TunnelKind::ALL {
                                        ui.selectable_value(&mut t.kind, kind, kind.label());
                                    }
                                });
                            ui.add(
                                egui::TextEdit::singleline(&mut t.listen_host)
                                    .desired_width(90.0)
                                    .hint_text("escucha"),
                            )
                            .on_hover_text(match t.kind {
                                TunnelKind::Remote => "Dirección de escucha en el servidor",
                                _ => "Dirección de escucha en este equipo",
                            });
                            ui.add(egui::DragValue::new(&mut t.listen_port).range(1..=65535));
                            if t.kind != TunnelKind::Dynamic {
                                ui.label("hacia");
                                ui.add(
                                    egui::TextEdit::singleline(&mut t.target_host)
                                        .desired_width(110.0)
                                        .hint_text("destino"),
                                );
                                ui.add(egui::DragValue::new(&mut t.target_port).range(1..=65535));
                            }
                            if ui.small_button("🗑").on_hover_text("Quitar túnel").clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove {
                        s.tunnels.remove(i);
                    }
                    if s.tunnels.is_empty() {
                        ui.weak(
                            "Sin túneles. Se abren al conectar el terminal y se cierran con él.",
                        );
                    }
                }
            });
    }

    /// Valida y guarda la sesión (y sus secretos en el baúl).
    fn commit(&mut self, tree: &mut Tree, vault: &mut Option<Vault>) -> Result<(), String> {
        let mut s = self.session.clone();
        s.name = s.name.trim().to_string();
        s.host = s.host.trim().to_string();
        s.username = s.username.trim().to_string();
        s.key_path = s.key_path.trim().to_string();
        s.serial.device = s.serial.device.trim().to_string();
        for t in &mut s.tunnels {
            t.listen_host = t.listen_host.trim().to_string();
            t.target_host = t.target_host.trim().to_string();
        }
        if !s.protocol.is_ssh() {
            // Opciones que solo tienen sentido sobre SSH.
            s.jump = None;
            s.tunnels.clear();
            s.x11_forwarding = false;
        }
        s.validate()?;

        if let Some(vault) = vault {
            let with_password = uses_password(&s);
            let res = (|| -> anyhow::Result<()> {
                if with_password && !self.password.is_empty() && self.save_password {
                    vault.set(&s.password_key(), &self.password)?;
                } else if self.forget_password || !with_password {
                    vault.remove(&s.password_key())?;
                }
                let uses_key = s.protocol.is_ssh() && s.auth == AuthMethod::Key;
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
