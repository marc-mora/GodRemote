//! Interfaz gráfica: árbol de equipos, pestañas y diálogos.

pub mod editor;
pub mod keys_view;
pub mod sftp_view;
pub mod util;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use egui::{Color32, RichText};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::model::{AuthMethod, Node, Protocol, Session, Tree};
use crate::sftp::SftpSource;
use crate::ssh::{Credentials, PromptRequest, Prompter, SecretAnswer, SshHandle, Waker};
use crate::terminal::{ConnState, INITIAL_COLS, INITIAL_ROWS, TerminalView};
use crate::vault::Vault;
use editor::{EditorResult, SessionEditor};
use keys_view::KeysWindow;
use sftp_view::{SftpState, SftpView};

const OK_COLOR: Color32 = Color32::from_rgb(0x8c, 0xc0, 0x6b);
const WARN_COLOR: Color32 = Color32::from_rgb(0xe6, 0xc5, 0x47);

enum TabKind {
    Terminal(Box<TerminalView>),
    Sftp(Box<SftpView>),
}

struct Tab {
    id: u64,
    title: String,
    /// Sesión de origen (para reconectar o abrir SFTP); `None` = shell local.
    session: Option<Session>,
    kind: TabKind,
}

enum VaultDialog {
    Unlock {
        password: String,
        error: Option<String>,
        focus: bool,
    },
    Create {
        password: String,
        confirm: String,
        error: Option<String>,
    },
    Change {
        password: String,
        confirm: String,
        error: Option<String>,
    },
}

enum ActivePrompt {
    HostKey {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
        reply: Option<oneshot::Sender<bool>>,
    },
    Secret {
        title: String,
        message: String,
        echo: bool,
        allow_save: bool,
        value: String,
        save: bool,
        focused: bool,
        reply: Option<oneshot::Sender<Option<SecretAnswer>>>,
    },
}

impl ActivePrompt {
    /// La conexión que preguntaba ya no existe (p. ej. se cerró la pestaña).
    fn abandoned(&self) -> bool {
        match self {
            ActivePrompt::HostKey { reply, .. } => reply.as_ref().is_none_or(|r| r.is_closed()),
            ActivePrompt::Secret { reply, .. } => reply.as_ref().is_none_or(|r| r.is_closed()),
        }
    }
}

enum TreeAction {
    Select(Uuid),
    Connect(Uuid),
    OpenAs(Uuid, Protocol),
    Edit(Uuid),
    Duplicate(Uuid),
    Delete(Uuid),
    NewSession(Option<Uuid>),
    NewFolder(Option<Uuid>),
    RenameFolder(Uuid),
    Move(Uuid, Option<Uuid>),
}

struct FolderDialog {
    parent: Option<Uuid>,
    /// Si es `Some`, se renombra esa carpeta en lugar de crear una.
    rename: Option<Uuid>,
    name: String,
    focused: bool,
}

struct RdpPrompt {
    session: Session,
    password: String,
    save: bool,
    focused: bool,
}

