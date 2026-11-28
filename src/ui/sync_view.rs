//! Ventana «Sincronización» y el trabajo de fondo que mantiene al día la
//! configuración con la carpeta de sincronización (véase [`crate::sync`]).
//!
//! Leer y escribir la carpeta (puede ser una unidad de red lenta) se hace
//! en hilos aparte. La fusión con lo local se hace en el hilo de la
//! interfaz, entre dos fotogramas: así no puede cruzarse con un cambio que
//! el usuario esté guardando.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::RichText;

use super::{App, VaultDialog};
use crate::i18n::tr;
use crate::macros::MacroStore;
use crate::model::Tree;
use crate::sync::{
    self, Applied, Conflict, Expect, Keyring, Remote, SYNC_FILE, SealError, Side, Snapshot, Stamp,
    SyncConfig, SyncError, SyncState,
};
use crate::theme::{Settings, ThemeStore, error_color, ok_color, warn_color};
use crate::trf;
use crate::tunnel_manager::TunnelStore;

/// Nombre con el que se guarda en el baúl la contraseña de sincronización.
pub const PASSWORD_KEY: &str = "sincronización/password";

/// Cada cuánto se mira (sin leer nada) si algo ha cambiado aquí o allí.
const POLL_EVERY: Duration = Duration::from_secs(15);
/// Espera antes de reintentar cuando la carpeta no está o está a medias.
const RETRY_AFTER: Duration = Duration::from_secs(30);
const MAX_LOG: usize = 60;

/// Cómo está la sincronización, para enseñarlo.
#[derive(Debug, Clone, PartialEq)]
enum Health {
    /// Todavía no se ha sincronizado desde que se abrió la aplicación.
    Unknown,
    UpToDate,
    /// Hay cambios sin enviar o sin traer (se sincronizará enseguida).
    Pending,
    /// La carpeta no está disponible o el fichero está a medias: se
    /// reintenta solo.
    Offline(String),
    WrongPassword,
    /// Algo que no se arregla reintentando.
    Failed(String),
}

/// Lo que devuelve un hilo de trabajo.
enum Done {
    Read(Box<Result<Remote, SyncError>>, Stamp),
    Written(Result<String, SyncError>, Stamp),
    Polled(Stamp),
}

/// Lo que el usuario escribe en la ventana, antes de aplicarlo.
struct SyncWindow {
    folder: String,
    name: String,
    password: String,
    repeat: String,
    /// Contraseña maestra del baúl de la carpeta, si es distinta.
    other_master: String,
    /// Lo que resultó de abrir con ella el baúl de la carpeta.
    other_preview: Option<VaultPreview>,
    /// Falta confirmar que se reescribe la carpeta.
    confirm_rewrite: bool,
    message: Option<(String, bool)>,
}

/// Estado de la sincronización en la aplicación.
pub(super) struct SyncUi {
    config: SyncConfig,
    state: SyncState,
    /// Contraseña de sincronización (solo en memoria) y sus claves.
    keys: Option<Arc<Mutex<Keyring>>>,
    /// La contraseña en memoria ya está (o no hace falta) en el baúl.
    stored: bool,
    window: Option<SyncWindow>,
    job: Option<Receiver<Done>>,
    /// Hay una lectura o una escritura en marcha (no un simple vistazo).
    syncing: bool,
    health: Health,
    /// Se ha pedido sincronizar en cuanto se pueda.
    wanted: bool,
    /// Lo ha pedido el usuario: se le cuenta el resultado.
    by_hand: bool,
    /// Reescribir la carpeta con lo de este equipo, sin leerla.
    rewrite: bool,
    next_sync: Instant,
    next_poll: Instant,
    /// Lo leído de la carpeta, a la espera de poder fusionarlo.
    held: Option<Remote>,
    /// Cómo estaban los ficheros tras la última sincronización.
    local_seen: Vec<(String, Stamp)>,
    remote_seen: Stamp,
    /// El baúl de la carpeta, cuando está en conflicto con el de aquí.
    their_vault: Option<String>,
    /// Por qué no se pudieron combinar los baúles la última vez que se pidió.
    vault_error: Option<String>,
    /// Copias «en conflicto» que hay en la carpeta.
    copies: Vec<String>,
    log: VecDeque<(String, String, bool)>,
}

impl SyncUi {
    pub(super) fn new() -> Self {
        let dir = crate::paths::data_dir();
        let now = Instant::now();
        Self {
            config: SyncConfig::load(&dir),
            state: SyncState::load(&dir),
            keys: None,
            stored: false,
            window: None,
            job: None,
            syncing: false,
            health: Health::Unknown,
            // Al arrancar, en cuanto haya contraseña.
            wanted: true,
            by_hand: false,
            rewrite: false,
            next_sync: now,
            next_poll: now + POLL_EVERY,
            held: None,
            local_seen: vec![],
            remote_seen: None,
            their_vault: None,
            vault_error: None,
            copies: vec![],
            log: VecDeque::new(),
        }
    }

    fn note(&mut self, message: impl Into<String>, is_error: bool) {
        let message = message.into();
        // El mismo aviso repetido (sin conexión cada medio minuto) no llena
        // la lista.
        if self
            .log
            .front()
            .is_some_and(|(_, last, _)| *last == message)
        {
            return;
        }
        self.log.push_front((
            chrono::Local::now().format("%H:%M:%S").to_string(),
            message,
            is_error,
        ));
        self.log.truncate(MAX_LOG);
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save(&crate::paths::data_dir()) {
            self.note(trf!("No se pudo guardar la configuración: {e}", e), true);
        }
    }

    fn save_state(&mut self) {
        if let Err(e) = self.state.save(&crate::paths::data_dir()) {
            self.note(trf!("No se pudo guardar el estado: {e}", e), true);
        }
    }

