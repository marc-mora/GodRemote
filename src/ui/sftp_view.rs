//! Explorador SFTP de dos paneles (local | remoto) con cola de transferencias.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};

use egui::RichText;

use super::util::{format_size, format_time};
use crate::i18n::tr;
use crate::sftp::{
    Conflict, ConflictAnswer, ConflictChoice, ConflictMemory, OnConflict, RemoteEntry, SftpClient,
    SftpCmd, SftpEvent, SyncItem, SyncMode, SyncOp, SyncScan, SyncState, SyncSummary,
    TransferStats, join_remote, mode_octal, mode_text, parent_remote, parse_mode, plan_sync,
};
use crate::{mark, trf};

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
    Done(TransferStats),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq)]
enum TransferKind {
    Upload,
    Download,
    Sync,
}

struct TransferRow {
    id: u64,
    kind: TransferKind,
    label: String,
    done: u64,
    total: u64,
    current: String,
    state: TransferState,
    /// Lote al que pertenece (véase [`Batch`]).
    batch: u64,
}

/// Transferencias lanzadas de una vez (una selección, varios ficheros
/// soltados…): para el usuario son una sola, así que comparten las
/// respuestas «aplicar a todos».
#[derive(Default)]
struct Batch {
    size: usize,
    memory: ConflictMemory,
}

/// Lo que se va a transferir: ruta local, ruta remota y nombre a mostrar.
type TransferItem = (PathBuf, String, String);

/// Comparación de las dos carpetas y plan para sincronizarlas.
struct SyncDialog {
    /// Identificador de la comparación: para cancelarla y reconocer su
    /// resultado.
    id: u64,
    local: PathBuf,
    remote: String,
    /// Carpeta que se está recorriendo.
    current: String,
    scan: Option<SyncScan>,
    error: Option<String>,
    mode: SyncMode,
    delete: bool,
    plan: Vec<SyncItem>,
    summary: SyncSummary,
    /// Ficheros y carpetas que sobran en el destino: lo que borraría la
    /// casilla.
    extra: (u32, u32),
    /// Elementos del plan que se enseñan en la lista (los iguales no).
    listed: Vec<usize>,
}

impl SyncDialog {
    fn replan(&mut self) {
        let Some(scan) = &self.scan else {
            return;
        };
        self.delete &= self.mode != SyncMode::Both;
        self.plan = plan_sync(scan, self.mode, self.delete);
        self.summary = SyncSummary::of(&self.plan);
        let with_delete = SyncSummary::of(&plan_sync(scan, self.mode, true));
        self.extra = (with_delete.delete_files, with_delete.delete_dirs);
        self.listed = (0..self.plan.len())
            .filter(|&i| {
                let item = &self.plan[i];
                match item.state {
                    SyncState::Equal => false,
                    // Las carpetas, solo si se hace algo con ellas o hay
                    // que explicar por qué no.
                    _ if item.is_dir => item.op != SyncOp::None || item.state == SyncState::Ignored,
                    _ => true,
                }
            })
            .collect();
    }
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
    Permissions {
        name: String,
        /// Carpeta de verdad (no un enlace): se puede aplicar al contenido.
        is_dir: bool,
        /// Se conocen los permisos actuales.
        known: bool,
        mode: u32,
        /// El valor tal como está escrito (puede no ser válido aún).
        text: String,
        recursive: bool,
    },
    Sync(Box<SyncDialog>),
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
    batches: HashMap<u64, Batch>,
    next_id: u64,
    dialog: Option<Dialog>,
    /// Transferencias que esperan a saber qué hacer con un destino que ya
    /// existe; se pregunta por la primera.
    conflicts: VecDeque<Conflict>,
    /// Casilla «hacer lo mismo con los demás» de esa pregunta.
    conflict_all: bool,
    message: Option<(String, bool)>,
    show_hidden: bool,
    pub secrets_to_save: Vec<(String, String)>,
    /// Carpeta inicial del usuario en el servidor (para expandir `~`).
    home: String,
    /// Ficheros remotos abiertos en un editor local.
    edited: Vec<Edited>,
    last_edit_check: std::time::Instant,
    /// Orden del editor (vacía = el programa asociado en el sistema). La
    /// fija la aplicación a partir de los ajustes.
    pub editor: String,
    /// La vista se abrió desde un terminal y puede seguir su carpeta.
    pub linked: bool,
    /// Ir a la carpeta en la que esté el terminal.
    pub follow: bool,
    /// Última carpeta del terminal a la que se ha ido.
    followed: Option<String>,
}

/// Un fichero remoto que se está editando en local: se descarga a una
/// carpeta temporal, se abre con el editor y se sube cada vez que cambia.
struct Edited {
    name: String,
    local: PathBuf,
    remote: String,
    /// Fecha y tamaño de la copia local la última vez que se subió.
    seen: Option<(std::time::SystemTime, u64)>,
    /// Transferencias en curso de este fichero.
    download: Option<u64>,
    upload: Option<u64>,
    status: String,
    failed: bool,
}

fn file_stamp(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
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

/// Orden elegida en el menú contextual de una entrada.
#[derive(Clone, Copy, PartialEq)]
enum MenuCmd {
    /// Subir o descargar la selección.
    Transfer,
    Edit,
    Rename,
    Permissions,
    Delete,
}

/// Resultado de pintar una lista de ficheros.
#[derive(Default)]
struct ListAction {
    /// Doble clic sobre (nombre, es_carpeta).
    activated: Option<(String, bool)>,
    /// Orden del menú contextual sobre la entrada con ese nombre.
    menu: Option<(String, MenuCmd)>,
}

struct Row<'a> {
    name: &'a str,
    is_dir: bool,
    is_link: bool,
    size: u64,
    mtime: Option<i64>,
    /// Permisos (solo se conocen en el panel remoto).
    mode: Option<u32>,
}