pub struct App {
    rt: tokio::runtime::Runtime,
    waker: Waker,
    tree: Tree,
    vault: Option<Vault>,
    vault_dialog: Option<VaultDialog>,
    tabs: Vec<Tab>,
    active: usize,
    next_tab_id: u64,
    prompter: Prompter,
    prompt_rx: Receiver<PromptRequest>,
    prompts: VecDeque<ActivePrompt>,
    notice_tx: Sender<(String, bool)>,
    notice_rx: Receiver<(String, bool)>,
    selected: Option<Uuid>,
    filter: String,
    editor: Option<SessionEditor>,
    folder_dialog: Option<FolderDialog>,
    confirm_delete: Option<Uuid>,
    rdp_prompt: Option<RdpPrompt>,
    keys_window: Option<KeysWindow>,
    toast: Option<(String, bool, Instant)>,
    quick_connect: String,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Runtime) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let ctx = cc.egui_ctx.clone();
        let waker: Waker = Arc::new(move || ctx.request_repaint());
        let (prompt_tx, prompt_rx) = std::sync::mpsc::channel();
        let (notice_tx, notice_rx) = std::sync::mpsc::channel();

        let mut toast = None;
        let tree = match Tree::load(&crate::paths::sessions_file()) {
            Ok(t) => t,
            Err(e) => {
                toast = Some((
                    format!("No se pudieron leer las sesiones: {e:#}"),
                    true,
                    Instant::now(),
                ));
                Tree::default()
            }
        };
        let vault_dialog = if Vault::exists(&crate::paths::vault_file()) {
            VaultDialog::Unlock {
                password: String::new(),
                error: None,
                focus: true,
            }
        } else {
            VaultDialog::Create {
                password: String::new(),
                confirm: String::new(),
                error: None,
            }
        };

        Self {
            rt,
            prompter: Prompter::new(prompt_tx, waker.clone()),
            waker,
            tree,
            vault: None,
            vault_dialog: Some(vault_dialog),
            tabs: vec![],
            active: 0,
            next_tab_id: 1,
            prompt_rx,
            prompts: VecDeque::new(),
            notice_tx,
            notice_rx,
            selected: None,
            filter: String::new(),
            editor: None,
            folder_dialog: None,
            confirm_delete: None,
            rdp_prompt: None,
            keys_window: None,
            toast,
            quick_connect: String::new(),
        }
    }

    fn notify(&mut self, msg: impl Into<String>, is_error: bool) {
        self.toast = Some((msg.into(), is_error, Instant::now()));
    }

    fn save_tree(&mut self) {
        if let Err(e) = self.tree.save(&crate::paths::sessions_file()) {
            self.notify(format!("No se pudieron guardar las sesiones: {e:#}"), true);
        }
    }

    fn creds_for(&self, session: &Session) -> Credentials {
        match &self.vault {
            Some(v) => Credentials {
                password: v.get(&session.password_key()).map(str::to_string),
                passphrase: v.get(&session.passphrase_key()).map(str::to_string),
            },
            None => Credentials::default(),
        }
    }

    fn store_secrets(&mut self, secrets: Vec<(String, String)>) {
        if secrets.is_empty() {
            return;
        }
        let Some(vault) = &mut self.vault else {
            return;
        };
        let mut error = None;
        for (key, value) in secrets {
            if let Err(e) = vault.set(&key, &value) {
                error = Some(format!("No se pudo guardar en el baúl: {e:#}"));
            }
        }
        match error {
            Some(e) => self.notify(e, true),
            None => self.notify("Credencial guardada en el baúl", false),
        }
    }

    fn push_tab(&mut self, title: String, session: Option<Session>, kind: TabKind) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(Tab {
            id,
            title,
            session,
            kind,
        });
        self.active = self.tabs.len() - 1;
    }

    fn new_terminal(&self, session: &Session) -> TerminalView {
        let backend = crate::ssh::spawn_shell(
            self.rt.handle(),
            session.clone(),
            self.creds_for(session),
            self.prompter.clone(),
            INITIAL_COLS,
            INITIAL_ROWS,
        );
        TerminalView::new(backend)
    }

    fn open_terminal(&mut self, session: Session) {
        let view = self.new_terminal(&session);
        self.push_tab(
            session.display_name(),
            Some(session),
            TabKind::Terminal(Box::new(view)),
        );
    }

    fn open_local_terminal(&mut self) {
        match crate::terminal::spawn_local_shell(self.waker.clone(), INITIAL_COLS, INITIAL_ROWS) {
            Ok(backend) => self.push_tab(
                "Terminal local".into(),
                None,
                TabKind::Terminal(Box::new(TerminalView::new(backend))),
            ),
            Err(e) => self.notify(format!("No se pudo abrir la shell local: {e:#}"), true),
        }
    }

    fn open_sftp(&mut self, session: Session, shared: Option<SshHandle>) {
        let source = match shared {
            Some(handle) => SftpSource::Existing(handle),
            None => SftpSource::Connect {
                creds: self.creds_for(&session),
                session: Box::new(session.clone()),
            },
        };
        let client = crate::sftp::spawn(self.rt.handle(), source, self.prompter.clone());
        self.push_tab(
            format!("{} (SFTP)", session.display_name()),
            Some(session),
            TabKind::Sftp(Box::new(SftpView::new(client))),
        );
    }

    fn open_rdp(&mut self, session: Session) {
        let stored = self
            .vault
            .as_ref()
            .and_then(|v| v.get(&session.password_key()).map(str::to_string));
        if stored.is_some() || cfg!(windows) {
            // En Windows, mstsc gestiona él mismo las credenciales.
            self.launch_rdp(&session, stored.as_deref());
        } else {
            self.rdp_prompt = Some(RdpPrompt {
                session,
                password: String::new(),
                save: self.vault.is_some(),
                focused: false,
            });
        }
    }

    fn launch_rdp(&mut self, session: &Session, password: Option<&str>) {
        match crate::rdp::launch(session, password) {
            Ok((mut child, log)) => {
                self.notify(
                    format!("Cliente RDP lanzado para {}", session.display_name()),
                    false,
                );
                let name = session.display_name();
                let tx = self.notice_tx.clone();
                let waker = self.waker.clone();
                std::thread::spawn(move || {
                    let status = child.wait();
                    let failed = !status.as_ref().is_ok_and(|s| s.success());
                    let msg = if failed {
                        let detail = log
                            .and_then(|p| std::fs::read_to_string(p).ok())
                            .map(|text| rdp_error_summary(&text))
                            .unwrap_or_default();
                        format!("RDP «{name}» terminó con error. {detail}")
                    } else {
                        format!("Sesión RDP «{name}» finalizada")
                    };
                    let _ = tx.send((msg, failed));
                    waker();
                });
            }
            Err(e) => self.notify(format!("{e:#}"), true),
        }
    }

    /// Abre una sesión con el protocolo indicado (o el suyo propio).
    fn connect(&mut self, session: Session, protocol: Option<Protocol>) {
        match protocol.unwrap_or(session.protocol) {
            Protocol::Ssh => self.open_terminal(session),
            Protocol::Sftp => self.open_sftp(session, None),
            Protocol::Rdp => self.open_rdp(session),
            // Pendiente de integrar.
            Protocol::Vnc | Protocol::Telnet | Protocol::Serial => {
                self.notify("Protocolo aún no disponible", true)
            }
        }
    }

    fn apply_tree_actions(&mut self, actions: Vec<TreeAction>) {
        for action in actions {
            match action {
                TreeAction::Select(id) => self.selected = Some(id),
                TreeAction::Connect(id) => {
                    if let Some(s) = self.tree.session(id).cloned() {
                        self.connect(s, None);
                    }
                }
                TreeAction::OpenAs(id, protocol) => {
                    if let Some(s) = self.tree.session(id).cloned() {
                        self.connect(s, Some(protocol));
                    }
                }
                TreeAction::Edit(id) => {
                    if let Some(s) = self.tree.session(id).cloned() {
                        let parent = self.tree.parent_of(id);
                        self.editor = Some(SessionEditor::edit(s, parent));
                    }
                }
                TreeAction::Duplicate(id) => {
                    if let Some(original) = self.tree.session(id).cloned() {
                        let mut copy = original.clone();
                        copy.id = Uuid::new_v4();
                        copy.name = format!("{} (copia)", original.display_name());
                        if let Some(vault) = &mut self.vault {
                            for (from, to) in [
                                (original.password_key(), copy.password_key()),
                                (original.passphrase_key(), copy.passphrase_key()),
                            ] {
                                if let Some(secret) = vault.get(&from).map(str::to_string) {
                                    let _ = vault.set(&to, &secret);
                                }
                            }
                        }
                        let parent = self.tree.parent_of(id);
                        self.tree.upsert_session(parent, copy);
                        self.save_tree();
                    }
                }
                TreeAction::Delete(id) => self.confirm_delete = Some(id),
                TreeAction::NewSession(parent) => self.editor = Some(SessionEditor::new(parent)),
                TreeAction::NewFolder(parent) => {
                    self.folder_dialog = Some(FolderDialog {
                        parent,
                        rename: None,
                        name: String::new(),
                        focused: false,
                    })
                }
                TreeAction::RenameFolder(id) => {
                    if let Some(Node::Folder(f)) = self.tree.find(id) {
                        self.folder_dialog = Some(FolderDialog {
                            parent: None,
                            rename: Some(id),
                            name: f.name.clone(),
                            focused: false,
                        });
                    }
                }
                TreeAction::Move(id, parent) => {
                    if self.tree.move_node(id, parent) {
                        self.save_tree();
                    } else {
                        self.notify("No se puede mover una carpeta dentro de sí misma", true);
                    }
                }
            }
        }
    }

    /// Carpeta donde crear cosas nuevas según la selección actual.
    fn target_folder(&self) -> Option<Uuid> {
        let id = self.selected?;
        match self.tree.find(id)? {
            Node::Folder(f) => Some(f.id),
            Node::Session(_) => self.tree.parent_of(id),
        }
    }

    // ------------------------------------------------------------------
    // Paneles
    // ------------------------------------------------------------------

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("GodRemote").strong().size(16.0));
            ui.separator();
            if ui
                .button("➕ Sesión")
                .on_hover_text("Nueva sesión SSH / SFTP / RDP")
                .clicked()
            {
                self.editor = Some(SessionEditor::new(self.target_folder()));
            }
            if ui
                .button("📁 Carpeta")
                .on_hover_text("Nueva carpeta de equipos")
                .clicked()
            {
                self.folder_dialog = Some(FolderDialog {
                    parent: self.target_folder(),
                    rename: None,
                    name: String::new(),
                    focused: false,
                });
            }
            if ui.button("💻 Terminal local").clicked() {
                self.open_local_terminal();
            }
            if ui
                .button("🔑 Claves")
                .on_hover_text("Gestionar claves SSH")
                .clicked()
            {
                self.keys_window = Some(KeysWindow::default());
            }
            let vault_exists = Vault::exists(&crate::paths::vault_file());
            ui.menu_button("🔐 Baúl", |ui| {
                if self.vault.is_some() {
                    if ui.button("Bloquear").clicked() {
                        self.vault = None;
                        ui.close();
                    }
                    if ui.button("Cambiar contraseña maestra…").clicked() {
                        self.vault_dialog = Some(VaultDialog::Change {
                            password: String::new(),
                            confirm: String::new(),
                            error: None,
                        });
                        ui.close();
                    }
                } else if vault_exists {
                    if ui.button("Desbloquear…").clicked() {
                        self.vault_dialog = Some(VaultDialog::Unlock {
                            password: String::new(),
                            error: None,
                            focus: true,
                        });
                        ui.close();
                    }
                } else if ui.button("Crear baúl…").clicked() {
                    self.vault_dialog = Some(VaultDialog::Create {
                        password: String::new(),
                        confirm: String::new(),
                        error: None,
                    });
                    ui.close();
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        if let Some((_, is_error, at)) = &self.toast {
            let ttl = if *is_error { 15 } else { 6 };
            if at.elapsed() > Duration::from_secs(ttl) {
                self.toast = None;
            } else {
                ui.ctx().request_repaint_after(Duration::from_secs(1));
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Primero el estado del baúl (derecha); el aviso ocupa el resto.
            match &self.vault {
                Some(v) => ui.colored_label(
                    OK_COLOR,
                    format!("🔓 Baúl abierto · {} secreto(s)", v.len()),
                ),
                None => ui.colored_label(WARN_COLOR, "🔒 Baúl bloqueado"),
            };
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                if let Some((msg, is_error, _)) = &self.toast {
                    let color = if *is_error {
                        ui.visuals().error_fg_color
                    } else {
                        OK_COLOR
                    };
                    ui.add(egui::Label::new(RichText::new(msg).color(color)).truncate())
                        .on_hover_text(msg);
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("🔍 Buscar equipo…")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(4.0);
        let mut actions = vec![];
        let folders = self.tree.folder_paths();
        egui::ScrollArea::vertical()
            .id_salt("tree_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.tree.nodes.is_empty() {
                    ui.add_space(12.0);
                    ui.weak("Aún no hay equipos.");
                    ui.weak("Crea una sesión con «➕ Sesión».");
                } else if self.filter.trim().is_empty() {
                    tree_ui(ui, &self.tree.nodes, self.selected, &folders, &mut actions);
                } else {
                    let needle = self.filter.trim().to_lowercase();
                    let mut matches = vec![];
                    collect_matches(&self.tree.nodes, &needle, &mut matches);
                    if matches.is_empty() {
                        ui.weak("Sin resultados.");
                    }
                    for s in matches {
                        session_row(ui, s, self.selected, &folders, &mut actions);
                    }
                }
                // Clic derecho en el hueco libre: crear en la raíz.
                let rest = ui.allocate_response(ui.available_size(), egui::Sense::click());
                rest.context_menu(|ui| {
                    if ui.button("➕ Nueva sesión").clicked() {
                        actions.push(TreeAction::NewSession(None));
                        ui.close();
                    }
                    if ui.button("📁 Nueva carpeta").clicked() {
                        actions.push(TreeAction::NewFolder(None));
                        ui.close();
                    }
                });
            });
        self.apply_tree_actions(actions);
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut close = None;
        egui::ScrollArea::horizontal()
            .id_salt("tab_bar")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (i, tab) in self.tabs.iter().enumerate() {
                        let (icon, color) = match &tab.kind {
                            TabKind::Terminal(t) => (
                                "💻",
                                match t.state {
                                    ConnState::Connecting => WARN_COLOR,
                                    ConnState::Connected => OK_COLOR,
                                    ConnState::Closed(_) => ui.visuals().error_fg_color,
                                },
                            ),
                            TabKind::Sftp(s) => (
                                "📂",
                                match s.state {
                                    SftpState::Connecting(_) => WARN_COLOR,
                                    SftpState::Ready => OK_COLOR,
                                    SftpState::Closed(_) => ui.visuals().error_fg_color,
                                },
                            ),
                        };
                        ui.scope(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            status_dot(ui, color);
                            let resp = ui.selectable_label(
                                self.active == i,
                                format!("{icon} {}", tab.title),
                            );
                            if resp.clicked() {
                                self.active = i;
                            }
                            if resp.middle_clicked()
                                || ui
                                    .small_button("×")
                                    .on_hover_text("Cerrar pestaña")
                                    .clicked()
                            {
                                close = Some(i);
                            }
                        });
                        ui.separator();
                    }
                });
            });
        if let Some(i) = close {
            self.tabs.remove(i);
            if self.active >= i && self.active > 0 {
                self.active -= 1;
            }
            self.focus_active_terminal();
        }
    }

    fn focus_active_terminal(&mut self) {
        if let Some(Tab {
            kind: TabKind::Terminal(t),
            ..
        }) = self.tabs.get_mut(self.active)
        {
            t.request_focus();
        }
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.18);
            ui.label(RichText::new("GodRemote").size(34.0).strong());
            ui.weak("Gestor de conexiones remotas · SSH · SFTP · RDP");
            ui.add_space(24.0);
            ui.label("Conexión rápida");
            let mut go = None;
            ui.allocate_ui(egui::vec2(520.0, 28.0), |ui| {
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.quick_connect)
                            .hint_text("usuario@host[:puerto]")
                            .desired_width(300.0),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        go = Some(Protocol::Ssh);
                    }
                    for p in Protocol::ALL {
                        if ui.button(p.label()).clicked() {
                            go = Some(p);
                        }
                    }
                });
            });
            if let Some(protocol) = go {
                match util::parse_quick_connect(&self.quick_connect, protocol) {
                    Ok(session) => self.connect(session, None),
                    Err(e) => self.notify(e, true),
                }
            }
            ui.add_space(24.0);
            ui.weak("Doble clic en un equipo del árbol para conectar.");
            ui.weak("Clic derecho para editar, mover, duplicar o abrir SFTP.");
        });
    }

    fn active_tab_ui(&mut self, ui: &mut egui::Ui) {
        let mut open_sftp: Option<(Session, Option<SshHandle>)> = None;
        let mut reconnect = false;
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        // Cada pestaña tiene su propio espacio de ids (foco, scroll…).
        let tab_id = tab.id;
        ui.push_id(tab_id, |ui| match &mut tab.kind {
            TabKind::Terminal(term) => {
                ui.horizontal(|ui| {
                    match &term.state {
                        ConnState::Connecting => {
                            ui.spinner();
                            ui.label("Conectando…");
                        }
                        ConnState::Connected => {
                            status_dot(ui, OK_COLOR);
                            ui.colored_label(OK_COLOR, "Conectado");
                        }
                        ConnState::Closed(_) => {
                            status_dot(ui, ui.visuals().error_fg_color);
                            ui.colored_label(ui.visuals().error_fg_color, "Desconectado");
                            if tab.session.is_some() && ui.button("⟳ Reconectar").clicked() {
                                reconnect = true;
                            }
                        }
                    }
                    if let Some(s) = &tab.session {
                        ui.weak(format!("{}@{}:{}", s.username, s.host, s.port));
                        if term.state == ConnState::Connected
                            && ui
                                .button("📂 SFTP")
                                .on_hover_text("Explorar ficheros sobre esta misma conexión")
                                .clicked()
                        {
                            open_sftp = Some((s.clone(), term.ssh.clone()));
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("A+")
                            .on_hover_text("Letra más grande")
                            .clicked()
                        {
                            term.font_size = (term.font_size + 1.0).min(32.0);
                        }
                        if ui
                            .small_button("A−")
                            .on_hover_text("Letra más pequeña")
                            .clicked()
                        {
                            term.font_size = (term.font_size - 1.0).max(8.0);
                        }
                    });
                });
                // Sin diálogos abiertos, el teclado es del terminal.
                egui::Frame::NONE.show(ui, |ui| term.ui(ui));
            }
            TabKind::Sftp(view) => view.ui(ui),
        });

        if reconnect && let Some(session) = self.tabs[self.active].session.clone() {
            let view = self.new_terminal(&session);
            self.tabs[self.active].kind = TabKind::Terminal(Box::new(view));
        }
        if let Some((session, handle)) = open_sftp {
            self.open_sftp(session, handle);
        }
    }

    // ------------------------------------------------------------------
    // Diálogos
    // ------------------------------------------------------------------

    fn vault_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.vault_dialog else {
            return;
        };
        let path = crate::paths::vault_file();
        let mut close = false;
        let mut opened: Option<Vault> = None;
        let mut changed = false;
        egui::Modal::new(egui::Id::unique("vault_dialog")).show(ctx, |ui| {
            ui.set_width(400.0);
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            match dialog {
                VaultDialog::Unlock {
                    password,
                    error,
                    focus,
                } => {
                    ui.heading("🔐 Desbloquear baúl");
                    ui.label(
                        "Introduce la contraseña maestra para usar las credenciales guardadas.",
                    );
                    let resp = ui.add(
                        egui::TextEdit::singleline(password)
                            .password(true)
                            .desired_width(f32::INFINITY),
                    );
                    // El foco se pide una sola vez (y tras cada intento fallido).
                    if *focus {
                        resp.request_focus();
                        *focus = false;
                    }
                    if let Some(e) = error {
                        ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Desbloquear").clicked() || enter {
                            match Vault::unlock(&path, password) {
                                Ok(v) => opened = Some(v),
                                Err(e) => {
                                    *error = Some(format!("{e:#}"));
                                    password.clear();
                                    *focus = true;
                                }
                            }
                        }
                        if ui.button("Continuar sin baúl").clicked() {
                            close = true;
                        }
                    });
                }
                VaultDialog::Create {
                    password,
                    confirm,
                    error,
                } => {
                    ui.heading("🔐 Crear baúl de credenciales");
                    ui.label(
                        "Elige una contraseña maestra. Con ella se cifran las contraseñas y \
                         passphrases guardadas (Argon2id + XChaCha20-Poly1305).",
                    );
                    ui.colored_label(
                        WARN_COLOR,
                        "Si la olvidas, los secretos guardados no se pueden recuperar.",
                    );
                    ui.add(
                        egui::TextEdit::singleline(password)
                            .password(true)
                            .hint_text("contraseña maestra")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add(
                        egui::TextEdit::singleline(confirm)
                            .password(true)
                            .hint_text("repetir")
                            .desired_width(f32::INFINITY),
                    );
                    if let Some(e) = error {
                        ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Crear baúl").clicked() || enter {
                            if password.is_empty() {
                                *error = Some("La contraseña no puede estar vacía".into());
                            } else if password != confirm {
                                *error = Some("Las contraseñas no coinciden".into());
                            } else {
                                match Vault::create(&path, password) {
                                    Ok(v) => opened = Some(v),
                                    Err(e) => *error = Some(format!("{e:#}")),
                                }
                            }
                        }
                        if ui.button("Ahora no").clicked() {
                            close = true;
                        }
                    });
                }
                VaultDialog::Change {
                    password,
                    confirm,
                    error,
                } => {
                    ui.heading("Cambiar contraseña maestra");
                    ui.add(
                        egui::TextEdit::singleline(password)
                            .password(true)
                            .hint_text("nueva contraseña maestra")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add(
                        egui::TextEdit::singleline(confirm)
                            .password(true)
                            .hint_text("repetir")
                            .desired_width(f32::INFINITY),
                    );
                    if let Some(e) = error {
                        ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Cambiar").clicked() || enter {
                            if password.is_empty() {
                                *error = Some("La contraseña no puede estar vacía".into());
                            } else if password != confirm {
                                *error = Some("Las contraseñas no coinciden".into());
                            } else {
                                match self.vault.as_mut().map(|v| v.change_master(password)) {
                                    Some(Ok(())) => changed = true,
                                    Some(Err(e)) => *error = Some(format!("{e:#}")),
                                    None => *error = Some("El baúl está bloqueado".into()),
                                }
                            }
                        }
                        if ui.button("Cancelar").clicked() {
                            close = true;
                        }
                    });
                }
            }
        });
        if let Some(v) = opened {
            self.vault = Some(v);
            self.vault_dialog = None;
            self.notify("Baúl desbloqueado", false);
        } else if changed {
            self.vault_dialog = None;
            self.notify("Contraseña maestra cambiada", false);
        } else if close {
            self.vault_dialog = None;
        }
    }

    fn prompt_ui(&mut self, ctx: &egui::Context) {
        while self.prompts.front().is_some_and(ActivePrompt::abandoned) {
            self.prompts.pop_front();
        }
        let vault_open = self.vault.is_some();
        let Some(prompt) = self.prompts.front_mut() else {
            return;
        };
        let mut done = false;
        egui::Modal::new(egui::Id::unique("ssh_prompt")).show(ctx, |ui| {
            ui.set_width(460.0);
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            match prompt {
                ActivePrompt::HostKey {
                    host,
                    port,
                    algorithm,
                    fingerprint,
                    reply,
                } => {
                    ui.heading("Host desconocido");
                    ui.label(format!(
                        "No se puede verificar la autenticidad de {host}:{port}. \
                         Es la primera vez que te conectas a este servidor."
                    ));
                    ui.add_space(4.0);
                    ui.label(format!("Huella de la clave {algorithm}:"));
                    ui.label(RichText::new(fingerprint.as_str()).monospace().strong());
                    ui.add_space(4.0);
                    ui.weak(
                        "Comprueba la huella con el administrador del servidor. Si confías, \
                         se guardará en known_hosts y se avisará si cambia.",
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Confiar y conectar").clicked() {
                            if let Some(r) = reply.take() {
                                let _ = r.send(true);
                            }
                            done = true;
                        }
                        if ui.button("Cancelar").clicked() || escape {
                            if let Some(r) = reply.take() {
                                let _ = r.send(false);
                            }
                            done = true;
                        }
                    });
                }
                ActivePrompt::Secret {
                    title,
                    message,
                    echo,
                    allow_save,
                    value,
                    save,
                    focused,
                    reply,
                } => {
                    ui.heading(title.as_str());
                    ui.label(message.as_str());
                    let resp = ui.add(
                        egui::TextEdit::singleline(value)
                            .password(!*echo)
                            .desired_width(f32::INFINITY),
                    );
                    if !*focused {
                        resp.request_focus();
                        *focused = true;
                    }
                    if *allow_save {
                        if vault_open {
                            ui.checkbox(save, "Guardar en el baúl cifrado");
                        } else {
                            ui.weak("Baúl bloqueado: no se guardará.");
                        }
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Aceptar").clicked() || enter {
                            if let Some(r) = reply.take() {
                                let _ = r.send(Some(SecretAnswer {
                                    value: std::mem::take(value),
                                    save: *save && *allow_save && vault_open,
                                }));
                            }
                            done = true;
                        }
                        if ui.button("Cancelar").clicked() || escape {
                            if let Some(r) = reply.take() {
                                let _ = r.send(None);
                            }
                            done = true;
                        }
                    });
                }
            }
        });
        if done {
            self.prompts.pop_front();
            self.focus_active_terminal();
        }
    }

    fn folder_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.folder_dialog else {
            return;
        };
        let mut close = false;
        let mut commit = false;
        egui::Modal::new(egui::Id::unique("folder_dialog")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.heading(if dialog.rename.is_some() {
                "Renombrar carpeta"
            } else {
                "Nueva carpeta"
            });
            let resp = ui.add(
                egui::TextEdit::singleline(&mut dialog.name)
                    .hint_text("nombre")
                    .desired_width(f32::INFINITY),
            );
            if !dialog.focused {
                resp.request_focus();
                dialog.focused = true;
            }
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let valid = !dialog.name.trim().is_empty();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(valid, egui::Button::new("Aceptar"))
                    .clicked()
                    || (enter && valid)
                {
                    commit = true;
                }
                if ui.button("Cancelar").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    close = true;
                }
            });
        });
        if commit {
            if let Some(dialog) = self.folder_dialog.take() {
                let name = dialog.name.trim().to_string();
                match dialog.rename {
                    Some(id) => {
                        if let Some(Node::Folder(f)) = self.tree.find_mut(id) {
                            f.name = name;
                        }
                    }
                    None => {
                        let id = self.tree.add_folder(dialog.parent, &name);
                        self.selected = Some(id);
                    }
                }
                self.save_tree();
            }
        } else if close {
            self.folder_dialog = None;
        }
    }

    fn confirm_delete_ui(&mut self, ctx: &egui::Context) {
        let Some(id) = self.confirm_delete else {
            return;
        };
        let Some(node) = self.tree.find(id) else {
            self.confirm_delete = None;
            return;
        };
        let name = node.name();
        let session_ids = Tree::session_ids(node);
        let is_folder = matches!(node, Node::Folder(_));
        let mut close = false;
        let mut delete = false;
        egui::Modal::new(egui::Id::unique("confirm_delete")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.heading("Eliminar");
            if is_folder {
                ui.label(format!(
                    "Se eliminará la carpeta «{name}» con las {} sesión(es) que contiene \
                     y sus credenciales guardadas.",
                    session_ids.len()
                ));
            } else {
                ui.label(format!(
                    "Se eliminará la sesión «{name}» y sus credenciales guardadas."
                ));
            }
            ui.weak("Esta acción no se puede deshacer.");
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new("Eliminar").color(ui.visuals().error_fg_color))
                    .clicked()
                {
                    delete = true;
                }
                if ui.button("Cancelar").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    close = true;
                }
            });
        });
        if delete {
            self.tree.remove(id);
            if let Some(vault) = &mut self.vault {
                for sid in session_ids {
                    let _ = vault.remove_prefix(&format!("{sid}/"));
                }
            }
            self.save_tree();
            self.confirm_delete = None;
            if self.selected == Some(id) {
                self.selected = None;
            }
        } else if close {
            self.confirm_delete = None;
        }
    }

    fn rdp_prompt_ui(&mut self, ctx: &egui::Context) {
        let vault_open = self.vault.is_some();
        let Some(prompt) = &mut self.rdp_prompt else {
            return;
        };
        let mut close = false;
        let mut launch = false;
        egui::Modal::new(egui::Id::unique("rdp_prompt")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.heading("Escritorio remoto");
            ui.label(format!(
                "Contraseña de {} en {}",
                if prompt.session.username.is_empty() {
                    "(usuario)"
                } else {
                    &prompt.session.username
                },
                prompt.session.host
            ));
            let resp = ui.add(
                egui::TextEdit::singleline(&mut prompt.password)
                    .password(true)
                    .desired_width(f32::INFINITY),
            );
            if !prompt.focused {
                resp.request_focus();
                prompt.focused = true;
            }
            if vault_open {
                ui.checkbox(&mut prompt.save, "Guardar en el baúl cifrado");
            } else {
                ui.weak("Baúl bloqueado: no se guardará.");
            }
            ui.weak(format!("Cliente: {}", crate::rdp::client_description()));
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let valid = !prompt.password.is_empty();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(valid, egui::Button::new("Conectar"))
                    .clicked()
                    || (enter && valid)
                {
                    launch = true;
                }
                if ui.button("Cancelar").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    close = true;
                }
            });
        });
        if launch {
            if let Some(prompt) = self.rdp_prompt.take() {
                // Solo se guarda si la sesión pertenece al árbol.
                if prompt.save && vault_open && self.tree.session(prompt.session.id).is_some() {
                    self.store_secrets(vec![(
                        prompt.session.password_key(),
                        prompt.password.clone(),
                    )]);
                }
                self.launch_rdp(&prompt.session, Some(&prompt.password));
            }
        } else if close {
            self.rdp_prompt = None;
        }
    }
}

