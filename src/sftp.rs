//! Cliente SFTP en segundo plano: listado, operaciones y transferencias.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use russh::Disconnect;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, OpenFlags};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::ssh::{Endpoint, Prompter, SshHandle};

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    /// Fecha de modificación (segundos Unix).
    pub mtime: Option<u32>,
    pub permissions: Option<u32>,
}

pub enum SftpSource {
    /// Abrir una conexión nueva (con sus saltos, si los tiene).
    Connect(Box<Endpoint>),
    /// Reutilizar una conexión SSH ya autenticada (p. ej. la del terminal).
    Existing(SshHandle),
}

/// Qué hacer cuando el destino de una transferencia ya existe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnConflict {
    /// Preguntar con [`SftpEvent::Conflict`].
    Ask,
    /// Sobrescribir sin preguntar.
    Replace,
}

/// Respuesta a un conflicto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    /// Continuar desde el tamaño que ya tiene el destino.
    Resume,
    Replace,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConflictAnswer {
    pub choice: ConflictChoice,
    /// Aplicar la misma respuesta al resto de la transferencia.
    pub all: bool,
}

/// El destino de un fichero ya existe y la transferencia espera a saber qué
/// hacer. Si `reply` se suelta sin contestar (p. ej. al cerrarse la vista),
/// la transferencia se cancela: nunca se queda esperando.
pub struct Conflict {
    pub id: u64,
    pub upload: bool,
    /// Ruta del destino que ya existe.
    pub path: String,
    pub source_size: u64,
    pub dest_size: u64,
    /// La transferencia es de una carpeta: puede haber más conflictos.
    pub multiple: bool,
    pub reply: oneshot::Sender<ConflictAnswer>,
}

impl Conflict {
    /// Un destino más pequeño que el origen puede ser una transferencia a
    /// medias y admite «Reanudar».
    pub fn can_resume(&self) -> bool {
        self.dest_size < self.source_size
    }
}

/// Respuestas «aplicar a todos» ya dadas. Se guardan aparte la de los
/// destinos que se pueden reanudar y la del resto, porque «Reanudar» solo
/// vale para los primeros.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConflictMemory {
    resumable: Option<ConflictChoice>,
    other: Option<ConflictChoice>,
}

impl ConflictMemory {
    pub fn get(&self, can_resume: bool) -> Option<ConflictChoice> {
        if can_resume {
            self.resumable
        } else {
            self.other
        }
    }

    pub fn remember(&mut self, can_resume: bool, choice: ConflictChoice) {
        // «Reanudar» no tiene sentido si el destino no es más pequeño.
        if can_resume {
            self.resumable = Some(choice);
        } else if choice != ConflictChoice::Resume {
            self.other = Some(choice);
        }
        // «Sustituir todos» y «Omitir todos» valen para los dos casos, salvo
        // que para el otro ya se hubiera dicho otra cosa.
        if choice != ConflictChoice::Resume {
            self.resumable.get_or_insert(choice);
            self.other.get_or_insert(choice);
        }
    }
}

/// Resumen de una transferencia terminada.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TransferStats {
    /// Bytes que de verdad se han enviado o recibido (al reanudar, solo el
    /// resto que faltaba).
    pub bytes: u64,
    /// Ficheros copiados, enteros o reanudados.
    pub copied: u32,
    pub resumed: u32,
    pub skipped: u32,
    pub deleted: u32,
}

pub enum SftpCmd {
    List(String),
    Mkdir(String),
    Remove {
        path: String,
        is_dir: bool,
    },
    Rename {
        from: String,
        to: String,
    },
    /// Cambia los permisos (los doce bits de `chmod`). `recursive` los
    /// aplica también al contenido de una carpeta, sin seguir enlaces.
    Chmod {
        path: String,
        mode: u32,
        recursive: bool,
    },
    Download {
        id: u64,
        remote: String,
        local: PathBuf,
        on_conflict: OnConflict,
    },
    Upload {
        id: u64,
        local: PathBuf,
        remote: String,
        on_conflict: OnConflict,
    },
    /// Compara dos carpetas sin tocar nada; el resultado llega en
    /// [`SftpEvent::SyncScanned`].
    SyncScan {
        id: u64,
        local: PathBuf,
        remote: String,
    },
    /// Ejecuta las acciones de un plan de sincronización ([`plan_sync`]).
    Sync {
        id: u64,
        local: PathBuf,
        remote: String,
        items: Vec<SyncItem>,
    },
    Cancel(u64),
}

pub enum SftpEvent {
    Status(String),
    /// Sesión SFTP lista; `home` es el directorio inicial.
    Ready {
        home: String,
    },
    Listing {
        path: String,
        entries: Vec<RemoteEntry>,
    },
    /// Una operación terminó bien (conviene refrescar el listado).
    Done(String),
    Progress {
        id: u64,
        done: u64,
        total: u64,
        current: String,
    },
    /// Una transferencia pregunta qué hacer con un destino que ya existe.
    Conflict(Conflict),
    TransferFinished {
        id: u64,
        error: Option<String>,
        stats: TransferStats,
    },
    SyncScanned {
        id: u64,
        result: Result<SyncScan, String>,
    },
    Error(String),
    SaveSecrets(Vec<(String, String)>),
    Closed(Option<String>),
}

pub struct SftpClient {
    pub cmd: mpsc::UnboundedSender<SftpCmd>,
    pub events: std::sync::mpsc::Receiver<SftpEvent>,
}

/// Une una ruta remota (POSIX) con un nombre.
pub fn join_remote(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Directorio padre de una ruta remota.
pub fn parent_remote(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
    }
}

/// `true` si el nombre es seguro como componente único de una ruta local.
/// Los nombres llegan del servidor: uno malicioso no debe poder hacer que
/// se escriba fuera de la carpeta de destino.
pub fn safe_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\0'])
        && !(cfg!(windows) && name.contains(['\\', ':']))
}

/// Permisos como los escribe `ls -l` (`rwxr-xr-x`), con setuid, setgid y
/// sticky (`s`, `S`, `t`, `T`).
pub fn mode_text(mode: u32) -> String {
    let bit = |mask: u32, ch: char| if mode & mask != 0 { ch } else { '-' };
    let exec = |mask: u32, special: u32, set: char| match (mode & mask != 0, mode & special != 0) {
        (true, false) => 'x',
        (false, false) => '-',
        (true, true) => set,
        (false, true) => set.to_ascii_uppercase(),
    };
    [
        bit(0o400, 'r'),
        bit(0o200, 'w'),
        exec(0o100, 0o4000, 's'),
        bit(0o040, 'r'),
        bit(0o020, 'w'),
        exec(0o010, 0o2000, 's'),
        bit(0o004, 'r'),
        bit(0o002, 'w'),
        exec(0o001, 0o1000, 't'),
    ]
    .iter()
    .collect()
}

/// Permisos en octal: tres cifras, o cuatro si hay setuid, setgid o sticky.
pub fn mode_octal(mode: u32) -> String {
    let mode = mode & 0o7777;
    if mode & 0o7000 != 0 {
        format!("{mode:04o}")
    } else {
        format!("{mode:03o}")
    }
}

/// Interpreta permisos escritos en octal (`755`, `0644`, `4755`) o como los
/// muestra `ls -l` (`rwxr-xr-x`, con o sin la letra del tipo delante).
pub fn parse_mode(text: &str) -> Option<u32> {
    let text = text.trim();
    if (1..=4).contains(&text.len()) && text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return u32::from_str_radix(text, 8).ok();
    }
    let chars: Vec<char> = text.chars().collect();
    let chars = match chars.len() {
        9 => &chars[..],
        10 => &chars[1..],
        _ => return None,
    };
    let mut mode = 0;
    for (i, &ch) in chars.iter().enumerate() {
        let mask = 0o400 >> i;
        // Bit especial que comparte hueco con la «x» de cada trío.
        let (special, set) = match i {
            2 => (0o4000, 's'),
            5 => (0o2000, 's'),
            8 => (0o1000, 't'),
            _ => (0, '\0'),
        };
        match ch {
            '-' => {}
            'r' if i % 3 == 0 => mode |= mask,
            'w' if i % 3 == 1 => mode |= mask,
            'x' if i % 3 == 2 => mode |= mask,
            c if special != 0 && c == set => mode |= mask | special,
            c if special != 0 && c == set.to_ascii_uppercase() => mode |= special,
            _ => return None,
        }
    }
    Some(mode)
}

/// Tolerancia al comparar fechas de modificación, en segundos: SFTP no da
/// fracciones y hay sistemas de ficheros (FAT) que solo guardan los pares.
pub const SYNC_TOLERANCE: i64 = 2;

