//! Explorador SFTP de dos paneles (local | remoto) con cola de transferencias.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use egui::RichText;

use super::util::{format_size, format_time};
use crate::sftp::{RemoteEntry, SftpClient, SftpCmd, SftpEvent, join_remote, parent_remote};

#[derive(Debug, Clone, PartialEq)]
pub enum SftpState {
    Connecting(String),
    Ready,
    Closed(Option<String>),
}

#[derive(Clone)]
struct LocalEntry {
    name: String,
    is_dir: bool,
    size: u64,
    mtime: Option<i64>,
}

enum TransferState {
    Running,
    Done,
    Failed(String),
}

struct TransferRow {
    id: u64,
    upload: bool,
    label: String,
    done: u64,
    total: u64,
    current: String,
    state: TransferState,
}

enum Dialog {
    Mkdir {
        remote: bool,
        name: String,
        focused: bool,
    },
    Rename {
        old: String,
        name: String,
        focused: bool,
    },
    ConfirmDelete {
        names: Vec<(String, bool)>,
    },
    /// Transferencias pendientes de confirmar porque sobrescriben destino.
    ConfirmOverwrite {
        upload: bool,
        names: Vec<String>,
        existing: Vec<String>,
    },
}

/// Acción diferida al cerrar un diálogo.
type AfterDialog = Box<dyn FnOnce(&mut SftpView)>;

pub struct SftpView {
    client: SftpClient,
    pub state: SftpState,
    remote_path: String,
    remote_edit: String,
    remote_entries: Vec<RemoteEntry>,
    remote_sel: BTreeSet<String>,
    remote_loading: bool,
    local_path: PathBuf,
    local_edit: String,
    local_entries: Vec<LocalEntry>,
    local_sel: BTreeSet<String>,
    local_error: Option<String>,
    transfers: Vec<TransferRow>,
    next_id: u64,
    dialog: Option<Dialog>,
    message: Option<(String, bool)>,
    show_hidden: bool,
    pub secrets_to_save: Vec<(String, String)>,
}

fn read_local(path: &Path) -> std::io::Result<Vec<LocalEntry>> {
    let mut out = vec![];
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        // `metadata` sigue enlaces: un enlace a carpeta se navega como carpeta.
        let meta = std::fs::metadata(entry.path()).or_else(|_| entry.metadata());
        let (is_dir, size, mtime) = match meta {
            Ok(m) => (
                m.is_dir(),
                m.len(),
                m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64),
            ),
            Err(_) => (false, 0, None),
        };
        out.push(LocalEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            is_dir,
            size,
            mtime,
        });
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

/// Resultado de pintar una lista de ficheros.
#[derive(Default)]
struct ListAction {
    /// Doble clic sobre (nombre, es_carpeta).
    activated: Option<(String, bool)>,
}

struct Row<'a> {
    name: &'a str,
    is_dir: bool,
    is_link: bool,
    size: u64,
    mtime: Option<i64>,
}