/// Resume la salida de FreeRDP: las primeras líneas de error, sin prefijos.
fn rdp_error_summary(log: &str) -> String {
    let mut out: Vec<&str> = vec![];
    for line in log.lines().filter(|l| l.contains("[ERROR]")) {
        // Formato: "[hora] [pid] [ERROR][módulo] - [función]: mensaje"
        let msg = line.rsplit_once("]: ").map_or(line, |(_, m)| m).trim();
        if !msg.is_empty() && !out.contains(&msg) {
            out.push(msg);
        }
        if out.len() == 2 {
            break;
        }
    }
    out.join(" · ")
}

/// Punto de estado (verde / ámbar / rojo).
fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

fn collect_matches<'a>(nodes: &'a [Node], needle: &str, out: &mut Vec<&'a Session>) {
    for n in nodes {
        match n {
            Node::Folder(f) => collect_matches(&f.children, needle, out),
            Node::Session(s) => {
                if s.display_name().to_lowercase().contains(needle)
                    || s.host.to_lowercase().contains(needle)
                    || s.username.to_lowercase().contains(needle)
                {
                    out.push(s);
                }
            }
        }
    }
}

fn move_menu(
    ui: &mut egui::Ui,
    id: Uuid,
    folders: &[(Uuid, String)],
    actions: &mut Vec<TreeAction>,
) {
    ui.menu_button("Mover a", |ui| {
        if ui.button("(raíz)").clicked() {
            actions.push(TreeAction::Move(id, None));
            ui.close();
        }
        for (fid, path) in folders {
            if *fid != id && ui.button(path).clicked() {
                actions.push(TreeAction::Move(id, Some(*fid)));
                ui.close();
            }
        }
    });
}