/// Sufijo de los ficheros que una sincronización tiene a medio copiar. Se
/// copian con este nombre y se renombran al terminar: así un corte nunca
/// deja con el nombre definitivo un fichero incompleto y más reciente, que
/// la siguiente sincronización tomaría por el bueno.
pub const PART_SUFFIX: &str = ".godremote-part";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncFile {
    pub size: u64,
    /// Fecha de modificación (segundos Unix).
    pub mtime: Option<i64>,
}

/// Contenido de una de las dos carpetas que se comparan, por ruta relativa
/// (siempre con `/`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncTree {
    pub files: BTreeMap<String, SyncFile>,
    pub dirs: BTreeSet<String>,
    /// Lo que no se sincroniza: enlaces simbólicos, ficheros especiales,
    /// nombres no válidos y restos a medio copiar.
    pub ignored: BTreeSet<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncScan {
    pub local: SyncTree,
    pub remote: SyncTree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// De local a remoto: se sube lo nuevo o más reciente.
    Upload,
    /// De remoto a local.
    Download,
    /// En los dos sentidos: gana el más reciente.
    Both,
}

/// En qué se diferencian los dos lados.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    Equal,
    OnlyLocal,
    OnlyRemote,
    LocalNewer,
    RemoteNewer,
    /// Misma fecha y distinto tamaño: no se sabe cuál es el bueno.
    Differ,
    /// Fichero en un lado y carpeta en el otro.
    TypeConflict,
    /// Es, o contiene, algo que no se sincroniza.
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOp {
    /// Subir el fichero (o crear la carpeta en el servidor).
    Upload,
    /// Descargar el fichero (o crear la carpeta en local).
    Download,
    DeleteLocal,
    DeleteRemote,
    /// No se toca.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncItem {
    /// Ruta relativa a las dos carpetas.
    pub path: String,
    pub is_dir: bool,
    pub state: SyncState,
    pub op: SyncOp,
    /// Bytes que se copiarían.
    pub size: u64,
}

/// Recuento de un plan, para enseñarlo antes de ejecutarlo.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SyncSummary {
    pub upload: u32,
    pub upload_bytes: u64,
    pub download: u32,
    pub download_bytes: u64,
    /// Carpetas (vacías) que se crearían.
    pub make_dirs: u32,
    pub delete_files: u32,
    pub delete_dirs: u32,
    pub equal: u32,
    /// Ficheros que solo están en un lado.
    pub only_local: u32,
    pub only_remote: u32,
    /// Ficheros distintos que el plan deja como están.
    pub untouched: u32,
    /// Lo que no se sincroniza (enlaces, ficheros especiales…).
    pub ignored: u32,
}

impl SyncSummary {
    pub fn of(items: &[SyncItem]) -> Self {
        let mut s = Self::default();
        for item in items {
            match (item.op, item.is_dir) {
                (SyncOp::Upload, false) => {
                    s.upload += 1;
                    s.upload_bytes += item.size;
                }
                (SyncOp::Download, false) => {
                    s.download += 1;
                    s.download_bytes += item.size;
                }
                (SyncOp::Upload | SyncOp::Download, true) => s.make_dirs += 1,
                (SyncOp::DeleteLocal | SyncOp::DeleteRemote, false) => s.delete_files += 1,
                (SyncOp::DeleteLocal | SyncOp::DeleteRemote, true) => s.delete_dirs += 1,
                (SyncOp::None, _) => {}
            }
            if item.is_dir {
                continue;
            }
            match item.state {
                SyncState::Equal => s.equal += 1,
                SyncState::OnlyLocal => s.only_local += 1,
                SyncState::OnlyRemote => s.only_remote += 1,
                SyncState::Ignored => s.ignored += 1,
                _ if item.op == SyncOp::None => s.untouched += 1,
                _ => {}
            }
        }
        s
    }

    /// `true` si el plan no copia ni borra nada.
    pub fn nothing_to_do(&self) -> bool {
        self.upload + self.download + self.make_dirs + self.delete_files + self.delete_dirs == 0
    }
}

/// Compara dos ficheros que existen en los dos lados.
fn compare_files(local: SyncFile, remote: SyncFile) -> SyncState {
    let newer = match (local.mtime, remote.mtime) {
        (Some(l), Some(r)) if l - r > SYNC_TOLERANCE => Some(SyncState::LocalNewer),
        (Some(l), Some(r)) if r - l > SYNC_TOLERANCE => Some(SyncState::RemoteNewer),
        // Sin fecha en algún lado no se puede saber cuál es más reciente.
        _ => None,
    };
    match newer {
        Some(state) => state,
        None if local.size == remote.size => SyncState::Equal,
        None => SyncState::Differ,
    }
}

/// Una ruta relativa tramo a tramo: `a`, `a/b`, `a/b/c`.
fn prefixes(rel: &str) -> impl Iterator<Item = &str> {
    rel.match_indices('/')
        .map(|(i, _)| &rel[..i])
        .chain((!rel.is_empty()).then_some(rel))
}

/// `true` si la ruta, o alguna carpeta que la contiene, está en `set`.
fn blocked_by(path: &str, set: &BTreeSet<&str>) -> bool {
    prefixes(path).any(|prefix| set.contains(prefix))
}

/// `true` si en `set` hay algo por debajo de la carpeta `dir`.
fn contains_below(dir: &str, set: &BTreeSet<&str>) -> bool {
    let prefix = format!("{dir}/");
    set.range(prefix.as_str()..)
        .next()
        .is_some_and(|p| p.starts_with(&prefix))
}

/// Decide qué hacer con cada fichero y carpeta para sincronizar las dos
/// carpetas comparadas. No toca nada: el plan se enseña y, si se acepta, se
/// ejecuta con [`SftpCmd::Sync`].
///
/// `delete` borra lo que sobra en el destino; solo vale en los modos de un
/// sentido (en los dos sentidos no sobra nada: se copia al otro lado).
pub fn plan_sync(scan: &SyncScan, mode: SyncMode, delete: bool) -> Vec<SyncItem> {
    let (local, remote) = (&scan.local, &scan.remote);
    let delete = delete && mode != SyncMode::Both;

    // Rutas que no se tocan, con todo lo que cuelgue de ellas: lo que no se
    // sincroniza (un enlace podría llevar fuera de la carpeta) y lo que es
    // fichero en un lado y carpeta en el otro.
    let ignored: BTreeSet<&str> = local
        .ignored
        .iter()
        .chain(&remote.ignored)
        .map(String::as_str)
        .collect();
    let mixed: BTreeSet<&str> = local
        .files
        .keys()
        .filter(|p| remote.dirs.contains(*p))
        .chain(remote.files.keys().filter(|p| local.dirs.contains(*p)))
        .map(String::as_str)
        .collect();
    let blocked = |path: &str| {
        if blocked_by(path, &mixed) {
            Some(SyncState::TypeConflict)
        } else if blocked_by(path, &ignored) {
            Some(SyncState::Ignored)
        } else {
            None
        }
    };

    let mut items = vec![];
    let paths: BTreeSet<&String> = local.files.keys().chain(remote.files.keys()).collect();
    for path in paths {
        let (l, r) = (local.files.get(path), remote.files.get(path));
        let state = match blocked(path) {
            Some(state) => state,
            None => match (l, r) {
                (Some(l), Some(r)) => compare_files(*l, *r),
                (Some(_), None) => SyncState::OnlyLocal,
                (None, _) => SyncState::OnlyRemote,
            },
        };
        let op = match (state, mode) {
            (SyncState::OnlyLocal, SyncMode::Upload | SyncMode::Both) => SyncOp::Upload,
            (SyncState::OnlyLocal, SyncMode::Download) if delete => SyncOp::DeleteLocal,
            (SyncState::OnlyRemote, SyncMode::Download | SyncMode::Both) => SyncOp::Download,
            (SyncState::OnlyRemote, SyncMode::Upload) if delete => SyncOp::DeleteRemote,
            (SyncState::LocalNewer, SyncMode::Upload | SyncMode::Both) => SyncOp::Upload,
            (SyncState::RemoteNewer, SyncMode::Download | SyncMode::Both) => SyncOp::Download,
            // Misma fecha y distinto tamaño: en un sentido manda el origen;
            // en los dos no hay forma de saber cuál es el bueno.
            (SyncState::Differ, SyncMode::Upload) => SyncOp::Upload,
            (SyncState::Differ, SyncMode::Download) => SyncOp::Download,
            _ => SyncOp::None,
        };
        let size = match op {
            SyncOp::Upload => l.map_or(0, |f| f.size),
            SyncOp::Download => r.map_or(0, |f| f.size),
            _ => 0,
        };
        items.push(SyncItem {
            path: path.clone(),
            is_dir: false,
            state,
            op,
            size,
        });
    }
    // Lo que no se sincroniza también sale en el plan: que se vea que está
    // ahí y que no se toca.
    for path in &ignored {
        if !local.files.contains_key(*path) && !remote.files.contains_key(*path) {
            items.push(SyncItem {
                path: path.to_string(),
                is_dir: false,
                state: SyncState::Ignored,
                op: SyncOp::None,
                size: 0,
            });
        }
    }

    // Carpetas que solo están en un lado: se crean en el otro (también las
    // vacías) o, si sobran, se borran cuando queden vacías.
    for path in local.dirs.symmetric_difference(&remote.dirs) {
        if blocked(path).is_some() {
            continue;
        }
        let in_local = local.dirs.contains(path);
        let state = if in_local {
            SyncState::OnlyLocal
        } else {
            SyncState::OnlyRemote
        };
        let extra = matches!(
            (in_local, mode),
            (true, SyncMode::Download) | (false, SyncMode::Upload)
        );
        let (state, op) = if !extra {
            let op = if in_local {
                SyncOp::Upload
            } else {
                SyncOp::Download
            };
            (state, op)
        } else if !delete {
            (state, SyncOp::None)
        } else if contains_below(path, &ignored) {
            // Con algo dentro que no se sincroniza, no quedará vacía.
            (SyncState::Ignored, SyncOp::None)
        } else if in_local {
            (state, SyncOp::DeleteLocal)
        } else {
            (state, SyncOp::DeleteRemote)
        };
        items.push(SyncItem {
            path: path.clone(),
            is_dir: true,
            state,
            op,
            size: 0,
        });
    }
    items.sort_by(|a, b| a.path.cmp(&b.path));
    items
}