fn file_list(
    ui: &mut egui::Ui,
    id: &str,
    rows: &[Row<'_>],
    selection: &mut BTreeSet<String>,
) -> ListAction {
    let mut action = ListAction::default();
    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show_rows(ui, row_height, rows.len(), |ui, range| {
            for row in &rows[range] {
                let selected = selection.contains(row.name);
                let icon = match (row.is_dir, row.is_link) {
                    (true, _) => "📁",
                    (false, true) => "🔗",
                    (false, false) => "📄",
                };
                let width = ui.available_width();
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(width, row_height), egui::Sense::click());
                if selected {
                    ui.painter()
                        .rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
                } else if response.hovered() {
                    ui.painter()
                        .rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
                }
                let text_color = if selected {
                    ui.visuals().strong_text_color()
                } else {
                    ui.visuals().text_color()
                };
                let font = egui::TextStyle::Body.resolve(ui.style());
                let meta_w = 190.0_f32.min(width * 0.45);
                // Nombre (recortado al ancho disponible).
                let name_rect = egui::Rect::from_min_max(
                    rect.min + egui::vec2(6.0, 0.0),
                    egui::pos2(rect.max.x - meta_w, rect.max.y),
                );
                ui.painter().with_clip_rect(name_rect).text(
                    egui::pos2(name_rect.min.x, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("{icon} {}", row.name),
                    font.clone(),
                    text_color,
                );
                // Tamaño y fecha, alineados a la derecha.
                let meta = format!(
                    "{}   {}",
                    if row.is_dir {
                        String::new()
                    } else {
                        format_size(row.size)
                    },
                    row.mtime.map(format_time).unwrap_or_default()
                );
                ui.painter().text(
                    egui::pos2(rect.max.x - 6.0, rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    meta,
                    egui::FontId::proportional(font.size - 1.5),
                    ui.visuals().weak_text_color(),
                );

                if response.double_clicked() {
                    action.activated = Some((row.name.to_string(), row.is_dir));
                } else if response.clicked() {
                    let multi = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
                    if multi {
                        if !selection.remove(row.name) {
                            selection.insert(row.name.to_string());
                        }
                    } else {
                        selection.clear();
                        selection.insert(row.name.to_string());
                    }
                }
            }
        });
    action
}

impl SftpView {
    pub fn new(client: SftpClient) -> Self {
        let local_path = crate::paths::home_dir();
        let mut view = Self {
            client,
            state: SftpState::Connecting("Conectando…".into()),
            remote_path: String::new(),
            remote_edit: String::new(),
            remote_entries: vec![],
            remote_sel: BTreeSet::new(),
            remote_loading: false,
            local_edit: local_path.display().to_string(),
            local_path,
            local_entries: vec![],
            local_sel: BTreeSet::new(),
            local_error: None,
            transfers: vec![],
            next_id: 1,
            dialog: None,
            message: None,
            show_hidden: false,
            secrets_to_save: vec![],
        };
        view.refresh_local();
        view
    }

    fn refresh_local(&mut self) {
        match read_local(&self.local_path) {
            Ok(entries) => {
                self.local_entries = entries;
                self.local_error = None;
            }
            Err(e) => {
                self.local_entries.clear();
                self.local_error = Some(e.to_string());
            }
        }
        let names: BTreeSet<&str> = self.local_entries.iter().map(|e| e.name.as_str()).collect();
        self.local_sel.retain(|n| names.contains(n.as_str()));
    }

    fn go_local(&mut self, path: PathBuf) {
        match read_local(&path) {
            Ok(entries) => {
                self.local_path = path.canonicalize().unwrap_or(path);
                self.local_edit = self.local_path.display().to_string();
                self.local_entries = entries;
                self.local_sel.clear();
                self.local_error = None;
            }
            Err(e) => {
                self.local_edit = self.local_path.display().to_string();
                self.message = Some((format!("No se puede abrir {}: {e}", path.display()), true));
            }
        }
    }

    fn list_remote(&mut self, path: String) {
        self.remote_loading = true;
        let _ = self.client.cmd.send(SftpCmd::List(path));
    }

    fn refresh_remote(&mut self) {
        if self.state == SftpState::Ready {
            self.list_remote(self.remote_path.clone());
        }
    }

    pub fn poll(&mut self) {
        let mut refresh_remote = false;
        let mut refresh_local = false;
        while let Ok(ev) = self.client.events.try_recv() {
            match ev {
                SftpEvent::Status(s) => {
                    if matches!(self.state, SftpState::Connecting(_)) {
                        self.state = SftpState::Connecting(s);
                    }
                }
                SftpEvent::Ready { home } => {
                    self.state = SftpState::Ready;
                    self.remote_path = home.clone();
                    self.remote_edit = home.clone();
                    self.list_remote(home);
                }
                SftpEvent::Listing { path, entries } => {
                    self.remote_loading = false;
                    if path != self.remote_path {
                        self.remote_sel.clear();
                    }
                    self.remote_edit = path.clone();
                    self.remote_path = path;
                    self.remote_entries = entries;
                    let names: BTreeSet<&str> = self
                        .remote_entries
                        .iter()
                        .map(|e| e.name.as_str())
                        .collect();
                    self.remote_sel.retain(|n| names.contains(n.as_str()));
                }
                SftpEvent::Done(msg) => {
                    self.message = Some((msg, false));
                    refresh_remote = true;
                }
                SftpEvent::Progress {
                    id,
                    done,
                    total,
                    current,
                } => {
                    if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                        t.done = done;
                        t.total = total;
                        t.current = current;
                    }
                }
                SftpEvent::TransferFinished { id, error } => {
                    if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                        if t.upload {
                            refresh_remote = true;
                        } else {
                            refresh_local = true;
                        }
                        t.state = match error {
                            None => TransferState::Done,
                            Some(e) => TransferState::Failed(e),
                        };
                    }
                }
                SftpEvent::Error(e) => {
                    self.remote_loading = false;
                    self.remote_edit = self.remote_path.clone();
                    self.message = Some((e, true));
                }
                SftpEvent::SaveSecrets(s) => self.secrets_to_save.extend(s),
                SftpEvent::Closed(err) => self.state = SftpState::Closed(err),
            }
        }
        if refresh_remote {
            self.refresh_remote();
        }
        if refresh_local {
            self.refresh_local();
        }
    }

    fn start_transfers(&mut self, upload: bool, names: &[String]) {
        for name in names {
            let id = self.next_id;
            self.next_id += 1;
            let local = self.local_path.join(name);
            let remote = join_remote(&self.remote_path, name);
            let cmd = if upload {
                SftpCmd::Upload { id, local, remote }
            } else {
                SftpCmd::Download { id, remote, local }
            };
            self.transfers.push(TransferRow {
                id,
                upload,
                label: name.clone(),
                done: 0,
                total: 0,
                current: String::new(),
                state: TransferState::Running,
            });
            let _ = self.client.cmd.send(cmd);
        }
    }

    /// Pide confirmación si alguna transferencia sobrescribe el destino.
    fn request_transfer(&mut self, upload: bool, names: Vec<String>) {
        if names.is_empty() || self.state != SftpState::Ready {
            return;
        }
        let existing: Vec<String> = names
            .iter()
            .filter(|n| {
                if upload {
                    self.remote_entries.iter().any(|e| &e.name == *n)
                } else {
                    self.local_path.join(n).exists()
                }
            })
            .cloned()
            .collect();
        if existing.is_empty() {
            self.start_transfers(upload, &names);
        } else {
            self.dialog = Some(Dialog::ConfirmOverwrite {
                upload,
                names,
                existing,
            });
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll();

        match &self.state {
            SftpState::Connecting(msg) => {
                ui.centered_and_justified(|ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(msg);
                    });
                });
                return;
            }
            SftpState::Closed(err) => {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| match err {
                    Some(e) => {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            format!("Conexión cerrada: {e}"),
                        );
                    }
                    None => {
                        ui.label("Conexión SFTP cerrada.");
                    }
                });
                return;
            }
            SftpState::Ready => {}
        }

        // --- Cola de transferencias (abajo) --------------------------------
        if !self.transfers.is_empty() {
            egui::Panel::bottom("sftp_transfers")
                .resizable(true)
                .default_size(110.0)
                .show(ui, |ui| self.transfers_ui(ui));
        }
        if let Some((msg, is_error)) = self.message.clone() {
            egui::Panel::bottom("sftp_message").show(ui, |ui| {
                ui.horizontal(|ui| {
                    let color = if is_error {
                        ui.visuals().error_fg_color
                    } else {
                        ui.visuals().weak_text_color()
                    };
                    ui.colored_label(color, msg);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").clicked() {
                            self.message = None;
                        }
                    });
                });
            });
        }

        // --- Paneles ------------------------------------------------------
        egui::CentralPanel::no_frame().show(ui, |ui| {
            ui.columns(2, |cols| {
                cols[0].push_id("local", |ui| self.local_pane(ui));
                cols[1].push_id("remote", |ui| self.remote_pane(ui));
            });
        });

        self.dialog_ui(ui.ctx());
    }

    fn local_pane(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Equipo local");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let names: Vec<String> = self.local_sel.iter().cloned().collect();
                if ui
                    .add_enabled(!names.is_empty(), egui::Button::new("Subir ➡"))
                    .on_hover_text("Subir la selección a la carpeta remota")
                    .clicked()
                {
                    self.request_transfer(true, names);
                }
            });
        });
        ui.horizontal(|ui| {
            if ui.button("⬆").on_hover_text("Carpeta superior").clicked()
                && let Some(parent) = self.local_path.parent()
            {
                self.go_local(parent.to_path_buf());
            }
            if ui.button("⟳").on_hover_text("Actualizar").clicked() {
                self.refresh_local();
            }
            if ui.button("➕📁").on_hover_text("Nueva carpeta").clicked() {
                self.dialog = Some(Dialog::Mkdir {
                    remote: false,
                    name: String::new(),
                    focused: false,
                });
            }
            let resp = ui
                .add(egui::TextEdit::singleline(&mut self.local_edit).desired_width(f32::INFINITY));
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.go_local(crate::paths::expand_tilde(&self.local_edit));
            }
        });
        ui.separator();
        if let Some(err) = &self.local_error {
            ui.colored_label(ui.visuals().error_fg_color, err);
        }
        let show_hidden = self.show_hidden;
        let rows: Vec<Row<'_>> = self
            .local_entries
            .iter()
            .filter(|e| show_hidden || !e.name.starts_with('.'))
            .map(|e| Row {
                name: &e.name,
                is_dir: e.is_dir,
                is_link: false,
                size: e.size,
                mtime: e.mtime,
            })
            .collect();
        let action = file_list(ui, "local_list", &rows, &mut self.local_sel);
        if let Some((name, is_dir)) = action.activated {
            if is_dir {
                self.go_local(self.local_path.join(name));
            } else {
                self.request_transfer(true, vec![name]);
            }
        }
    }

    fn remote_pane(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Servidor remoto");
            if self.remote_loading {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let names: Vec<String> = self.remote_sel.iter().cloned().collect();
                if ui
                    .add_enabled(!names.is_empty(), egui::Button::new("⬅ Descargar"))
                    .on_hover_text("Descargar la selección a la carpeta local")
                    .clicked()
                {
                    self.request_transfer(false, names);
                }
                ui.checkbox(&mut self.show_hidden, "Ocultos");
            });
        });
        ui.horizontal(|ui| {
            if ui.button("⬆").on_hover_text("Carpeta superior").clicked() {
                self.list_remote(parent_remote(&self.remote_path));
            }
            if ui.button("⟳").on_hover_text("Actualizar").clicked() {
                self.refresh_remote();
            }
            if ui.button("➕📁").on_hover_text("Nueva carpeta").clicked() {
                self.dialog = Some(Dialog::Mkdir {
                    remote: true,
                    name: String::new(),
                    focused: false,
                });
            }
            let one = (self.remote_sel.len() == 1)
                .then(|| self.remote_sel.iter().next().cloned())
                .flatten();
            if ui
                .add_enabled(one.is_some(), egui::Button::new("✏"))
                .on_hover_text("Renombrar")
                .clicked()
                && let Some(old) = one
            {
                self.dialog = Some(Dialog::Rename {
                    name: old.clone(),
                    old,
                    focused: false,
                });
            }
            if ui
                .add_enabled(!self.remote_sel.is_empty(), egui::Button::new("🗑"))
                .on_hover_text("Eliminar")
                .clicked()
            {
                let names = self
                    .remote_entries
                    .iter()
                    .filter(|e| self.remote_sel.contains(&e.name))
                    // Un enlace a carpeta se borra como fichero (no se sigue).
                    .map(|e| (e.name.clone(), e.is_dir && !e.is_symlink))
                    .collect();
                self.dialog = Some(Dialog::ConfirmDelete { names });
            }
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.remote_edit).desired_width(f32::INFINITY),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.list_remote(self.remote_edit.trim().to_string());
            }
        });
        ui.separator();
        let show_hidden = self.show_hidden;
        let rows: Vec<Row<'_>> = self
            .remote_entries
            .iter()
            .filter(|e| show_hidden || !e.name.starts_with('.'))
            .map(|e| Row {
                name: &e.name,
                is_dir: e.is_dir,
                is_link: e.is_symlink,
                size: e.size,
                mtime: e.mtime.map(i64::from),
            })
            .collect();
        let action = file_list(ui, "remote_list", &rows, &mut self.remote_sel);
        if let Some((name, is_dir)) = action.activated {
            if is_dir {
                self.list_remote(join_remote(&self.remote_path, &name));
            } else {
                self.request_transfer(false, vec![name]);
            }
        }
    }

    fn transfers_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Transferencias");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Limpiar terminadas").clicked() {
                    self.transfers
                        .retain(|t| matches!(t.state, TransferState::Running));
                }
            });
        });
        let mut cancel = None;
        egui::ScrollArea::vertical()
            .id_salt("transfer_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for t in self.transfers.iter().rev() {
                    ui.horizontal(|ui| {
                        ui.label(if t.upload { "⬆" } else { "⬇" });
                        ui.allocate_ui_with_layout(
                            egui::vec2(200.0, 18.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_min_width(200.0);
                                ui.add(egui::Label::new(&t.label).truncate());
                            },
                        );
                        match &t.state {
                            TransferState::Running => {
                                let frac = if t.total > 0 {
                                    t.done as f32 / t.total as f32
                                } else {
                                    0.0
                                };
                                ui.add(egui::ProgressBar::new(frac).desired_width(220.0).text(
                                    format!("{} / {}", format_size(t.done), format_size(t.total)),
                                ));
                                if ui.small_button("Cancelar").clicked() {
                                    cancel = Some(t.id);
                                }
                                ui.add(
                                    egui::Label::new(RichText::new(&t.current).weak()).truncate(),
                                );
                            }
                            TransferState::Done => {
                                ui.colored_label(
                                    crate::theme::ok_color(),
                                    format!("Completada ({})", format_size(t.done)),
                                );
                            }
                            TransferState::Failed(e) => {
                                ui.colored_label(
                                    ui.visuals().error_fg_color,
                                    format!("Error: {e}"),
                                );
                            }
                        }
                    });
                }
            });
        if let Some(id) = cancel {
            let _ = self.client.cmd.send(SftpCmd::Cancel(id));
        }
    }

    fn dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        let mut close = false;
        let mut after: Option<AfterDialog> = None;
        egui::Modal::new(egui::Id::unique("sftp_dialog")).show(ctx, |ui| {
            ui.set_width(380.0);
            match dialog {
                Dialog::Mkdir {
                    remote,
                    name,
                    focused,
                } => {
                    ui.heading("Nueva carpeta");
                    ui.label(if *remote {
                        "En el servidor remoto:"
                    } else {
                        "En el equipo local:"
                    });
                    let resp = ui.text_edit_singleline(name);
                    if !*focused {
                        resp.request_focus();
                        *focused = true;
                    }
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        let valid = !name.trim().is_empty() && !name.contains(['/', '\\']);
                        if ui.add_enabled(valid, egui::Button::new("Crear")).clicked()
                            || (enter && valid)
                        {
                            let (remote, name) = (*remote, name.trim().to_string());
                            after = Some(Box::new(move |this| {
                                if remote {
                                    let path = join_remote(&this.remote_path, &name);
                                    let _ = this.client.cmd.send(SftpCmd::Mkdir(path));
                                } else {
                                    match std::fs::create_dir(this.local_path.join(&name)) {
                                        Ok(()) => this.refresh_local(),
                                        Err(e) => {
                                            this.message =
                                                Some((format!("No se pudo crear: {e}"), true))
                                        }
                                    }
                                }
                            }));
                            close = true;
                        }
                        if ui.button("Cancelar").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::Rename { old, name, focused } => {
                    ui.heading("Renombrar");
                    ui.label(format!("Nuevo nombre para «{old}»:"));
                    let resp = ui.text_edit_singleline(name);
                    if !*focused {
                        resp.request_focus();
                        *focused = true;
                    }
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        let valid = !name.trim().is_empty()
                            && !name.contains('/')
                            && name.trim() != old.as_str();
                        if ui
                            .add_enabled(valid, egui::Button::new("Renombrar"))
                            .clicked()
                            || (enter && valid)
                        {
                            let (old, new) = (old.clone(), name.trim().to_string());
                            after = Some(Box::new(move |this| {
                                let _ = this.client.cmd.send(SftpCmd::Rename {
                                    from: join_remote(&this.remote_path, &old),
                                    to: join_remote(&this.remote_path, &new),
                                });
                            }));
                            close = true;
                        }
                        if ui.button("Cancelar").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::ConfirmDelete { names } => {
                    ui.heading("Eliminar del servidor");
                    ui.label(format!(
                        "Se eliminarán definitivamente {} elemento(s) del servidor remoto:",
                        names.len()
                    ));
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for (name, is_dir) in names.iter() {
                                ui.label(format!(
                                    "{} {name}{}",
                                    if *is_dir { "📁" } else { "📄" },
                                    if *is_dir {
                                        "  (con todo su contenido)"
                                    } else {
                                        ""
                                    }
                                ));
                            }
                        });
                    ui.horizontal(|ui| {
                        if ui
                            .button(RichText::new("Eliminar").color(ui.visuals().error_fg_color))
                            .clicked()
                        {
                            let names = names.clone();
                            after = Some(Box::new(move |this| {
                                for (name, is_dir) in names {
                                    let _ = this.client.cmd.send(SftpCmd::Remove {
                                        path: join_remote(&this.remote_path, &name),
                                        is_dir,
                                    });
                                }
                                this.remote_sel.clear();
                            }));
                            close = true;
                        }
                        if ui.button("Cancelar").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::ConfirmOverwrite {
                    upload,
                    names,
                    existing,
                } => {
                    ui.heading("Sobrescribir");
                    ui.label(if *upload {
                        "Ya existen en el servidor y se sobrescribirán:"
                    } else {
                        "Ya existen en el equipo local y se sobrescribirán:"
                    });
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for name in existing.iter() {
                                ui.label(format!("• {name}"));
                            }
                        });
                    ui.horizontal(|ui| {
                        if ui.button("Sobrescribir").clicked() {
                            let (upload, names) = (*upload, names.clone());
                            after =
                                Some(Box::new(move |this| this.start_transfers(upload, &names)));
                            close = true;
                        }
                        if existing.len() < names.len() && ui.button("Omitir existentes").clicked()
                        {
                            let upload = *upload;
                            let rest: Vec<String> = names
                                .iter()
                                .filter(|n| !existing.contains(n))
                                .cloned()
                                .collect();
                            after = Some(Box::new(move |this| this.start_transfers(upload, &rest)));
                            close = true;
                        }
                        if ui.button("Cancelar").clicked() {
                            close = true;
                        }
                    });
                }
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
        });
        if close {
            self.dialog = None;
        }
        if let Some(f) = after {
            f(self);
        }
    }
}