fn file_list(
    ui: &mut egui::Ui,
    id: &str,
    rows: &[Row<'_>],
    selection: &mut BTreeSet<String>,
    remote: bool,
) -> ListAction {
    let mut action = ListAction::default();
    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    let list_id = ui.scope_id().with(id);
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
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(width, row_height), egui::Sense::hover());
                // Identificador por nombre: el menú contextual sigue a su
                // entrada aunque la lista se desplace o se actualice.
                let response = ui.interact(rect, list_id.with(row.name), egui::Sense::click());
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
                // La columna de permisos solo cabe si el panel es ancho; si
                // no, quedan en el texto de ayuda.
                let mode = row.mode.filter(|_| width >= 430.0);
                let mode_w = if mode.is_some() { 80.0 } else { 0.0 };
                // Nombre (recortado al ancho disponible).
                let name_rect = egui::Rect::from_min_max(
                    rect.min + egui::vec2(6.0, 0.0),
                    egui::pos2(rect.max.x - meta_w - mode_w, rect.max.y),
                );
                ui.painter().with_clip_rect(name_rect).text(
                    egui::pos2(name_rect.min.x, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("{icon} {}", row.name),
                    font.clone(),
                    text_color,
                );
                if let Some(mode) = mode {
                    ui.painter().text(
                        egui::pos2(rect.max.x - meta_w - 8.0, rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        mode_text(mode),
                        egui::FontId::monospace(font.size - 2.5),
                        ui.visuals().weak_text_color(),
                    );
                }
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
                } else if response.secondary_clicked() && !selected {
                    // El menú actúa sobre la selección: la entrada pulsada
                    // pasa a serlo si no formaba parte de ella.
                    selection.clear();
                    selection.insert(row.name.to_string());
                }
                response.context_menu(|ui| {
                    let mut item = |ui: &mut egui::Ui, text: &str, cmd: MenuCmd| {
                        if ui.button(text).clicked() {
                            action.menu = Some((row.name.to_string(), cmd));
                            ui.close();
                        }
                    };
                    if remote {
                        item(ui, tr("⬅ Descargar"), MenuCmd::Transfer);
                        if !row.is_dir {
                            item(ui, tr("📝 Editar"), MenuCmd::Edit);
                        }
                        item(ui, tr("✏ Renombrar…"), MenuCmd::Rename);
                        item(ui, tr("🔒 Permisos…"), MenuCmd::Permissions);
                        ui.separator();
                        item(ui, tr("🗑 Eliminar…"), MenuCmd::Delete);
                    } else {
                        item(ui, tr("Subir ➡"), MenuCmd::Transfer);
                    }
                });
                if let Some(mode) = row.mode {
                    response.on_hover_text(trf!(
                        "Permisos: {text} ({octal})",
                        text = mode_text(mode),
                        octal = mode_octal(mode)
                    ));
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
            state: SftpState::Connecting(tr("Conectando…").into()),
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
            batches: HashMap::new(),
            next_id: 1,
            dialog: None,
            conflicts: VecDeque::new(),
            conflict_all: false,
            message: None,
            show_hidden: false,
            secrets_to_save: vec![],
            home: String::new(),
            edited: vec![],
            last_edit_check: std::time::Instant::now(),
            editor: String::new(),
            linked: false,
            follow: true,
            followed: None,
        };
        view.refresh_local();
        view
    }

    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id - 1
    }

    /// Descarga un fichero remoto a una carpeta temporal para abrirlo con
    /// el editor local.
    fn edit_remote(&mut self, name: &str) {
        let remote = join_remote(&self.remote_path, name);
        if let Some(existing) = self.edited.iter().find(|e| e.remote == remote) {
            // Ya está abierto: se vuelve a mostrar en el editor.
            if existing.download.is_none() {
                let _ = crate::paths::open_in_editor(&self.editor, &existing.local);
            }
            return;
        }
        // Una carpeta por fichero: conserva su nombre (y su extensión) sin
        // chocar con otros que se llamen igual.
        let dir = crate::paths::edit_dir().join(uuid::Uuid::new_v4().to_string());
        // El nombre viene del servidor: no debe poder salirse de la carpeta.
        let safe: String = name
            .chars()
            .map(|c| {
                if matches!(c, '/' | '\\' | ':') {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let safe = if safe.trim_matches('.').is_empty() {
            "fichero".to_string()
        } else {
            safe
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            let msg = trf!(
                "No se pudo crear {path}: {error}",
                path = dir.display(),
                error = e
            );
            self.message = Some((msg, true));
            return;
        }
        let local = dir.join(safe);
        let id = self.new_id();
        let _ = self.client.cmd.send(SftpCmd::Download {
            id,
            remote: remote.clone(),
            local: local.clone(),
            on_conflict: OnConflict::Replace,
        });
        self.edited.push(Edited {
            name: name.to_string(),
            local,
            remote,
            seen: None,
            download: Some(id),
            upload: None,
            status: tr("Descargando…").into(),
            failed: false,
        });
    }

    /// Resultado de una transferencia de un fichero en edición. Devuelve
    /// `true` si la transferencia era suya.
    fn edit_transfer_finished(&mut self, id: u64, error: Option<&str>) -> bool {
        let editor = self.editor.clone();
        let Some(edited) = self
            .edited
            .iter_mut()
            .find(|e| e.download == Some(id) || e.upload == Some(id))
        else {
            return false;
        };
        let now = chrono::Local::now().format("%H:%M:%S");
        if edited.download.take() == Some(id) {
            match error {
                None => {
                    edited.seen = file_stamp(&edited.local);
                    match crate::paths::open_in_editor(&editor, &edited.local) {
                        Ok(()) => {
                            edited.status = tr("Abierto en el editor: se sube al guardar").into();
                            edited.failed = false;
                        }
                        Err(e) => {
                            edited.status = trf!("No se pudo abrir el editor: {error}", error = e);
                            edited.failed = true;
                        }
                    }
                }
                Some(e) => {
                    edited.status = trf!("No se pudo descargar: {error}", error = e);
                    edited.failed = true;
                }
            }
        } else {
            edited.upload = None;
            match error {
                None => {
                    edited.status = trf!("Subido a las {time}", time = now);
                    edited.failed = false;
                }
                Some(e) => {
                    edited.status = trf!("No se pudo subir: {error}", error = e);
                    edited.failed = true;
                    // Se reintenta la próxima vez que se guarde.
                }
            }
        }
        true
    }

    /// Sube los ficheros en edición cuya copia local ha cambiado.
    fn upload_edited(&mut self) {
        if self.edited.is_empty()
            || self.last_edit_check.elapsed() < std::time::Duration::from_millis(700)
        {
            return;
        }
        self.last_edit_check = std::time::Instant::now();
        if self.state != SftpState::Ready {
            return;
        }
        for edited in &mut self.edited {
            if edited.download.is_some() || edited.upload.is_some() {
                continue;
            }
            let Some(stamp) = file_stamp(&edited.local) else {
                continue;
            };
            if edited.seen == Some(stamp) {
                continue;
            }
            edited.seen = Some(stamp);
            let id = self.next_id;
            self.next_id += 1;
            // Guardar en el editor es sobrescribir: no se pregunta.
            let _ = self.client.cmd.send(SftpCmd::Upload {
                id,
                local: edited.local.clone(),
                remote: edited.remote.clone(),
                on_conflict: OnConflict::Replace,
            });
            edited.upload = Some(id);
            edited.status = tr("Subiendo los cambios…").into();
        }
    }

    /// Lleva el panel remoto a la carpeta del terminal del que procede,
    /// si se le ha pedido seguirlo. `dir` puede empezar por `~`.
    /// Hay transferencias (o una sincronización) en marcha: cerrar la
    /// pestaña las dejaría a medias.
    pub fn is_transferring(&self) -> bool {
        self.transfers
            .iter()
            .any(|t| matches!(t.state, TransferState::Running))
    }

    pub fn follow_dir(&mut self, dir: &str) {
        if !self.linked || !self.follow || self.state != SftpState::Ready || self.home.is_empty() {
            return;
        }
        let path = match dir.strip_prefix('~') {
            Some(rest) => format!("{}{rest}", self.home.trim_end_matches('/')),
            None => dir.to_string(),
        };
        // Solo cuando el terminal cambia de carpeta: si el usuario navega a
        // mano por el panel, no se le devuelve a la fuerza.
        if self.followed.as_deref() == Some(path.as_str()) {
            return;
        }
        self.followed = Some(path.clone());
        if path != self.remote_path {
            self.list_remote(path);
        }
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
                let msg = trf!(
                    "No se puede abrir {path}: {error}",
                    path = path.display(),
                    error = e
                );
                self.message = Some((msg, true));
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
                    self.home = home.clone();
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
                    } else if let Some(Dialog::Sync(sync)) = &mut self.dialog
                        && sync.id == id
                    {
                        sync.current = current;
                    }
                }
                SftpEvent::Conflict(conflict) => self.conflict_arrived(conflict),
                SftpEvent::TransferFinished { id, error, stats } => {
                    // Si quedaba alguna pregunta suya, ya no hace falta.
                    self.conflicts.retain(|c| c.id != id);
                    if self.edit_transfer_finished(id, error.as_deref()) {
                        refresh_remote = true;
                    } else if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                        refresh_remote |= t.kind != TransferKind::Download;
                        refresh_local |= t.kind != TransferKind::Upload;
                        t.state = match error {
                            None => TransferState::Done(stats),
                            Some(e) => TransferState::Failed(e),
                        };
                        let batch = t.batch;
                        let running = |t: &TransferRow| {
                            t.batch == batch && matches!(t.state, TransferState::Running)
                        };
                        if !self.transfers.iter().any(running) {
                            self.batches.remove(&batch);
                        }
                    }
                }
                SftpEvent::SyncScanned { id, result } => {
                    if let Some(Dialog::Sync(sync)) = &mut self.dialog
                        && sync.id == id
                    {
                        match result {
                            Ok(scan) => {
                                sync.scan = Some(scan);
                                sync.replan();
                            }
                            Err(e) => sync.error = Some(e),
                        }
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
        self.upload_edited();
    }

    /// Lanza las transferencias de un lote. Si algún destino ya existe, el
    /// cliente lo pregunta fichero a fichero (véase [`Self::conflict_ui`]).
    fn start_transfers(&mut self, upload: bool, items: Vec<TransferItem>) {
        if items.is_empty() || self.state != SftpState::Ready {
            return;
        }
        let batch = self.new_id();
        self.batches.insert(
            batch,
            Batch {
                size: items.len(),
                memory: ConflictMemory::default(),
            },
        );
        for (local, remote, label) in items {
            let id = self.new_id();
            let on_conflict = OnConflict::Ask;
            let (kind, cmd) = if upload {
                let cmd = SftpCmd::Upload {
                    id,
                    local,
                    remote,
                    on_conflict,
                };
                (TransferKind::Upload, cmd)
            } else {
                let cmd = SftpCmd::Download {
                    id,
                    remote,
                    local,
                    on_conflict,
                };
                (TransferKind::Download, cmd)
            };
            self.transfers.push(TransferRow {
                id,
                kind,
                label,
                done: 0,
                total: 0,
                current: String::new(),
                state: TransferState::Running,
                batch,
            });
            let _ = self.client.cmd.send(cmd);
        }
    }

    /// Transfiere entradas de las carpetas que se están viendo.
    fn request_transfer(&mut self, upload: bool, names: Vec<String>) {
        let items = names
            .into_iter()
            .map(|name| {
                let local = self.local_path.join(&name);
                (local, join_remote(&self.remote_path, &name), name)
            })
            .collect();
        self.start_transfers(upload, items);
    }

    /// Sube a la carpeta remota visible ficheros o carpetas de cualquier
    /// sitio del equipo (los que se sueltan sobre la pestaña).
    fn upload_paths(&mut self, paths: Vec<PathBuf>) {
        let mut items = vec![];
        for path in paths {
            match path.file_name() {
                Some(name) => {
                    let name = name.to_string_lossy().to_string();
                    items.push((path, join_remote(&self.remote_path, &name), name));
                }
                None => {
                    let msg = trf!(
                        "No se puede subir {path}: no tiene nombre",
                        path = path.display()
                    );
                    self.message = Some((msg, true));
                }
            }
        }
        self.start_transfers(true, items);
    }

    /// Atiende lo que se arrastra desde el sistema: sube lo que se suelte y
    /// devuelve `true` mientras haya algo encima, para indicarlo.
    fn accept_drops(&mut self, ui: &egui::Ui) -> bool {
        let (hovering, dropped) =
            ui.input(|i| (!i.raw.hovered_files.is_empty(), i.raw.dropped_files.clone()));
        if !hovering && dropped.is_empty() {
            return false;
        }
        // Con una ventana o un diálogo encima, lo que se suelta es para
        // ellos (la de temas, por ejemplo, importa lo que recibe).
        let covered = self.dialog.is_some()
            || !self.conflicts.is_empty()
            || ui.ctx().memory(|m| {
                m.top_modal_layer().is_some()
                    || m.areas()
                        .visible_layer_ids()
                        .iter()
                        .any(|layer| layer.order == egui::Order::Middle)
            });
        if covered {
            return false;
        }
        let paths: Vec<PathBuf> = dropped
            .iter()
            .map(|f| f.path().to_path_buf())
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
        self.upload_paths(paths);
        hovering
    }

    /// Indicación de que lo que se arrastra se subirá al soltarlo.
    fn drop_hint(&self, ui: &egui::Ui, rect: egui::Rect) {
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("sftp_drop_hint"),
        ));
        let visuals = ui.visuals();
        painter.rect_filled(rect, 0.0, visuals.extreme_bg_color.gamma_multiply(0.85));
        painter.rect_stroke(
            rect.shrink(10.0),
            8.0,
            egui::Stroke::new(2.0, visuals.selection.bg_fill),
            egui::StrokeKind::Inside,
        );
        // Una ruta larga se parte en varias líneas en vez de salirse.
        let color = visuals.strong_text_color();
        let galley = painter.layout(
            trf!("⬆ Suelta para subir a {path}", path = self.remote_path),
            egui::FontId::proportional(20.0),
            color,
            rect.width() - 60.0,
        );
        painter.galley(rect.center() - galley.size() / 2.0, galley, color);
    }

    /// Una transferencia pregunta qué hacer con un destino que ya existe.
    fn conflict_arrived(&mut self, conflict: Conflict) {
        let remembered = self
            .transfers
            .iter()
            .find(|t| t.id == conflict.id)
            .and_then(|t| self.batches.get(&t.batch))
            .and_then(|b| b.memory.get(conflict.can_resume()));
        match remembered {
            // Ya se contestó «para todos» en otra transferencia del lote.
            Some(choice) => {
                let _ = conflict.reply.send(ConflictAnswer { choice, all: true });
            }
            None => self.conflicts.push_back(conflict),
        }
    }

    /// Contesta a la pregunta en curso. `None` cancela todo el lote.
    fn answer_conflict(&mut self, choice: Option<ConflictChoice>, all: bool) {
        let Some(conflict) = self.conflicts.pop_front() else {
            return;
        };
        self.conflict_all = false;
        let batch = self
            .transfers
            .iter()
            .find(|t| t.id == conflict.id)
            .map(|t| t.batch);
        let Some(choice) = choice else {
            // Al soltar `reply` sin contestar, la transferencia se cancela;
            // a las demás del lote hay que pedírselo.
            let _ = self.client.cmd.send(SftpCmd::Cancel(conflict.id));
            for t in &self.transfers {
                if Some(t.batch) == batch && matches!(t.state, TransferState::Running) {
                    let _ = self.client.cmd.send(SftpCmd::Cancel(t.id));
                }
            }
            return;
        };
        if all && let Some(batch) = batch.and_then(|b| self.batches.get_mut(&b)) {
            batch.memory.remember(conflict.can_resume(), choice);
        }
        let _ = conflict.reply.send(ConflictAnswer { choice, all });
        if all {
            // Las que ya estaban en cola y ahora tienen respuesta.
            for pending in std::mem::take(&mut self.conflicts) {
                self.conflict_arrived(pending);
            }
        }
    }

    /// Compara la carpeta local con la remota para sincronizarlas.
    fn open_sync(&mut self) {
        if self.state != SftpState::Ready {
            return;
        }
        let id = self.new_id();
        let _ = self.client.cmd.send(SftpCmd::SyncScan {
            id,
            local: self.local_path.clone(),
            remote: self.remote_path.clone(),
        });
        self.dialog = Some(Dialog::Sync(Box::new(SyncDialog {
            id,
            local: self.local_path.clone(),
            remote: self.remote_path.clone(),
            current: String::new(),
            scan: None,
            error: None,
            mode: SyncMode::Upload,
            delete: false,
            plan: vec![],
            summary: SyncSummary::default(),
            extra: (0, 0),
            listed: vec![],
        })));
    }

    /// Ejecuta un plan de sincronización ya confirmado.
    fn start_sync(&mut self, local: PathBuf, remote: String, items: Vec<SyncItem>) {
        let id = self.new_id();
        self.transfers.push(TransferRow {
            id,
            kind: TransferKind::Sync,
            label: trf!("Sincronizar {remote}", remote),
            done: 0,
            total: 0,
            current: String::new(),
            state: TransferState::Running,
            batch: id,
        });
        let _ = self.client.cmd.send(SftpCmd::Sync {
            id,
            local,
            remote,
            items,
        });
    }

    fn open_permissions(&mut self, name: &str) {
        let Some(entry) = self.remote_entries.iter().find(|e| e.name == name) else {
            return;
        };
        // Los de un enlace son los suyos, no los del destino que se cambia.
        let known = entry.permissions.filter(|_| !entry.is_symlink);
        let mode = known.map_or(if entry.is_dir { 0o755 } else { 0o644 }, |p| p & 0o7777);
        self.dialog = Some(Dialog::Permissions {
            name: name.to_string(),
            is_dir: entry.is_dir && !entry.is_symlink,
            known: known.is_some(),
            mode,
            text: mode_octal(mode),
            recursive: false,
        });
    }

    fn confirm_delete(&mut self) {
        let names = self
            .remote_entries
            .iter()
            .filter(|e| self.remote_sel.contains(&e.name))
            // Un enlace a carpeta se borra como fichero (no se sigue).
            .map(|e| (e.name.clone(), e.is_dir && !e.is_symlink))
            .collect();
        self.dialog = Some(Dialog::ConfirmDelete { names });
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
                            trf!("Conexión cerrada: {error}", error = e),
                        );
                    }
                    None => {
                        ui.label(tr("Conexión SFTP cerrada."));
                    }
                });
                return;
            }
            SftpState::Ready => {}
        }

        let area = ui.max_rect();
        let dragging = self.accept_drops(ui);

        // --- Ficheros en edición y cola de transferencias (abajo) ----------
        if !self.edited.is_empty() {
            egui::Panel::bottom("sftp_edited").show(ui, |ui| self.edited_ui(ui));
        }
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

        if self.dialog.is_some() {
            self.dialog_ui(ui.ctx());
        } else {
            self.conflict_ui(ui.ctx());
        }
        if dragging {
            self.drop_hint(ui, area);
        }
    }

    fn local_pane(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong(tr("Equipo local"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let names: Vec<String> = self.local_sel.iter().cloned().collect();
                if ui
                    .add_enabled(!names.is_empty(), egui::Button::new(tr("Subir ➡")))
                    .on_hover_text(tr("Subir la selección a la carpeta remota"))
                    .clicked()
                {
                    self.request_transfer(true, names);
                }
                if ui
                    .button(tr("Sincronizar…"))
                    .on_hover_text(tr(
                        "Comparar esta carpeta con la del servidor y ver qué habría que \
                         copiar en cada sentido; no se toca nada hasta confirmarlo",
                    ))
                    .clicked()
                {
                    self.open_sync();
                }
            });
        });
        ui.horizontal(|ui| {
            if ui
                .button("⬆")
                .on_hover_text(tr("Carpeta superior"))
                .clicked()
                && let Some(parent) = self.local_path.parent()
            {
                self.go_local(parent.to_path_buf());
            }
            if ui.button("⟳").on_hover_text(tr("Actualizar")).clicked() {
                self.refresh_local();
            }
            if ui
                .button("➕📁")
                .on_hover_text(tr("Nueva carpeta"))
                .clicked()
            {
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
                mode: None,
            })
            .collect();
        let action = file_list(ui, "local_list", &rows, &mut self.local_sel, false);
        if let Some((name, is_dir)) = action.activated {
            if is_dir {
                self.go_local(self.local_path.join(name));
            } else {
                self.request_transfer(true, vec![name]);
            }
        }
        if action.menu.is_some() {
            // El menú local solo tiene «Subir».
            self.request_transfer(true, self.local_sel.iter().cloned().collect());
        }
    }

    fn remote_pane(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong(tr("Servidor remoto"));
            if self.remote_loading {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let names: Vec<String> = self.remote_sel.iter().cloned().collect();
                if ui
                    .add_enabled(!names.is_empty(), egui::Button::new(tr("⬅ Descargar")))
                    .on_hover_text(tr("Descargar la selección a la carpeta local"))
                    .clicked()
                {
                    self.request_transfer(false, names);
                }
                ui.checkbox(&mut self.show_hidden, tr("Ocultos"));
                if self.linked {
                    ui.checkbox(&mut self.follow, tr("Seguir al terminal"))
                        .on_hover_text(tr(
                            "Al cambiar de carpeta en el terminal del que procede esta \
                             pestaña, el panel va a esa carpeta",
                        ));
                }
            });
        });
        ui.horizontal(|ui| {
            if ui
                .button("⬆")
                .on_hover_text(tr("Carpeta superior"))
                .clicked()
            {
                self.list_remote(parent_remote(&self.remote_path));
            }
            if ui.button("⟳").on_hover_text(tr("Actualizar")).clicked() {
                self.refresh_remote();
            }
            if ui
                .button("➕📁")
                .on_hover_text(tr("Nueva carpeta"))
                .clicked()
            {
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
                .on_hover_text(tr("Renombrar"))
                .clicked()
                && let Some(old) = one.clone()
            {
                self.dialog = Some(Dialog::Rename {
                    name: old.clone(),
                    old,
                    focused: false,
                });
            }
            let one_file = one.as_ref().filter(|name| {
                self.remote_entries
                    .iter()
                    .any(|e| e.name == **name && !e.is_dir)
            });
            if ui
                .add_enabled(one_file.is_some(), egui::Button::new("📝"))
                .on_hover_text(tr(
                    "Editar con el editor de este equipo: se descarga, se abre y se sube solo \
                     cada vez que lo guardes",
                ))
                .clicked()
                && let Some(name) = one_file.cloned()
            {
                self.edit_remote(&name);
            }
            if ui
                .add_enabled(one.is_some(), egui::Button::new("🔒"))
                .on_hover_text(tr("Permisos"))
                .clicked()
                && let Some(name) = one.clone()
            {
                self.open_permissions(&name);
            }
            if ui
                .add_enabled(!self.remote_sel.is_empty(), egui::Button::new("🗑"))
                .on_hover_text(tr("Eliminar"))
                .clicked()
            {
                self.confirm_delete();
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
                mode: e.permissions,
            })
            .collect();
        let action = file_list(ui, "remote_list", &rows, &mut self.remote_sel, true);
        if let Some((name, is_dir)) = action.activated {
            if is_dir {
                self.list_remote(join_remote(&self.remote_path, &name));
            } else {
                self.request_transfer(false, vec![name]);
            }
        }
        match action.menu {
            Some((_, MenuCmd::Transfer)) => {
                self.request_transfer(false, self.remote_sel.iter().cloned().collect());
            }
            Some((name, MenuCmd::Edit)) => self.edit_remote(&name),
            Some((old, MenuCmd::Rename)) => {
                self.dialog = Some(Dialog::Rename {
                    name: old.clone(),
                    old,
                    focused: false,
                });
            }
            Some((name, MenuCmd::Permissions)) => self.open_permissions(&name),
            Some((_, MenuCmd::Delete)) => self.confirm_delete(),
            None => {}
        }
    }

    /// Ficheros remotos abiertos en el editor local, con su estado.
    fn edited_ui(&mut self, ui: &mut egui::Ui) {
        if self.edited.is_empty() {
            return;
        }
        // Mientras haya ficheros en edición se vigila si cambian.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(800));
        ui.strong(tr("En edición"));
        let mut close = None;
        let mut reopen = None;
        for (i, edited) in self.edited.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("📝 {}", edited.name))
                    .on_hover_text(&edited.remote);
                let color = if edited.failed {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().weak_text_color()
                };
                ui.colored_label(color, &edited.status);
                if edited.download.is_none() && ui.small_button(tr("Abrir")).clicked() {
                    reopen = Some(i);
                }
                if ui
                    .small_button("×")
                    .on_hover_text(tr("Dejar de vigilarlo y borrar la copia local"))
                    .clicked()
                {
                    close = Some(i);
                }
            });
        }
        if let Some(i) = reopen {
            let _ = crate::paths::open_in_editor(&self.editor, &self.edited[i].local);
        }
        if let Some(i) = close {
            let edited = self.edited.remove(i);
            remove_edit_copy(&edited.local);
        }
    }

    fn transfers_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong(tr("Transferencias"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(tr("Limpiar terminadas")).clicked() {
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
                        ui.label(match t.kind {
                            TransferKind::Upload => "⬆",
                            TransferKind::Download => "⬇",
                            TransferKind::Sync => "⟳",
                        });
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
                                if ui.small_button(tr("Cancelar")).clicked() {
                                    cancel = Some(t.id);
                                }
                                ui.add(
                                    egui::Label::new(RichText::new(&t.current).weak()).truncate(),
                                );
                            }
                            TransferState::Done(stats) => {
                                ui.colored_label(
                                    crate::theme::ok_color(),
                                    finished_text(t.kind, t.done, stats),
                                );
                            }
                            TransferState::Failed(e) => {
                                ui.colored_label(
                                    ui.visuals().error_fg_color,
                                    trf!("Error: {error}", error = e),
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

    /// Pregunta qué hacer con el destino que ya existe de la primera
    /// transferencia que espera.
    fn conflict_ui(&mut self, ctx: &egui::Context) {
        let Some(conflict) = self.conflicts.front() else {
            return;
        };
        let batch_size = self
            .transfers
            .iter()
            .find(|t| t.id == conflict.id)
            .and_then(|t| self.batches.get(&t.batch))
            .map_or(1, |b| b.size);
        // Con un solo fichero no hay «demás» a los que aplicarlo.
        let multiple = conflict.multiple || batch_size > 1;
        let mut all = self.conflict_all;
        // `Some(None)` es cancelar.
        let mut answer: Option<Option<ConflictChoice>> = None;
        egui::Modal::new(egui::Id::unique("sftp_conflict")).show(ctx, |ui| {
            ui.set_width(430.0);
            ui.heading(tr("El fichero ya existe"));
            ui.label(if conflict.upload {
                tr("En el servidor:")
            } else {
                tr("En el equipo local:")
            });
            ui.add(egui::Label::new(RichText::new(&conflict.path).monospace()).wrap());
            ui.label(trf!(
                "Origen: {source}   ·   Destino: {dest}",
                source = format_size(conflict.source_size),
                dest = format_size(conflict.dest_size)
            ));
            if conflict.can_resume() {
                ui.label(
                    RichText::new(tr(
                        "El destino es más pequeño: puede ser una transferencia que se quedó a \
                         medias. «Reanudar» da por bueno lo que ya hay y envía solo lo que falta.",
                    ))
                    .weak(),
                );
            }
            if multiple {
                ui.checkbox(
                    &mut all,
                    tr("Hacer lo mismo con los demás de esta transferencia"),
                );
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if conflict.can_resume() && ui.button(tr("Reanudar")).clicked() {
                    answer = Some(Some(ConflictChoice::Resume));
                }
                if ui.button(tr("Sustituir")).clicked() {
                    answer = Some(Some(ConflictChoice::Replace));
                }
                if ui.button(tr("Omitir")).clicked() {
                    answer = Some(Some(ConflictChoice::Skip));
                }
                if ui
                    .button(tr("Cancelar"))
                    .on_hover_text(tr("Cancela la transferencia"))
                    .clicked()
                {
                    answer = Some(None);
                }
            });
            // Escape deja este fichero como está y sigue con el resto.
            if answer.is_none() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(Some(ConflictChoice::Skip));
                all = false;
            }
        });
        self.conflict_all = all;
        if let Some(choice) = answer {
            self.answer_conflict(choice, all && multiple);
        }
    }

    fn dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        let mut close = false;
        let mut after: Option<AfterDialog> = None;
        egui::Modal::new(egui::Id::unique("sftp_dialog")).show(ctx, |ui| {
            ui.set_width(match dialog {
                Dialog::Sync(_) => 600.0,
                _ => 380.0,
            });
            match dialog {
                Dialog::Mkdir {
                    remote,
                    name,
                    focused,
                } => {
                    ui.heading(tr("Nueva carpeta"));
                    ui.label(if *remote {
                        tr("En el servidor remoto:")
                    } else {
                        tr("En el equipo local:")
                    });
                    let resp = ui.text_edit_singleline(name);
                    if !*focused {
                        resp.request_focus();
                        *focused = true;
                    }
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        let valid = !name.trim().is_empty() && !name.contains(['/', '\\']);
                        if ui
                            .add_enabled(valid, egui::Button::new(tr("Crear")))
                            .clicked()
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
                                            let msg = trf!("No se pudo crear: {error}", error = e);
                                            this.message = Some((msg, true))
                                        }
                                    }
                                }
                            }));
                            close = true;
                        }
                        if ui.button(tr("Cancelar")).clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::Rename { old, name, focused } => {
                    ui.heading(tr("Renombrar"));
                    ui.label(trf!("Nuevo nombre para «{old}»:", old));
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
                            .add_enabled(valid, egui::Button::new(tr("Renombrar")))
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
                        if ui.button(tr("Cancelar")).clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::ConfirmDelete { names } => {
                    ui.heading(tr("Eliminar del servidor"));
                    ui.label(trf!(
                        "Se eliminarán definitivamente {count} elemento(s) del servidor remoto:",
                        count = names.len()
                    ));
                    egui::ScrollArea::vertical()
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for (name, is_dir) in names.iter() {
                                ui.label(if *is_dir {
                                    trf!("📁 {name}  (con todo su contenido)", name)
                                } else {
                                    format!("📄 {name}")
                                });
                            }
                        });
                    ui.horizontal(|ui| {
                        if ui
                            .button(
                                RichText::new(tr("Eliminar")).color(ui.visuals().error_fg_color),
                            )
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
                        if ui.button(tr("Cancelar")).clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::Permissions {
                    name,
                    is_dir,
                    known,
                    mode,
                    text,
                    recursive,
                } => {
                    ui.heading(tr("Permisos"));
                    ui.label(trf!("«{name}»", name));
                    if !*known {
                        ui.label(
                            RichText::new(tr(
                                "No se conocen sus permisos actuales (si es un enlace, se \
                                 cambian los de su destino).",
                            ))
                            .weak(),
                        );
                    }
                    egui::Grid::new("sftp_permissions")
                        .num_columns(4)
                        .spacing([18.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("");
                            ui.label(tr("Lectura"));
                            ui.label(tr("Escritura"));
                            ui.label(tr("Ejecución"));
                            ui.end_row();
                            for (who, shift) in
                                [(tr("Dueño"), 6), (tr("Grupo"), 3), (tr("Otros"), 0)]
                            {
                                ui.label(who);
                                for bit in [4, 2, 1] {
                                    let mask = bit << shift;
                                    let mut on = *mode & mask != 0;
                                    if ui.checkbox(&mut on, "").changed() {
                                        *mode ^= mask;
                                        *text = mode_octal(*mode);
                                    }
                                }
                                ui.end_row();
                            }
                        });
                    let mut valid = true;
                    ui.horizontal(|ui| {
                        ui.label(tr("Octal:"));
                        let resp = ui.add(egui::TextEdit::singleline(text).desired_width(70.0));
                        match parse_mode(text) {
                            Some(parsed) => {
                                if resp.changed() {
                                    *mode = parsed;
                                }
                                ui.label(RichText::new(mode_text(parsed)).monospace());
                            }
                            None => {
                                valid = false;
                                ui.colored_label(ui.visuals().error_fg_color, tr("no es válido"));
                            }
                        }
                    });
                    if *is_dir {
                        ui.checkbox(recursive, tr("Aplicar también a todo su contenido"));
                    }
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(valid, egui::Button::new(tr("Aplicar")))
                            .clicked()
                            || (enter && valid)
                        {
                            let (name, mode) = (name.clone(), *mode);
                            let recursive = *recursive && *is_dir;
                            after = Some(Box::new(move |this| {
                                let _ = this.client.cmd.send(SftpCmd::Chmod {
                                    path: join_remote(&this.remote_path, &name),
                                    mode,
                                    recursive,
                                });
                            }));
                            close = true;
                        }
                        if ui.button(tr("Cancelar")).clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::Sync(sync) => {
                    ui.heading(tr("Sincronizar carpetas"));
                    egui::Grid::new("sftp_sync_roots")
                        .num_columns(2)
                        .show(ui, |ui| {
                            ui.label(tr("Local:"));
                            let local = sync.local.display().to_string();
                            ui.add(egui::Label::new(RichText::new(local).monospace()).truncate());
                            ui.end_row();
                            ui.label(tr("Remoto:"));
                            ui.add(
                                egui::Label::new(RichText::new(&sync.remote).monospace())
                                    .truncate(),
                            );
                            ui.end_row();
                        });
                    ui.separator();
                    if let Some(error) = &sync.error {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            trf!("No se pudieron comparar las carpetas: {error}", error),
                        );
                        if ui.button(tr("Cerrar")).clicked() {
                            close = true;
                        }
                    } else if sync.scan.is_none() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(tr("Comparando las dos carpetas…"));
                        });
                        ui.add(egui::Label::new(RichText::new(&sync.current).weak()).truncate());
                        if ui.button(tr("Cancelar")).clicked() {
                            close = true;
                        }
                    } else if let Some(confirmed) = sync_plan_ui(ui, sync) {
                        if confirmed {
                            let items: Vec<SyncItem> = sync
                                .plan
                                .iter()
                                .filter(|item| item.op != SyncOp::None)
                                .cloned()
                                .collect();
                            let (local, remote) = (sync.local.clone(), sync.remote.clone());
                            after =
                                Some(Box::new(move |this| this.start_sync(local, remote, items)));
                        }
                        close = true;
                    }
                }
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
        });
        if close {
            // Una comparación a medias no tiene a quién entregar el resultado.
            if let Some(Dialog::Sync(sync)) = &self.dialog
                && sync.scan.is_none()
                && sync.error.is_none()
            {
                let _ = self.client.cmd.send(SftpCmd::Cancel(sync.id));
            }
            self.dialog = None;
        }
        if let Some(f) = after {
            f(self);
        }
    }
}