type Emit = Arc<dyn Fn(SftpEvent) + Send + Sync>;

pub fn spawn(rt: &tokio::runtime::Handle, source: SftpSource, prompter: Prompter) -> SftpClient {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    let emit: Emit = {
        let prompter = prompter.clone();
        Arc::new(move |ev| {
            let _ = ev_tx.send(ev);
            prompter.wake();
        })
    };
    rt.spawn(async move {
        let result = run(source, &prompter, cmd_rx, emit.clone()).await;
        emit(SftpEvent::Closed(result.err().map(|e| format!("{e:#}"))));
    });
    SftpClient {
        cmd: cmd_tx,
        events: ev_rx,
    }
}

async fn run(
    source: SftpSource,
    prompter: &Prompter,
    mut cmd_rx: mpsc::UnboundedReceiver<SftpCmd>,
    emit: Emit,
) -> Result<()> {
    let (handle, owned) = match source {
        SftpSource::Existing(h) => (h, false),
        SftpSource::Connect(endpoint) => {
            let status = {
                let emit = emit.clone();
                move |s: String| emit(SftpEvent::Status(s))
            };
            let connected = crate::ssh::connect_endpoint(&endpoint, prompter, &status).await?;
            if !connected.to_save.is_empty() {
                emit(SftpEvent::SaveSecrets(connected.to_save));
            }
            (connected.handle, true)
        }
    };

    emit(SftpEvent::Status("Iniciando SFTP…".into()));
    let channel = handle.channel_open_session().await?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .context("El servidor no ofrece el subsistema SFTP")?;
    let sftp = Arc::new(
        SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| anyhow!("No se pudo iniciar SFTP: {e}"))?,
    );
    let home = sftp
        .canonicalize(".")
        .await
        .map(|p| p.to_string())
        .unwrap_or_else(|_| "/".into());
    emit(SftpEvent::Ready { home });

    let cancels: Cancels = Arc::default();
    // Lo que se lleva la tarea de cada transferencia.
    let start = |id: u64, on_conflict: OnConflict| {
        let transfer = Arc::new(Transfer::new(id, on_conflict, &cancels, &emit));
        (transfer, sftp.clone(), cancels.clone(), emit.clone())
    };

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            SftpCmd::List(path) => match list(&sftp, &path).await {
                Ok((path, entries)) => emit(SftpEvent::Listing { path, entries }),
                Err(e) => emit(SftpEvent::Error(format!("No se pudo listar {path}: {e:#}"))),
            },
            SftpCmd::Mkdir(path) => match sftp.create_dir(path.as_str()).await {
                Ok(()) => emit(SftpEvent::Done(format!("Carpeta creada: {path}"))),
                Err(e) => emit(SftpEvent::Error(format!("No se pudo crear {path}: {e}"))),
            },
            SftpCmd::Remove { path, is_dir } => {
                let res = if is_dir {
                    remove_dir_all(&sftp, &path).await
                } else {
                    sftp.remove_file(path.as_str()).await.map_err(Into::into)
                };
                match res {
                    Ok(()) => emit(SftpEvent::Done(format!("Eliminado: {path}"))),
                    Err(e) => emit(SftpEvent::Error(format!(
                        "No se pudo eliminar {path}: {e:#}"
                    ))),
                }
            }
            SftpCmd::Rename { from, to } => match sftp.rename(from.as_str(), to.as_str()).await {
                Ok(()) => emit(SftpEvent::Done(format!("Renombrado a {to}"))),
                Err(e) => emit(SftpEvent::Error(format!("No se pudo renombrar: {e}"))),
            },
            SftpCmd::Chmod {
                path,
                mode,
                recursive,
            } => match chmod(&sftp, &path, mode, recursive).await {
                Ok(1) => emit(SftpEvent::Done(format!(
                    "Permisos de {path}: {}",
                    mode_octal(mode)
                ))),
                Ok(n) => emit(SftpEvent::Done(format!(
                    "Permisos de {path} y su contenido ({n} elementos): {}",
                    mode_octal(mode)
                ))),
                Err(e) => emit(SftpEvent::Error(format!(
                    "No se pudieron cambiar los permisos de {path}: {e:#}"
                ))),
            },
            SftpCmd::Cancel(id) => {
                if let Some(flag) = cancels.lock().unwrap().get(&id) {
                    flag.store(true, Ordering::Relaxed);
                }
            }
            SftpCmd::Download {
                id,
                remote,
                local,
                on_conflict,
            } => {
                let (transfer, sftp, cancels, emit) = start(id, on_conflict);
                tokio::spawn(async move {
                    let res = download(&sftp, &remote, &local, &transfer).await;
                    transfer.finish(res, &cancels, &emit);
                });
            }
            SftpCmd::Upload {
                id,
                local,
                remote,
                on_conflict,
            } => {
                let (transfer, sftp, cancels, emit) = start(id, on_conflict);
                tokio::spawn(async move {
                    let res = upload(&sftp, &local, &remote, &transfer).await;
                    transfer.finish(res, &cancels, &emit);
                });
            }
            SftpCmd::SyncScan { id, local, remote } => {
                let (transfer, sftp, cancels, emit) = start(id, OnConflict::Replace);
                tokio::spawn(async move {
                    let result = sync_scan(&sftp, &local, &remote, &transfer).await;
                    cancels.lock().unwrap().remove(&id);
                    emit(SftpEvent::SyncScanned {
                        id,
                        result: result.map_err(|e| format!("{e:#}")),
                    });
                });
            }
            SftpCmd::Sync {
                id,
                local,
                remote,
                items,
            } => {
                let (transfer, sftp, cancels, emit) = start(id, OnConflict::Replace);
                tokio::spawn(async move {
                    let res = sync_run(&sftp, &local, &remote, &items, &transfer).await;
                    transfer.finish(res, &cancels, &emit);
                });
            }
        }
    }

    // La pestaña se cerró: se cancelan las transferencias en curso.
    for flag in cancels.lock().unwrap().values() {
        flag.store(true, Ordering::Relaxed);
    }
    let _ = sftp.close().await;
    if owned {
        let _ = handle.disconnect(Disconnect::ByApplication, "", "es").await;
    }
    Ok(())
}

