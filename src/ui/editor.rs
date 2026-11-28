//! Diálogo de creación / edición de sesiones.

use uuid::Uuid;

use crate::i18n::tr;
use crate::keys::KeyInfo;
use crate::model::{
    AuthMethod, Protocol, SerialFlow, SerialParity, Session, Tree, Tunnel, TunnelKind,
};
use crate::trf;
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
    backdrop: super::backdrop_view::BackdropEditor,
    /// Certificado de la clave privada.
    cert: super::auth_extras::CertField,
    /// Secreto TOTP del segundo factor.
    totp: super::auth_extras::TotpField,
    /// El cliente RDP integrado está disponible (lo incluye la edición y
    /// no está desactivado en los ajustes). Lo fija la aplicación.
    pub embedded_rdp: bool,
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
/// Lista ordenable de saltos: filas con su posición y botones para subir,
/// bajar o quitar, y un desplegable para añadir. `own` es la sesión que se
/// está editando (no puede saltar a través de sí misma).
pub(super) fn jump_list_ui(
    ui: &mut egui::Ui,
    tree: &Tree,
    jumps: &mut Vec<Uuid>,
    own: Option<Uuid>,
    salt: &str,
) {
    let sessions = tree.ssh_sessions();
    let name_of = |id: Uuid| {
        sessions
            .iter()
            .find(|(sid, _)| *sid == id)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| tr("(sesión eliminada)").into())
    };
    let mut move_up = None;
    let mut move_down = None;
    let mut remove = None;
    let count = jumps.len();
    for (i, id) in jumps.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(format!("{}.", i + 1));
            ui.allocate_ui_with_layout(
                egui::vec2(FIELD_WIDTH, 18.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_width(FIELD_WIDTH);
                    ui.add(egui::Label::new(name_of(*id)).truncate());
                },
            );
            if ui
                .add_enabled(i > 0, egui::Button::new("⬆").small())
                .on_hover_text(tr("Antes en el camino"))
                .clicked()
            {
                move_up = Some(i);
            }
            if ui
                .add_enabled(i + 1 < count, egui::Button::new("⬇").small())
                .on_hover_text(tr("Después en el camino"))
                .clicked()
            {
                move_down = Some(i);
            }
            if ui
                .small_button("🗑")
                .on_hover_text(tr("Quitar salto"))
                .clicked()
            {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = move_up {
        jumps.swap(i, i - 1);
    }
    if let Some(i) = move_down {
        jumps.swap(i, i + 1);
    }
    if let Some(i) = remove {
        jumps.remove(i);
    }

    // Añadir: cualquier sesión SSH que no sea la propia ni esté ya.
    let candidates: Vec<&(Uuid, String)> = sessions
        .iter()
        .filter(|(id, _)| Some(*id) != own && !jumps.contains(id))
        .collect();
    let mut add = None;
    egui::ComboBox::from_id_salt((salt, "jump_add"))
        .selected_text(tr("➕ Añadir salto…"))
        .width(FIELD_WIDTH)
        .show_ui(ui, |ui| {
            if candidates.is_empty() {
                ui.weak(tr("No hay más sesiones SSH en el árbol"));
            }
            for (id, name) in candidates {
                if ui.selectable_label(false, name).clicked() {
                    add = Some(*id);
                }
            }
        });
    if let Some(id) = add {
        jumps.push(id);
    }
}