    fn ask(&mut self, by_hand: bool) {
        self.wanted = true;
        self.by_hand |= by_hand;
    }

    fn ready(&self) -> bool {
        self.config.enabled && !self.config.folder.trim().is_empty() && self.keys.is_some()
    }

    /// Texto corto y color para la barra de estado.
    fn summary(&self) -> Option<(String, egui::Color32)> {
        if !self.config.enabled {
            return None;
        }
        let conflicts = self.state.conflicts.len();
        Some(if self.keys.is_none() {
            (
                tr("Sincronización: falta la contraseña").into(),
                warn_color(),
            )
        } else if self.syncing {
            (tr("Sincronizando…").into(), ok_color())
        } else if conflicts > 0 {
            (
                trf!("Sincronización: {conflicts} conflicto(s)", conflicts),
                warn_color(),
            )
        } else {
            match &self.health {
                Health::UpToDate => (tr("Sincronizado").into(), ok_color()),
                Health::Unknown | Health::Pending => {
                    (tr("Sincronización pendiente").into(), warn_color())
                }
                Health::Offline(_) => (tr("Sincronización: sin carpeta").into(), warn_color()),
                Health::WrongPassword => (
                    tr("Sincronización: contraseña incorrecta").into(),
                    error_color(),
                ),
                Health::Failed(_) => (tr("Sincronización: error").into(), error_color()),
            }
        })
    }
}