async fn list(sftp: &SftpSession, path: &str) -> Result<(String, Vec<RemoteEntry>)> {
    let canonical = sftp.canonicalize(path).await?.to_string();
    let mut entries = vec![];
    for entry in sftp.read_dir(canonical.as_str()).await? {
        let name = entry.file_name().to_string();
        // Se descartan nombres imposibles o peligrosos ("..", con "/"…).
        if !safe_component(&name) {
            continue;
        }
        let meta = entry.metadata();
        let file_type = meta.file_type();
        let mut is_dir = file_type.is_dir();
        let is_symlink = file_type.is_symlink();
        if is_symlink {
            // Se sigue el enlace para saber si apunta a un directorio.
            if let Ok(target) = sftp.metadata(join_remote(&canonical, &name)).await {
                is_dir = target.file_type().is_dir();
            }
        }
        entries.push(RemoteEntry {
            name,
            is_dir,
            is_symlink,
            size: meta.size.unwrap_or(0),
            mtime: meta.mtime,
            permissions: meta.permissions,
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok((canonical, entries))
}

async fn remove_dir_all(sftp: &SftpSession, path: &str) -> Result<()> {
    // Recorrido iterativo; los enlaces simbólicos se borran sin seguirlos.
    let mut stack = vec![(path.to_string(), false)];
    while let Some((dir, visited)) = stack.pop() {
        if visited {
            sftp.remove_dir(dir.as_str()).await?;
            continue;
        }
        stack.push((dir.clone(), true));
        for entry in sftp.read_dir(dir.as_str()).await? {
            let name = entry.file_name().to_string();
            if name == "." || name == ".." {
                continue;
            }
            let child = join_remote(&dir, &name);
            if entry.file_type().is_dir() {
                stack.push((child, false));
            } else {
                sftp.remove_file(child.as_str()).await?;
            }
        }
    }
    Ok(())
}

/// Cambia los permisos de `path` y, si `recursive`, de todo lo que cuelga
/// de él. Devuelve cuántos elementos ha cambiado.
async fn chmod(sftp: &SftpSession, path: &str, mode: u32, recursive: bool) -> Result<u32> {
    let mode = mode & 0o7777;
    let set = async |path: &str| {
        let attrs = FileAttributes {
            permissions: Some(mode),
            ..FileAttributes::empty()
        };
        sftp.set_metadata(path, attrs).await
    };
    // Un enlace a una carpeta no se recorre: llevaría fuera del árbol.
    if !recursive || !sftp.symlink_metadata(path).await?.file_type().is_dir() {
        set(path).await?;
        return Ok(1);
    }
    let mut changed = 0;
    // Cada carpeta se cambia después de su contenido: con unos permisos que
    // quiten el paso, ya no se podría entrar a cambiar lo de dentro.
    let mut stack = vec![(path.to_string(), false)];
    while let Some((dir, visited)) = stack.pop() {
        if visited {
            set(&dir).await.with_context(|| format!("en {dir}"))?;
            changed += 1;
            continue;
        }
        let entries = match sftp.read_dir(dir.as_str()).await {
            Ok(entries) => entries,
            Err(_) => {
                // Y al revés: si no se puede leer, quizá sí con los nuevos.
                set(&dir).await.with_context(|| format!("en {dir}"))?;
                sftp.read_dir(dir.as_str())
                    .await
                    .with_context(|| format!("listando {dir}"))?
            }
        };
        stack.push((dir.clone(), true));
        for entry in entries {
            let name = entry.file_name().to_string();
            if !safe_component(&name) {
                continue;
            }
            let child = join_remote(&dir, &name);
            let file_type = entry.file_type();
            if file_type.is_dir() {
                stack.push((child, false));
            } else if !file_type.is_symlink() {
                // `setstat` sigue los enlaces: se dejan como están.
                set(&child).await.with_context(|| format!("en {child}"))?;
                changed += 1;
            }
        }
    }
    Ok(changed)
}

type Cancels = Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>;

struct Transfer {
    id: u64,
    cancel: Arc<AtomicBool>,
    emit: Emit,
    done: Mutex<(u64, u64, std::time::Instant)>,
    on_conflict: OnConflict,
    /// Respuestas «aplicar a todos» de esta transferencia.
    memory: Mutex<ConflictMemory>,
    /// Se transfiere una carpeta: puede haber más de un conflicto.
    multiple: AtomicBool,
    stats: Mutex<TransferStats>,
}

impl Transfer {
    fn new(id: u64, on_conflict: OnConflict, cancels: &Cancels, emit: &Emit) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        cancels.lock().unwrap().insert(id, cancel.clone());
        Self {
            id,
            cancel,
            emit: emit.clone(),
            done: Mutex::new((0, 0, std::time::Instant::now())),
            on_conflict,
            memory: Mutex::default(),
            multiple: AtomicBool::new(false),
            stats: Mutex::default(),
        }
    }

    /// Da la transferencia por terminada y lo notifica.
    fn finish(&self, result: Result<()>, cancels: &Cancels, emit: &Emit) {
        cancels.lock().unwrap().remove(&self.id);
        emit(SftpEvent::TransferFinished {
            id: self.id,
            error: result.err().map(|e| format!("{e:#}")),
            stats: *self.stats.lock().unwrap(),
        });
    }

    fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            bail!("Cancelado");
        }
        Ok(())
    }

    fn add_total(&self, bytes: u64) {
        self.done.lock().unwrap().1 += bytes;
    }

    /// Suma bytes transferidos y notifica (como mucho ~10 veces por segundo).
    fn advance(&self, bytes: u64, current: &str, force: bool) {
        self.stats.lock().unwrap().bytes += bytes;
        self.progress(bytes, current, force);
    }

    /// Avanza el progreso sin contar los bytes como transferidos: lo que ya
    /// estaba en el destino al reanudar.
    fn progress(&self, bytes: u64, current: &str, force: bool) {
        let mut d = self.done.lock().unwrap();
        d.0 += bytes;
        if force || d.2.elapsed().as_millis() >= 100 {
            d.2 = std::time::Instant::now();
            (self.emit)(SftpEvent::Progress {
                id: self.id,
                done: d.0,
                total: d.1.max(d.0),
                current: current.to_string(),
            });
        }
    }

    fn count(&self, update: impl FnOnce(&mut TransferStats)) {
        update(&mut self.stats.lock().unwrap());
    }

    /// Decide qué hacer con un destino que ya existe, preguntando a la vista
    /// si hace falta.
    async fn resolve(
        &self,
        upload: bool,
        path: String,
        source_size: u64,
        dest_size: u64,
    ) -> Result<ConflictChoice> {
        if self.on_conflict == OnConflict::Replace {
            return Ok(ConflictChoice::Replace);
        }
        let can_resume = dest_size < source_size;
        if let Some(choice) = self.memory.lock().unwrap().get(can_resume) {
            return Ok(choice);
        }
        let (reply, mut answer) = oneshot::channel();
        (self.emit)(SftpEvent::Conflict(Conflict {
            id: self.id,
            upload,
            path,
            source_size,
            dest_size,
            multiple: self.multiple.load(Ordering::Relaxed),
            reply,
        }));
        let answer = loop {
            // Mientras se espera, la transferencia se puede seguir cancelando.
            self.check()?;
            match tokio::time::timeout(Duration::from_millis(200), &mut answer).await {
                Ok(Ok(answer)) => break answer,
                // La vista se cerró (o descartó la pregunta) sin contestar.
                Ok(Err(_)) => bail!("Cancelado"),
                Err(_) => {}
            }
        };
        // Sin nada que reanudar, lo prudente es no tocar el destino.
        let choice = match answer.choice {
            ConflictChoice::Resume if !can_resume => ConflictChoice::Skip,
            choice => choice,
        };
        if answer.all {
            self.memory.lock().unwrap().remember(can_resume, choice);
        }
        Ok(choice)
    }
}

const CHUNK: usize = 256 * 1024;

async fn download(sftp: &SftpSession, remote: &str, local: &Path, t: &Transfer) -> Result<()> {
    let meta = sftp.metadata(remote).await?;
    if meta.file_type().is_dir() {
        t.multiple.store(true, Ordering::Relaxed);
        // Recorrido iterativo del árbol remoto.
        let mut stack = vec![(remote.to_string(), local.to_path_buf())];
        while let Some((rdir, ldir)) = stack.pop() {
            t.check()?;
            tokio::fs::create_dir_all(&ldir)
                .await
                .with_context(|| format!("creando {}", ldir.display()))?;
            for entry in sftp.read_dir(rdir.as_str()).await? {
                let name = entry.file_name().to_string();
                if name == "." || name == ".." {
                    continue;
                }
                // Un servidor malicioso no debe poder escribir fuera del destino.
                if !safe_component(&name) {
                    bail!("Nombre remoto no válido: {name:?}");
                }
                let rchild = join_remote(&rdir, &name);
                let lchild = ldir.join(&name);
                let file_type = entry.file_type();
                if file_type.is_dir() {
                    stack.push((rchild, lchild));
                } else if file_type.is_file() {
                    let size = entry.metadata().size.unwrap_or(0);
                    download_one(sftp, &rchild, &lchild, size, t).await?;
                }
                // Enlaces y ficheros especiales se omiten en descargas recursivas.
            }
        }
        Ok(())
    } else {
        download_one(sftp, remote, local, meta.size.unwrap_or(0), t).await
    }
}