/// Una clave en el desplegable para elegirla: su ruta y de qué tipo es.
pub(super) fn key_label(key: &KeyInfo) -> String {
    let (path, algorithm) = (key.path.display(), &key.algorithm);
    if key.encrypted {
        trf!("{path}  ({algorithm}, cifrada)", path, algorithm)
    } else {
        format!("{path}  ({algorithm})")
    }
}

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
            backdrop: Default::default(),
            cert: Default::default(),
            totp: Default::default(),
            embedded_rdp: crate::HAS_EMBEDDED_RDP,
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
            tr("Nueva sesión")
        } else {
            tr("Editar sesión")
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
                        if self.session.protocol.uses_network() {
                            self.jumps_ui(ui, tree);
                        }
                        if self.session.protocol == Protocol::Ssh {
                            self.advanced_ssh(ui);
                        }
                        if self.session.protocol.has_terminal() {
                            self.terminal_look(ui, tree);
                        }
                    });
                ui.add_space(6.0);
                if let Some(err) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let mut save = None;
                    if ui.button(tr("Guardar")).clicked() {
                        save = Some(false);
                    }
                    if ui.button(tr("Guardar y conectar")).clicked() {
                        save = Some(true);
                    }
                    if ui.button(tr("Cancelar")).clicked() {
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
        let embedded_rdp = self.embedded_rdp;
        let inherited = tree.inherited(self.parent);
        // Hereda la autenticación de su carpeta (si esta la define).
        let auth_from_folder =
            self.session.protocol.is_ssh() && self.session.inherit_auth && inherited.auth.is_some();
        let user_hint = match &inherited.username {
            Some((folder, user)) => trf!("{user} (de la carpeta «{folder}»)", user, folder),
            None => String::new(),
        };
        let s = &mut self.session;
        egui::Grid::new("session_form")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label(tr("Nombre"));
                ui.add(
                    egui::TextEdit::singleline(&mut s.name)
                        .hint_text(tr("p. ej. web-01 (opcional)"))
                        .desired_width(FIELD_WIDTH),
                );
                ui.end_row();

                ui.label(tr("Protocolo"));
                ui.horizontal(|ui| {
                    for p in Protocol::ALL {
                        if ui
                            .selectable_label(s.protocol == p, tr(p.label()))
                            .clicked()
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
                    ui.label(tr("Host"));
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut s.host)
                                .hint_text(tr("nombre o IP"))
                                .desired_width(FIELD_WIDTH - 110.0),
                        );
                        ui.label(tr("Puerto"));
                        ui.add(egui::DragValue::new(&mut s.port).range(1..=65535).speed(1));
                    });
                    ui.end_row();
                }

                match s.protocol {
                    Protocol::Ssh | Protocol::Sftp => {
                        ui.label(tr("Usuario"));
                        ui.add(
                            egui::TextEdit::singleline(&mut s.username)
                                .hint_text(user_hint.as_str())
                                .desired_width(FIELD_WIDTH),
                        );
                        ui.end_row();
                        ui.label(tr("Autenticación"));
                        ui.vertical(|ui| {
                            if let Some((folder, method, key)) = &inherited.auth {
                                let detail = if *method == AuthMethod::Key && !key.is_empty() {
                                    format!("{} {key}", tr(method.label()))
                                } else {
                                    tr(method.label()).to_string()
                                };
                                ui.checkbox(
                                    &mut s.inherit_auth,
                                    trf!("La de la carpeta «{folder}» ({detail})", folder, detail),
                                );
                            } else if s.inherit_auth {
                                ui.weak(tr(
                                    "Su carpeta ya no define la autenticación: se usa esta.",
                                ));
                            }
                            if !auth_from_folder {
                                ui.horizontal(|ui| {
                                    for m in AuthMethod::ALL {
                                        ui.selectable_value(&mut s.auth, m, tr(m.label()));
                                    }
                                });
                            }
                        });
                        ui.end_row();
                    }
                    Protocol::Rdp => {
                        ui.label(tr("Usuario"));
                        ui.add(
                            egui::TextEdit::singleline(&mut s.username)
                                .hint_text(user_hint.as_str())
                                .desired_width(FIELD_WIDTH),
                        );
                        ui.end_row();
                        ui.label(tr("Dominio"));
                        ui.add(
                            egui::TextEdit::singleline(&mut s.rdp.domain)
                                .hint_text(tr("opcional"))
                                .desired_width(FIELD_WIDTH),
                        );
                        ui.end_row();
                    }
                    Protocol::Telnet => {
                        ui.label("");
                        ui.colored_label(
                            crate::theme::warn_color(),
                            tr("Telnet no cifra nada: úsalo solo en redes de confianza."),
                        );
                        ui.end_row();
                    }
                    Protocol::Vnc | Protocol::Serial => {}
                }

                if auth_from_folder {
                    // La contraseña o la clave son las de la carpeta.
                } else if uses_password(s) {
                    ui.label(tr("Contraseña"));
                    ui.vertical(|ui| {
                        let stored = vault.is_some_and(|v| v.contains(&s.password_key()));
                        let hint = if stored && !self.forget_password {
                            tr("guardada en el baúl (escribe para cambiarla)")
                        } else {
                            tr("se pedirá al conectar si se deja vacía")
                        };
                        ui.add(
                            egui::TextEdit::singleline(&mut self.password)
                                .password(true)
                                .hint_text(hint)
                                .desired_width(FIELD_WIDTH),
                        );
                        if vault.is_some() {
                            ui.checkbox(&mut self.save_password, tr("Guardar en el baúl cifrado"));
                            if stored {
                                ui.checkbox(
                                    &mut self.forget_password,
                                    tr("Olvidar la contraseña guardada"),
                                );
                            }
                        } else {
                            ui.weak(tr("Baúl bloqueado: la contraseña no se guardará."));
                        }
                        if s.protocol == Protocol::Vnc {
                            ui.weak(tr(
                                "VNC no cifra la sesión y solo usa 8 caracteres de contraseña.",
                            ));
                        }
                    });
                    ui.end_row();
                } else if s.protocol.is_ssh() && s.auth == AuthMethod::Key {
                    ui.label(tr("Clave privada"));
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
                                        ui.weak(tr("No se han encontrado claves"));
                                    }
                                    for k in &self.keys {
                                        if ui.selectable_label(false, key_label(k)).clicked() {
                                            s.key_path = k.path.display().to_string();
                                        }
                                    }
                                });
                            if ui
                                .button("📂")
                                .on_hover_text(tr("Elegir el fichero de la clave"))
                                .clicked()
                                && let Some(path) = rfd::FileDialog::new()
                                    .set_title(tr("Clave privada SSH"))
                                    .pick_file()
                            {
                                s.key_path = path.display().to_string();
                            }
                        });
                        self.cert.ui(ui, &s.key_path, &mut s.cert_path, FIELD_WIDTH);
                        let stored = vault.is_some_and(|v| v.contains(&s.passphrase_key()));
                        let hint = if stored && !self.forget_passphrase {
                            tr("passphrase guardada en el baúl")
                        } else {
                            tr("passphrase (si la clave está cifrada)")
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
                                    tr("Olvidar la passphrase guardada"),
                                );
                            }
                        } else {
                            ui.weak(tr("Baúl bloqueado: la passphrase no se guardará."));
                        }
                    });
                    ui.end_row();
                } else if s.protocol.is_ssh() {
                    ui.label("");
                    ui.weak(tr("Se usarán las claves cargadas en ssh-agent / Pageant."));
                    ui.end_row();
                }

                if s.protocol.is_ssh() {
                    ui.label(tr("Segundo factor"));
                    let key = s.totp_key();
                    self.totp
                        .ui(ui, vault, &key, &mut s.totp_autofill, FIELD_WIDTH);
                    ui.end_row();
                }

                if s.protocol == Protocol::Rdp {
                    ui.label(tr("Cliente"));
                    ui.vertical(|ui| {
                        if embedded_rdp {
                            ui.horizontal(|ui| {
                                ui.selectable_value(
                                    &mut s.rdp.embedded,
                                    true,
                                    tr("Integrado (pestaña)"),
                                );
                                ui.selectable_value(&mut s.rdp.embedded, false, tr("Externo"));
                            });
                        } else if crate::HAS_EMBEDDED_RDP {
                            ui.weak(tr(
                                "Externo: el cliente integrado está desactivado en Ajustes.",
                            ));
                        } else {
                            ui.weak(tr("Externo: esta edición no incluye el cliente integrado."));
                        }
                        if !(s.rdp.embedded && embedded_rdp) {
                            ui.weak(trf!(
                                "Se usará {client}",
                                client = crate::rdp::client_description()
                            ));
                        }
                    });
                    ui.end_row();

                    ui.label(tr("Pantalla"));
                    ui.vertical(|ui| {
                        let fixed_size = if s.rdp.embedded && embedded_rdp {
                            ui.checkbox(&mut s.rdp.fit_window, tr("Usar el tamaño de la pestaña"));
                            !s.rdp.fit_window
                        } else {
                            ui.checkbox(&mut s.rdp.fullscreen, tr("Pantalla completa"));
                            !s.rdp.fullscreen
                        };
                        ui.add_enabled_ui(fixed_size, |ui| {
                            ui.horizontal(|ui| {
                                ui.add(egui::DragValue::new(&mut s.rdp.width).range(640..=7680));
                                ui.label("×");
                                ui.add(egui::DragValue::new(&mut s.rdp.height).range(480..=4320));
                            });
                        });
                        ui.checkbox(&mut s.rdp.clipboard, tr("Compartir portapapeles"));
                    });
                    ui.end_row();
                }

                if s.protocol == Protocol::Serial {
                    ui.label(tr("Dispositivo"));
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
                                    ui.weak(tr("No se han detectado puertos serie"));
                                }
                                for port in &self.serial_ports {
                                    if ui.selectable_label(false, port).clicked() {
                                        s.serial.device = port.clone();
                                    }
                                }
                            });
                    });
                    ui.end_row();

                    ui.label(tr("Velocidad"));
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("serial_baud")
                            .selected_text(trf!("{baud} baudios", baud = s.serial.baud))
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

                    ui.label(tr("Formato"));
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("serial_data")
                            .selected_text(trf!("{bits} bits", bits = s.serial.data_bits))
                            .width(70.0)
                            .show_ui(ui, |ui| {
                                for bits in [5u8, 6, 7, 8] {
                                    ui.selectable_value(
                                        &mut s.serial.data_bits,
                                        bits,
                                        trf!("{bits} bits", bits),
                                    );
                                }
                            });
                        let parity = |p: SerialParity| match p {
                            SerialParity::None => tr("Sin paridad"),
                            SerialParity::Odd => tr("Impar"),
                            SerialParity::Even => tr("Par"),
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
                            .selected_text(trf!("{bits} de parada", bits = s.serial.stop_bits))
                            .width(100.0)
                            .show_ui(ui, |ui| {
                                for bits in [1u8, 2] {
                                    ui.selectable_value(
                                        &mut s.serial.stop_bits,
                                        bits,
                                        trf!("{bits} de parada", bits),
                                    );
                                }
                            });
                    });
                    ui.end_row();

                    ui.label(tr("Control de flujo"));
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut s.serial.flow, SerialFlow::None, tr("Ninguno"));
                        ui.selectable_value(&mut s.serial.flow, SerialFlow::Software, "XON/XOFF");
                        ui.selectable_value(&mut s.serial.flow, SerialFlow::Hardware, "RTS/CTS");
                    });
                    ui.end_row();
                }

                ui.label(tr("Carpeta"));
                let folders = tree.folder_paths();
                let current = self
                    .parent
                    .and_then(|id| folders.iter().find(|(fid, _)| *fid == id))
                    .map(|(_, path)| path.clone())
                    .unwrap_or_else(|| tr("(raíz)").into());
                egui::ComboBox::from_id_salt("folder_picker")
                    .selected_text(current)
                    .width(FIELD_WIDTH)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.parent, None, tr("(raíz)"));
                        for (id, path) in &folders {
                            ui.selectable_value(&mut self.parent, Some(*id), path);
                        }
                    });
                ui.end_row();

                ui.label(tr("Notas"));
                ui.add(
                    egui::TextEdit::multiline(&mut s.notes)
                        .desired_rows(2)
                        .desired_width(FIELD_WIDTH),
                );
                ui.end_row();
            });
    }

    /// Lista ordenada de saltos (bastiones SSH) para llegar al equipo.
    fn jumps_ui(&mut self, ui: &mut egui::Ui, tree: &Tree) {
        let inherited = tree.inherited(self.parent);
        let parent = self.parent;
        let s = &mut self.session;
        let effective = tree.resolve(s, parent).jumps.len();
        let header = if effective == 0 {
            tr("Saltos (bastiones SSH): conexión directa").to_string()
        } else {
            trf!("Saltos (bastiones SSH): {count}", count = effective)
        };
        egui::CollapsingHeader::new(header)
            .id_salt("session_jumps")
            .default_open(effective > 0)
            .show(ui, |ui| {
                ui.weak(tr(
                    "Se conecta al primero, desde él al segundo, y así hasta el destino. \
                     Cada salto es una sesión SSH del árbol.",
                ));
                if let Some((folder, jumps)) = &inherited.jumps {
                    ui.checkbox(
                        &mut s.inherit_jumps,
                        trf!("Usar los saltos de la carpeta «{folder}»", folder),
                    );
                    if s.inherit_jumps && jumps.is_empty() {
                        ui.weak(tr("La carpeta indica conexión directa, sin saltos."));
                    }
                } else if s.inherit_jumps {
                    ui.weak(tr(
                        "Su carpeta ya no define saltos: se usan los de esta sesión.",
                    ));
                }
                let inheriting = s.inherit_jumps && inherited.jumps.is_some();
                if !inheriting {
                    jump_list_ui(ui, tree, &mut s.jumps, Some(s.id), "session");
                }

                // Camino resultante (incluye los saltos que cada salto
                // necesite a su vez) o el motivo por el que no es válido.
                let resolved = tree.resolve(s, parent);
                if !resolved.jumps.is_empty() {
                    match tree.jump_path(&resolved) {
                        Ok(path) => {
                            let mut route = vec![tr("este equipo").to_string()];
                            route.extend(path.iter().map(|hop| hop.display_name()));
                            route.push(if s.protocol.uses_network() && !s.host.trim().is_empty() {
                                s.host.trim().to_string()
                            } else {
                                tr("destino").to_string()
                            });
                            ui.label(trf!("Camino: {route}", route = route.join("  >  ")));
                        }
                        Err(e) => {
                            ui.colored_label(ui.visuals().error_fg_color, e);
                        }
                    }
                    if s.protocol == Protocol::Rdp && !(s.rdp.embedded && self.embedded_rdp) {
                        ui.colored_label(
                            crate::theme::warn_color(),
                            tr("Los saltos solo funcionan con el cliente RDP integrado."),
                        );
                    }
                }
            });
    }

    /// Reenvío X11 y túneles (solo terminales SSH).
    fn advanced_ssh(&mut self, ui: &mut egui::Ui) {
        let s = &mut self.session;
        let configured = s.x11_forwarding
            || !s.tunnels.is_empty()
            || !s.on_connect.trim().is_empty()
            || s.agent_forwarding
            || s.bypass_proxy;
        egui::CollapsingHeader::new(tr("Avanzado: órdenes al conectar, agente, X11 y túneles"))
            .id_salt("ssh_advanced")
            .default_open(configured)
            .show(ui, |ui| {
                ui.label(tr("Órdenes al conectar"));
                ui.add(
                    egui::TextEdit::multiline(&mut s.on_connect)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(2)
                        .desired_width(430.0)
                        .hint_text(tr("una por línea; por ejemplo: cd /var/log")),
                )
                .on_hover_text(tr(
                    "Se escriben solas en el terminal cuando la shell termina de saludar, \
                     también al reconectar. Admiten las variables de los snippets: {{host}}, \
                     {{usuario}}, {{nombre}}, {{fecha}}…",
                ));
                ui.checkbox(
                    &mut s.agent_forwarding,
                    tr("Reenviar el agente SSH (como ssh -A)"),
                )
                .on_hover_text(tr(
                    "Las claves de tu agente SSH quedan disponibles en este equipo para \
                         saltar desde él a otros. Quien administre ese equipo puede usarlas \
                         mientras dure la sesión: actívalo solo en equipos de confianza",
                ));
                ui.checkbox(
                    &mut s.bypass_proxy,
                    tr("Conectar directamente, sin el proxy de salida de los ajustes"),
                );
                ui.add_space(4.0);
                ui.checkbox(
                    &mut s.x11_forwarding,
                    tr("Reenviar aplicaciones gráficas (X11)"),
                )
                .on_hover_text(tr("Necesita un servidor X local: el escritorio en Linux, \
                     VcXsrv/Xming en Windows o XQuartz en macOS"));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.strong(tr("Túneles"));
                    if ui.small_button(tr("➕ Añadir")).clicked() {
                        s.tunnels.push(Tunnel::default());
                    }
                });
                let mut remove = None;
                for (i, t) in s.tunnels.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt(("tunnel_kind", i))
                            .selected_text(tr(t.kind.label()))
                            .width(150.0)
                            .show_ui(ui, |ui| {
                                for kind in TunnelKind::ALL {
                                    ui.selectable_value(&mut t.kind, kind, tr(kind.label()));
                                }
                            });
                        ui.add(
                            egui::TextEdit::singleline(&mut t.listen_host)
                                .desired_width(90.0)
                                .hint_text(tr("escucha")),
                        )
                        .on_hover_text(match t.kind {
                            TunnelKind::Remote => tr("Dirección de escucha en el servidor"),
                            _ => tr("Dirección de escucha en este equipo"),
                        });
                        ui.add(egui::DragValue::new(&mut t.listen_port).range(1..=65535));
                        if t.kind != TunnelKind::Dynamic {
                            ui.label(tr("hacia"));
                            ui.add(
                                egui::TextEdit::singleline(&mut t.target_host)
                                    .desired_width(110.0)
                                    .hint_text(tr("destino")),
                            );
                            ui.add(egui::DragValue::new(&mut t.target_port).range(1..=65535));
                        }
                        if ui
                            .small_button("🗑")
                            .on_hover_text(tr("Quitar túnel"))
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    s.tunnels.remove(i);
                }
                if s.tunnels.is_empty() {
                    ui.weak(tr(
                        "Sin túneles. Se abren al conectar el terminal y se cierran con él.",
                    ));
                }
            });
    }

    /// Aspecto del terminal de este equipo: barra de información y fondo.
    fn terminal_look(&mut self, ui: &mut egui::Ui, tree: &Tree) {
        let inherited = tree.inherited(self.parent);
        let s = &mut self.session;
        let configured = s.backdrop.is_some() || !s.server_info || s.color.is_some();
        egui::CollapsingHeader::new(tr("Terminal: color de la pestaña, barra y fondo"))
            .id_salt("terminal_look")
            .default_open(configured)
            .show(ui, |ui| {
                super::util::tab_color_ui(ui, tr("Color propio para su pestaña"), &mut s.color);
                if let Some((folder, _)) = &inherited.color
                    && s.color.is_none()
                {
                    ui.weak(trf!(
                        "Sin color propio se usa el de la carpeta «{folder}».",
                        folder
                    ));
                }
                if s.protocol == Protocol::Ssh {
                    ui.checkbox(
                        &mut s.server_info,
                        tr("Mostrar la barra de información del servidor"),
                    )
                    .on_hover_text(tr(
                        "CPU, memoria, disco, red… Se consultan por un canal aparte de la \
                         misma conexión. Desmárcalo en equipos sin shell POSIX (routers, \
                         conmutadores) o donde no quieras órdenes adicionales",
                    ));
                }
                if let Some(folder) = &inherited.server_info_off
                    && s.protocol == Protocol::Ssh
                {
                    ui.weak(trf!(
                        "La carpeta «{folder}» desactiva la barra en sus equipos.",
                        folder
                    ));
                }
                if let Some(folder) = &inherited.backdrop
                    && s.backdrop.is_none()
                {
                    ui.weak(trf!(
                        "Sin fondo propio se usa el de la carpeta «{folder}».",
                        folder
                    ));
                }
                let mut own = s.backdrop.is_some();
                if ui
                    .checkbox(&mut own, tr("Fondo propio para este equipo"))
                    .on_hover_text(tr(
                        "Sustituye al fondo del tema: por ejemplo, «PRODUCCIÓN» en rojo \
                         para no confundir servidores",
                    ))
                    .changed()
                {
                    s.backdrop = own.then(Default::default);
                }
                if let Some(backdrop) = &mut s.backdrop {
                    ui.set_max_width(440.0);
                    self.backdrop.show(ui, backdrop, "session");
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
        s.cert_path = s.cert_path.trim().to_string();
        s.serial.device = s.serial.device.trim().to_string();
        for t in &mut s.tunnels {
            t.listen_host = t.listen_host.trim().to_string();
            t.target_host = t.target_host.trim().to_string();
        }
        if s.protocol != Protocol::Ssh {
            // Túneles y X11 solo tienen sentido en un terminal SSH.
            s.tunnels.clear();
            s.x11_forwarding = false;
        }
        if !s.protocol.uses_network() {
            s.jumps.clear();
        }
        if !s.protocol.has_terminal() {
            s.backdrop = None;
        }
        if let Some(backdrop) = &mut s.backdrop {
            backdrop.sanitize();
        }
        // Se valida tal como se conectará: con lo que herede de su carpeta.
        let resolved = tree.resolve(&s, self.parent);
        resolved.validate()?;
        // Los saltos deben formar un camino válido (sin bucles, todos SSH).
        tree.jump_path(&resolved)?;
        let own_auth = !(s.inherit_auth && resolved.secret_owner.is_some());

        if let Some(vault) = vault {
            let with_password = uses_password(&s) && own_auth;
            let res = (|| -> anyhow::Result<()> {
                if with_password && !self.password.is_empty() && self.save_password {
                    vault.set(&s.password_key(), &self.password)?;
                } else if self.forget_password || !with_password {
                    vault.remove(&s.password_key())?;
                }
                let uses_key = s.protocol.is_ssh() && s.auth == AuthMethod::Key && own_auth;
                if uses_key && !self.passphrase.is_empty() {
                    vault.set(&s.passphrase_key(), &self.passphrase)?;
                } else if self.forget_passphrase || !uses_key {
                    vault.remove(&s.passphrase_key())?;
                }
                Ok(())
            })();
            res.map_err(|e| {
                trf!(
                    "No se pudo actualizar el baúl: {error}",
                    error = format!("{e:#}")
                )
            })?;
        }
        self.totp.commit(&mut s, vault.as_mut())?;

        let id = s.id;
        tree.upsert_session(self.parent, s.clone());
        if !self.is_new && self.parent != self.original_parent {
            tree.move_node(id, self.parent);
        }
        self.session = s;
        Ok(())
    }
}