fn session_row(
    ui: &mut egui::Ui,
    s: &Session,
    selected: Option<Uuid>,
    folders: &[(Uuid, String)],
    actions: &mut Vec<TreeAction>,
) {
    let label = format!("{} {}", s.protocol.icon(), s.display_name());
    let resp = ui
        .selectable_label(selected == Some(s.id), label)
        .on_hover_text(format!(
            "{} · {}{}:{}{}",
            s.protocol.label(),
            if s.username.is_empty() {
                String::new()
            } else {
                format!("{}@", s.username)
            },
            s.host,
            s.port,
            if s.protocol != Protocol::Rdp && s.auth != AuthMethod::Password {
                format!(" · {}", s.auth.label())
            } else {
                String::new()
            }
        ));
    if resp.double_clicked() {
        actions.push(TreeAction::Connect(s.id));
    } else if resp.clicked() {
        actions.push(TreeAction::Select(s.id));
    }
    resp.context_menu(|ui| {
        if ui.button("Conectar").clicked() {
            actions.push(TreeAction::Connect(s.id));
            ui.close();
        }
        match s.protocol {
            Protocol::Ssh => {
                if ui.button("📂 Abrir SFTP").clicked() {
                    actions.push(TreeAction::OpenAs(s.id, Protocol::Sftp));
                    ui.close();
                }
            }
            Protocol::Sftp => {
                if ui.button("💻 Abrir terminal SSH").clicked() {
                    actions.push(TreeAction::OpenAs(s.id, Protocol::Ssh));
                    ui.close();
                }
            }
            Protocol::Rdp | Protocol::Vnc | Protocol::Telnet | Protocol::Serial => {}
        }
        ui.separator();
        if ui.button("✏ Editar…").clicked() {
            actions.push(TreeAction::Edit(s.id));
            ui.close();
        }
        if ui.button("Duplicar").clicked() {
            actions.push(TreeAction::Duplicate(s.id));
            ui.close();
        }
        move_menu(ui, s.id, folders, actions);
        ui.separator();
        if ui.button("🗑 Eliminar…").clicked() {
            actions.push(TreeAction::Delete(s.id));
            ui.close();
        }
    });
}