/// Descarga un fichero; si el destino ya existe, antes se decide qué hacer.
async fn download_one(
    sftp: &SftpSession,
    remote: &str,
    local: &Path,
    size: u64,
    t: &Transfer,
) -> Result<()> {
    let mut resume = false;
    if let Ok(meta) = tokio::fs::metadata(local).await
        && meta.is_file()
    {
        let path = local.display().to_string();
        match t.resolve(false, path, size, meta.len()).await? {
            ConflictChoice::Skip => {
                t.count(|s| s.skipped += 1);
                return Ok(());
            }
            ConflictChoice::Replace => {}
            ConflictChoice::Resume => resume = true,
        }
    }
    t.add_total(size);
    download_file(sftp, remote, local, resume, t).await
}

/// Copia un fichero remoto a uno local. Con `resume` no se trunca el
/// destino: se continúa desde el tamaño que ya tiene.
async fn download_file(
    sftp: &SftpSession,
    remote: &str,
    local: &Path,
    resume: bool,
    t: &Transfer,
) -> Result<()> {
    let mut src = sftp.open(remote).await?;
    let mut dst = if resume {
        let mut dst = tokio::fs::OpenOptions::new()
            .write(true)
            .open(local)
            .await
            .with_context(|| format!("abriendo {}", local.display()))?;
        let offset = dst.metadata().await?.len();
        if src.metadata().await?.size.is_some_and(|size| offset > size) {
            bail!(
                "No se puede reanudar {}: ya es mayor que el origen",
                local.display()
            );
        }
        src.seek(SeekFrom::Start(offset)).await?;
        dst.seek(SeekFrom::Start(offset)).await?;
        t.count(|s| s.resumed += 1);
        t.progress(offset, remote, false);
        dst
    } else {
        tokio::fs::File::create(local)
            .await
            .with_context(|| format!("creando {}", local.display()))?
    };
    let mut buf = vec![0u8; CHUNK];
    loop {
        t.check()?;
        let n = src.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).await?;
        t.advance(n as u64, remote, false);
    }
    dst.flush().await?;
    t.advance(0, remote, true);
    t.count(|s| s.copied += 1);
    Ok(())
}

async fn upload(sftp: &SftpSession, local: &Path, remote: &str, t: &Transfer) -> Result<()> {
    let meta = tokio::fs::metadata(local)
        .await
        .with_context(|| format!("leyendo {}", local.display()))?;
    if meta.is_dir() {
        t.multiple.store(true, Ordering::Relaxed);
        let mut stack = vec![(local.to_path_buf(), remote.to_string())];
        while let Some((ldir, rdir)) = stack.pop() {
            t.check()?;
            // En una carpeta recién creada no puede haber nada que choque.
            let existed = sftp.metadata(rdir.as_str()).await.is_ok();
            if !existed {
                sftp.create_dir(rdir.as_str()).await?;
            }
            let mut entries = tokio::fs::read_dir(&ldir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let name = entry.file_name().to_string_lossy().to_string();
                let rchild = join_remote(&rdir, &name);
                let file_type = entry.file_type().await?;
                if file_type.is_dir() {
                    stack.push((entry.path(), rchild));
                } else if file_type.is_file() {
                    let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                    upload_one(sftp, &entry.path(), &rchild, size, existed, t).await?;
                }
            }
        }
        Ok(())
    } else {
        upload_one(sftp, local, remote, meta.len(), true, t).await
    }
}

/// Sube un fichero; si el destino ya existe, antes se decide qué hacer.
async fn upload_one(
    sftp: &SftpSession,
    local: &Path,
    remote: &str,
    size: u64,
    may_exist: bool,
    t: &Transfer,
) -> Result<()> {
    let mut resume = false;
    // Sin preguntas (p. ej. al guardar un fichero en edición) no hace falta
    // consultar antes al servidor.
    if may_exist
        && t.on_conflict == OnConflict::Ask
        && let Ok(meta) = sftp.metadata(remote).await
        && !meta.file_type().is_dir()
    {
        let dest_size = meta.size.unwrap_or(0);
        match t.resolve(true, remote.to_string(), size, dest_size).await? {
            ConflictChoice::Skip => {
                t.count(|s| s.skipped += 1);
                return Ok(());
            }
            ConflictChoice::Replace => {}
            ConflictChoice::Resume => resume = true,
        }
    }
    t.add_total(size);
    upload_file(sftp, local, remote, resume, t).await
}

/// Copia un fichero local a uno remoto. Con `resume` no se trunca el
/// destino: se continúa desde el tamaño que ya tiene.
async fn upload_file(
    sftp: &SftpSession,
    local: &Path,
    remote: &str,
    resume: bool,
    t: &Transfer,
) -> Result<()> {
    let mut src = tokio::fs::File::open(local)
        .await
        .with_context(|| format!("abriendo {}", local.display()))?;
    let name = local.display().to_string();
    let mut dst = if resume {
        let mut dst = sftp.open_with_flags(remote, OpenFlags::WRITE).await?;
        let offset = dst.metadata().await?.size.unwrap_or(0);
        if offset > src.metadata().await?.len() {
            bail!("No se puede reanudar {remote}: ya es mayor que el origen");
        }
        src.seek(SeekFrom::Start(offset)).await?;
        dst.seek(SeekFrom::Start(offset)).await?;
        t.count(|s| s.resumed += 1);
        t.progress(offset, &name, false);
        dst
    } else {
        sftp.create(remote).await?
    };
    let mut buf = vec![0u8; CHUNK];
    loop {
        t.check()?;
        let n = src.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).await?;
        t.advance(n as u64, &name, false);
    }
    dst.flush().await?;
    dst.shutdown().await?;
    t.advance(0, &name, true);
    t.count(|s| s.copied += 1);
    Ok(())
}

fn rel_join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

fn unix_secs(time: std::io::Result<std::time::SystemTime>) -> Option<i64> {
    let secs = time.ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    i64::try_from(secs).ok()
}

/// Recorre las dos carpetas y anota lo que hay en cada una. No sigue
/// enlaces simbólicos en ninguno de los dos lados.
async fn sync_scan(
    sftp: &SftpSession,
    local: &Path,
    remote: &str,
    t: &Transfer,
) -> Result<SyncScan> {
    let (local, remote) = tokio::try_join!(scan_local(local, t), scan_remote(sftp, remote, t))?;
    Ok(SyncScan { local, remote })
}

async fn scan_local(root: &Path, t: &Transfer) -> Result<SyncTree> {
    let mut tree = SyncTree::default();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        t.check()?;
        t.progress(0, &dir.display().to_string(), false);
        let mut entries = tokio::fs::read_dir(&dir)
            .await
            .with_context(|| format!("leyendo {}", dir.display()))?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            // Un nombre que no se puede escribir tal cual en el otro lado.
            let Some(name) = name.to_str().filter(|n| safe_component(n)) else {
                tree.ignored.insert(rel_join(&rel, &name.to_string_lossy()));
                continue;
            };
            let child = rel_join(&rel, name);
            // `file_type` no sigue enlaces: uno a una carpeta no se recorre.
            let file_type = entry.file_type().await?;
            if name.ends_with(PART_SUFFIX) {
                tree.ignored.insert(child);
            } else if file_type.is_dir() {
                tree.dirs.insert(child.clone());
                stack.push((entry.path(), child));
            } else if file_type.is_file() {
                let meta = entry.metadata().await?;
                let file = SyncFile {
                    size: meta.len(),
                    mtime: unix_secs(meta.modified()),
                };
                tree.files.insert(child, file);
            } else {
                tree.ignored.insert(child);
            }
        }
    }
    Ok(tree)
}