/// Texto de una transferencia terminada: lo principal y, separado por
/// comas, lo que haya que añadir.
fn finished_text(kind: TransferKind, done: u64, stats: &TransferStats) -> String {
    let mut parts = vec![match kind {
        TransferKind::Sync => trf!(
            "Completada: {copied} ({size})",
            copied = counted(stats.copied, mark!("{n} copiado"), mark!("{n} copiados")),
            size = format_size(stats.bytes)
        ),
        _ if stats.copied == 0 && stats.skipped > 0 => tr("Omitida").to_string(),
        _ => trf!("Completada ({size})", size = format_size(done)),
    }];
    if stats.resumed > 0 {
        // Al reanudar solo viaja lo que faltaba.
        parts.push(trf!(
            "reanudada: faltaban {size}",
            size = format_size(stats.bytes)
        ));
    }
    if stats.skipped > 0 && stats.copied > 0 {
        parts.push(counted(
            stats.skipped,
            mark!("{n} omitido"),
            mark!("{n} omitidos"),
        ));
    }
    if stats.deleted > 0 {
        parts.push(counted(
            stats.deleted,
            mark!("{n} borrado"),
            mark!("{n} borrados"),
        ));
    }
    parts.join(", ")
}

/// «1 fichero», «3 ficheros»: la frase del singular o la del plural (sin
/// traducir), con el número en su hueco `{n}`.
fn counted(n: u32, one: &str, many: &str) -> String {
    crate::i18n::render(tr(if n == 1 { one } else { many }), &[("n", &n)])
}

