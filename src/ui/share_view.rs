//! Ventana «Carpetas compartidas» y el trabajo de fondo que publica las
//! carpetas propias y mantiene al día las que se siguen (véase
//! [`crate::share`]).
//!
//! Los ficheros compartidos se leen y se escriben en hilos aparte (suelen
//! estar en una unidad de red). Lo leído se aplica al árbol en el hilo de la
//! interfaz, y nunca mientras haya un editor abierto.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::RichText;
use uuid::Uuid;

use super::sync_view::clock;
use super::{App, TreeAction};
use crate::model::{AuthMethod, Tree};
use crate::share::{
    self, Export, FILE_SUFFIX, Fetched, Publication, ShareError, ShareStore, Subscription,
};
use crate::sync::{self, Keyring, Stamp};
use crate::theme::{error_color, ok_color, warn_color};

/// Cada cuánto se mira si ha cambiado un fichero que se sigue.
const POLL_EVERY: Duration = Duration::from_secs(30);
/// Cada cuánto se mira si ha cambiado una carpeta que se publica.
const CHECK_EVERY: Duration = Duration::from_secs(5);

type Keys = Arc<Mutex<Keyring>>;

/// Una suscripción mirada: cómo está su fichero y, si se ha leído, lo que
/// traía.
type Looked = (Uuid, Stamp, Option<Result<Fetched, ShareError>>);

/// Lo que devuelve un hilo de trabajo.
enum Done {
    Fetched(Vec<Looked>),
    Published(Box<Publication>, Box<Result<Export, ShareError>>),
    /// Primera lectura de una carpeta a la que se quiere suscribir.
    Subscribed(String, Keys, Box<Result<Fetched, ShareError>>),
}

/// Lo que el usuario escribe en la ventana, antes de aplicarlo.
#[derive(Default)]
struct ShareWindow {
    // Suscribirse.
    path: String,
    password: String,
    // Publicar.
    folder: Option<Uuid>,
    target: String,
    new_password: String,
    repeat: String,
    auto: bool,
    /// Contraseña que se está escribiendo para una entrada que no la tiene.
    missing: HashMap<Uuid, String>,
    /// «Mi usuario», «mi autenticación» y «mi clave» de cada suscripción.
    mine: HashMap<Uuid, (String, Option<AuthMethod>, String)>,
    /// Suscripción que se ha pedido dejar y falta confirmar.
    leaving: Option<Uuid>,
    message: Option<(String, bool)>,
}

/// Estado de las carpetas compartidas en la aplicación.
pub(super) struct ShareUi {
    store: ShareStore,
    /// Contraseñas en memoria, por publicación o suscripción.
    keys: HashMap<Uuid, Keys>,
    /// Las que ya están en el baúl.
    stored: BTreeSet<Uuid>,
    window: Option<ShareWindow>,
    job: Option<Receiver<Done>>,
    next_poll: Instant,
    next_check: Instant,
    /// Cómo estaba cada fichero seguido la última vez que se leyó.
    seen: HashMap<Uuid, Stamp>,
    /// Último resultado de cada publicación o suscripción.
    status: HashMap<Uuid, (String, bool)>,
    /// Suscripciones que hay que leer ya, aunque el fichero no cambie.
    forced: BTreeSet<Uuid>,
    /// Publicaciones que hay que escribir ya.
    publish_now: BTreeSet<Uuid>,
    /// Lo leído que espera a que se cierre un editor.
    held: Vec<(Uuid, Fetched)>,
    /// `sessions.json`, la última vez que se miró.
    tree_seen: Stamp,
    /// Las marcas del árbol no están al día.
    stale_marks: bool,
    /// Hay que mirar si alguna carpeta publicada ha cambiado, aunque el
    /// árbol no lo haya hecho (acaba de llegar su contraseña).
    recheck: bool,
}

impl ShareUi {
    pub(super) fn new() -> Self {
        let now = Instant::now();
        Self {
            store: ShareStore::load(&crate::paths::data_dir()),
            keys: HashMap::new(),
            stored: BTreeSet::new(),
            window: None,
            job: None,
            next_poll: now,
            next_check: now,
            seen: HashMap::new(),
            status: HashMap::new(),
            forced: BTreeSet::new(),
            publish_now: BTreeSet::new(),
            held: vec![],
            tree_seen: None,
            stale_marks: true,
            recheck: false,
        }
    }

    fn save(&mut self) -> Option<String> {
        self.store
            .save(&crate::paths::data_dir())
            .err()
            .map(|e| format!("No se pudo guardar shares.json: {e}"))
    }

    fn is_empty(&self) -> bool {
        self.store.publications.is_empty() && self.store.subscriptions.is_empty()
    }

    fn set_status(&mut self, id: Uuid, text: impl Into<String>, is_error: bool) {
        self.status.insert(id, (text.into(), is_error));
    }
}

fn keyring(password: &str) -> Keys {
    Arc::new(Mutex::new(Keyring::new(password)))
}

impl App {
    pub(super) fn open_share_window(&mut self) {
        self.shares.window = Some(ShareWindow {
            auto: true,
            folder: self
                .target_folder()
                .or_else(|| self.own_folders().first().map(|(id, _)| *id)),
            ..Default::default()
        });
    }