async fn scan_remote(sftp: &SftpSession, root: &str, t: &Transfer) -> Result<SyncTree> {
    let mut tree = SyncTree::default();
    let mut stack = vec![(root.to_string(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        t.check()?;
        t.progress(0, &dir, false);
        let entries = sftp
            .read_dir(dir.as_str())
            .await
            .with_context(|| format!("listando {dir}"))?;
        for entry in entries {
            let name = entry.file_name();
            let name = name.as_str();
            let child = rel_join(&rel, name);
            // Los nombres llegan del servidor: los que no son un componente
            // normal no se sincronizan. Tampoco los que no eran UTF-8, que
            // llegan con el carácter de sustitución en lugar del original.
            if !safe_component(name) || name.contains(char::REPLACEMENT_CHARACTER) {
                tree.ignored.insert(child);
                continue;
            }
            let meta = entry.metadata();
            // El tipo es el del propio enlace, no el de su destino.
            let file_type = meta.file_type();
            if name.ends_with(PART_SUFFIX) {
                tree.ignored.insert(child);
            } else if file_type.is_dir() {
                tree.dirs.insert(child.clone());
                stack.push((join_remote(&dir, name), child));
            } else if file_type.is_file() {
                let file = SyncFile {
                    size: meta.size.unwrap_or(0),
                    mtime: meta.mtime.map(i64::from),
                };
                tree.files.insert(child, file);
            } else {
                tree.ignored.insert(child);
            }
        }
    }
    Ok(tree)
}

/// Las dos carpetas de una sincronización. Todo lo que se escribe o se
/// borra pasa por aquí, y nunca sale de ellas: las rutas relativas se
/// validan componente a componente y no se atraviesan enlaces simbólicos.
struct SyncRoots<'a> {
    sftp: &'a SftpSession,
    local: &'a Path,
    remote: &'a str,
    /// Carpetas ya comprobadas (existen y no son enlaces).
    local_dirs: HashSet<String>,
    remote_dirs: HashSet<String>,
}

impl SyncRoots<'_> {
    fn local_path(&self, rel: &str) -> PathBuf {
        rel.split('/')
            .fold(self.local.to_path_buf(), |p, c| p.join(c))
    }

    fn remote_path(&self, rel: &str) -> String {
        rel.split('/')
            .fold(self.remote.to_string(), |p, c| join_remote(&p, c))
    }

    /// Comprueba que la carpeta local `rel` es una carpeta de verdad en
    /// todos sus tramos; con `create`, crea los que falten.
    async fn local_dir(&mut self, rel: &str, create: bool) -> Result<()> {
        for prefix in prefixes(rel) {
            if self.local_dirs.contains(prefix) {
                continue;
            }
            let path = self.local_path(prefix);
            match tokio::fs::symlink_metadata(&path).await {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => bail!(
                    "{} no es una carpeta (no se siguen enlaces)",
                    path.display()
                ),
                Err(_) if create => tokio::fs::create_dir(&path)
                    .await
                    .with_context(|| format!("creando {}", path.display()))?,
                Err(e) => bail!("{}: {e}", path.display()),
            }
            self.local_dirs.insert(prefix.to_string());
        }
        Ok(())
    }

    /// Lo mismo que [`Self::local_dir`], en el servidor.
    async fn remote_dir(&mut self, rel: &str, create: bool) -> Result<()> {
        for prefix in prefixes(rel) {
            if self.remote_dirs.contains(prefix) {
                continue;
            }
            let path = self.remote_path(prefix);
            match self.sftp.symlink_metadata(path.as_str()).await {
                Ok(meta) if meta.file_type().is_dir() => {}
                Ok(_) => bail!("{path} no es una carpeta (no se siguen enlaces)"),
                Err(_) if create => self
                    .sftp
                    .create_dir(path.as_str())
                    .await
                    .with_context(|| format!("creando {path}"))?,
                Err(e) => bail!("{path}: {e}"),
            }
            self.remote_dirs.insert(prefix.to_string());
        }
        Ok(())
    }

    /// Sube un fichero con nombre provisional y, ya completo y con su fecha,
    /// lo pone en su sitio.
    async fn upload(&mut self, rel: &str, t: &Transfer) -> Result<()> {
        self.remote_dir(parent_rel(rel), true).await?;
        let local = self.local_path(rel);
        let remote = self.remote_path(rel);
        let part = format!("{remote}{PART_SUFFIX}");
        let sftp = self.sftp;
        let copy = async {
            let meta = tokio::fs::symlink_metadata(&local).await?;
            if !meta.is_file() {
                bail!("ya no es un fichero normal");
            }
            upload_file(sftp, &local, &part, false, t).await?;
            // Sin la fecha del origen, la próxima comparación lo vería como
            // más reciente y lo copiaría otra vez. Los permisos, los que ya
            // tuviera el destino.
            let mtime = unix_secs(meta.modified()).map(|s| s.clamp(0, u32::MAX as i64) as u32);
            let permissions = match sftp.symlink_metadata(remote.as_str()).await {
                Ok(old) if old.file_type().is_file() => old.permissions.map(|p| p & 0o7777),
                _ => None,
            };
            let attrs = FileAttributes {
                permissions,
                atime: mtime,
                mtime,
                ..FileAttributes::empty()
            };
            sftp.set_metadata(part.as_str(), attrs).await?;
            // Sin la extensión de OpenSSH, `rename` no pisa un destino.
            if !sftp.posix_rename(part.as_str(), remote.as_str()).await? {
                let _ = sftp.remove_file(remote.as_str()).await;
                sftp.rename(part.as_str(), remote.as_str()).await?;
            }
            anyhow::Ok(())
        };
        let result = copy.await.with_context(|| format!("subiendo {rel}"));
        if result.is_err() {
            let _ = sftp.remove_file(part.as_str()).await;
        }
        result
    }

    /// Como [`Self::upload`], en el otro sentido.
    async fn download(&mut self, rel: &str, t: &Transfer) -> Result<()> {
        self.local_dir(parent_rel(rel), true).await?;
        let local = self.local_path(rel);
        let remote = self.remote_path(rel);
        let mut part = local.clone().into_os_string();
        part.push(PART_SUFFIX);
        let part = PathBuf::from(part);
        let sftp = self.sftp;
        let copy = async {
            let meta = sftp.symlink_metadata(remote.as_str()).await?;
            if !meta.file_type().is_file() {
                bail!("ya no es un fichero normal");
            }
            // Si quedara algo con el nombre provisional (aunque fuera un
            // enlace), se quita: se escribe en un fichero nuevo.
            let _ = tokio::fs::remove_file(&part).await;
            download_file(sftp, &remote, &part, false, t).await?;
            if let Ok(old) = tokio::fs::symlink_metadata(&local).await
                && old.is_file()
            {
                tokio::fs::set_permissions(&part, old.permissions()).await?;
            }
            if let Some(mtime) = meta.mtime {
                let file = tokio::fs::OpenOptions::new()
                    .write(true)
                    .open(&part)
                    .await?
                    .into_std()
                    .await;
                file.set_modified(UNIX_EPOCH + Duration::from_secs(mtime.into()))?;
            }
            tokio::fs::rename(&part, &local).await?;
            anyhow::Ok(())
        };
        let result = copy.await.with_context(|| format!("descargando {rel}"));
        if result.is_err() {
            let _ = tokio::fs::remove_file(&part).await;
        }
        result
    }
}

/// Carpeta (relativa) que contiene a `rel`; vacía si es la raíz.
fn parent_rel(rel: &str) -> &str {
    rel.rfind('/').map_or("", |i| &rel[..i])
}