fn files_text(n: u32) -> String {
    counted(n, mark!("{n} fichero"), mark!("{n} ficheros"))
}

fn dirs_text(n: u32) -> String {
    counted(n, mark!("{n} carpeta"), mark!("{n} carpetas"))
}

/// «3 ficheros y 1 carpeta».
fn count_text(files: u32, dirs: u32) -> String {
    let files = files_text(files);
    match dirs {
        0 => files,
        _ => trf!("{files} y {dirs}", files, dirs = dirs_text(dirs)),
    }
}

/// Por qué un elemento del plan se copia, se borra o se deja como está.
fn sync_reason(item: &SyncItem) -> &'static str {
    use SyncOp::*;
    use SyncState::*;
    match (item.state, item.op) {
        (OnlyLocal, Upload) | (OnlyRemote, Download) => tr("nuevo"),
        (OnlyLocal, DeleteLocal) | (OnlyRemote, DeleteRemote) => tr("sobra: se borra"),
        (OnlyLocal, _) => tr("solo en local"),
        (OnlyRemote, _) => tr("solo en el servidor"),
        (LocalNewer, Upload) => tr("más reciente en local"),
        (LocalNewer, _) => tr("más reciente en local: no se toca"),
        (RemoteNewer, Download) => tr("más reciente en el servidor"),
        (RemoteNewer, _) => tr("más reciente en el servidor: no se toca"),
        (Differ, None) => tr("misma fecha y distinto tamaño: no se toca"),
        (Differ, _) => tr("distinto tamaño"),
        (TypeConflict, _) => tr("fichero en un lado y carpeta en el otro: no se toca"),
        (Ignored, _) if item.is_dir => tr("contiene algo que no se sincroniza: no se borra"),
        (Ignored, _) => tr("enlace o fichero especial: no se sincroniza"),
        (Equal, _) => tr("igual"),
    }
}