    /// Carpetas propias (no las que se siguen), con su ruta legible.
    fn own_folders(&self) -> Vec<(Uuid, String)> {
        self.tree
            .folder_paths()
            .into_iter()
            .filter(|(id, _)| !share::is_shared(*id))
            .collect()
    }

    /// Las carpetas compartidas son de solo lectura: lo que las cambiaría
    /// no se hace (y se explica). Devuelve la acción si puede seguir.
    pub(super) fn share_guard(&mut self, action: TreeAction) -> Option<TreeAction> {
        const READ_ONLY: &str = "Es de una carpeta compartida, de solo lectura. Duplícalo para \
                                 tener una copia tuya que puedas cambiar.";
        match action {
            TreeAction::Edit(id) if share::is_shared(id) => self.notify(READ_ONLY, true),
            TreeAction::Delete(id) | TreeAction::RenameFolder(id) if share::is_shared_root(id) => {
                // Lo que se puede cambiar de ella (o dejar de seguirla)
                // está en su ventana.
                self.open_share_window();
            }
            TreeAction::Delete(id) | TreeAction::RenameFolder(id) if share::is_shared(id) => {
                self.notify(READ_ONLY, true);
            }
            TreeAction::Move(id, parent)
                if share::is_shared(id) || parent.is_some_and(share::is_shared) =>
            {
                self.notify(
                    "Las carpetas compartidas no se reorganizan: son de quien las publica",
                    true,
                );
            }
            TreeAction::NewSession(Some(parent)) | TreeAction::NewFolder(Some(parent))
                if share::is_shared(parent) =>
            {
                self.notify(
                    "No se puede crear nada dentro de una carpeta compartida",
                    true,
                );
            }
            TreeAction::Duplicate(id) if share::is_shared(id) => self.share_duplicate(id),
            other => return Some(other),
        }
        None
    }

    /// Copia propia de un equipo compartido: fuera de la carpeta, con lo que
    /// heredaba de ella ya resuelto y con sus credenciales guardadas.
    fn share_duplicate(&mut self, id: Uuid) {
        let Some(resolved) = self.tree.resolved_session(id) else {
            return;
        };
        let mut copy = resolved.clone();
        copy.id = Uuid::new_v4();
        copy.name = format!("{} (copia)", resolved.display_name());
        copy.inherit_auth = false;
        copy.inherit_jumps = false;
        copy.secret_owner = None;
        if let Some(vault) = &mut self.vault {
            for (from, to) in [
                (resolved.password_key(), copy.password_key()),
                (resolved.passphrase_key(), copy.passphrase_key()),
            ] {
                if let Some(secret) = vault.get(&from).map(str::to_string) {
                    let _ = vault.set(&to, &secret);
                }
            }
        }
        self.selected = Some(copy.id);
        self.tree.upsert_session(None, copy);
        self.save_tree();
        self.notify(
            "Copia creada fuera de la carpeta compartida: ya es tuya y la puedes cambiar",
            false,
        );
    }

