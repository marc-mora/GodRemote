//! Cliente SFTP en segundo plano: listado, operaciones y transferencias.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use russh::Disconnect;
use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::model::Session;
use crate::ssh::{Credentials, Prompter, SshHandle};

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
    /// Abrir una conexión nueva.
    Connect {
        session: Box<Session>,
        creds: Credentials,
    },
    /// Reutilizar una conexión SSH ya autenticada (p. ej. la del terminal).
    Existing(SshHandle),
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
    Download {
        id: u64,
        remote: String,
        local: PathBuf,
    },
    Upload {
        id: u64,
        local: PathBuf,
        remote: String,
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
    TransferFinished {
        id: u64,
        error: Option<String>,
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
        SftpSource::Connect { session, creds } => {
            let status = {
                let emit = emit.clone();
                move |s: String| emit(SftpEvent::Status(s))
            };
            let connected = crate::ssh::connect(&session, creds, prompter, &status).await?;
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

    let cancels: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>> = Arc::default();

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
            SftpCmd::Cancel(id) => {
                if let Some(flag) = cancels.lock().unwrap().get(&id) {
                    flag.store(true, Ordering::Relaxed);
                }
            }
            SftpCmd::Download { id, remote, local } => {
                let transfer = Transfer::new(id, &cancels, &emit);
                let sftp = sftp.clone();
                let cancels = cancels.clone();
                let emit = emit.clone();
                tokio::spawn(async move {
                    let res = download(&sftp, &remote, &local, &transfer).await;
                    cancels.lock().unwrap().remove(&id);
                    emit(SftpEvent::TransferFinished {
                        id,
                        error: res.err().map(|e| format!("{e:#}")),
                    });
                });
            }
            SftpCmd::Upload { id, local, remote } => {
                let transfer = Transfer::new(id, &cancels, &emit);
                let sftp = sftp.clone();
                let cancels = cancels.clone();
                let emit = emit.clone();
                tokio::spawn(async move {
                    let res = upload(&sftp, &local, &remote, &transfer).await;
                    cancels.lock().unwrap().remove(&id);
                    emit(SftpEvent::TransferFinished {
                        id,
                        error: res.err().map(|e| format!("{e:#}")),
                    });
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

struct Transfer {
    id: u64,
    cancel: Arc<AtomicBool>,
    emit: Emit,
    done: Mutex<(u64, u64, std::time::Instant)>,
}

impl Transfer {
    fn new(id: u64, cancels: &Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>, emit: &Emit) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        cancels.lock().unwrap().insert(id, cancel.clone());
        Self {
            id,
            cancel,
            emit: emit.clone(),
            done: Mutex::new((0, 0, std::time::Instant::now())),
        }
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
}

const CHUNK: usize = 256 * 1024;

async fn download(sftp: &SftpSession, remote: &str, local: &Path, t: &Transfer) -> Result<()> {
    let meta = sftp.metadata(remote).await?;
    if meta.file_type().is_dir() {
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
                    t.add_total(size);
                    download_file(sftp, &rchild, &lchild, t).await?;
                }
                // Enlaces y ficheros especiales se omiten en descargas recursivas.
            }
        }
        Ok(())
    } else {
        t.add_total(meta.size.unwrap_or(0));
        download_file(sftp, remote, local, t).await
    }
}

async fn download_file(sftp: &SftpSession, remote: &str, local: &Path, t: &Transfer) -> Result<()> {
    let mut src = sftp.open(remote).await?;
    let mut dst = tokio::fs::File::create(local)
        .await
        .with_context(|| format!("creando {}", local.display()))?;
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
    Ok(())
}

async fn upload(sftp: &SftpSession, local: &Path, remote: &str, t: &Transfer) -> Result<()> {
    let meta = tokio::fs::metadata(local)
        .await
        .with_context(|| format!("leyendo {}", local.display()))?;
    if meta.is_dir() {
        let mut stack = vec![(local.to_path_buf(), remote.to_string())];
        while let Some((ldir, rdir)) = stack.pop() {
            t.check()?;
            if sftp.metadata(rdir.as_str()).await.is_err() {
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
                    t.add_total(entry.metadata().await.map(|m| m.len()).unwrap_or(0));
                    upload_file(sftp, &entry.path(), &rchild, t).await?;
                }
            }
        }
        Ok(())
    } else {
        t.add_total(meta.len());
        upload_file(sftp, local, remote, t).await
    }
}

async fn upload_file(sftp: &SftpSession, local: &Path, remote: &str, t: &Transfer) -> Result<()> {
    let mut src = tokio::fs::File::open(local)
        .await
        .with_context(|| format!("abriendo {}", local.display()))?;
    let mut dst = sftp.create(remote).await?;
    let mut buf = vec![0u8; CHUNK];
    let name = local.display().to_string();
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
}