pub(super) fn clock(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// Rutas de claves privadas que usan los equipos del árbol y que no existen
/// en este equipo (como mucho, unas cuantas).
fn missing_keys(tree: &Tree) -> Vec<String> {
    let mut missing: Vec<String> = tree
        .session_paths()
        .into_iter()
        .filter_map(|(id, _)| tree.resolved_session(id))
        .filter(|s| s.protocol.is_ssh() && s.auth == crate::model::AuthMethod::Key)
        .map(|s| s.key_path.trim().to_string())
        .filter(|path| !path.is_empty() && !crate::paths::expand_tilde(path).is_file())
        .collect();
    missing.sort();
    missing.dedup();
    missing.truncate(8);
    missing
}

/// A quién pertenece un secreto del baúl, sin enseñar su valor.
fn secret_label(key: &str, tree: &Tree) -> String {
    let (owner, what) = key.split_once('/').unwrap_or((key, ""));
    let owner = uuid::Uuid::parse_str(owner)
        .ok()
        .and_then(|id| tree.find(id))
        .map(|node| node.name())
        .unwrap_or_else(|| owner.to_string());
    match what {
        "password" => trf!("contraseña de «{owner}»", owner),
        "passphrase" => trf!("passphrase de «{owner}»", owner),
        "totp" => trf!("secreto TOTP de «{owner}»", owner),
        _ => trf!("secreto de «{owner}»", owner),
    }
}

/// Qué pasaría al combinar los dos baúles: secretos que entrarían, que se
/// quitarían y los que chocan (a quién pertenecen); o por qué no se puede.
type VaultPreview = Result<(usize, usize, Vec<String>), String>;

/// Lo que se puede hacer con el conflicto del baúl.
enum VaultChoice {
    /// Combinar secreto a secreto; en los que chocan gana este lado.
    Combine(Side),
    /// Quedarse con un baúl entero.
    Whole(Side),
}

impl App {
    pub(super) fn open_sync_window(&mut self) {
        self.sync.window = Some(SyncWindow {
            folder: self.sync.config.folder.clone(),
            name: self.sync.config.name.clone(),
            password: String::new(),
            repeat: String::new(),
            other_master: String::new(),
            other_preview: None,
            confirm_rewrite: false,
            message: None,
        });
    }

    /// Indicador de la barra de estado; al pulsarlo se abre la ventana.
    pub(super) fn sync_status(&mut self, ui: &mut egui::Ui) {
        let Some((text, color)) = self.sync.summary() else {
            return;
        };
        ui.separator();
        let label = ui
            .add(
                egui::Label::new(RichText::new(format!("🔄 {text}")).color(color))
                    .sense(egui::Sense::click()),
            )
            .on_hover_text(tr(
                "Sincronización de la configuración entre tus equipos (pulsa para abrirla)",
            ));
        if label.clicked() {
            self.open_sync_window();
        }
    }

    /// Lo que impide aplicar ahora cambios llegados de la carpeta: una
    /// ventana de edición abierta se quedaría con datos viejos.
    pub(super) fn sync_blocked_by(&self) -> Option<&'static str> {
        if self.editor.is_some() {
            Some(tr("el editor de equipos"))
        } else if self.folder_editor.is_some() {
            Some(tr("el editor de carpetas"))
        } else if self.macros_window.is_some() {
            Some(tr("la ventana de snippets"))
        } else if self.themes_window.is_some() {
            Some(tr("la ventana de temas"))
        } else if self.tunnels_window.is_some() {
            Some(tr("el gestor de túneles"))
        } else if self.confirm_delete.is_some() || self.vault_dialog.is_some() {
            Some(tr("un diálogo"))
        } else {
            None
        }
    }

    /// La contraseña de sincronización va y viene del baúl cuando está
    /// abierto; con el baúl cerrado vive solo en memoria.
    fn sync_password(&mut self) {
        let Some(vault) = &mut self.vault else {
            return;
        };
        match &self.sync.keys {
            None => {
                if let Some(password) = vault.get(PASSWORD_KEY) {
                    self.sync.keys = Some(Arc::new(Mutex::new(Keyring::new(password))));
                    self.sync.stored = true;
                    self.sync.ask(false);
                }
            }
            Some(keys) if !self.sync.stored => {
                let password = keys
                    .lock()
                    .map(|k| k.password().to_string())
                    .unwrap_or_default();
                if !password.is_empty() {
                    if let Err(e) = vault.set(PASSWORD_KEY, &password) {
                        self.sync.note(
                            trf!("No se pudo guardar en el baúl: {e}", e = format!("{e:#}")),
                            true,
                        );
                    }
                    self.sync.stored = true;
                }
            }
            Some(_) => {}
        }
    }

    fn sync_spawn(&mut self, work: impl FnOnce() -> Done + Send + 'static) {
        let (tx, rx) = channel();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work());
            waker();
        });
        self.sync.job = Some(rx);
    }

    fn sync_start_read(&mut self) {
        let Some(keys) = self.sync.keys.clone() else {
            return;
        };
        let folder = self.sync.config.folder_path();
        self.sync.syncing = true;
        self.sync.wanted = false;
        self.sync_spawn(move || {
            let seen = sync::stamp(&folder.join(SYNC_FILE));
            let result = match keys.lock() {
                Ok(mut keys) => sync::read_folder(&folder, &mut keys),
                Err(_) => Err(SyncError::Io(tr("el llavero no está disponible").into())),
            };
            Done::Read(Box::new(result), seen)
        });
    }

    fn sync_start_write(&mut self, snapshot: Snapshot, remote: &Remote, expect: Expect) {
        let Some(keys) = self.sync.keys.clone() else {
            return;
        };
        let folder = self.sync.config.folder_path();
        let params = remote.params.clone();
        self.sync.syncing = true;
        self.sync_spawn(move || {
            let result = match keys.lock() {
                Ok(mut keys) => {
                    sync::write_folder(&folder, &snapshot, &mut keys, params.as_ref(), &expect)
                }
                Err(_) => Err(SyncError::Io(tr("el llavero no está disponible").into())),
            };
            Done::Written(result, sync::stamp(&folder.join(SYNC_FILE)))
        });
    }

    /// Vuelve a cargar del disco lo que la sincronización ha cambiado. A
    /// diferencia de restaurar una copia, no cierra nada ni toca las
    /// pestañas abiertas.
    fn sync_reload(&mut self, applied: Applied) {
        if applied.tree {
            match Tree::load(&crate::paths::sessions_file()) {
                Ok(tree) => self.tree = tree,
                Err(e) => self.notify(
                    trf!(
                        "No se pudieron leer las sesiones: {e}",
                        e = format!("{e:#}")
                    ),
                    true,
                ),
            }
            if self.selected.is_some_and(|id| self.tree.find(id).is_none()) {
                self.selected = None;
            }
        }
        if applied.themes {
            self.themes = ThemeStore::load(&crate::paths::themes_dir());
        }
        if applied.settings {
            self.settings = Settings::load(&crate::paths::settings_file());
            crate::highlight::set(&self.settings.highlight);
            crate::terminal::set_paste_guard(self.settings.confirm_paste);
        }
        if applied.themes || applied.settings {
            self.settings.fix_themes(|name| self.themes.exists(name));
            // Puede ser otro tema, o el mismo con otros colores.
            self.applied_theme = None;
        }
        if applied.macros {
            self.macros = MacroStore::load(&crate::paths::macros_file()).unwrap_or_default();
        }
        if applied.tunnels {
            self.tunnel_store = TunnelStore::load(&crate::paths::tunnels_file());
            // Los túneles en marcha que ya no existen se paran.
            let store = &self.tunnel_store;
            self.tunnel_runs.retain(|id, _| store.get(*id).is_some());
        }
        if applied.vault {
            let reloaded = self.vault.as_mut().map(|vault| vault.reload());
            match reloaded {
                Some(Ok(())) => {}
                Some(Err(_)) => {
                    // Es otro baúl (otra contraseña maestra): hay que abrirlo.
                    self.vault = None;
                    self.vault_window = None;
                    self.vault_dialog = Some(VaultDialog::Unlock {
                        password: String::new(),
                        error: None,
                        focus: true,
                    });
                    self.notify(
                        tr("El baúl ha cambiado en otro equipo: ábrelo con su contraseña maestra"),
                        false,
                    );
                }
                None => self.notify(
                    tr("La sincronización ha traído el baúl de otro equipo: ya puedes abrirlo"),
                    false,
                ),
            }
        }
    }

    /// Fusiona lo leído de la carpeta con lo local y, si hace falta, lanza
    /// la escritura.
    fn sync_merge(&mut self, remote: Remote) {
        if let Some(what) = self.sync_blocked_by() {
            // Se guarda lo leído y se fusiona al cerrar la ventana.
            self.sync.held = Some(remote);
            self.sync.health = Health::Pending;
            self.sync.note(
                trf!("A la espera de que cierres {what} para sincronizar", what),
                false,
            );
            return;
        }
        let dir = crate::paths::data_dir();
        let rewrite = std::mem::take(&mut self.sync.rewrite);
        let source = if rewrite {
            Remote::default()
        } else {
            remote.clone()
        };
        let by_hand = std::mem::take(&mut self.sync.by_hand);
        self.sync.copies = remote.copies.clone();
        let had_conflicts = self.sync.state.conflicts.len();
        match sync::merge_local(&dir, &self.sync.config, &mut self.sync.state, &source) {
            Ok(outcome) => {
                self.sync_reload(outcome.applied);
                self.sync.local_seen = sync::local_stamp(&dir);
                for note in &outcome.notes {
                    self.sync.note(note.clone(), false);
                }
                if outcome.applied.tree {
                    // Las rutas de las claves viajan como texto: puede que
                    // aquí no exista el fichero.
                    let missing = missing_keys(&self.tree);
                    if !missing.is_empty() {
                        self.sync.note(
                            trf!(
                                "Claves privadas que no están en este equipo (se avisará al \
                                 conectar): {lista}",
                                lista = missing.join(", ")
                            ),
                            false,
                        );
                    }
                }
                if outcome.pulled + outcome.pushed > 0 || rewrite {
                    self.sync.note(
                        trf!(
                            "Sincronizado: {recibidos} cambio(s) recibidos y {enviados} enviados",
                            recibidos = outcome.pulled,
                            enviados = outcome.pushed
                        ),
                        false,
                    );
                }
                if let Some(copy) = &outcome.safety_copy {
                    self.sync.note(
                        trf!(
                            "Antes de aplicar los cambios se guardó una copia en {ruta}",
                            ruta = copy.display()
                        ),
                        false,
                    );
                }
                self.sync.their_vault = outcome.their_vault.clone();
                if let Some(window) = &mut self.sync.window {
                    // Lo averiguado sobre el otro baúl era de otra lectura.
                    window.other_preview = None;
                }
                let conflicts = outcome.conflicts.len();
                if conflicts > had_conflicts {
                    self.notify(
                        trf!(
                            "Sincronización: {conflicts} conflicto(s) por resolver (ábrela desde \
                             la barra de estado)",
                            conflicts
                        ),
                        true,
                    );
                } else if by_hand && conflicts == 0 {
                    self.notify(tr("Configuración sincronizada"), false);
                }
                self.sync.health = Health::UpToDate;
                self.sync.next_sync =
                    Instant::now() + Duration::from_secs(u64::from(self.sync.config.minutes) * 60);
                if let Some(snapshot) = outcome.write {
                    let expect = if rewrite {
                        Expect::Anything
                    } else {
                        Expect::Unchanged(remote.fingerprint.clone())
                    };
                    self.sync_start_write(snapshot, &source, expect);
                }
                // Con el baúl abierto, dos baúles que han cambiado se
                // combinan solos si ningún secreto choca.
                self.sync_combine_vaults(None, "");
            }
            Err(e) => self.sync_failed(e, by_hand),
        }
    }

    fn sync_failed(&mut self, e: SyncError, by_hand: bool) {
        let text = e.to_string();
        let now = Instant::now();
        self.sync.next_sync = now + RETRY_AFTER;
        match &e {
            SyncError::Changed => {
                // Otro equipo se adelantó: se vuelve a leer ya.
                self.sync.next_sync = now + Duration::from_secs(1);
                self.sync.health = Health::Pending;
            }
            SyncError::Seal(SealError::WrongPassword) => {
                // Quizá se cambió en otro equipo y el baúl ya trae la nueva.
                let newer = self
                    .vault
                    .as_ref()
                    .and_then(|v| v.get(PASSWORD_KEY))
                    .map(str::to_string);
                let current = self
                    .sync
                    .keys
                    .as_ref()
                    .and_then(|k| k.lock().ok().map(|k| k.password().to_string()));
                if newer.is_some() && newer != current && !by_hand {
                    self.sync.keys = newer.map(|p| Arc::new(Mutex::new(Keyring::new(&p))));
                    self.sync.next_sync = now;
                    self.sync.health = Health::Pending;
                    return;
                }
                self.sync.health = Health::WrongPassword;
            }
            _ if e.is_transient() => self.sync.health = Health::Offline(text.clone()),
            _ => self.sync.health = Health::Failed(text.clone()),
        }
        self.sync.note(text.clone(), !e.is_transient());
        if by_hand && e != SyncError::Changed {
            self.notify(text, true);
        }
    }

    /// Qué pasaría al combinar el baúl de este equipo con el de la carpeta.
    /// Con `master` hay que derivar una clave (tarda): solo si se pide.
    fn sync_vault_preview(&self, master: Option<&str>) -> Option<VaultPreview> {
        let (Some(vault), Some(theirs)) = (&self.vault, &self.sync.their_vault) else {
            return None;
        };
        Some(
            vault
                .read_other(theirs.as_bytes(), master)
                .map(|their_secrets| {
                    let base = self
                        .sync
                        .state
                        .vault_base()
                        .and_then(|b| vault.read_other(b.as_bytes(), None).ok());
                    let plan = sync::plan_vault_merge(
                        base.as_deref(),
                        vault.entries(),
                        &their_secrets,
                        Side::Mine,
                    );
                    (
                        plan.set.len(),
                        plan.remove.len(),
                        plan.clashes
                            .iter()
                            .map(|key| secret_label(key, &self.tree))
                            .collect(),
                    )
                })
                .map_err(|e| format!("{e:#}")),
        )
    }

    /// Combina el baúl de este equipo con el de la carpeta. Sin `choice`
    /// solo lo hace si no hay que elegir nada (ningún secreto choca).
    /// `other_master` es la contraseña maestra del otro baúl, si es otra.
    /// Devuelve si se ha hecho; si falla, el motivo queda en `vault_error`.
    fn sync_combine_vaults(&mut self, choice: Option<VaultChoice>, other_master: &str) -> bool {
        let Some(theirs) = self.sync.their_vault.clone() else {
            return false;
        };
        let Some(conflict) = self
            .sync
            .state
            .conflicts
            .iter()
            .find(|c| c.is_vault())
            .cloned()
        else {
            return false;
        };
        let (prefer, by_hand) = match choice {
            Some(VaultChoice::Whole(side)) => {
                self.sync.state.decide(&conflict, side);
                self.sync.save_state();
                self.sync.ask(true);
                return true;
            }
            Some(VaultChoice::Combine(side)) => (side, true),
            None => (Side::Mine, false),
        };
        let Some(vault) = &mut self.vault else {
            return false;
        };
        let other_master = (!other_master.is_empty()).then_some(other_master);
        let dir = crate::paths::data_dir();
        let state = &mut self.sync.state;
        let result = (|| -> anyhow::Result<Option<(usize, usize)>> {
            let their_secrets = vault.read_other(theirs.as_bytes(), other_master)?;
            let base = state
                .vault_base()
                .and_then(|base| vault.read_other(base.as_bytes(), None).ok());
            let plan =
                sync::plan_vault_merge(base.as_deref(), vault.entries(), &their_secrets, prefer);
            if !by_hand && !plan.clashes.is_empty() {
                return Ok(None);
            }
            // El baúl se va a cambiar: antes, una copia de lo que hay.
            sync::safety_copy(&dir)?;
            vault.apply(&plan.set, &plan.remove)?;
            state.keep_merged_vault(&dir, &theirs)?;
            Ok(Some((plan.set.len(), plan.remove.len())))
        })();
        match result {
            Ok(Some((added, removed))) => {
                self.sync.save_state();
                self.sync.note(
                    trf!(
                        "Baúl combinado con el de la carpeta: {added} secreto(s) añadidos o \
                         cambiados y {removed} quitados",
                        added,
                        removed
                    ),
                    false,
                );
                self.sync.their_vault = None;
                self.sync.ask(by_hand);
                true
            }
            Ok(None) => false,
            Err(e) => {
                if by_hand {
                    self.sync.vault_error = Some(format!("{e:#}"));
                }
                false
            }
        }
    }

    /// Lo que hay que hacer en cada fotograma: recoger el trabajo de fondo
    /// y lanzar la siguiente sincronización cuando toque.
    pub(super) fn sync_tick(&mut self, ctx: &egui::Context) {
        if !self.sync.config.enabled {
            return;
        }
        self.sync_password();
        if let Some(job) = &self.sync.job {
            match job.try_recv() {
                Ok(done) => {
                    self.sync.job = None;
                    self.sync.syncing = false;
                    match done {
                        Done::Read(result, seen) => match *result {
                            Ok(remote) => {
                                self.sync.remote_seen = seen;
                                self.sync_merge(remote);
                            }
                            Err(e) => {
                                let by_hand = std::mem::take(&mut self.sync.by_hand);
                                if self.sync.rewrite && !e.is_transient() {
                                    // No se puede leer, pero se ha pedido
                                    // sustituirla.
                                    self.sync_merge(Remote::default());
                                } else {
                                    self.sync_failed(e, by_hand);
                                }
                            }
                        },
                        Done::Written(result, seen) => match result {
                            Ok(_) => self.sync.remote_seen = seen,
                            Err(e) => self.sync_failed(e, false),
                        },
                        Done::Polled(seen) => {
                            if seen != self.sync.remote_seen {
                                self.sync.health = Health::Pending;
                                self.sync.ask(false);
                            }
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.sync.job = None;
                    self.sync.syncing = false;
                }
            }
        }
        // Lo leído que esperaba a que se cerrase un editor.
        if self.sync.held.is_some()
            && self.sync.job.is_none()
            && self.sync_blocked_by().is_none()
            && let Some(remote) = self.sync.held.take()
        {
            self.sync_merge(remote);
        }
        if self.sync.ready() && self.sync.job.is_none() && self.sync.held.is_none() {
            let now = Instant::now();
            if now >= self.sync.next_poll {
                self.sync.next_poll = now + POLL_EVERY;
                // ¿Ha cambiado algo aquí? (Allí se mira en un hilo: puede
                // ser una unidad de red que tarde en contestar.)
                if sync::local_stamp(&crate::paths::data_dir()) != self.sync.local_seen {
                    self.sync.health = Health::Pending;
                    self.sync.ask(false);
                } else if !matches!(self.sync.health, Health::Offline(_)) {
                    let file = self.sync.config.folder_path().join(SYNC_FILE);
                    self.sync_spawn(move || Done::Polled(sync::stamp(&file)));
                }
            }
            if self.sync.job.is_none() && (self.sync.wanted || now >= self.sync.next_sync) {
                // Entretanto, que no se relance en cada fotograma.
                self.sync.next_sync = now + RETRY_AFTER;
                self.sync_start_read();
            }
        }
        // Sin actividad no habría otro fotograma en el que mirarlo.
        ctx.request_repaint_after(if self.sync.job.is_some() {
            Duration::from_millis(200)
        } else {
            Duration::from_secs(5)
        });
    }

    /// Ventana «Sincronización».
    pub(super) fn sync_window_ui(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.sync.window.take() else {
            return;
        };
        let mut open = true;
        let mut sync_now = false;
        let mut decisions: Vec<(Conflict, Side)> = vec![];
        let mut vault_choice = None;
        let mut new_password = None;
        let mut rewrite = false;
        let mut remove_copies = false;
        let mut unlock = false;
        let mut open_folder: Option<PathBuf> = None;
        let was_enabled = self.sync.config.enabled;
        let minutes_before = self.sync.config.minutes;
        let vault_open = self.vault.is_some();
        // Qué pasaría al combinar los baúles, para explicarlo. Si el de la
        // carpeta tiene otra contraseña maestra, lo que se averiguó con ella.
        let mut try_other = false;
        let vault_plan = match self.sync_vault_preview(None) {
            Some(Err(_)) if window.other_preview.is_some() => window.other_preview.clone(),
            other => other,
        };
        egui::Window::new(tr("Sincronización"))
            .id(egui::Id::unique("sync_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos([260.0, 60.0])
            .show(ctx, |ui| {
                ui.set_width(620.0);
                ui.weak(tr(
                    "Guarda tu configuración, cifrada, en una carpeta que ya repliques entre tus \
                     equipos (Dropbox, OneDrive, Syncthing, una unidad de red…) y la mantiene al \
                     día en todos.",
                ));
                let sync = &mut self.sync;
                ui.checkbox(
                    &mut sync.config.enabled,
                    tr("Sincronizar la configuración de este equipo"),
                );
                egui::Grid::new("sync_grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(tr("Carpeta"));
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut window.folder)
                                    .hint_text(tr("carpeta que ya se replica entre tus equipos"))
                                    .desired_width(400.0),
                            );
                            if ui.button(tr("Elegir…")).clicked()
                                && let Some(folder) = rfd::FileDialog::new()
                                    .set_title(tr("Carpeta de sincronización"))
                                    .pick_folder()
                            {
                                window.folder = folder.display().to_string();
                            }
                        });
                        ui.end_row();
                        ui.label(tr("Este equipo"));
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut window.name)
                                    .hint_text(tr("nombre con el que lo verán los demás"))
                                    .desired_width(220.0),
                            );
                            ui.label(tr("Cada"));
                            ui.add(
                                egui::DragValue::new(&mut sync.config.minutes)
                                    .range(1..=1440)
                                    .suffix(" min"),
                            )
                            .on_hover_text(tr(
                                "También se sincroniza al arrancar y a los pocos segundos de \
                                 cambiar algo aquí o en la carpeta",
                            ));
                        });
                        ui.end_row();
                        ui.label(tr("Contraseña"));
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut window.password)
                                    .password(true)
                                    .hint_text(if sync.keys.is_some() {
                                        tr("(puesta; escribe otra para cambiarla)")
                                    } else {
                                        tr("contraseña de sincronización")
                                    })
                                    .desired_width(200.0),
                            );
                            ui.add(
                                egui::TextEdit::singleline(&mut window.repeat)
                                    .password(true)
                                    .hint_text(tr("repítela"))
                                    .desired_width(140.0),
                            );
                            let valid =
                                !window.password.is_empty() && window.password == window.repeat;
                            if ui
                                .add_enabled(valid, egui::Button::new(tr("Usar")))
                                .on_disabled_hover_text(tr("Escribe la contraseña dos veces"))
                                .clicked()
                            {
                                new_password = Some(std::mem::take(&mut window.password));
                                window.repeat.clear();
                            }
                        });
                        ui.end_row();
                    });
                match (&sync.keys, vault_open) {
                    (None, true) => ui.colored_label(
                        warn_color(),
                        tr(
                            "Falta la contraseña de sincronización: escríbela (la misma en todos \
                             tus equipos).",
                        ),
                    ),
                    (None, false) => ui.colored_label(
                        warn_color(),
                        tr(
                            "Falta la contraseña de sincronización: abre el baúl, donde se \
                             guarda, o escríbela.",
                        ),
                    ),
                    (Some(_), true) => ui.weak(tr("🔐 La contraseña está guardada en el baúl.")),
                    (Some(_), false) => ui.weak(tr(
                        "La contraseña está solo en memoria: con el baúl cerrado no se guarda y \
                         habrá que escribirla al volver a abrir GodRemote.",
                    )),
                };

                ui.separator();
                ui.horizontal(|ui| {
                    let (text, color) = if !sync.config.enabled {
                        (
                            tr("Desactivada").to_string(),
                            ui.visuals().weak_text_color(),
                        )
                    } else if sync.syncing {
                        (tr("Sincronizando…").to_string(), ok_color())
                    } else if !sync.state.conflicts.is_empty() {
                        (
                            trf!(
                                "{n} conflicto(s) por resolver",
                                n = sync.state.conflicts.len()
                            ),
                            warn_color(),
                        )
                    } else {
                        match &sync.health {
                            Health::Unknown => (tr("Sin sincronizar todavía").into(), warn_color()),
                            Health::UpToDate => (tr("Al día").into(), ok_color()),
                            Health::Pending => {
                                (tr("Hay cambios por sincronizar").into(), warn_color())
                            }
                            Health::Offline(e) => (trf!("{e}. Se reintentará.", e), warn_color()),
                            Health::WrongPassword => (
                                tr("La contraseña no es la de esta carpeta").into(),
                                error_color(),
                            ),
                            Health::Failed(e) => (e.clone(), error_color()),
                        }
                    };
                    ui.strong(tr("Estado:"));
                    ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
                });
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            sync.ready() && !sync.syncing,
                            egui::Button::new(tr("🔄 Sincronizar ahora")),
                        )
                        .on_disabled_hover_text(tr("Actívala e indica la carpeta y la contraseña"))
                        .clicked()
                    {
                        sync_now = true;
                    }
                    if let Some(at) = sync.state.last_sync {
                        ui.weak(trf!(
                            "Última: {fecha} · revisión {revision}",
                            fecha = clock(at),
                            revision = sync.state.revision
                        ));
                    }
                });
                if matches!(sync.health, Health::WrongPassword | Health::Failed(_)) && sync.ready()
                {
                    ui.horizontal_wrapped(|ui| {
                        if window.confirm_rewrite {
                            ui.colored_label(
                                warn_color(),
                                tr(
                                    "Se sustituirá lo que haya en la carpeta por la configuración \
                                     de este equipo, cifrada con la contraseña de ahora. Los \
                                     demás equipos la fusionarán con la suya.",
                                ),
                            );
                            if ui.button(tr("Sí, reescribir")).clicked() {
                                rewrite = true;
                                window.confirm_rewrite = false;
                            }
                            if ui.button(tr("No")).clicked() {
                                window.confirm_rewrite = false;
                            }
                        } else if ui
                            .button(tr("Reescribir la carpeta con lo de este equipo…"))
                            .on_hover_text(tr(
                                "Para cuando el fichero de la carpeta está dañado o se quiere \
                                 cambiar la contraseña",
                            ))
                            .clicked()
                        {
                            window.confirm_rewrite = true;
                        }
                    });
                }
                if !sync.state.devices.is_empty() {
                    let mut devices: Vec<_> = sync.state.devices.iter().collect();
                    devices.sort_by_key(|(_, d)| std::cmp::Reverse(d.revision));
                    let list = devices
                        .iter()
                        .map(|(id, d)| {
                            let (nombre, revision, visto) = (&d.name, d.revision, clock(d.seen));
                            if **id == sync.config.device {
                                trf!(
                                    "{nombre} · este (revisión {revision}, {visto})",
                                    nombre,
                                    revision,
                                    visto
                                )
                            } else {
                                trf!(
                                    "{nombre} (revisión {revision}, {visto})",
                                    nombre,
                                    revision,
                                    visto
                                )
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" · ");
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(tr("Equipos vistos:"));
                        ui.label(list);
                    });
                }
                if !sync.copies.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(
                            warn_color(),
                            trf!(
                                "El servicio que replica la carpeta ha dejado {n} copia(s) «en \
                                 conflicto». No hacen falta: cada equipo vuelve a enviar lo suyo.",
                                n = sync.copies.len()
                            ),
                        )
                        .on_hover_text(sync.copies.join("\n"));
                        if ui.button(tr("Borrarlas")).clicked() {
                            remove_copies = true;
                        }
                    });
                }

                if !sync.state.conflicts.is_empty() {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.strong(trf!(
                            "Conflictos por resolver ({n})",
                            n = sync.state.conflicts.len()
                        ));
                        ui.weak(tr("cambiados aquí y en otro equipo de forma distinta"));
                    });
                    let others: Vec<&Conflict> = sync
                        .state
                        .conflicts
                        .iter()
                        .filter(|c| !c.is_vault())
                        .collect();
                    if others.len() > 1 {
                        ui.horizontal(|ui| {
                            for (label, side) in [
                                (tr("Quedarme con todo lo mío"), Side::Mine),
                                (tr("Quedarme con todo lo de la carpeta"), Side::Theirs),
                            ] {
                                if ui.button(label).clicked() {
                                    decisions.extend(others.iter().map(|c| ((*c).clone(), side)));
                                }
                            }
                        });
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("sync_conflicts")
                        .max_height(230.0)
                        .show(ui, |ui| {
                            for conflict in &sync.state.conflicts {
                                ui.group(|ui| {
                                    ui.set_width(ui.available_width());
                                    // Redactados ahora: el idioma puede no ser el de
                                    // cuando se creó el conflicto.
                                    let (title, detail) = conflict.texts();
                                    ui.strong(title);
                                    ui.label(detail);
                                    if conflict.is_vault() {
                                        vault_conflict_ui(
                                            ui,
                                            &mut window,
                                            vault_open,
                                            vault_plan.as_ref(),
                                            &mut try_other,
                                            &mut vault_choice,
                                            &mut unlock,
                                        );
                                    } else {
                                        ui.horizontal(|ui| {
                                            if ui.button(tr("Lo mío")).clicked() {
                                                decisions.push((conflict.clone(), Side::Mine));
                                            }
                                            if ui.button(tr("Lo de la carpeta")).clicked() {
                                                decisions.push((conflict.clone(), Side::Theirs));
                                            }
                                        });
                                    }
                                });
                            }
                        });
                    ui.weak(tr(
                        "Mientras no elijas, cada lado conserva lo suyo. Antes de cambiar nada \
                         en este equipo se guarda una copia de su configuración.",
                    ));
                }

                if let Some((message, is_error)) = &window.message {
                    ui.separator();
                    let color = if *is_error { error_color() } else { ok_color() };
                    ui.colored_label(color, message);
                }
                if !sync.log.is_empty() {
                    ui.separator();
                    egui::CollapsingHeader::new(tr("Últimos avisos"))
                        .id_salt("sync_log")
                        .default_open(false)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("sync_log_scroll")
                                .max_height(110.0)
                                .show(ui, |ui| {
                                    for (at, message, is_error) in &sync.log {
                                        let text = format!("{at}  {message}");
                                        if *is_error {
                                            ui.colored_label(error_color(), text);
                                        } else {
                                            ui.label(text);
                                        }
                                    }
                                });
                        });
                }
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.weak(tr(
                        "Se sincronizan los equipos y sus carpetas, los snippets, los túneles, \
                         los temas, los ajustes y el baúl (tal cual, cifrado con su contraseña \
                         maestra). No: las claves privadas (solo su ruta), los registros, las \
                         grabaciones, el historial de órdenes ni los ajustes propios de cada \
                         equipo (editor, shell local, carpeta de registros, proxy, servidor X).",
                    ));
                    if ui
                        .small_button(tr("Abrir la carpeta de las copias previas"))
                        .on_hover_text(tr(
                            "Los ficheros «antes-de-sincronizar-…» se restauran desde \
                             «Importar y copias de seguridad»",
                        ))
                        .clicked()
                    {
                        open_folder = Some(crate::paths::data_dir());
                    }
                });
            });

        // --- Lo pedido en la ventana -----------------------------------
        let folder = window.folder.trim().to_string();
        let name = window.name.trim().to_string();
        let mut config_changed = was_enabled != self.sync.config.enabled;
        if folder != self.sync.config.folder {
            self.sync.config.folder = folder;
            // Otra carpeta es otra historia: lo que se estuviera leyendo de
            // la anterior ya no vale, y se fusiona como la primera vez.
            self.sync.health = Health::Unknown;
            self.sync.held = None;
            self.sync.job = None;
            self.sync.syncing = false;
            self.sync.copies.clear();
            config_changed = true;
        }
        if !name.is_empty() && name != self.sync.config.name {
            self.sync.config.name = name;
            config_changed = true;
        }
        if config_changed || minutes_before != self.sync.config.minutes {
            self.sync.config.sanitize();
            self.sync.save_config();
            if self.sync.config.enabled && config_changed {
                self.sync.ask(false);
            }
        }
        if let Some(password) = new_password {
            self.sync.keys = Some(Arc::new(Mutex::new(Keyring::new(&password))));
            self.sync.stored = false;
            self.sync.health = Health::Unknown;
            self.sync.ask(true);
            window.message = Some((
                if self.vault.is_some() {
                    tr("Contraseña guardada en el baúl").into()
                } else {
                    tr("Contraseña puesta (solo en memoria: el baúl está cerrado)").into()
                },
                false,
            ));
        }
        if sync_now {
            window.message = None;
            match self.sync_blocked_by() {
                Some(what) => {
                    window.message = Some((trf!("Cierra antes {what}", what), true));
                }
                None => self.sync.ask(true),
            }
        }
        if rewrite {
            self.sync.rewrite = true;
            self.sync.ask(true);
        }
        if !decisions.is_empty() {
            for (conflict, side) in &decisions {
                self.sync.state.decide(conflict, *side);
            }
            self.sync.save_state();
            self.sync.ask(true);
        }
        if try_other {
            // Deriva una clave (tarda un momento): solo cuando se pide.
            match self.sync_vault_preview(Some(&window.other_master)) {
                Some(Err(e)) => window.message = Some((e, true)),
                preview => {
                    window.other_preview = preview;
                    window.message = None;
                }
            }
        }
        if let Some(choice) = vault_choice {
            // La ventana está fuera de `self` mientras se dibuja.
            let other_master = window.other_master.clone();
            if self.sync_combine_vaults(Some(choice), &other_master) {
                window.other_master.clear();
                window.other_preview = None;
                window.message = None;
            } else if let Some(e) = self.sync.vault_error.take() {
                window.message = Some((e, true));
            }
        }
        if remove_copies {
            let removed = sync::remove_conflict_copies(&self.sync.config.folder_path());
            self.sync.copies.clear();
            self.sync.note(
                trf!("{removed} copia(s) en conflicto borradas", removed),
                false,
            );
        }
        if unlock {
            self.vault_dialog = Some(VaultDialog::Unlock {
                password: String::new(),
                error: None,
                focus: true,
            });
        }
        if let Some(folder) = open_folder
            && let Err(e) = crate::paths::open_with_system(&folder)
        {
            self.notify(
                trf!(
                    "No se pudo abrir {ruta}: {error}",
                    ruta = folder.display(),
                    error = e
                ),
                true,
            );
        }
        if open {
            self.sync.window = Some(window);
        } else {
            self.focus_active_terminal();
        }
    }
}