    fn share_spawn(&mut self, work: impl FnOnce() -> Done + Send + 'static) {
        let (tx, rx) = channel();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work());
            waker();
        });
        self.shares.job = Some(rx);
    }

    /// Las contraseñas van y vienen del baúl cuando está abierto, y se
    /// olvidan las de los equipos compartidos que han cambiado de dirección.
    fn share_vault(&mut self) {
        let Some(vault) = &mut self.vault else {
            return;
        };
        let shares = &mut self.shares;
        let entries: Vec<(Uuid, bool)> = shares
            .store
            .subscriptions
            .iter()
            .map(|s| (s.id, false))
            .chain(shares.store.publications.iter().map(|p| (p.id, true)))
            .collect();
        for (id, publishing) in entries {
            let key = share::password_key(id, publishing);
            match shares.keys.get(&id) {
                None => {
                    if let Some(password) = vault.get(&key) {
                        shares.keys.insert(id, keyring(password));
                        shares.stored.insert(id);
                        shares.forced.insert(id);
                        shares.recheck = true;
                    }
                }
                Some(keys) if !shares.stored.contains(&id) => {
                    let password = keys
                        .lock()
                        .map(|k| k.password().to_string())
                        .unwrap_or_default();
                    if !password.is_empty() {
                        let _ = vault.set(&key, &password);
                    }
                    shares.stored.insert(id);
                }
                Some(_) => {}
            }
        }
        let mut forgotten = 0;
        let mut cleared = false;
        for sub in &mut shares.store.subscriptions {
            for id in std::mem::take(&mut sub.forget) {
                cleared = true;
                let key = format!("{id}/password");
                if vault.contains(&key) && vault.remove(&key).is_ok() {
                    forgotten += 1;
                }
            }
        }
        if cleared {
            let _ = shares.save();
        }
        if forgotten > 0 {
            self.notify(
                format!(
                    "Se ha olvidado la contraseña guardada de {forgotten} equipo(s) compartidos \
                     cuya dirección ha cambiado: se te pedirá al conectar"
                ),
                false,
            );
        }
    }

    /// Aplica al árbol lo leído de una carpeta que se sigue.
    fn share_apply(&mut self, id: Uuid, fetched: Fetched) {
        let Some(index) = self
            .shares
            .store
            .subscriptions
            .iter()
            .position(|s| s.id == id)
        else {
            return;
        };
        if fetched.fingerprint == self.shares.store.subscriptions[index].file
            && self.tree.find(id).is_some()
        {
            self.shares.set_status(id, "Al día", false);
            return;
        }
        if self.sync_blocked_by().is_some() {
            self.shares.held.retain(|(held, _)| *held != id);
            self.shares.held.push((id, fetched));
            return;
        }
        let sub = &mut self.shares.store.subscriptions[index];
        let previous = sub.content.clone();
        match sub.accept(&fetched) {
            Ok(mut changes) => {
                changes.rescued = sub.place(&mut self.tree, previous.as_ref());
                let name = sub.name().to_string();
                let pending_forget = !sub.forget.is_empty();
                self.save_tree();
                if let Some(e) = self.shares.save() {
                    self.notify(e, true);
                }
                self.shares.stale_marks = true;
                self.shares.set_status(id, "Al día", false);
                // Las pestañas de un equipo que ya no está siguen como
                // estaban, pero no se reabrirán al arrancar.
                let orphans: Vec<&str> = changes
                    .removed
                    .iter()
                    .filter(|(gone, _)| {
                        self.tabs
                            .iter()
                            .any(|t| t.session.as_ref().is_some_and(|s| s.id == *gone))
                    })
                    .map(|(_, name)| name.as_str())
                    .collect();
                if !orphans.is_empty() {
                    self.notify(
                        format!(
                            "{} ya no está(n) en la carpeta compartida «{name}»: sus pestañas \
                             siguen abiertas, pero no se volverán a abrir solas",
                            orphans.join(", ")
                        ),
                        true,
                    );
                } else if previous.is_some() && !changes.is_empty() {
                    self.notify(
                        format!("Carpeta compartida «{name}»: {}", changes.summary()),
                        !changes.readdressed.is_empty(),
                    );
                }
                if pending_forget && self.vault.is_none() {
                    self.shares.set_status(
                        id,
                        "Al día. Al abrir el baúl se olvidará la contraseña guardada de los \
                         equipos que han cambiado de dirección",
                        false,
                    );
                }
            }
            Err(e) => self.shares.set_status(id, e.to_string(), true),
        }
    }

    /// Lo que hay que hacer en cada fotograma.
    pub(super) fn share_tick(&mut self, ctx: &egui::Context) {
        if self.shares.is_empty() && self.shares.job.is_none() {
            if self.shares.stale_marks {
                share::mark(&self.tree, &BTreeSet::new());
                self.shares.stale_marks = false;
            }
            return;
        }
        self.share_vault();
        // --- Trabajo terminado ---------------------------------------
        let done = self.shares.job.as_ref().map(|job| job.try_recv());
        match done {
            Some(Ok(done)) => {
                self.shares.job = None;
                self.share_done(done);
            }
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => self.shares.job = None,
            _ => {}
        }
        if !self.shares.held.is_empty() && self.sync_blocked_by().is_none() {
            for (id, fetched) in std::mem::take(&mut self.shares.held) {
                self.share_apply(id, fetched);
            }
        }
        // --- El árbol y sus marcas -----------------------------------
        let now = Instant::now();
        let mut tree_changed = std::mem::take(&mut self.shares.recheck);
        if now >= self.shares.next_check {
            self.shares.next_check = now + CHECK_EVERY;
            let seen = sync::stamp(&crate::paths::sessions_file());
            if seen != self.shares.tree_seen {
                self.shares.tree_seen = seen;
                self.shares.stale_marks = true;
                tree_changed = true;
            }
            // Una carpeta seguida que falta del árbol (se restauró una
            // copia de seguridad, por ejemplo) se vuelve a poner; y lo que
            // el usuario haya metido dentro de una se saca a la raíz: la
            // carpeta es de quien la publica.
            if self.sync_blocked_by().is_some() {
                // Con un editor abierto no se toca el árbol: se mira luego.
                self.shares.recheck |= tree_changed;
            } else {
                let mut rescued = 0;
                let mut replaced = false;
                for sub in &self.shares.store.subscriptions {
                    let missing = sub.content.is_some() && self.tree.find(sub.id).is_none();
                    if missing || (tree_changed && sub.has_strangers(&self.tree)) {
                        rescued += sub.place(&mut self.tree, sub.content.as_ref());
                        replaced = true;
                    }
                }
                if replaced {
                    self.save_tree();
                    self.shares.stale_marks = true;
                }
                if rescued > 0 {
                    self.notify(
                        format!(
                            "{rescued} elemento(s) que estaban dentro de una carpeta compartida \
                             han pasado a la raíz: esa carpeta es de quien la publica"
                        ),
                        false,
                    );
                }
            }
        }
        if self.shares.stale_marks {
            share::mark(&self.tree, &self.shares.store.roots());
            self.shares.stale_marks = false;
        }
        // --- Publicar lo que ha cambiado -------------------------------
        if self.shares.job.is_none() {
            let wanted = self
                .shares
                .store
                .publications
                .iter()
                .find(|p| {
                    if !self.shares.keys.contains_key(&p.id) {
                        return false;
                    }
                    if self.shares.publish_now.contains(&p.id) {
                        return true;
                    }
                    // Solo se recalcula cuando el árbol ha cambiado.
                    p.auto
                        && tree_changed
                        && matches!(
                            share::export(&self.tree, p.folder),
                            Some(Ok(export)) if export.digest != p.digest
                        )
                })
                .cloned();
            if let Some(publication) = wanted {
                self.shares.publish_now.remove(&publication.id);
                self.share_start_publish(publication);
            }
        }
        // --- Mirar los ficheros que se siguen ---------------------------
        if self.shares.job.is_none()
            && (now >= self.shares.next_poll || !self.shares.forced.is_empty())
        {
            self.shares.next_poll = now + POLL_EVERY;
            let forced = std::mem::take(&mut self.shares.forced);
            let work: Vec<(Uuid, PathBuf, Keys, Stamp, bool)> = self
                .shares
                .store
                .subscriptions
                .iter()
                .filter_map(|s| {
                    let keys = self.shares.keys.get(&s.id)?.clone();
                    Some((
                        s.id,
                        s.file_path(),
                        keys,
                        self.shares.seen.get(&s.id).copied().flatten(),
                        forced.contains(&s.id),
                    ))
                })
                .collect();
            if !work.is_empty() {
                self.share_spawn(move || {
                    Done::Fetched(
                        work.into_iter()
                            .map(|(id, path, keys, seen, forced)| {
                                let stamp = sync::stamp(&path);
                                if !forced && stamp.is_some() && stamp == seen {
                                    return (id, stamp, None);
                                }
                                let result = match keys.lock() {
                                    Ok(mut keys) => share::fetch(&path, &mut keys),
                                    Err(_) => Err(ShareError::Io("llavero no disponible".into())),
                                };
                                (id, stamp, Some(result))
                            })
                            .collect(),
                    )
                });
            }
        }
        // Sin actividad no habría otro fotograma en el que mirarlo.
        ctx.request_repaint_after(if self.shares.job.is_some() {
            Duration::from_millis(200)
        } else {
            CHECK_EVERY
        });
    }

    fn share_start_publish(&mut self, mut publication: Publication) {
        let Some(keys) = self.shares.keys.get(&publication.id).cloned() else {
            return;
        };
        // El hilo trabaja con una copia del árbol tal como está ahora.
        let tree: Tree = self.tree.clone();
        let publisher = sync::SyncConfig::load(&crate::paths::data_dir()).name;
        self.share_spawn(move || {
            let result = match keys.lock() {
                Ok(mut keys) => share::publish(&tree, &mut publication, &publisher, &mut keys),
                Err(_) => Err(ShareError::Io("llavero no disponible".into())),
            };
            Done::Published(Box::new(publication), Box::new(result))
        });
    }

    fn share_done(&mut self, done: Done) {
        match done {
            Done::Fetched(results) => {
                for (id, stamp, result) in results {
                    match result {
                        None => {}
                        Some(Ok(fetched)) => {
                            self.shares.seen.insert(id, stamp);
                            self.share_apply(id, fetched);
                        }
                        Some(Err(e)) => {
                            // Lo que había sigue valiendo.
                            if !e.is_transient() {
                                self.shares.seen.insert(id, stamp);
                            }
                            let text = if e.is_transient() {
                                format!("{e}. Se usa la última copia y se reintentará.")
                            } else {
                                e.to_string()
                            };
                            self.shares.set_status(id, text, !e.is_transient());
                        }
                    }
                }
            }
            Done::Published(publication, result) => {
                let id = publication.id;
                match *result {
                    Ok(export) => {
                        let mut text = format!(
                            "Publicada: {} equipo(s), revisión {}",
                            export.report.sessions, publication.revision
                        );
                        if export.report.outside_jumps > 0 {
                            text.push_str(&format!(
                                " · {} salto(s) pasan por equipos de fuera de la carpeta: a \
                                 quien la siga no le funcionarán",
                                export.report.outside_jumps
                            ));
                        }
                        if export.report.dropped > 0 {
                            text.push_str(&format!(
                                " · {} equipo(s) no se publican (puerto serie o sin dirección)",
                                export.report.dropped
                            ));
                        }
                        let store = &mut self.shares.store;
                        match store.publications.iter_mut().find(|p| p.id == id) {
                            Some(existing) => {
                                // Lo que se haya cambiado en la ventana
                                // entretanto (automática o no) se respeta.
                                existing.revision = publication.revision;
                                existing.digest = publication.digest.clone();
                                existing.published_at = publication.published_at;
                            }
                            None => {
                                store.publications.push(*publication);
                                if let Some(window) = &mut self.shares.window {
                                    window.message = Some((
                                        "Carpeta publicada. Da a tus compañeros la ruta del \
                                         fichero y la contraseña."
                                            .into(),
                                        false,
                                    ));
                                }
                            }
                        }
                        if let Some(e) = self.shares.save() {
                            self.notify(e, true);
                        }
                        self.shares.set_status(id, text, false);
                    }
                    Err(e) => {
                        let known = self.shares.store.publications.iter().any(|p| p.id == id);
                        if !known {
                            // No llegó a publicarse: no se recuerda.
                            self.shares.keys.remove(&id);
                            if let Some(window) = &mut self.shares.window {
                                window.message = Some((e.to_string(), true));
                            }
                        }
                        self.shares.set_status(id, e.to_string(), !e.is_transient());
                    }
                }
            }
            Done::Subscribed(path, keys, result) => {
                let message = match *result {
                    Ok(fetched) => self.share_subscribe(&path, keys, fetched),
                    Err(e) => Err(e.to_string()),
                };
                if let Some(window) = &mut self.shares.window {
                    window.message = Some(match message {
                        Ok(text) => {
                            window.path.clear();
                            (text, false)
                        }
                        Err(text) => (text, true),
                    });
                }
            }
        }
    }

    /// Da de alta una suscripción con lo que se acaba de leer.
    fn share_subscribe(
        &mut self,
        path: &str,
        keys: Keys,
        fetched: Fetched,
    ) -> Result<String, String> {
        let store = &self.shares.store;
        if store
            .publications
            .iter()
            .any(|p| p.id == fetched.published.id)
        {
            return Err("Esa carpeta la publicas tú desde este equipo".into());
        }
        let sub = Subscription::new(path, &fetched);
        if store.subscriptions.iter().any(|s| s.id == sub.id) {
            return Err("Ya sigues esa carpeta".into());
        }
        if self.tree.find(sub.id).is_some() {
            return Err("Ya tienes en el árbol una carpeta con ese identificador".into());
        }
        let id = sub.id;
        let name = fetched.published.folder.name.clone();
        let count = fetched.report.sessions;
        self.shares.store.subscriptions.push(sub);
        self.shares.keys.insert(id, keys);
        if let Some(e) = self.shares.save() {
            self.notify(e, true);
        }
        self.share_apply(id, fetched);
        self.selected = Some(id);
        Ok(format!(
            "Ahora sigues «{name}»: {count} equipo(s). Está en el árbol, marcada como compartida."
        ))
    }

    /// Deja de seguir una carpeta; con `keep` sus equipos se quedan como
    /// una carpeta propia.
    fn share_leave(&mut self, id: Uuid, keep: bool) {
        let Some(index) = self
            .shares
            .store
            .subscriptions
            .iter()
            .position(|s| s.id == id)
        else {
            return;
        };
        let sub = self.shares.store.subscriptions.remove(index);
        if let Some(vault) = &mut self.vault {
            let _ = vault.remove(&share::password_key(id, false));
            if !keep {
                for session in sub.session_ids().into_iter().chain([id]) {
                    let _ = vault.remove_prefix(&format!("{session}/"));
                }
            }
        }
        if !keep {
            sub.remove_from(&mut self.tree);
            if self.selected == Some(id) {
                self.selected = None;
            }
        }
        self.save_tree();
        self.shares.keys.remove(&id);
        self.shares.stored.remove(&id);
        self.shares.held.retain(|(held, _)| *held != id);
        self.shares.stale_marks = true;
        if let Some(e) = self.shares.save() {
            self.notify(e, true);
        }
        share::mark(&self.tree, &self.shares.store.roots());
        self.notify(
            if keep {
                "Ya no sigues la carpeta: sus equipos se quedan como una carpeta tuya"
            } else {
                "Ya no sigues la carpeta: se ha quitado del árbol"
            },
            false,
        );
    }

    /// Ventana «Carpetas compartidas».
    pub(super) fn share_window_ui(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.shares.window.take() else {
            return;
        };
        let mut open = true;
        let folders = self.own_folders();
        let vault_open = self.vault.is_some();
        let busy = self.shares.job.is_some();
        // Lo que se pide desde la ventana.
        let mut subscribe = false;
        let mut publish = false;
        let mut refresh: Option<Uuid> = None;
        let mut leave: Option<(Uuid, bool)> = None;
        let mut republish: Option<Uuid> = None;
        let mut stop: Option<Uuid> = None;
        let mut passwords: Vec<(Uuid, String)> = vec![];
        let mut mine: Option<Uuid> = None;
        let mut changed_auto = false;
        egui::Window::new("Carpetas compartidas")
            .id(egui::Id::unique("share_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos([240.0, 50.0])
            .default_height(680.0)
            .show(ctx, |ui| {
                ui.set_width(660.0);
                let shares = &mut self.shares;
                egui::ScrollArea::vertical()
                    .id_salt("share_scroll")
                    .max_height(640.0)
                    .show(ui, |ui| {
                        ui.strong("Carpetas de compañeros que sigo");
                        ui.weak(
                            "Aparecen en el árbol, de solo lectura, y se actualizan solas. Puedes \
                             conectar a sus equipos, duplicarlos a una carpeta tuya y usar tu \
                             usuario y tu clave sin tocar lo publicado.",
                        );
                        if shares.store.subscriptions.is_empty() {
                            ui.weak("(ninguna todavía)");
                        }
                        for sub in &shares.store.subscriptions {
                            ui.group(|ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal_wrapped(|ui| {
                                    ui.strong(format!("👥 {}", sub.name()));
                                    ui.weak(format!(
                                        "de {} · revisión {} · publicada el {}",
                                        if sub.publisher.is_empty() {
                                            "(sin nombre)"
                                        } else {
                                            &sub.publisher
                                        },
                                        sub.revision,
                                        clock(sub.published_at)
                                    ));
                                });
                                ui.add(
                                    egui::Label::new(RichText::new(&sub.path).weak()).truncate(),
                                );
                                if let Some((text, is_error)) = shares.status.get(&sub.id) {
                                    let color = if *is_error { error_color() } else { ok_color() };
                                    ui.add(
                                        egui::Label::new(RichText::new(text).color(color)).wrap(),
                                    );
                                }
                                for warning in &sub.warnings {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(format!("• {warning}"))
                                                .color(warn_color()),
                                        )
                                        .wrap(),
                                    );
                                }
                                let has_key = shares.keys.contains_key(&sub.id);
                                if !has_key {
                                    missing_password_ui(
                                        ui,
                                        &mut window,
                                        sub.id,
                                        vault_open,
                                        &mut passwords,
                                    );
                                }
                                let entry = window.mine.entry(sub.id).or_insert_with(|| {
                                    (
                                        sub.my_username.clone(),
                                        sub.my_auth,
                                        sub.my_key_path.clone(),
                                    )
                                });
                                ui.horizontal_wrapped(|ui| {
                                    ui.label("Mi usuario");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut entry.0)
                                            .hint_text("el publicado")
                                            .desired_width(110.0),
                                    );
                                    ui.label("Mi autenticación");
                                    egui::ComboBox::from_id_salt(("share_auth", sub.id))
                                        .selected_text(match entry.1 {
                                            Some(AuthMethod::Key) => "Clave privada",
                                            Some(AuthMethod::Agent) => "Agente SSH",
                                            _ => "La publicada",
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(&mut entry.1, None, "La publicada");
                                            ui.selectable_value(
                                                &mut entry.1,
                                                Some(AuthMethod::Key),
                                                "Clave privada",
                                            );
                                            ui.selectable_value(
                                                &mut entry.1,
                                                Some(AuthMethod::Agent),
                                                "Agente SSH",
                                            );
                                        });
                                    if entry.1 == Some(AuthMethod::Key) {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut entry.2)
                                                .hint_text("~/.ssh/id_ed25519")
                                                .desired_width(150.0),
                                        );
                                    }
                                    let current = (
                                        sub.my_username.clone(),
                                        sub.my_auth,
                                        sub.my_key_path.clone(),
                                    );
                                    if ui
                                        .add_enabled(
                                            *entry != current,
                                            egui::Button::new("Aplicar"),
                                        )
                                        .on_hover_text(
                                            "Tu usuario y tu clave para los equipos SSH de esta \
                                             carpeta. No cambia nada para los demás",
                                        )
                                        .clicked()
                                    {
                                        mine = Some(sub.id);
                                    }
                                });
                                ui.horizontal_wrapped(|ui| {
                                    if ui
                                        .add_enabled(
                                            has_key && !busy,
                                            egui::Button::new("🔄 Actualizar ahora"),
                                        )
                                        .clicked()
                                    {
                                        refresh = Some(sub.id);
                                    }
                                    if window.leaving == Some(sub.id) {
                                        ui.label("¿Qué hago con sus equipos?");
                                        if ui.button("Quitarlos").clicked() {
                                            leave = Some((sub.id, false));
                                        }
                                        if ui
                                            .button("Dejarlos como carpeta mía")
                                            .on_hover_text(
                                                "Se quedan como están ahora, ya no se actualizan y \
                                                 puedes cambiarlos",
                                            )
                                            .clicked()
                                        {
                                            leave = Some((sub.id, true));
                                        }
                                        if ui.button("Cancelar").clicked() {
                                            window.leaving = None;
                                        }
                                    } else if ui.button("Dejar de seguir…").clicked() {
                                        window.leaving = Some(sub.id);
                                    }
                                });
                            });
                        }
                        ui.horizontal(|ui| {
                            ui.label("Seguir otra:");
                            ui.add(
                                egui::TextEdit::singleline(&mut window.path)
                                    .hint_text("fichero publicado por un compañero")
                                    .desired_width(250.0),
                            );
                            if ui.button("Elegir…").clicked()
                                && let Some(file) = rfd::FileDialog::new()
                                    .set_title("Fichero de la carpeta compartida")
                                    .add_filter("Carpeta de GodRemote", &["json"])
                                    .pick_file()
                            {
                                window.path = file.display().to_string();
                            }
                            ui.add(
                                egui::TextEdit::singleline(&mut window.password)
                                    .password(true)
                                    .hint_text("contraseña")
                                    .desired_width(120.0),
                            );
                            let ready = !window.path.trim().is_empty()
                                && !window.password.is_empty()
                                && !busy;
                            if ui
                                .add_enabled(ready, egui::Button::new("Suscribirme"))
                                .clicked()
                            {
                                subscribe = true;
                            }
                        });

                        ui.separator();
                        ui.strong("Carpetas mías que publico");
                        ui.weak(
                            "Se publican los equipos de la carpeta y de sus subcarpetas con su \
                             dirección, usuario, ruta de la clave, saltos, notas y colores. No se \
                             publican contraseñas ni nada del baúl, ni las órdenes al conectar, \
                             los túneles, los reenvíos del agente o de X11, ni los equipos de \
                             puerto serie.",
                        );
                        if shares.store.publications.is_empty() {
                            ui.weak("(ninguna todavía)");
                        }
                        for publication in &mut shares.store.publications {
                            ui.group(|ui| {
                                ui.set_width(ui.available_width());
                                let name = folders
                                    .iter()
                                    .find(|(id, _)| *id == publication.folder)
                                    .map(|(_, path)| path.as_str());
                                ui.horizontal_wrapped(|ui| {
                                    match name {
                                        Some(name) => ui.strong(format!("📁 {name}")),
                                        None => ui.colored_label(
                                            error_color(),
                                            "(la carpeta ya no existe en este equipo)",
                                        ),
                                    };
                                    if let Some(at) = publication.published_at {
                                        ui.weak(format!(
                                            "revisión {} · publicada el {}",
                                            publication.revision,
                                            clock(at)
                                        ));
                                    }
                                });
                                ui.add(
                                    egui::Label::new(RichText::new(&publication.path).weak())
                                        .truncate(),
                                );
                                if let Some((text, is_error)) = shares.status.get(&publication.id) {
                                    let color = if *is_error { error_color() } else { ok_color() };
                                    ui.add(
                                        egui::Label::new(RichText::new(text).color(color)).wrap(),
                                    );
                                }
                                let has_key = shares.keys.contains_key(&publication.id);
                                if !has_key {
                                    missing_password_ui(
                                        ui,
                                        &mut window,
                                        publication.id,
                                        vault_open,
                                        &mut passwords,
                                    );
                                }
                                ui.horizontal_wrapped(|ui| {
                                    if ui
                                        .checkbox(
                                            &mut publication.auto,
                                            "Volver a publicarla sola cuando cambie",
                                        )
                                        .changed()
                                    {
                                        changed_auto = true;
                                    }
                                    if ui
                                        .add_enabled(
                                            has_key && !busy && name.is_some(),
                                            egui::Button::new("Publicar ahora"),
                                        )
                                        .clicked()
                                    {
                                        republish = Some(publication.id);
                                    }
                                    if ui
                                        .button("Dejar de publicar")
                                        .on_hover_text(
                                            "El fichero ya publicado no se borra: quien lo siga \
                                             conserva lo último. Bórralo tú si quieres retirarlo",
                                        )
                                        .clicked()
                                    {
                                        stop = Some(publication.id);
                                    }
                                });
                            });
                        }
                        ui.label("Publicar otra:");
                        egui::Grid::new("share_publish")
                            .num_columns(2)
                            .spacing([8.0, 6.0])
                            .show(ui, |ui| {
                                ui.label("Carpeta");
                                let current = window
                                    .folder
                                    .and_then(|id| folders.iter().find(|(fid, _)| *fid == id))
                                    .map(|(_, path)| path.clone())
                                    .unwrap_or_else(|| "(elige una)".to_string());
                                egui::ComboBox::from_id_salt("share_folder")
                                    .selected_text(current)
                                    .width(320.0)
                                    .show_ui(ui, |ui| {
                                        for (id, path) in &folders {
                                            ui.selectable_value(
                                                &mut window.folder,
                                                Some(*id),
                                                path,
                                            );
                                        }
                                    });
                                ui.end_row();
                                ui.label("Fichero");
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut window.target)
                                            .hint_text(
                                                "en una carpeta a la que lleguen tus compañeros",
                                            )
                                            .desired_width(400.0),
                                    );
                                    if ui.button("Elegir…").clicked() {
                                        let name = window
                                            .folder
                                            .and_then(|id| {
                                                folders.iter().find(|(fid, _)| *fid == id)
                                            })
                                            .map(|(_, path)| {
                                                path.rsplit(" / ")
                                                    .next()
                                                    .unwrap_or(path)
                                                    .to_string()
                                            })
                                            .unwrap_or_else(|| "carpeta".to_string());
                                        if let Some(file) = rfd::FileDialog::new()
                                            .set_title("Dónde publicar la carpeta")
                                            .set_file_name(format!("{name}{FILE_SUFFIX}"))
                                            .save_file()
                                        {
                                            window.target = file.display().to_string();
                                        }
                                    }
                                });
                                ui.end_row();
                                ui.label("Contraseña");
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut window.new_password)
                                            .password(true)
                                            .hint_text("la que darás a tus compañeros")
                                            .desired_width(200.0),
                                    );
                                    ui.add(
                                        egui::TextEdit::singleline(&mut window.repeat)
                                            .password(true)
                                            .hint_text("repítela")
                                            .desired_width(140.0),
                                    );
                                });
                                ui.end_row();
                            });
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut window.auto, "Volver a publicarla sola cuando cambie");
                            let ready = window.folder.is_some()
                                && !window.target.trim().is_empty()
                                && !window.new_password.is_empty()
                                && window.new_password == window.repeat
                                && !busy;
                            if ui
                                .add_enabled(ready, egui::Button::new("Publicar"))
                                .on_disabled_hover_text(
                                    "Elige la carpeta y el fichero, y escribe la contraseña dos \
                                     veces",
                                )
                                .clicked()
                            {
                                publish = true;
                            }
                        });
                        if let Some((message, is_error)) = &window.message {
                            ui.separator();
                            let color = if *is_error { error_color() } else { ok_color() };
                            ui.add(egui::Label::new(RichText::new(message).color(color)).wrap());
                        }
                        ui.separator();
                        ui.weak(
                            "Las contraseñas de las carpetas se guardan en el baúl; con el baúl \
                             cerrado se piden y solo duran hasta cerrar GodRemote. Quien tenga la \
                             contraseña y pueda escribir en la ruta puede sustituir lo publicado: \
                             compártela solo con quien deba publicar o leer.",
                        );
                    });
            });

        // --- Lo pedido en la ventana -----------------------------------
        for (id, password) in passwords {
            self.shares.keys.insert(id, keyring(&password));
            self.shares.stored.remove(&id);
            self.shares.forced.insert(id);
            self.shares.recheck = true;
            self.shares.status.remove(&id);
        }
        if subscribe {
            let path = window.path.trim().to_string();
            let keys = keyring(&std::mem::take(&mut window.password));
            let file = crate::paths::expand_tilde(&path);
            window.message = Some(("Leyendo la carpeta…".into(), false));
            self.share_spawn(move || {
                let result = match keys.lock() {
                    Ok(mut ring) => share::fetch(&file, &mut ring),
                    Err(_) => Err(ShareError::Io("llavero no disponible".into())),
                };
                Done::Subscribed(path, keys, Box::new(result))
            });
        }
        if publish && let Some(folder) = window.folder {
            let publication = Publication {
                folder,
                path: window.target.trim().to_string(),
                auto: window.auto,
                ..Default::default()
            };
            self.shares.keys.insert(
                publication.id,
                keyring(&std::mem::take(&mut window.new_password)),
            );
            window.repeat.clear();
            window.target.clear();
            window.message = None;
            self.share_start_publish(publication);
        }
        if let Some(id) = refresh {
            self.shares.forced.insert(id);
        }
        if let Some(id) = republish {
            self.shares.publish_now.insert(id);
        }
        if let Some(id) = stop {
            self.shares.store.publications.retain(|p| p.id != id);
            self.shares.keys.remove(&id);
            self.shares.stored.remove(&id);
            if let Some(vault) = &mut self.vault {
                let _ = vault.remove(&share::password_key(id, true));
            }
            changed_auto = true;
        }
        if changed_auto && let Some(e) = self.shares.save() {
            self.notify(e, true);
        }
        if let Some(id) = mine
            && let Some((username, auth, key_path)) = window.mine.get(&id).cloned()
        {
            match self.sync_blocked_by() {
                Some(what) => window.message = Some((format!("Cierra antes {what}"), true)),
                None => {
                    if let Some(sub) = self
                        .shares
                        .store
                        .subscriptions
                        .iter_mut()
                        .find(|s| s.id == id)
                    {
                        sub.my_username = username.trim().to_string();
                        sub.my_auth = auth;
                        sub.my_key_path = key_path.trim().to_string();
                        sub.place(&mut self.tree, sub.content.as_ref());
                        window.mine.remove(&id);
                    }
                    self.save_tree();
                    self.shares.stale_marks = true;
                    if let Some(e) = self.shares.save() {
                        self.notify(e, true);
                    }
                }
            }
        }
        if let Some((id, keep)) = leave {
            window.leaving = None;
            window.mine.remove(&id);
            self.share_leave(id, keep);
        }
        if open {
            self.shares.window = Some(window);
        } else {
            self.focus_active_terminal();
        }
    }
}

/// Campo para dar la contraseña de una carpeta que no la tiene en memoria.
fn missing_password_ui(
    ui: &mut egui::Ui,
    window: &mut ShareWindow,
    id: Uuid,
    vault_open: bool,
    passwords: &mut Vec<(Uuid, String)>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.colored_label(
            warn_color(),
            if vault_open {
                "Falta su contraseña:"
            } else {
                "Falta su contraseña (abre el baúl, donde se guarda, o escríbela):"
            },
        );
        let text = window.missing.entry(id).or_default();
        ui.add(
            egui::TextEdit::singleline(text)
                .password(true)
                .desired_width(160.0),
        );
        if ui
            .add_enabled(!text.is_empty(), egui::Button::new("Usar"))
            .clicked()
        {
            passwords.push((id, std::mem::take(text)));
        }
    });
}