/// Ejecuta un plan de sincronización: primero copia y solo al final, si
/// todo ha ido bien, borra lo que sobra.
async fn sync_run(
    sftp: &SftpSession,
    local: &Path,
    remote: &str,
    items: &[SyncItem],
    t: &Transfer,
) -> Result<()> {
    // Las rutas salieron de nombres del servidor: se vuelven a validar antes
    // de tocar nada.
    for item in items {
        if !item.path.split('/').all(safe_component) {
            bail!("Ruta no válida en el plan: {:?}", item.path);
        }
    }
    let mut roots = SyncRoots {
        sftp,
        local,
        remote,
        local_dirs: HashSet::new(),
        remote_dirs: HashSet::new(),
    };
    let copies = |item: &&SyncItem| matches!(item.op, SyncOp::Upload | SyncOp::Download);
    t.add_total(items.iter().filter(copies).map(|i| i.size).sum());

    // El plan viene ordenado por ruta: cada carpeta, antes que su contenido.
    for item in items.iter().filter(copies) {
        t.check()?;
        match (item.op, item.is_dir) {
            (SyncOp::Upload, true) => roots.remote_dir(&item.path, true).await?,
            (SyncOp::Download, true) => roots.local_dir(&item.path, true).await?,
            (SyncOp::Upload, false) => roots.upload(&item.path, t).await?,
            (_, false) => roots.download(&item.path, t).await?,
            _ => {}
        }
    }

    // Borrados: los ficheros y después las carpetas, de dentro afuera. Solo
    // se quitan enlaces de nombres (`unlink`, `rmdir`): un enlace simbólico
    // que hubiera aparecido entretanto se borraría él, no su destino.
    let deletes = |item: &&SyncItem| matches!(item.op, SyncOp::DeleteLocal | SyncOp::DeleteRemote);
    let (dirs, files): (Vec<&SyncItem>, Vec<&SyncItem>) =
        items.iter().filter(deletes).partition(|item| item.is_dir);
    for item in files.into_iter().chain(dirs.into_iter().rev()) {
        t.check()?;
        let parent = parent_rel(&item.path);
        if item.op == SyncOp::DeleteLocal {
            roots.local_dir(parent, false).await?;
            let path = roots.local_path(&item.path);
            let res = if item.is_dir {
                tokio::fs::remove_dir(&path).await
            } else {
                tokio::fs::remove_file(&path).await
            };
            res.with_context(|| format!("borrando {}", path.display()))?;
        } else {
            roots.remote_dir(parent, false).await?;
            let path = roots.remote_path(&item.path);
            let res = if item.is_dir {
                sftp.remove_dir(path.as_str()).await
            } else {
                sftp.remove_file(path.as_str()).await
            };
            res.with_context(|| format!("borrando {path}"))?;
        }
        t.count(|s| s.deleted += 1);
        t.progress(0, &item.path, false);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths() {
        assert_eq!(join_remote("/", "etc"), "/etc");
        assert_eq!(join_remote("/home/u", "f.txt"), "/home/u/f.txt");
        assert_eq!(parent_remote("/home/u"), "/home");
        assert_eq!(parent_remote("/home"), "/");
        assert_eq!(parent_remote("/"), "/");
        assert_eq!(parent_remote("/a/b/"), "/a");
    }

    #[test]
    fn unsafe_names_rejected() {
        assert!(safe_component("fichero.txt"));
        assert!(!safe_component(".."));
        assert!(!safe_component("a/b"));
        assert!(!safe_component("."));
        assert!(!safe_component("/etc/passwd"));
        assert_eq!(safe_component("..\\x"), !cfg!(windows));
        assert!(!safe_component(""));
    }

    #[test]
    fn modes_as_text_and_octal() {
        assert_eq!(mode_text(0o755), "rwxr-xr-x");
        assert_eq!(mode_text(0o640), "rw-r-----");
        assert_eq!(mode_text(0), "---------");
        assert_eq!(mode_text(0o4755), "rwsr-xr-x");
        assert_eq!(mode_text(0o2644), "rw-r-Sr--");
        assert_eq!(mode_text(0o1777), "rwxrwxrwt");
        assert_eq!(mode_text(0o1776), "rwxrwxrwT");
        // Los bits del tipo de fichero no cuentan.
        assert_eq!(mode_text(0o100644), "rw-r--r--");
        assert_eq!(mode_octal(0o755), "755");
        assert_eq!(mode_octal(0o7), "007");
        assert_eq!(mode_octal(0o4755), "4755");
        assert_eq!(mode_octal(0o40755), "755");

        assert_eq!(parse_mode("755"), Some(0o755));
        assert_eq!(parse_mode("0644"), Some(0o644));
        assert_eq!(parse_mode(" 4755 "), Some(0o4755));
        assert_eq!(parse_mode("7"), Some(0o7));
        assert_eq!(parse_mode("rwxr-xr-x"), Some(0o755));
        assert_eq!(parse_mode("-rw-r--r--"), Some(0o644));
        assert_eq!(parse_mode("drwxrwxrwt"), Some(0o1777));
        assert_eq!(parse_mode("rwsr-Sr-T"), Some(0o7744));
        for bad in [
            "",
            "8",
            "75555",
            "-755",
            "rwxrwxrw",
            "rwxrwxrwz",
            "wrxr-xr-x",
            "rwtr-xr-x",
            "abc",
        ] {
            assert_eq!(parse_mode(bad), None, "{bad:?}");
        }
        // Ida y vuelta de todos los valores posibles, por los dos caminos.
        for mode in 0..=0o7777 {
            assert_eq!(parse_mode(&mode_octal(mode)), Some(mode));
            assert_eq!(parse_mode(&mode_text(mode)), Some(mode), "{mode:o}");
        }
    }

    #[test]
    fn remembered_conflict_answers() {
        use ConflictChoice::*;
        let answers = |m: &ConflictMemory| (m.get(true), m.get(false));
        let mut m = ConflictMemory::default();
        assert_eq!(answers(&m), (None, None));
        // «Reanudar todos» no dice nada de los que no se pueden reanudar…
        m.remember(true, Resume);
        assert_eq!(answers(&m), (Some(Resume), None));
        // …y lo que se decida para esos no cambia lo ya dicho.
        m.remember(false, Replace);
        assert_eq!(answers(&m), (Some(Resume), Some(Replace)));

        // «Omitir todos» y «Sustituir todos» valen para los dos casos.
        let mut m = ConflictMemory::default();
        m.remember(true, Skip);
        assert_eq!(answers(&m), (Some(Skip), Some(Skip)));
        let mut m = ConflictMemory::default();
        m.remember(false, Replace);
        assert_eq!(answers(&m), (Some(Replace), Some(Replace)));

        // Reanudar lo que no se puede reanudar no se recuerda.
        let mut m = ConflictMemory::default();
        m.remember(false, Resume);
        assert_eq!(answers(&m), (None, None));
    }

    fn tree(files: &[(&str, u64, i64)], dirs: &[&str], ignored: &[&str]) -> SyncTree {
        SyncTree {
            files: files
                .iter()
                .map(|&(path, size, mtime)| {
                    let mtime = Some(mtime);
                    (path.to_string(), SyncFile { size, mtime })
                })
                .collect(),
            dirs: dirs.iter().map(|d| d.to_string()).collect(),
            ignored: ignored.iter().map(|d| d.to_string()).collect(),
        }
    }

    /// Estado y acción de una ruta del plan (`None` si no aparece).
    fn planned(plan: &[SyncItem], path: &str) -> Option<(SyncState, SyncOp)> {
        let mut found = plan.iter().filter(|i| i.path == path);
        let item = found.next()?;
        assert!(found.next().is_none(), "{path} aparece dos veces");
        Some((item.state, item.op))
    }

    fn sample_scan() -> SyncScan {
        SyncScan {
            local: tree(
                &[
                    ("igual.txt", 10, 1000),
                    ("nuevo.txt", 5, 1000),
                    ("reciente-local.txt", 7, 2000),
                    ("reciente-remoto.txt", 7, 1000),
                    ("tolerancia.txt", 4, 1000),
                    ("justo-fuera.txt", 4, 1000),
                    ("empate.txt", 4, 1000),
                    ("sub/dentro.txt", 1, 1000),
                ],
                &["sub", "vacia", "comun"],
                &[],
            ),
            remote: tree(
                &[
                    ("igual.txt", 10, 1000),
                    ("reciente-local.txt", 7, 1000),
                    ("reciente-remoto.txt", 9, 2000),
                    ("tolerancia.txt", 4, 1002),
                    ("justo-fuera.txt", 4, 1003),
                    ("empate.txt", 8, 1001),
                    ("sobra.txt", 3, 500),
                    ("vieja/a.txt", 2, 500),
                ],
                &["vieja", "vieja/hueca", "comun"],
                &[],
            ),
        }
    }

    #[test]
    fn sync_plan_local_to_remote() {
        use SyncOp::{DeleteRemote, None as Nothing, Upload};
        use SyncState::*;
        let scan = sample_scan();
        let plan = plan_sync(&scan, SyncMode::Upload, false);
        let at = |path| planned(&plan, path);
        assert_eq!(at("igual.txt"), Some((Equal, Nothing)));
        assert_eq!(at("tolerancia.txt"), Some((Equal, Nothing)));
        assert_eq!(at("nuevo.txt"), Some((OnlyLocal, Upload)));
        assert_eq!(at("sub/dentro.txt"), Some((OnlyLocal, Upload)));
        assert_eq!(at("reciente-local.txt"), Some((LocalNewer, Upload)));
        // Lo que es más reciente en el destino no se pisa.
        assert_eq!(at("reciente-remoto.txt"), Some((RemoteNewer, Nothing)));
        assert_eq!(at("justo-fuera.txt"), Some((RemoteNewer, Nothing)));
        // Misma fecha y distinto tamaño: manda el origen.
        assert_eq!(at("empate.txt"), Some((Differ, Upload)));
        // Lo que sobra en el destino se queda si no se pide borrarlo.
        assert_eq!(at("sobra.txt"), Some((OnlyRemote, Nothing)));
        assert_eq!(at("vieja/a.txt"), Some((OnlyRemote, Nothing)));
        assert_eq!(at("vieja"), Some((OnlyRemote, Nothing)));
        // Las carpetas que faltan se crean, también las vacías; las que
        // están en los dos lados no aparecen.
        assert_eq!(at("sub"), Some((OnlyLocal, Upload)));
        assert_eq!(at("vacia"), Some((OnlyLocal, Upload)));
        assert_eq!(at("comun"), None);
        // Ordenado por ruta: cada carpeta, antes que su contenido.
        assert!(plan.windows(2).all(|w| w[0].path < w[1].path));

        let summary = SyncSummary::of(&plan);
        assert_eq!(
            summary,
            SyncSummary {
                upload: 4,
                upload_bytes: 5 + 1 + 7 + 4,
                make_dirs: 2,
                equal: 2,
                only_local: 2,
                only_remote: 2,
                untouched: 2,
                ..Default::default()
            }
        );
        assert!(!summary.nothing_to_do());

        // Con borrado: solo cambia lo que sobra en el destino.
        let plan = plan_sync(&scan, SyncMode::Upload, true);
        let at = |path| planned(&plan, path);
        assert_eq!(at("sobra.txt"), Some((OnlyRemote, DeleteRemote)));
        assert_eq!(at("vieja/a.txt"), Some((OnlyRemote, DeleteRemote)));
        assert_eq!(at("vieja"), Some((OnlyRemote, DeleteRemote)));
        assert_eq!(at("vieja/hueca"), Some((OnlyRemote, DeleteRemote)));
        assert_eq!(at("nuevo.txt"), Some((OnlyLocal, Upload)));
        assert_eq!(at("reciente-remoto.txt"), Some((RemoteNewer, Nothing)));
        let summary = SyncSummary::of(&plan);
        assert_eq!((summary.delete_files, summary.delete_dirs), (2, 2));
        assert_eq!(summary.upload, 4);
        // Nunca se borra nada del origen.
        assert!(plan.iter().all(|i| i.op != SyncOp::DeleteLocal));
    }

    #[test]
    fn sync_plan_remote_to_local() {
        use SyncOp::{DeleteLocal, Download, None as Nothing};
        use SyncState::*;
        let scan = sample_scan();
        let plan = plan_sync(&scan, SyncMode::Download, false);
        let at = |path| planned(&plan, path);
        assert_eq!(at("sobra.txt"), Some((OnlyRemote, Download)));
        assert_eq!(at("vieja/a.txt"), Some((OnlyRemote, Download)));
        assert_eq!(at("vieja"), Some((OnlyRemote, Download)));
        assert_eq!(at("vieja/hueca"), Some((OnlyRemote, Download)));
        assert_eq!(at("reciente-remoto.txt"), Some((RemoteNewer, Download)));
        assert_eq!(at("justo-fuera.txt"), Some((RemoteNewer, Download)));
        assert_eq!(at("empate.txt"), Some((Differ, Download)));
        assert_eq!(at("reciente-local.txt"), Some((LocalNewer, Nothing)));
        assert_eq!(at("nuevo.txt"), Some((OnlyLocal, Nothing)));
        assert_eq!(at("sub"), Some((OnlyLocal, Nothing)));
        let summary = SyncSummary::of(&plan);
        assert_eq!(summary.download, 5);
        assert_eq!(summary.download_bytes, 3 + 2 + 9 + 4 + 8);
        assert_eq!((summary.upload, summary.delete_files), (0, 0));

        let plan = plan_sync(&scan, SyncMode::Download, true);
        let at = |path| planned(&plan, path);
        assert_eq!(at("nuevo.txt"), Some((OnlyLocal, DeleteLocal)));
        assert_eq!(at("sub/dentro.txt"), Some((OnlyLocal, DeleteLocal)));
        assert_eq!(at("sub"), Some((OnlyLocal, DeleteLocal)));
        assert_eq!(at("vacia"), Some((OnlyLocal, DeleteLocal)));
        assert_eq!(at("reciente-local.txt"), Some((LocalNewer, Nothing)));
        let summary = SyncSummary::of(&plan);
        assert_eq!((summary.delete_files, summary.delete_dirs), (2, 2));
        assert!(plan.iter().all(|i| i.op != SyncOp::DeleteRemote));
    }

    #[test]
    fn sync_plan_both_ways() {
        use SyncOp::{Download, None as Nothing, Upload};
        use SyncState::*;
        let scan = sample_scan();
        let plan = plan_sync(&scan, SyncMode::Both, false);
        let at = |path| planned(&plan, path);
        assert_eq!(at("nuevo.txt"), Some((OnlyLocal, Upload)));
        assert_eq!(at("sobra.txt"), Some((OnlyRemote, Download)));
        assert_eq!(at("reciente-local.txt"), Some((LocalNewer, Upload)));
        assert_eq!(at("reciente-remoto.txt"), Some((RemoteNewer, Download)));
        assert_eq!(at("sub"), Some((OnlyLocal, Upload)));
        assert_eq!(at("vieja/hueca"), Some((OnlyRemote, Download)));
        // Sin una fecha que decida, no se toca ninguno de los dos.
        assert_eq!(at("empate.txt"), Some((Differ, Nothing)));
        assert_eq!(SyncSummary::of(&plan).untouched, 1);
        // En los dos sentidos no se borra nunca, se pida o no.
        assert_eq!(plan_sync(&scan, SyncMode::Both, true), plan);
    }

    #[test]
    fn sync_plan_of_equal_folders_does_nothing() {
        let side = tree(&[("a.txt", 3, 100), ("d/b.txt", 4, 200)], &["d"], &[]);
        let scan = SyncScan {
            local: side.clone(),
            remote: side,
        };
        for mode in [SyncMode::Upload, SyncMode::Download, SyncMode::Both] {
            for delete in [false, true] {
                let summary = SyncSummary::of(&plan_sync(&scan, mode, delete));
                assert!(summary.nothing_to_do(), "{mode:?} {delete}");
                assert_eq!(summary.equal, 2);
            }
        }
        // Sin fecha en un lado, deciden los tamaños.
        let mut scan = scan;
        scan.remote.files.get_mut("a.txt").unwrap().mtime = None;
        let plan = plan_sync(&scan, SyncMode::Both, false);
        assert_eq!(planned(&plan, "a.txt").unwrap().0, SyncState::Equal);
        scan.remote.files.get_mut("a.txt").unwrap().size = 9;
        let plan = plan_sync(&scan, SyncMode::Both, false);
        assert_eq!(planned(&plan, "a.txt").unwrap().0, SyncState::Differ);
    }

    #[test]
    fn sync_plan_never_goes_through_links_or_type_conflicts() {
        use SyncOp::{DeleteRemote, None as Nothing};
        use SyncState::*;
        let scan = SyncScan {
            local: tree(
                &[
                    ("mixto", 1, 100),
                    ("enlace-remoto", 1, 100),
                    ("ok.txt", 1, 100),
                ],
                &["carpeta-aqui"],
                // Un enlace local donde el servidor tiene una carpeta.
                &["enlace"],
            ),
            remote: tree(
                &[
                    ("enlace/dentro.txt", 1, 100),
                    ("mixto/dentro.txt", 1, 100),
                    ("carpeta-aqui", 1, 100),
                    ("restos/f.txt", 1, 100),
                    ("limpia/f.txt", 1, 100),
                ],
                &["enlace", "enlace/sub", "mixto", "restos", "limpia"],
                &["enlace-remoto", "restos/enlace"],
            ),
        };
        for mode in [SyncMode::Upload, SyncMode::Download, SyncMode::Both] {
            for delete in [false, true] {
                let plan = plan_sync(&scan, mode, delete);
                let at = |path| planned(&plan, path);
                // Nada por debajo de un enlace, en ningún sentido: copiar
                // ahí sería escribir fuera de la carpeta. El enlace sale en
                // el plan, como algo que no se toca.
                assert_eq!(at("enlace/dentro.txt"), Some((Ignored, Nothing)));
                assert_eq!(at("enlace"), Some((Ignored, Nothing)));
                assert_eq!(at("restos/enlace"), Some((Ignored, Nothing)));
                assert_eq!(at("enlace/sub"), None);
                assert_eq!(SyncSummary::of(&plan).ignored, 4);
                // Ni encima de un enlace del otro lado.
                assert_eq!(at("enlace-remoto"), Some((Ignored, Nothing)));
                // Fichero en un lado y carpeta en el otro.
                assert_eq!(at("mixto"), Some((TypeConflict, Nothing)));
                assert_eq!(at("mixto/dentro.txt"), Some((TypeConflict, Nothing)));
                assert_eq!(at("carpeta-aqui"), Some((TypeConflict, Nothing)));
                assert_eq!(at("ok.txt").unwrap().0, OnlyLocal);
            }
        }
        // Una carpeta que sobra se borra con su contenido, salvo que tenga
        // dentro algo que no se sincroniza: no quedaría vacía.
        let plan = plan_sync(&scan, SyncMode::Upload, true);
        let at = |path| planned(&plan, path);
        assert_eq!(at("limpia/f.txt"), Some((OnlyRemote, DeleteRemote)));
        assert_eq!(at("limpia"), Some((OnlyRemote, DeleteRemote)));
        assert_eq!(at("restos/f.txt"), Some((OnlyRemote, DeleteRemote)));
        assert_eq!(at("restos"), Some((Ignored, Nothing)));
    }

    #[test]
    fn relative_path_prefixes() {
        let all = |rel| prefixes(rel).collect::<Vec<_>>();
        assert_eq!(all("a/b/c"), ["a", "a/b", "a/b/c"]);
        assert_eq!(all("solo"), ["solo"]);
        assert_eq!(all("ñ/x"), ["ñ", "ñ/x"]);
        assert!(all("").is_empty());
        assert_eq!(parent_rel("a/b/c"), "a/b");
        assert_eq!(parent_rel("solo"), "");
    }
}