/// Opciones para el conflicto del baúl: combinarlos secreto a secreto (con
/// el baúl abierto) o quedarse con uno entero.
fn vault_conflict_ui(
    ui: &mut egui::Ui,
    window: &mut SyncWindow,
    vault_open: bool,
    plan: Option<&VaultPreview>,
    try_other: &mut bool,
    choice: &mut Option<VaultChoice>,
    unlock: &mut bool,
) {
    match plan {
        Some(Ok((added, removed, clashes))) => {
            ui.label(trf!(
                "Se pueden combinar: de la carpeta entrarían {added} secreto(s) y se quitarían \
                 {removed} que se borraron allí.",
                added,
                removed
            ));
            if clashes.is_empty() {
                if ui.button(tr("Combinar los dos baúles")).clicked() {
                    *choice = Some(VaultChoice::Combine(Side::Mine));
                }
            } else {
                ui.colored_label(
                    warn_color(),
                    trf!(
                        "Con valores distintos en los dos: {lista}",
                        lista = clashes.join(", ")
                    ),
                );
                ui.horizontal_wrapped(|ui| {
                    if ui.button(tr("Combinar; en esos, lo mío")).clicked() {
                        *choice = Some(VaultChoice::Combine(Side::Mine));
                    }
                    if ui
                        .button(tr("Combinar; en esos, lo de la carpeta"))
                        .clicked()
                    {
                        *choice = Some(VaultChoice::Combine(Side::Theirs));
                    }
                });
            }
        }
        Some(Err(e)) => {
            ui.colored_label(warn_color(), e);
            ui.horizontal(|ui| {
                ui.label(tr("Contraseña maestra del baúl de la carpeta:"));
                ui.add(
                    egui::TextEdit::singleline(&mut window.other_master)
                        .password(true)
                        .desired_width(180.0),
                );
                if ui
                    .add_enabled(
                        !window.other_master.is_empty(),
                        egui::Button::new(tr("Abrir")),
                    )
                    .clicked()
                {
                    *try_other = true;
                }
            });
            ui.weak(tr(
                "Con ella se pueden combinar los dos baúles secreto a secreto.",
            ));
        }
        None if !vault_open => {
            ui.horizontal_wrapped(|ui| {
                ui.label(tr(
                    "Abre el baúl para combinarlos secreto a secreto, sin perder nada.",
                ));
                if ui.button(tr("Desbloquear…")).clicked() {
                    *unlock = true;
                }
            });
        }
        None => {
            ui.weak(tr("Vuelve a sincronizar para ver cómo combinarlos."));
        }
    }
    ui.horizontal_wrapped(|ui| {
        ui.weak(tr("O uno entero (el otro queda en la copia previa):"));
        if ui.small_button(tr("Mi baúl")).clicked() {
            *choice = Some(VaultChoice::Whole(Side::Mine));
        }
        if ui.small_button(tr("El de la carpeta")).clicked() {
            *choice = Some(VaultChoice::Whole(Side::Theirs));
        }
    });
}