fn tree_ui(
    ui: &mut egui::Ui,
    nodes: &[Node],
    selected: Option<Uuid>,
    folders: &[(Uuid, String)],
    actions: &mut Vec<TreeAction>,
) {
    for node in nodes {
        match node {
            Node::Folder(f) => {
                let title = if selected == Some(f.id) {
                    RichText::new(format!("📁 {}", f.name)).strong()
                } else {
                    RichText::new(format!("📁 {}", f.name))
                };
                let header = egui::CollapsingHeader::new(title)
                    .id_salt(f.id)
                    .default_open(true)
                    .show(ui, |ui| {
                        if f.children.is_empty() {
                            ui.weak("(vacía)");
                        }
                        tree_ui(ui, &f.children, selected, folders, actions);
                    });
                if header.header_response.clicked() {
                    actions.push(TreeAction::Select(f.id));
                }
                header.header_response.context_menu(|ui| {
                    if ui.button("➕ Nueva sesión aquí").clicked() {
                        actions.push(TreeAction::NewSession(Some(f.id)));
                        ui.close();
                    }
                    if ui.button("📁 Nueva subcarpeta").clicked() {
                        actions.push(TreeAction::NewFolder(Some(f.id)));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("✏ Renombrar…").clicked() {
                        actions.push(TreeAction::RenameFolder(f.id));
                        ui.close();
                    }
                    move_menu(ui, f.id, folders, actions);
                    ui.separator();
                    if ui.button("🗑 Eliminar…").clicked() {
                        actions.push(TreeAction::Delete(f.id));
                        ui.close();
                    }
                });
            }
            Node::Session(s) => session_row(ui, s, selected, folders, actions),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // --- Eventos de fondo ---------------------------------------------
        while let Ok(req) = self.prompt_rx.try_recv() {
            self.prompts.push_back(match req {
                PromptRequest::HostKey {
                    host,
                    port,
                    algorithm,
                    fingerprint,
                    reply,
                } => ActivePrompt::HostKey {
                    host,
                    port,
                    algorithm,
                    fingerprint,
                    reply: Some(reply),
                },
                PromptRequest::Secret {
                    title,
                    message,
                    echo,
                    allow_save,
                    reply,
                } => ActivePrompt::Secret {
                    title,
                    message,
                    echo,
                    allow_save,
                    value: String::new(),
                    save: allow_save && self.vault.is_some(),
                    focused: false,
                    reply: Some(reply),
                },
            });
        }
        while let Ok((msg, is_error)) = self.notice_rx.try_recv() {
            self.notify(msg, is_error);
        }
        // Las pestañas en segundo plano también procesan sus eventos.
        let mut secrets = vec![];
        for tab in &mut self.tabs {
            match &mut tab.kind {
                TabKind::Terminal(t) => {
                    t.poll();
                    secrets.append(&mut t.secrets_to_save);
                }
                TabKind::Sftp(s) => {
                    s.poll();
                    secrets.append(&mut s.secrets_to_save);
                }
            }
        }
        // Solo se guardan secretos de sesiones que existen en el árbol.
        secrets.retain(|(key, _)| {
            key.split('/')
                .next()
                .and_then(|id| Uuid::parse_str(id).ok())
                .is_some_and(|id| self.tree.session(id).is_some())
        });
        self.store_secrets(secrets);

        // --- Disposición --------------------------------------------------
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(3.0);
            self.toolbar(ui);
            ui.add_space(3.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(250.0)
            .size_range(160.0..=480.0)
            .show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            if self.tabs.is_empty() {
                self.welcome(ui);
            } else {
                self.tab_bar(ui);
                ui.separator();
                self.active_tab_ui(ui);
            }
        });

        // --- Ventanas y diálogos ------------------------------------------
        let ctx = ui.ctx().clone();
        if let Some(editor) = &mut self.editor {
            match editor.show(&ctx, &mut self.tree, &mut self.vault) {
                EditorResult::Open => {}
                EditorResult::Cancelled => self.editor = None,
                EditorResult::Saved { connect } => {
                    let session = editor.session.clone();
                    self.selected = Some(session.id);
                    self.editor = None;
                    self.save_tree();
                    if connect {
                        self.connect(session, None);
                    }
                }
            }
        }
        if let Some(win) = &mut self.keys_window
            && !win.show(&ctx)
        {
            self.keys_window = None;
        }
        self.folder_dialog_ui(&ctx);
        self.confirm_delete_ui(&ctx);
        self.rdp_prompt_ui(&ctx);
        self.vault_dialog_ui(&ctx);
        self.prompt_ui(&ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::rdp_error_summary;

    #[test]
    fn freerdp_log_summary() {
        let log = "[12:36:09:851] [1:2] [WARN][com.freerdp.client.x11] - [f]: aviso\n\
                   [12:36:09:872] [1:2] [ERROR][com.freerdp.core] - [get_next_addrinfo]: ERRCONNECT_CONNECT_FAILED [0x00020006]\n\
                   [12:36:09:872] [1:2] [ERROR][com.freerdp.core] - [get_next_addrinfo]: ERRCONNECT_CONNECT_FAILED [0x00020006]\n\
                   [12:36:09:872] [1:2] [ERROR][com.freerdp.core.nego] - [nego_connect]: Failed to connect\n\
                   [12:36:09:873] [1:2] [ERROR][x] - [y]: tercero\n";
        assert_eq!(
            rdp_error_summary(log),
            "ERRCONNECT_CONNECT_FAILED [0x00020006] · Failed to connect"
        );
        assert_eq!(rdp_error_summary("sin errores"), "");
    }
}