/// Plan de sincronización: modo, resumen y lista de lo que se haría.
/// Devuelve `Some(true)` cuando el usuario lo confirma y `Some(false)`
/// cuando lo descarta.
fn sync_plan_ui(ui: &mut egui::Ui, sync: &mut SyncDialog) -> Option<bool> {
    let error_color = ui.visuals().error_fg_color;
    let mut mode_changed = false;
    ui.horizontal(|ui| {
        for (mode, label) in [
            (SyncMode::Upload, tr("Local ➡ remoto")),
            (SyncMode::Download, tr("Remoto ➡ local")),
            (SyncMode::Both, tr("En los dos sentidos")),
        ] {
            mode_changed |= ui.radio_value(&mut sync.mode, mode, label).changed();
        }
    });
    if mode_changed {
        // Al cambiar de sentido cambia lo que se borraría: hay que volver
        // a pedirlo a propósito.
        sync.delete = false;
        sync.replan();
    }
    ui.label(
        RichText::new(match sync.mode {
            SyncMode::Upload => {
                tr("Sube lo que solo está en local y lo que en local es más reciente.")
            }
            SyncMode::Download => {
                tr("Descarga lo que solo está en el servidor y lo que allí es más reciente.")
            }
            SyncMode::Both => tr("Copia cada fichero hacia el lado donde falta o es más antiguo."),
        })
        .weak(),
    );

    let (files, dirs) = sync.extra;
    // De dónde se borra lo que sobra: del lado que recibe.
    let from_server = sync.mode == SyncMode::Upload;
    if sync.mode == SyncMode::Both {
        ui.add_enabled(
            false,
            egui::Checkbox::new(&mut false, tr("Borrar lo que sobra en el destino")),
        )
        .on_disabled_hover_text(tr(
            "Solo en los modos de un sentido: en los dos no sobra nada",
        ));
    } else {
        let count = count_text(files, dirs);
        let label = if from_server {
            trf!(
                "Borrar del servidor lo que no está en el origen: {count}",
                count
            )
        } else {
            trf!(
                "Borrar del equipo local lo que no está en el origen: {count}",
                count
            )
        };
        let checkbox = egui::Checkbox::new(&mut sync.delete, label);
        if ui.add_enabled(files + dirs > 0, checkbox).changed() {
            sync.replan();
        }
    }
    let summary = sync.summary;
    if sync.delete {
        let count = count_text(summary.delete_files, summary.delete_dirs);
        let warning = if from_server {
            trf!(
                "Se BORRARÁN del servidor {count}. No se puede deshacer.",
                count
            )
        } else {
            trf!(
                "Se BORRARÁN del equipo local {count}. No se puede deshacer.",
                count
            )
        };
        ui.label(RichText::new(warning).color(error_color).strong());
    }
    ui.separator();

    egui::Grid::new("sftp_sync_summary")
        .num_columns(2)
        .spacing([18.0, 4.0])
        .show(ui, |ui| {
            let files = |n: u32, bytes: u64| match n {
                0 => tr("nada").to_string(),
                _ => format!("{} ({})", count_text(n, 0), format_size(bytes)),
            };
            ui.label(tr("⬆ Subir"));
            ui.label(files(summary.upload, summary.upload_bytes));
            ui.end_row();
            ui.label(tr("⬇ Descargar"));
            ui.label(files(summary.download, summary.download_bytes));
            ui.end_row();
            if summary.make_dirs > 0 {
                ui.label(tr("📁 Crear"));
                ui.label(dirs_text(summary.make_dirs));
                ui.end_row();
            }
            ui.label(tr("🗑 Borrar"));
            if sync.delete {
                let text = count_text(summary.delete_files, summary.delete_dirs);
                ui.label(RichText::new(text).color(error_color).strong());
            } else {
                ui.label(tr("nada"));
            }
            ui.end_row();
            ui.label(tr("Iguales"));
            ui.label(count_text(summary.equal, 0));
            ui.end_row();
            ui.label(tr("Solo en un lado"));
            ui.label(trf!(
                "{local} en local, {remote} en el servidor",
                local = summary.only_local,
                remote = summary.only_remote
            ));
            ui.end_row();
            if summary.untouched > 0 {
                ui.label(tr("Distintos sin tocar"));
                ui.label(count_text(summary.untouched, 0))
                    .on_hover_text(tr("Más abajo se indica el motivo de cada uno"));
                ui.end_row();
            }
            if summary.ignored > 0 {
                ui.label(tr("No se sincronizan"));
                ui.label(trf!(
                    "{count} (enlaces, ficheros especiales o lo que cuelga de ellos)",
                    count = summary.ignored
                ));
                ui.end_row();
            }
        });
    ui.separator();

    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 2.0;
    egui::ScrollArea::vertical()
        .id_salt("sftp_sync_plan")
        .max_height(220.0)
        .auto_shrink([false, true])
        .show_rows(ui, row_height, sync.listed.len(), |ui, range| {
            for &index in &sync.listed[range] {
                let item = &sync.plan[index];
                let (icon, color) = match item.op {
                    SyncOp::Upload => ("⬆", ui.visuals().text_color()),
                    SyncOp::Download => ("⬇", ui.visuals().text_color()),
                    SyncOp::DeleteLocal | SyncOp::DeleteRemote => ("🗑", error_color),
                    SyncOp::None => ("•", ui.visuals().weak_text_color()),
                };
                let width = ui.available_width();
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(width, row_height), egui::Sense::hover());
                let font = egui::TextStyle::Body.resolve(ui.style());
                let mut reason = sync_reason(item).to_string();
                if item.size > 0 {
                    reason = format!("{}   {reason}", format_size(item.size));
                }
                let reason_rect = ui.painter().text(
                    egui::pos2(rect.max.x - 4.0, rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    reason,
                    egui::FontId::proportional(font.size - 1.5),
                    ui.visuals().weak_text_color(),
                );
                // La ruta, recortada para no pisar el motivo.
                let path_rect = egui::Rect::from_min_max(
                    rect.min,
                    egui::pos2(reason_rect.min.x - 10.0, rect.max.y),
                );
                let slash = if item.is_dir { "/" } else { "" };
                ui.painter().with_clip_rect(path_rect).text(
                    egui::pos2(rect.min.x + 4.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("{icon} {}{slash}", item.path),
                    font,
                    color,
                );
            }
        });
    if sync.listed.is_empty() {
        ui.label(RichText::new(tr("Las dos carpetas tienen lo mismo.")).weak());
    }
    ui.separator();

    let mut outcome = None;
    let nothing = summary.nothing_to_do();
    ui.horizontal(|ui| {
        let label = if sync.delete {
            RichText::new(tr("Sincronizar y borrar")).color(error_color)
        } else {
            RichText::new(tr("Sincronizar"))
        };
        if ui.add_enabled(!nothing, egui::Button::new(label)).clicked() {
            outcome = Some(true);
        }
        if ui.button(tr("Cancelar")).clicked() {
            outcome = Some(false);
        }
        if nothing {
            ui.label(RichText::new(tr("No hay nada que copiar ni borrar.")).weak());
        }
    });
    outcome
}

/// Borra la copia local de un fichero en edición (y su carpeta temporal).
fn remove_edit_copy(local: &Path) {
    let _ = std::fs::remove_file(local);
    if let Some(dir) = local.parent() {
        let _ = std::fs::remove_dir(dir);
    }
}

impl Drop for SftpView {
    fn drop(&mut self) {
        // Las copias locales de los ficheros en edición no se dejan atrás.
        for edited in &self.edited {
            remove_edit_copy(&edited.local);
        }
    }
}

/// La vista funcionando sin ventana: los fotogramas se ejecutan a mano, se
/// pulsa sobre los textos que pinta y el cliente SFTP es de mentira (se ve
/// lo que la vista le manda y se le entregan los eventos a mano).
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc::Sender;

    use tokio::sync::mpsc::UnboundedReceiver;
    use tokio::sync::oneshot;

    use super::*;
    use crate::sftp::{SyncFile, SyncTree};

    #[derive(Debug)]
    struct Dropped(PathBuf);

    impl egui::DroppedFile for Dropped {
        fn path(&self) -> &Path {
            &self.0
        }

        fn bytes(&self) -> Result<Vec<u8>, String> {
            Err("no se lee en las pruebas".into())
        }
    }

    fn dropped(path: &str) -> egui::DroppedFileHandle {
        Arc::new(Dropped(PathBuf::from(path)))
    }

    fn hovered(path: &str) -> egui::HoveredFile {
        egui::HoveredFile {
            path: Some(PathBuf::from(path)),
            ..Default::default()
        }
    }

    struct Harness {
        ctx: egui::Context,
        view: SftpView,
        cmds: UnboundedReceiver<SftpCmd>,
        events: Sender<SftpEvent>,
        /// Textos pintados en el último fotograma y dónde.
        texts: Vec<(String, egui::Rect)>,
        time: f64,
        /// Hay otra ventana de la aplicación abierta encima de la vista.
        other_window: bool,
    }

    fn entry(name: &str, is_dir: bool, is_symlink: bool, permissions: u32) -> RemoteEntry {
        RemoteEntry {
            name: name.to_string(),
            is_dir,
            is_symlink,
            size: 1234,
            mtime: Some(1_700_000_000),
            permissions: Some(permissions),
        }
    }

    impl Harness {
        /// Vista recién creada, aún conectando.
        fn connecting() -> Self {
            let (cmd, cmds) = tokio::sync::mpsc::unbounded_channel();
            let (events, rx) = std::sync::mpsc::channel();
            let client = SftpClient { cmd, events: rx };
            Self {
                ctx: egui::Context::default(),
                view: SftpView::new(client),
                cmds,
                events,
                texts: vec![],
                time: 0.0,
                other_window: false,
            }
        }

        /// Vista conectada, viendo `/srv/datos`.
        fn new() -> Self {
            let mut h = Self::connecting();
            h.event(SftpEvent::Ready {
                home: "/srv/datos".into(),
            });
            h.event(SftpEvent::Listing {
                path: "/srv/datos".into(),
                entries: vec![
                    entry("carpeta-remota", true, false, 0o40755),
                    entry("enlace-remoto", false, true, 0o120777),
                    entry("script-remoto.sh", false, false, 0o100644),
                ],
            });
            h.settle();
            assert!(matches!(h.sent()[..], [SftpCmd::List(_)]));
            h
        }

        fn event(&mut self, event: SftpEvent) {
            self.events.send(event).unwrap();
        }

        fn frame_with(&mut self, edit: impl FnOnce(&mut egui::RawInput)) {
            self.time += 1.0 / 60.0;
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                time: Some(self.time),
                ..Default::default()
            };
            edit(&mut input);
            let (view, other_window) = (&mut self.view, self.other_window);
            let mut output = self.ctx.run_ui(input, |ui| {
                view.ui(ui);
                if other_window {
                    egui::Window::new("Otra ventana").show(ui.ctx(), |ui| ui.label("…"));
                }
            });
            output.textures_delta.clear();
            fn collect(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
                match shape {
                    egui::Shape::Text(text) => {
                        let rect = text.galley.rect.translate(text.pos.to_vec2());
                        out.push((text.galley.text().to_string(), rect));
                    }
                    egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, out)),
                    _ => {}
                }
            }
            self.texts.clear();
            for clipped in &output.shapes {
                collect(&clipped.shape, &mut self.texts);
            }
        }

        fn frame(&mut self) {
            self.frame_with(|_| {});
        }

        /// Unos cuantos fotogramas: lo recién abierto termina de colocarse.
        fn settle(&mut self) {
            for _ in 0..4 {
                self.frame();
            }
        }

        fn sees(&self, text: &str) -> bool {
            self.texts.iter().any(|(t, _)| t.contains(text))
        }

        /// Dónde está pintado un texto: el que coincide entero o, si no,
        /// uno que lo contenga.
        fn rect_of(&self, text: &str) -> egui::Rect {
            // De atrás adelante: lo último pintado es lo que queda encima.
            let mut texts = self.texts.iter().rev();
            let exact = texts.clone().find(|(t, _)| t == text);
            let found = exact.or_else(|| texts.find(|(t, _)| t.contains(text)));
            match found {
                Some((_, rect)) => *rect,
                None => panic!("no se ve {text:?} entre {:?}", self.all_texts()),
            }
        }

        fn all_texts(&self) -> Vec<&str> {
            self.texts.iter().map(|(t, _)| t.as_str()).collect()
        }

        fn click_at(&mut self, pos: egui::Pos2, button: egui::PointerButton) {
            // Separado del clic anterior, para que no cuente como doble.
            self.time += 1.0;
            let press = |pressed| egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            self.frame_with(|i| i.events.push(egui::Event::PointerMoved(pos)));
            self.frame_with(|i| i.events.push(press(true)));
            self.frame_with(|i| i.events.push(press(false)));
            self.settle();
        }

        fn click(&mut self, text: &str) {
            let pos = self.rect_of(text).center();
            self.click_at(pos, egui::PointerButton::Primary);
        }

        fn right_click(&mut self, text: &str) {
            let pos = self.rect_of(text).center();
            self.click_at(pos, egui::PointerButton::Secondary);
        }

        fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
            let event = |pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            };
            self.frame_with(|i| {
                i.events.push(egui::Event::ModifiersChanged(modifiers));
                i.events.push(event(true));
            });
            self.frame_with(|i| {
                i.events.push(event(false));
                let none = egui::Modifiers::NONE;
                i.events.push(egui::Event::ModifiersChanged(none));
            });
            self.settle();
        }

        /// Sustituye el contenido del campo de texto que tiene el foco.
        fn retype(&mut self, text: &str) {
            self.key(egui::Key::A, egui::Modifiers::COMMAND);
            self.frame_with(|i| i.events.push(egui::Event::Text(text.to_string())));
            self.settle();
        }

        /// Órdenes que la vista ha mandado al cliente desde la última vez.
        fn sent(&mut self) -> Vec<SftpCmd> {
            std::iter::from_fn(|| self.cmds.try_recv().ok()).collect()
        }

        /// Hace llegar a la vista la pregunta de una transferencia.
        fn conflict(
            &mut self,
            id: u64,
            source_size: u64,
            dest_size: u64,
            multiple: bool,
        ) -> oneshot::Receiver<ConflictAnswer> {
            let (reply, answer) = oneshot::channel();
            self.event(SftpEvent::Conflict(Conflict {
                id,
                upload: true,
                path: format!("/srv/datos/fichero-{id}"),
                source_size,
                dest_size,
                multiple,
                reply,
            }));
            self.settle();
            answer
        }

        /// Suelta ficheros sobre la vista y devuelve los identificadores de
        /// las subidas que lanza.
        fn drop_files(&mut self, paths: &[&str]) -> Vec<u64> {
            self.frame_with(|i| i.dropped_files = paths.iter().map(|p| dropped(p)).collect());
            self.settle();
            self.sent()
                .into_iter()
                .map(|cmd| match cmd {
                    SftpCmd::Upload { id, .. } => id,
                    _ => panic!("se esperaba una subida"),
                })
                .collect()
        }
    }

    #[test]
    fn dropped_files_are_uploaded_to_the_visible_remote_folder() {
        let mut h = Harness::new();
        // Mientras se arrastra por encima se indica adónde irá, sin subir.
        h.frame_with(|i| i.hovered_files.push(hovered("/tmp/x/informe.pdf")));
        assert!(
            h.sees("Suelta para subir a /srv/datos"),
            "{:?}",
            h.all_texts()
        );
        assert!(h.sent().is_empty());
        h.frame();
        assert!(!h.sees("Suelta para subir"));

        // Al soltar: una subida por cada cosa, a la carpeta remota visible
        // y preguntando si el destino ya existe.
        h.frame_with(|i| {
            i.dropped_files = vec![dropped("/tmp/x/informe.pdf"), dropped("/tmp/x/carpeta")];
        });
        let sent = h.sent();
        let uploads: Vec<(&Path, &str)> = sent
            .iter()
            .map(|cmd| match cmd {
                SftpCmd::Upload {
                    local,
                    remote,
                    on_conflict: OnConflict::Ask,
                    ..
                } => (local.as_path(), remote.as_str()),
                _ => panic!("se esperaba una subida"),
            })
            .collect();
        assert_eq!(
            uploads,
            [
                (Path::new("/tmp/x/informe.pdf"), "/srv/datos/informe.pdf"),
                (Path::new("/tmp/x/carpeta"), "/srv/datos/carpeta"),
            ]
        );
        // Las dos van en el mismo lote y aparecen en la cola.
        assert_eq!(h.view.transfers.len(), 2);
        assert_eq!(h.view.transfers[0].batch, h.view.transfers[1].batch);
        h.settle();
        assert!(h.sees("informe.pdf") && h.sees("Transferencias"));
        // Lo soltado no se vuelve a subir en los fotogramas siguientes.
        assert!(h.sent().is_empty());

        // Algo sin nombre (la raíz) no se puede subir: se avisa.
        assert!(h.drop_files(&["/"]).is_empty());
        assert!(h.sees("No se puede subir /"));
    }

    #[test]
    fn drops_only_count_when_the_view_is_ready_and_uncovered() {
        let file = "/tmp/x/informe.pdf";
        let hover_and_drop = |h: &mut Harness| {
            h.frame_with(|i| i.hovered_files.push(hovered(file)));
            let hint = h.sees("Suelta para subir");
            (hint, h.drop_files(&[file]).len())
        };

        // Conectando todavía.
        let mut h = Harness::connecting();
        h.settle();
        assert_eq!(hover_and_drop(&mut h), (false, 0));
        // Conexión cerrada.
        h.event(SftpEvent::Closed(None));
        h.settle();
        assert_eq!(hover_and_drop(&mut h), (false, 0));

        // Con un diálogo de la vista abierto.
        let mut h = Harness::new();
        h.view.dialog = Some(Dialog::Mkdir {
            remote: true,
            name: String::new(),
            focused: false,
        });
        h.settle();
        assert_eq!(hover_and_drop(&mut h), (false, 0));
        h.view.dialog = None;
        h.settle();
        assert_eq!(hover_and_drop(&mut h), (true, 1));

        // Con otra ventana encima (la de temas también acepta ficheros).
        h.other_window = true;
        h.settle();
        assert_eq!(hover_and_drop(&mut h), (false, 0));
        h.other_window = false;
        h.settle();
        assert_eq!(hover_and_drop(&mut h), (true, 1));
    }

    #[test]
    fn conflicts_are_asked_and_answers_apply_to_the_whole_batch() {
        let mut h = Harness::new();
        let ids = h.drop_files(&["/tmp/x/a.bin", "/tmp/x/b.bin", "/tmp/x/c.bin"]);
        let all = "Hacer lo mismo con los demás de esta transferencia";

        // Destino más pequeño: se ofrece reanudar.
        let mut first = h.conflict(ids[0], 3000, 1000, false);
        assert!(h.sees("El fichero ya existe") && h.sees("/srv/datos/fichero-"));
        assert!(h.sees("Origen: 2.9 KB") && h.sees("Destino: 1000 B"));
        assert!(first.try_recv().is_err());
        h.click(all);
        h.click("Reanudar");
        let answer = ConflictAnswer {
            choice: ConflictChoice::Resume,
            all: true,
        };
        assert_eq!(first.try_recv(), Ok(answer));
        assert!(!h.sees("El fichero ya existe"));

        // Otra subida del mismo lote: la respuesta ya está dada.
        let mut second = h.conflict(ids[1], 500, 100, false);
        assert_eq!(second.try_recv(), Ok(answer));
        assert!(!h.sees("El fichero ya existe"));

        // «Reanudar» no vale para un destino que no es más pequeño: se
        // pregunta, sin esa opción y con la casilla otra vez desmarcada.
        let mut third = h.conflict(ids[2], 500, 500, false);
        assert!(h.sees("El fichero ya existe") && !h.sees("Reanudar"));
        assert!(!h.view.conflict_all);
        h.click("Omitir");
        let skip = ConflictAnswer {
            choice: ConflictChoice::Skip,
            all: false,
        };
        assert_eq!(third.try_recv(), Ok(skip));

        // Un fichero suelto no tiene «demás»: no hay casilla. Escape lo
        // deja como está.
        let ids = h.drop_files(&["/tmp/x/solo.bin"]);
        let mut single = h.conflict(ids[0], 500, 500, false);
        assert!(h.sees("Sustituir") && !h.sees(all));
        h.key(egui::Key::Escape, egui::Modifiers::NONE);
        assert_eq!(single.try_recv(), Ok(skip));
        // …pero una carpeta sí, aunque sea una sola transferencia.
        let ids = h.drop_files(&["/tmp/x/carpeta"]);
        let mut folder = h.conflict(ids[0], 500, 500, true);
        assert!(h.sees(all));
        h.click("Sustituir");
        let replace = ConflictAnswer {
            choice: ConflictChoice::Replace,
            all: false,
        };
        assert_eq!(folder.try_recv(), Ok(replace));
    }

    #[test]
    fn queued_conflicts_follow_an_answer_for_all_and_cancel_stops_the_batch() {
        let mut h = Harness::new();
        let ids = h.drop_files(&["/tmp/x/a.bin", "/tmp/x/b.bin", "/tmp/x/c.bin"]);
        // Tres preguntas a la vez: se enseña la primera y las otras esperan.
        let mut answers: Vec<_> = ids.iter().map(|&id| h.conflict(id, 9, 9, false)).collect();
        assert_eq!(h.view.conflicts.len(), 3);
        h.click("Hacer lo mismo con los demás de esta transferencia");
        h.click("Sustituir");
        let replace = ConflictAnswer {
            choice: ConflictChoice::Replace,
            all: true,
        };
        for answer in &mut answers {
            assert_eq!(answer.try_recv(), Ok(replace));
        }
        assert!(h.view.conflicts.is_empty() && !h.sees("El fichero ya existe"));

        // «Cancelar» cancela todas las del lote y deja sin respuesta la
        // pregunta (con lo que su transferencia también se cancela).
        let ids = h.drop_files(&["/tmp/x/d.bin", "/tmp/x/e.bin"]);
        let mut pending = h.conflict(ids[0], 9, 9, false);
        h.click("Cancelar");
        let mut cancelled: Vec<u64> = h
            .sent()
            .iter()
            .map(|cmd| match cmd {
                SftpCmd::Cancel(id) => *id,
                _ => panic!("se esperaba una cancelación"),
            })
            .collect();
        cancelled.sort();
        cancelled.dedup();
        assert_eq!(cancelled, ids);
        assert_eq!(
            pending.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        );

        // Si la transferencia termina (o se cancela) antes de contestar, la
        // pregunta se retira.
        let ids = h.drop_files(&["/tmp/x/f.bin"]);
        let _pending = h.conflict(ids[0], 9, 9, false);
        assert!(h.sees("El fichero ya existe"));
        h.event(SftpEvent::TransferFinished {
            id: ids[0],
            error: Some("Cancelado".into()),
            stats: TransferStats::default(),
        });
        h.settle();
        assert!(!h.sees("El fichero ya existe") && h.sees("Error: Cancelado"));
        assert!(matches!(h.sent()[..], [SftpCmd::List(_)]));

        // Al cerrar la vista con una pregunta pendiente, la transferencia
        // se entera: no se queda esperando.
        let ids = h.drop_files(&["/tmp/x/g.bin"]);
        let mut pending = h.conflict(ids[0], 9, 9, false);
        drop(h);
        assert_eq!(
            pending.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        );
    }

    #[test]
    fn permissions_dialog_keeps_boxes_and_octal_in_sync() {
        let mut h = Harness::new();
        // La lista remota enseña los permisos de cada entrada.
        assert!(h.sees("rw-r--r--") && h.sees("rwxr-xr-x"));

        // Menú contextual de una entrada y, en él, «Permisos…».
        h.right_click("script-remoto.sh");
        let selected: Vec<_> = h.view.remote_sel.iter().collect();
        assert_eq!(selected, ["script-remoto.sh"]);
        h.click("Permisos…");
        let state = |h: &Harness| match &h.view.dialog {
            Some(Dialog::Permissions { mode, text, .. }) => (*mode, text.clone()),
            _ => panic!("no está abierto el diálogo de permisos"),
        };
        assert_eq!(state(&h), (0o644, "644".to_string()));
        assert!(h.sees("«script-remoto.sh»") && h.sees("Lectura") && h.sees("Otros"));
        // Un fichero no tiene contenido al que aplicarlo.
        assert!(!h.sees("Aplicar también"));

        // Marcar una casilla actualiza el octal…
        let cell = |h: &Harness, who: &str, what: &str| {
            egui::pos2(h.rect_of(what).min.x + 9.0, h.rect_of(who).center().y)
        };
        let pos = cell(&h, "Dueño", "Ejecución");
        h.click_at(pos, egui::PointerButton::Primary);
        assert_eq!(state(&h), (0o744, "744".to_string()));
        assert!(h.sees("rwxr--r--"));
        let pos = cell(&h, "Otros", "Lectura");
        h.click_at(pos, egui::PointerButton::Primary);
        assert_eq!(state(&h), (0o740, "740".to_string()));

        // …y escribir el octal, las casillas.
        h.click("740");
        h.retype("600");
        assert_eq!(state(&h).0, 0o600);
        assert!(h.sees("rw-------"));
        // Un valor a medias o imposible no cambia nada ni se puede aplicar.
        h.retype("68");
        assert_eq!(state(&h), (0o600, "68".to_string()));
        assert!(h.sees("no es válido"));
        h.key(egui::Key::Enter, egui::Modifiers::NONE);
        assert!(h.view.dialog.is_some() && h.sent().is_empty());
        // También vale como lo escribe `ls`, y los bits especiales.
        h.click("68");
        h.retype("rwxr-x---");
        assert_eq!(state(&h).0, 0o750);
        h.retype("4755");
        assert_eq!(state(&h).0, 0o4755);
        assert!(h.sees("rwsr-xr-x"));
        // Las casillas conservan los bits especiales.
        let pos = cell(&h, "Otros", "Ejecución");
        h.click_at(pos, egui::PointerButton::Primary);
        assert_eq!(state(&h), (0o4754, "4754".to_string()));

        h.click("Aplicar");
        assert!(h.view.dialog.is_none());
        assert!(matches!(
            &h.sent()[..],
            [SftpCmd::Chmod { path, mode: 0o4754, recursive: false }]
                if path == "/srv/datos/script-remoto.sh"
        ));

        // En una carpeta se puede aplicar también a su contenido.
        h.right_click("carpeta-remota");
        h.click("Permisos…");
        assert_eq!(state(&h), (0o755, "755".to_string()));
        h.click("Aplicar también a todo su contenido");
        h.click("Aplicar");
        assert!(matches!(
            &h.sent()[..],
            [SftpCmd::Chmod { path, mode: 0o755, recursive: true }]
                if path == "/srv/datos/carpeta-remota"
        ));

        // De un enlace no se saben los permisos de su destino; y cancelar
        // no manda nada.
        h.view.open_permissions("enlace-remoto");
        h.settle();
        assert!(h.sees("No se conocen sus permisos"));
        h.click("Cancelar");
        assert!(h.view.dialog.is_none() && h.sent().is_empty());
    }

    fn tree(files: &[(&str, u64, i64)], dirs: &[&str]) -> SyncTree {
        SyncTree {
            files: files
                .iter()
                .map(|&(path, size, mtime)| {
                    let mtime = Some(mtime);
                    (path.to_string(), SyncFile { size, mtime })
                })
                .collect(),
            dirs: dirs.iter().map(|d| d.to_string()).collect(),
            ..Default::default()
        }
    }

    fn sample_scan() -> SyncScan {
        SyncScan {
            local: tree(
                &[("igual.txt", 5, 100), ("nuevo.txt", 2048, 100)],
                &["solo-local"],
            ),
            remote: tree(
                &[("igual.txt", 5, 100), ("sobra/viejo.txt", 7, 100)],
                &["sobra"],
            ),
        }
    }

    impl Harness {
        /// Abre «Sincronizar…» y devuelve el identificador de la comparación.
        fn open_sync(&mut self) -> u64 {
            self.click("Sincronizar…");
            let local = self.view.local_path.clone();
            match &self.sent()[..] {
                [
                    SftpCmd::SyncScan {
                        id,
                        local: l,
                        remote,
                    },
                ] => {
                    assert_eq!((l, remote.as_str()), (&local, "/srv/datos"));
                    *id
                }
                _ => panic!("se esperaba una comparación"),
            }
        }

        fn scanned(&mut self, id: u64, result: Result<SyncScan, String>) {
            self.event(SftpEvent::SyncScanned { id, result });
            self.settle();
        }

        fn sync(&self) -> &SyncDialog {
            match &self.view.dialog {
                Some(Dialog::Sync(sync)) => sync,
                _ => panic!("no está abierto el diálogo de sincronización"),
            }
        }
    }

    #[test]
    fn sync_shows_the_plan_and_runs_it_only_when_confirmed() {
        let mut h = Harness::new();
        let id = h.open_sync();
        assert!(h.sees("Comparando las dos carpetas"));
        h.event(SftpEvent::Progress {
            id,
            done: 0,
            total: 0,
            current: "/srv/datos/sobra".into(),
        });
        h.settle();
        assert!(h.sees("/srv/datos/sobra"));

        h.scanned(id, Ok(sample_scan()));
        // Plan por defecto: de local a remoto y sin borrar nada.
        assert_eq!((h.sync().mode, h.sync().delete), (SyncMode::Upload, false));
        assert!(h.sees("1 fichero (2.0 KB)"), "{:?}", h.all_texts());
        assert!(h.sees("1 en local, 1 en el servidor"));
        assert!(h.sees("nuevo.txt") && h.sees("sobra/viejo.txt"));
        assert!(h.sees("solo en el servidor"));
        // Lo que se borraría está a la vista en la casilla, sin marcar.
        let delete = "Borrar del servidor lo que no está en el origen: 1 fichero y 1 carpeta";
        assert!(h.sees(delete) && !h.sees("Se BORRARÁN"));
        // Hasta aquí no se ha mandado hacer nada.
        assert!(h.sent().is_empty());

        // Al marcarla, el aviso y el botón lo dicen bien claro.
        h.click(delete);
        assert!(h.sync().delete);
        assert!(h.sees("Se BORRARÁN del servidor 1 fichero y 1 carpeta"));
        assert!(h.sees("Sincronizar y borrar") && h.sees("sobra: se borra"));

        // Cambiar de sentido la desmarca: lo que sobra ya es otra cosa.
        h.click("Remoto ➡ local");
        assert_eq!(
            (h.sync().mode, h.sync().delete),
            (SyncMode::Download, false)
        );
        assert!(!h.sees("Se BORRARÁN"));
        assert!(
            h.sees("Borrar del equipo local lo que no está en el origen: 1 fichero y 1 carpeta")
        );
        // En los dos sentidos no se puede borrar.
        h.click("En los dos sentidos");
        assert_eq!(h.sync().mode, SyncMode::Both);
        h.click("Borrar lo que sobra en el destino");
        assert!(!h.sync().delete && !h.sees("Se BORRARÁN"));
        let summary = h.sync().summary;
        assert_eq!((summary.upload, summary.download), (1, 1));

        // Se confirma el plan de subida, sin borrado.
        h.click("Local ➡ remoto");
        assert!(h.sent().is_empty());
        h.click("Sincronizar");
        assert!(h.view.dialog.is_none());
        let expected: Vec<SyncItem> = plan_sync(&sample_scan(), SyncMode::Upload, false)
            .into_iter()
            .filter(|item| item.op != SyncOp::None)
            .collect();
        assert_eq!(expected.len(), 2);
        let sent = h.sent();
        let sync_id = match &sent[..] {
            [
                SftpCmd::Sync {
                    id,
                    local,
                    remote,
                    items,
                },
            ] => {
                assert_eq!((local, remote.as_str()), (&h.view.local_path, "/srv/datos"));
                assert_eq!(items, &expected);
                *id
            }
            _ => panic!("se esperaba una sincronización"),
        };
        // Va a la cola de transferencias, con su progreso y su «Cancelar».
        h.event(SftpEvent::Progress {
            id: sync_id,
            done: 1024,
            total: 2048,
            current: "nuevo.txt".into(),
        });
        h.settle();
        assert!(h.sees("Sincronizar /srv/datos") && h.sees("1.0 KB / 2.0 KB"));
        h.click("Cancelar");
        assert!(matches!(h.sent()[..], [SftpCmd::Cancel(id)] if id == sync_id));
        // Al terminar se actualiza el panel remoto.
        let stats = TransferStats {
            bytes: 2048,
            copied: 1,
            ..Default::default()
        };
        h.event(SftpEvent::TransferFinished {
            id: sync_id,
            error: None,
            stats,
        });
        h.settle();
        assert!(h.sees("Completada: 1 copiado (2.0 KB)"));
        assert!(matches!(&h.sent()[..], [SftpCmd::List(path)] if path == "/srv/datos"));
    }

    #[test]
    fn sync_with_delete_and_ways_out_of_the_dialog() {
        // Con la casilla marcada, el plan que se ejecuta incluye el borrado.
        let mut h = Harness::new();
        let id = h.open_sync();
        h.scanned(id, Ok(sample_scan()));
        h.click("Borrar del servidor");
        h.click("Sincronizar y borrar");
        let sent = h.sent();
        let [SftpCmd::Sync { items, .. }] = &sent[..] else {
            panic!("se esperaba una sincronización");
        };
        let ops: Vec<(&str, SyncOp)> = items.iter().map(|i| (i.path.as_str(), i.op)).collect();
        assert_eq!(
            ops,
            [
                ("nuevo.txt", SyncOp::Upload),
                ("sobra", SyncOp::DeleteRemote),
                ("sobra/viejo.txt", SyncOp::DeleteRemote),
                ("solo-local", SyncOp::Upload),
            ]
        );

        // Cancelar el plan no hace nada.
        let id = h.open_sync();
        h.scanned(id, Ok(sample_scan()));
        h.click("Cancelar");
        assert!(h.view.dialog.is_none() && h.sent().is_empty());

        // Cerrar durante la comparación la cancela, y su resultado, si
        // llega tarde, se ignora.
        let id = h.open_sync();
        h.click("Cancelar");
        assert!(matches!(h.sent()[..], [SftpCmd::Cancel(of)] if of == id));
        h.scanned(id, Ok(sample_scan()));
        assert!(h.view.dialog.is_none());
        let id = h.open_sync();
        h.key(egui::Key::Escape, egui::Modifiers::NONE);
        assert!(matches!(h.sent()[..], [SftpCmd::Cancel(of)] if of == id));

        // Un fallo al comparar se explica.
        let id = h.open_sync();
        h.scanned(id, Err("leyendo /srv/datos: permiso denegado".into()));
        assert!(h.sees("No se pudieron comparar las carpetas: leyendo /srv/datos"));
        h.click("Cerrar");
        assert!(h.view.dialog.is_none() && h.sent().is_empty());

        // Con las carpetas iguales no hay nada que confirmar.
        let same = tree(&[("a.txt", 1, 100)], &[]);
        let scan = SyncScan {
            local: same.clone(),
            remote: same,
        };
        let id = h.open_sync();
        h.scanned(id, Ok(scan));
        assert!(h.sees("No hay nada que copiar ni borrar"));
        assert!(h.sees("Las dos carpetas tienen lo mismo"));
        h.click("Sincronizar");
        assert!(h.view.dialog.is_some() && h.sent().is_empty());
    }

    #[test]
    fn finished_transfers_say_what_happened() {
        let stats = |bytes, copied, resumed, skipped, deleted| TransferStats {
            bytes,
            copied,
            resumed,
            skipped,
            deleted,
        };
        let text = |kind, done, stats| finished_text(kind, done, &stats);
        use TransferKind::*;
        assert_eq!(
            text(Upload, 2048, stats(2048, 1, 0, 0, 0)),
            "Completada (2.0 KB)"
        );
        assert_eq!(
            text(Download, 3072, stats(1024, 1, 1, 0, 0)),
            "Completada (3.0 KB), reanudada: faltaban 1.0 KB"
        );
        assert_eq!(text(Upload, 0, stats(0, 0, 0, 1, 0)), "Omitida");
        assert_eq!(
            text(Upload, 2048, stats(2048, 3, 0, 2, 0)),
            "Completada (2.0 KB), 2 omitidos"
        );
        assert_eq!(
            text(Sync, 2048, stats(2048, 2, 0, 0, 3)),
            "Completada: 2 copiados (2.0 KB), 3 borrados"
        );
        assert_eq!(count_text(1, 0), "1 fichero");
        assert_eq!(count_text(3, 1), "3 ficheros y 1 carpeta");
        assert_eq!(count_text(0, 2), "0 ficheros y 2 carpetas");
        assert_eq!(
            text(Sync, 9, stats(9, 1, 0, 0, 1)),
            "Completada: 1 copiado (9 B), 1 borrado"
        );
    }
}
