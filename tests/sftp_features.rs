//! Explorador SFTP contra un `sshd` real de OpenSSH: permisos, reanudación
//! de transferencias, destinos que ya existen, sincronización de carpetas y
//! ficheros soltados sobre la vista.
//!
//! Se omiten si no hay `sshd` instalado o no se ejecutan como root (sshd
//! necesita root para abrir sesiones). El «servidor» es esta misma máquina,
//! así que lo remoto se comprueba mirando directamente el disco.
#![cfg(unix)]

mod common;

use std::collections::BTreeSet;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, UNIX_EPOCH};

use common::{Killed, answers, find_program, is_root, prompter, start_sshd};
use godremote::keys::{self, KeyAlgorithm};
use godremote::model::{AuthMethod, Session};
use godremote::sftp::{
    self, Conflict, ConflictAnswer, ConflictChoice, OnConflict, PART_SUFFIX, SftpClient, SftpCmd,
    SftpEvent, SftpSource, SyncFile, SyncItem, SyncMode, SyncOp, SyncScan, SyncState, SyncSummary,
    TransferStats, plan_sync,
};
use godremote::ssh::{Credentials, Endpoint};
use godremote::ui::sftp_view::{SftpState, SftpView};

/// Cada prueba arranca su `sshd` y fija `GODREMOTE_KNOWN_HOSTS`: van de
/// una en una.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Fechas de modificación de los ficheros de prueba (segundos Unix).
const OLD: u64 = 1_600_000_000;
const NEW: u64 = 1_700_000_000;

/// Un `sshd` de usar y tirar con un cliente SFTP conectado a él.
struct Env {
    client: SftpClient,
    rt: tokio::runtime::Runtime,
    endpoint: Endpoint,
    next_id: u64,
    _sshd: Killed,
    dir: tempfile::TempDir,
    _serial: MutexGuard<'static, ()>,
}

/// Lo que se supo de un conflicto (la respuesta ya se ha enviado).
#[derive(Debug)]
struct Asked {
    upload: bool,
    path: String,
    source_size: u64,
    dest_size: u64,
    multiple: bool,
}

/// Resultado de una transferencia.
#[derive(Debug)]
struct Finished {
    error: Option<String>,
    stats: TransferStats,
    asked: Vec<Asked>,
    /// Último progreso notificado: (hecho, total).
    progress: Option<(u64, u64)>,
}

impl Finished {
    fn ok(self) -> Self {
        assert!(self.error.is_none(), "{:?}", self.error);
        self
    }
}

fn wait<T>(client: &SftpClient, mut pick: impl FnMut(SftpEvent) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match client
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(SftpEvent::Error(e)) => panic!("error SFTP: {e}"),
            Ok(SftpEvent::Closed(e)) => panic!("SFTP cerrado: {e:?}"),
            Ok(ev) => {
                if let Some(v) = pick(ev) {
                    return v;
                }
            }
            Err(_) => panic!("tiempo de espera agotado en SFTP"),
        }
    }
}

fn connect(rt: &tokio::runtime::Runtime, endpoint: &Endpoint) -> SftpClient {
    let client = sftp::spawn(
        rt.handle(),
        SftpSource::Connect(Box::new(endpoint.clone())),
        prompter(answers(true, &[])),
    );
    wait(&client, |ev| {
        matches!(ev, SftpEvent::Ready { .. }).then_some(())
    });
    client
}

impl Env {
    /// `None` (y la prueba se omite) si no se puede arrancar un `sshd`.
    fn start() -> Option<Self> {
        let serial = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        if find_program("sshd").is_none() || !is_root() {
            eprintln!("OMITIDA: hace falta sshd y ejecutar como root");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: las pruebas van de una en una y esta aún no ha creado hilos.
        unsafe { std::env::set_var("GODREMOTE_KNOWN_HOSTS", dir.path().join("known_hosts")) };
        let key = keys::generate(dir.path(), "id", KeyAlgorithm::Ed25519, "", None).unwrap();
        let authorized = dir.path().join("authorized_keys");
        std::fs::write(&authorized, format!("{}\n", key.public_openssh)).unwrap();
        let sftp_server = [
            "/usr/lib/openssh/sftp-server",
            "/usr/libexec/openssh/sftp-server",
            "/usr/libexec/sftp-server",
        ]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .unwrap_or("internal-sftp");
        let (sshd, port, _log) = start_sshd(
            dir.path(),
            &authorized,
            &format!("Subsystem sftp {sftp_server}\n"),
        );
        let user = std::process::Command::new("id")
            .arg("-un")
            .output()
            .unwrap();
        let session = Session {
            name: "sftp".into(),
            host: "127.0.0.1".into(),
            port,
            username: String::from_utf8_lossy(&user.stdout).trim().to_string(),
            auth: AuthMethod::Key,
            key_path: key.path.display().to_string(),
            ..Default::default()
        };
        let endpoint = Endpoint::direct(session, Credentials::default());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = connect(&rt, &endpoint);
        Some(Self {
            client,
            rt,
            endpoint,
            next_id: 1,
            _sshd: sshd,
            dir,
            _serial: serial,
        })
    }

    /// Carpeta nueva dentro del directorio temporal de la prueba.
    fn folder(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn send(&self, cmd: SftpCmd) {
        self.client.cmd.send(cmd).unwrap();
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn upload(&mut self, local: &Path, remote: &Path, on_conflict: OnConflict) -> u64 {
        let id = self.id();
        self.send(SftpCmd::Upload {
            id,
            local: local.to_path_buf(),
            remote: remote.display().to_string(),
            on_conflict,
        });
        id
    }

    fn download(&mut self, remote: &Path, local: &Path, on_conflict: OnConflict) -> u64 {
        let id = self.id();
        self.send(SftpCmd::Download {
            id,
            remote: remote.display().to_string(),
            local: local.to_path_buf(),
            on_conflict,
        });
        id
    }

    /// Espera a que termine la transferencia `id`, contestando a sus
    /// conflictos con `answer` (`None` descarta la pregunta sin contestar).
    fn finish(
        &self,
        id: u64,
        mut answer: impl FnMut(&Conflict) -> Option<ConflictAnswer>,
    ) -> Finished {
        let mut asked = vec![];
        let mut progress = None;
        let (error, stats) = wait(&self.client, |ev| match ev {
            SftpEvent::Conflict(conflict) if conflict.id == id => {
                asked.push(Asked {
                    upload: conflict.upload,
                    path: conflict.path.clone(),
                    source_size: conflict.source_size,
                    dest_size: conflict.dest_size,
                    multiple: conflict.multiple,
                });
                if let Some(answer) = answer(&conflict) {
                    conflict.reply.send(answer).unwrap();
                }
                None
            }
            SftpEvent::Progress {
                id: of,
                done,
                total,
                ..
            } if of == id => {
                assert!(done <= total);
                progress = Some((done, total));
                None
            }
            SftpEvent::TransferFinished {
                id: of,
                error,
                stats,
            } if of == id => Some((error, stats)),
            _ => None,
        });
        Finished {
            error,
            stats,
            asked,
            progress,
        }
    }

    /// Mensaje de la siguiente operación terminada.
    fn done(&self) -> String {
        wait(&self.client, |ev| match ev {
            SftpEvent::Done(msg) => Some(msg),
            _ => None,
        })
    }

    fn scan(&mut self, local: &Path, remote: &Path) -> SyncScan {
        let id = self.id();
        self.send(SftpCmd::SyncScan {
            id,
            local: local.to_path_buf(),
            remote: remote.display().to_string(),
        });
        wait(&self.client, |ev| match ev {
            SftpEvent::SyncScanned { id: of, result } if of == id => Some(result),
            _ => None,
        })
        .expect("comparación")
    }

    /// Ejecuta lo que el plan dice que hay que hacer.
    fn sync(&mut self, local: &Path, remote: &Path, plan: &[SyncItem]) -> Finished {
        let id = self.id();
        self.send(SftpCmd::Sync {
            id,
            local: local.to_path_buf(),
            remote: remote.display().to_string(),
            items: actions(plan),
        });
        self.finish(id, |_| panic!("una sincronización no pregunta"))
    }
}

/// Lo que se le manda ejecutar al cliente: el plan sin lo que no se toca.
fn actions(plan: &[SyncItem]) -> Vec<SyncItem> {
    plan.iter()
        .filter(|item| item.op != SyncOp::None)
        .cloned()
        .collect()
}

fn reply(choice: ConflictChoice, all: bool) -> Option<ConflictAnswer> {
    Some(ConflictAnswer { choice, all })
}

fn set_mtime(path: &Path, secs: u64) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

/// Escribe un fichero (creando sus carpetas) con una fecha de modificación.
fn write(path: &Path, content: impl AsRef<[u8]>, mtime: u64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
    set_mtime(path, mtime);
}

fn mtime(path: &Path) -> u64 {
    let modified = std::fs::metadata(path).unwrap().modified().unwrap();
    modified.duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn perms(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Contenido sin repeticiones a la vista: un trozo fuera de su sitio se nota.
fn pattern(len: usize) -> Vec<u8> {
    (0..len as u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect()
}

/// Ficheros que hay por debajo de una carpeta (sin seguir enlaces).
fn files_under(root: &Path) -> Vec<String> {
    let mut found = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
            } else {
                let rel = entry.path().strip_prefix(root).unwrap().to_path_buf();
                found.push(rel.display().to_string());
            }
        }
    }
    found.sort();
    found
}

#[test]
fn permissions_single_and_recursive() {
    let Some(env) = Env::start() else {
        return;
    };
    let root = env.folder("permisos");
    write(&root.join("a.txt"), "a", OLD);
    write(&root.join("sub/b.txt"), "b", OLD);
    write(&root.join("sub/hondo/c.sh"), "c", OLD);
    std::fs::create_dir(root.join("sub/vacia")).unwrap();
    let inside = [
        "a.txt",
        "sub",
        "sub/b.txt",
        "sub/hondo",
        "sub/hondo/c.sh",
        "sub/vacia",
    ];
    for rel in inside {
        let path = root.join(rel);
        chmod(&path, if path.is_dir() { 0o755 } else { 0o644 });
    }
    chmod(&root, 0o755);
    // Fuera del árbol, alcanzable solo a través de enlaces.
    let outside = env.folder("fuera");
    write(&outside.join("secreto.txt"), "s", OLD);
    chmod(&outside.join("secreto.txt"), 0o600);
    chmod(&outside, 0o700);
    symlink(outside.join("secreto.txt"), root.join("sub/enlace")).unwrap();
    symlink(&outside, root.join("sub/enlace_dir")).unwrap();
    let remote = |rel: &str| root.join(rel).display().to_string();

    // El listado trae los permisos actuales de cada entrada.
    env.send(SftpCmd::List(root.display().to_string()));
    let entries = wait(&env.client, |ev| match ev {
        SftpEvent::Listing { entries, .. } => Some(entries),
        _ => None,
    });
    let listed = |name: &str| {
        let entry = entries.iter().find(|e| e.name == name).unwrap();
        entry.permissions.unwrap() & 0o7777
    };
    assert_eq!(listed("a.txt"), 0o644);
    assert_eq!(listed("sub"), 0o755);

    // Un fichero.
    env.send(SftpCmd::Chmod {
        path: remote("a.txt"),
        mode: 0o600,
        recursive: false,
    });
    assert!(env.done().ends_with(": 600"));
    assert_eq!(perms(&root.join("a.txt")), 0o600);
    assert_eq!(perms(&root.join("sub/b.txt")), 0o644);

    // Los doce bits: también setuid, setgid y sticky.
    env.send(SftpCmd::Chmod {
        path: remote("a.txt"),
        mode: 0o4755,
        recursive: false,
    });
    assert!(env.done().ends_with(": 4755"));
    assert_eq!(perms(&root.join("a.txt")), 0o4755);

    // Una carpeta sin «recursivo»: solo ella.
    env.send(SftpCmd::Chmod {
        path: remote("sub"),
        mode: 0o700,
        recursive: false,
    });
    env.done();
    assert_eq!(perms(&root.join("sub")), 0o700);
    assert_eq!(perms(&root.join("sub/b.txt")), 0o644);
    assert_eq!(perms(&root.join("sub/hondo")), 0o755);

    // Recursivo: la carpeta y todo lo que cuelga de ella.
    env.send(SftpCmd::Chmod {
        path: root.display().to_string(),
        mode: 0o750,
        recursive: true,
    });
    let msg = env.done();
    assert!(msg.contains("(7 elementos)"), "{msg}");
    assert_eq!(perms(&root), 0o750);
    for rel in inside {
        assert_eq!(perms(&root.join(rel)), 0o750, "{rel}");
    }
    // …sin seguir los enlaces: lo de fuera no se ha tocado.
    assert_eq!(perms(&outside.join("secreto.txt")), 0o600);
    assert_eq!(perms(&outside), 0o700);

    // Sobre un enlace a una carpeta cambia su destino (como `chmod`), pero
    // no entra en él aunque se pida recursivo.
    env.send(SftpCmd::Chmod {
        path: remote("sub/enlace_dir"),
        mode: 0o711,
        recursive: true,
    });
    env.done();
    assert_eq!(perms(&outside), 0o711);
    assert_eq!(perms(&outside.join("secreto.txt")), 0o600);

    // Un fallo se notifica sin tumbar la sesión.
    env.send(SftpCmd::Chmod {
        path: remote("no-existe"),
        mode: 0o644,
        recursive: true,
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match env
            .client
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(SftpEvent::Error(e)) => {
                assert!(e.contains("permisos") && e.contains("no-existe"), "{e}");
                break;
            }
            Ok(_) => {}
            Err(_) => panic!("no llegó el error"),
        }
    }
    env.send(SftpCmd::Chmod {
        path: remote("sub/b.txt"),
        mode: 0o640,
        recursive: false,
    });
    env.done();
    assert_eq!(perms(&root.join("sub/b.txt")), 0o640);
}

#[test]
fn resume_upload_sends_only_the_rest() {
    let Some(mut env) = Env::start() else {
        return;
    };
    let big = pattern(3_000_000);
    let local = env.dir.path().join("origen.bin");
    std::fs::write(&local, &big).unwrap();
    let dest = env.folder("remoto").join("destino.bin");

    // Una subida que se quedó en el primer millón de bytes.
    std::fs::write(&dest, &big[..1_000_000]).unwrap();
    let id = env.upload(&local, &dest, OnConflict::Ask);
    let done = env
        .finish(id, |conflict| {
            assert!(conflict.can_resume());
            reply(ConflictChoice::Resume, false)
        })
        .ok();
    assert_eq!(done.asked.len(), 1);
    let asked = &done.asked[0];
    assert!(asked.upload && !asked.multiple);
    assert_eq!(asked.path, dest.display().to_string());
    assert_eq!((asked.source_size, asked.dest_size), (3_000_000, 1_000_000));
    assert_eq!(std::fs::read(&dest).unwrap(), big);
    // Solo ha viajado lo que faltaba…
    assert_eq!(
        done.stats,
        TransferStats {
            bytes: 2_000_000,
            copied: 1,
            resumed: 1,
            ..Default::default()
        }
    );
    // …aunque el progreso cuenta el fichero entero.
    assert_eq!(done.progress, Some((3_000_000, 3_000_000)));

    // Y continúa de verdad donde lo dejó el destino, sin reescribir el
    // principio: si este es otro, se conserva tal cual.
    let marked = vec![0xEE_u8; 500_000];
    std::fs::write(&dest, &marked).unwrap();
    let id = env.upload(&local, &dest, OnConflict::Ask);
    let done = env
        .finish(id, |_| reply(ConflictChoice::Resume, false))
        .ok();
    assert_eq!(done.stats.bytes, 2_500_000);
    let result = std::fs::read(&dest).unwrap();
    assert_eq!(result.len(), big.len());
    assert!(result[..500_000] == marked[..]);
    assert!(result[500_000..] == big[500_000..]);

    // Nada que reanudar si no hay destino: se sube entero y sin preguntar.
    std::fs::remove_file(&dest).unwrap();
    let id = env.upload(&local, &dest, OnConflict::Ask);
    let done = env.finish(id, |_| panic!("no hay conflicto")).ok();
    assert_eq!((done.stats.bytes, done.stats.resumed), (3_000_000, 0));
    assert_eq!(std::fs::read(&dest).unwrap(), big);
}

#[test]
fn resume_download_receives_only_the_rest() {
    let Some(mut env) = Env::start() else {
        return;
    };
    let big = pattern(3_000_000);
    let remote = env.folder("remoto").join("origen.bin");
    std::fs::write(&remote, &big).unwrap();
    let dest = env.folder("local").join("destino.bin");

    // Una descarga que se quedó a medias.
    std::fs::write(&dest, &big[..1_234_567]).unwrap();
    let id = env.download(&remote, &dest, OnConflict::Ask);
    let done = env
        .finish(id, |conflict| {
            assert!(conflict.can_resume());
            reply(ConflictChoice::Resume, false)
        })
        .ok();
    assert_eq!(done.asked.len(), 1);
    let asked = &done.asked[0];
    assert!(!asked.upload && !asked.multiple);
    assert_eq!(asked.path, dest.display().to_string());
    assert_eq!((asked.source_size, asked.dest_size), (3_000_000, 1_234_567));
    assert_eq!(std::fs::read(&dest).unwrap(), big);
    assert_eq!(
        done.stats,
        TransferStats {
            bytes: 3_000_000 - 1_234_567,
            copied: 1,
            resumed: 1,
            ..Default::default()
        }
    );
    assert_eq!(done.progress, Some((3_000_000, 3_000_000)));

    // Continúa desde donde está el destino y no toca lo anterior.
    let marked = vec![0xEE_u8; 700_000];
    std::fs::write(&dest, &marked).unwrap();
    let id = env.download(&remote, &dest, OnConflict::Ask);
    let done = env
        .finish(id, |_| reply(ConflictChoice::Resume, false))
        .ok();
    assert_eq!(done.stats.bytes, 2_300_000);
    let result = std::fs::read(&dest).unwrap();
    assert_eq!(result.len(), big.len());
    assert!(result[..700_000] == marked[..]);
    assert!(result[700_000..] == big[700_000..]);
}

#[test]
fn existing_destination_is_replaced_or_skipped() {
    let Some(mut env) = Env::start() else {
        return;
    };
    let source = env.dir.path().join("origen.txt");
    let (local, remote) = (env.folder("local"), env.folder("remoto"));
    std::fs::write(&source, "contenido nuevo").unwrap();

    for upload in [true, false] {
        let dest = if upload { &remote } else { &local }.join("destino.txt");
        let transfer = |env: &mut Env, on_conflict| {
            if upload {
                env.upload(&source, &dest, on_conflict)
            } else {
                env.download(&source, &dest, on_conflict)
            }
        };

        // Mismo tamaño: no hay nada que reanudar; se pregunta igualmente.
        std::fs::write(&dest, "contenido viejo").unwrap();
        let id = transfer(&mut env, OnConflict::Ask);
        let done = env
            .finish(id, |conflict| {
                assert!(!conflict.can_resume());
                reply(ConflictChoice::Skip, false)
            })
            .ok();
        assert_eq!(done.asked.len(), 1);
        assert_eq!(done.asked[0].upload, upload);
        assert_eq!(
            (done.asked[0].source_size, done.asked[0].dest_size),
            (15, 15)
        );
        assert_eq!(text(&dest), "contenido viejo");
        assert_eq!(
            done.stats,
            TransferStats {
                skipped: 1,
                ..Default::default()
            }
        );

        let id = transfer(&mut env, OnConflict::Ask);
        let done = env
            .finish(id, |_| reply(ConflictChoice::Replace, false))
            .ok();
        assert_eq!(done.asked.len(), 1);
        assert_eq!(text(&dest), "contenido nuevo");
        assert_eq!((done.stats.copied, done.stats.bytes), (1, 15));

        // Un destino mayor se sustituye entero, sin restos del anterior.
        std::fs::write(&dest, "contenido viejo y bastante más largo").unwrap();
        let id = transfer(&mut env, OnConflict::Ask);
        env.finish(id, |_| reply(ConflictChoice::Replace, false))
            .ok();
        assert_eq!(text(&dest), "contenido nuevo");

        // «Reanudar» sobre un destino que no es más pequeño no lo toca.
        std::fs::write(&dest, "contenido viejo y bastante más largo").unwrap();
        let id = transfer(&mut env, OnConflict::Ask);
        let done = env
            .finish(id, |_| reply(ConflictChoice::Resume, false))
            .ok();
        assert_eq!(text(&dest), "contenido viejo y bastante más largo");
        assert_eq!((done.stats.skipped, done.stats.copied), (1, 0));

        // Sin preguntas (como al guardar un fichero en edición).
        let id = transfer(&mut env, OnConflict::Replace);
        let done = env.finish(id, |_| panic!("no debía preguntar")).ok();
        assert!(done.asked.is_empty());
        assert_eq!(text(&dest), "contenido nuevo");

        // Si el destino no existe tampoco hay nada que preguntar.
        std::fs::remove_file(&dest).unwrap();
        let id = transfer(&mut env, OnConflict::Ask);
        env.finish(id, |_| panic!("no hay conflicto")).ok();
        assert_eq!(text(&dest), "contenido nuevo");
    }
}

#[test]
fn folder_transfers_can_apply_an_answer_to_all() {
    let Some(mut env) = Env::start() else {
        return;
    };
    // Los de la raíz se transfieren antes que los de las subcarpetas.
    let fill = |root: &Path| {
        write(&root.join("a.txt"), "aaaa-completo", OLD);
        write(&root.join("sub/b.txt"), "bbbb", OLD);
        write(&root.join("sub/c.txt"), "cccc", OLD);
        write(&root.join("sub/d.txt"), "dddd-completo", OLD);
        write(&root.join("sub/nuevo.txt"), "nuevo", OLD);
    };
    let partial = |root: &Path| {
        write(&root.join("a.txt"), "aaaa", OLD);
        write(&root.join("sub/b.txt"), "BBBB", OLD);
        write(&root.join("sub/c.txt"), "CCCC", OLD);
        write(&root.join("sub/d.txt"), "dddd", OLD);
    };

    for upload in [true, false] {
        let source = env.folder(&format!("origen-{upload}"));
        let dest = env.folder(&format!("destino-{upload}"));
        fill(&source);
        let transfer = |env: &mut Env| {
            if upload {
                env.upload(&source, &dest, OnConflict::Ask)
            } else {
                env.download(&source, &dest, OnConflict::Ask)
            }
        };

        // «Omitir todos»: una sola pregunta y solo llega lo que faltaba.
        partial(&dest);
        let id = transfer(&mut env);
        let done = env.finish(id, |_| reply(ConflictChoice::Skip, true)).ok();
        assert_eq!(done.asked.len(), 1, "{:?}", done.asked);
        assert!(done.asked[0].multiple);
        assert_eq!(done.asked[0].upload, upload);
        assert_eq!(text(&dest.join("a.txt")), "aaaa");
        assert_eq!(text(&dest.join("sub/b.txt")), "BBBB");
        assert_eq!(text(&dest.join("sub/d.txt")), "dddd");
        assert_eq!(text(&dest.join("sub/nuevo.txt")), "nuevo");
        assert_eq!((done.stats.skipped, done.stats.copied), (4, 1));

        // Sin «todos», se pregunta por cada uno.
        let id = transfer(&mut env);
        let done = env.finish(id, |_| reply(ConflictChoice::Skip, false)).ok();
        assert_eq!(done.asked.len(), 5);
        assert_eq!(done.stats.skipped, 5);

        // «Reanudar todos» vale para los que son más pequeños; por los
        // demás se vuelve a preguntar (una vez, si también es «todos»).
        std::fs::remove_file(dest.join("sub/nuevo.txt")).unwrap();
        let id = transfer(&mut env);
        let done = env
            .finish(id, |conflict| match conflict.can_resume() {
                true => reply(ConflictChoice::Resume, true),
                false => reply(ConflictChoice::Replace, true),
            })
            .ok();
        assert_eq!(done.asked.len(), 2, "{:?}", done.asked);
        assert!(done.asked[0].path.ends_with("a.txt"));
        assert!(done.asked[0].dest_size < done.asked[0].source_size);
        assert_eq!(done.asked[1].dest_size, done.asked[1].source_size);
        assert_eq!(files_under(&dest), files_under(&source));
        for rel in files_under(&source) {
            assert_eq!(text(&dest.join(&rel)), text(&source.join(&rel)), "{rel}");
        }
        assert_eq!(
            done.stats,
            TransferStats {
                // Lo que faltaba de «a» y de «d», más «b», «c» y «nuevo».
                bytes: 9 + 9 + 4 + 4 + 5,
                copied: 5,
                resumed: 2,
                ..Default::default()
            }
        );
    }
}

#[test]
fn unanswered_conflict_never_blocks() {
    let Some(mut env) = Env::start() else {
        return;
    };
    let local = env.dir.path().join("origen.txt");
    std::fs::write(&local, "contenido nuevo").unwrap();
    let dest = env.folder("remoto").join("destino.txt");
    std::fs::write(&dest, "viejo").unwrap();

    // La pregunta se descarta sin contestar: la transferencia se cancela.
    let id = env.upload(&local, &dest, OnConflict::Ask);
    let done = env.finish(id, |_| None);
    assert_eq!(done.error.as_deref(), Some("Cancelado"));
    assert_eq!(text(&dest), "viejo");

    // Se cancela mientras espera la respuesta (que sigue pendiente).
    let id = env.upload(&local, &dest, OnConflict::Ask);
    let pending = wait(&env.client, |ev| match ev {
        SftpEvent::Conflict(conflict) if conflict.id == id => Some(conflict),
        _ => None,
    });
    env.send(SftpCmd::Cancel(id));
    let done = env.finish(id, |_| panic!("ya había preguntado"));
    assert_eq!(done.error.as_deref(), Some("Cancelado"));
    assert_eq!(text(&dest), "viejo");
    drop(pending);

    // Se cierra la vista con la pregunta en el aire: no queda ninguna tarea
    // esperando para siempre. Con un runtime aparte para poder contarlas.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let client = connect(&rt, &env.endpoint);
    client
        .cmd
        .send(SftpCmd::Upload {
            id: 1,
            local: local.clone(),
            remote: dest.display().to_string(),
            on_conflict: OnConflict::Ask,
        })
        .unwrap();
    let pending = wait(&client, |ev| match ev {
        SftpEvent::Conflict(conflict) => Some(conflict),
        _ => None,
    });
    assert!(rt.metrics().num_alive_tasks() > 0);
    drop(client);
    let deadline = Instant::now() + Duration::from_secs(15);
    while rt.metrics().num_alive_tasks() > 0 {
        assert!(
            Instant::now() < deadline,
            "quedan {} tareas vivas tras cerrar la vista",
            rt.metrics().num_alive_tasks()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // Ni siquiera con la respuesta aún sin soltar; y contestar tarde no
    // hace nada.
    assert!(
        pending
            .reply
            .send(ConflictAnswer {
                choice: ConflictChoice::Replace,
                all: false,
            })
            .is_err()
    );
    assert_eq!(text(&dest), "viejo");
}

/// Dos carpetas para sincronizar del `origen` al `destino`, con un caso de
/// cada tipo; y, fuera de ellas, lo que solo se alcanza por enlaces.
fn one_way_fixture(source: &Path, dest: &Path, outside: &Path) {
    write(&source.join("nuevo.txt"), "nuevo", NEW);
    write(&source.join("sub/hondo/datos.bin"), pattern(300_000), OLD);
    write(&source.join("igual.txt"), "igual", OLD);
    write(
        &source.join("cambiado.txt"),
        "versión nueva del origen",
        NEW,
    );
    write(&source.join("viejo-en-origen.txt"), "origen viejo", OLD);
    std::fs::create_dir(source.join("vacia")).unwrap();
    symlink(outside, source.join("enlace-origen")).unwrap();

    // Dentro de la tolerancia de 2 s: cuenta como la misma fecha.
    write(&dest.join("igual.txt"), "igual", OLD + 2);
    write(&dest.join("cambiado.txt"), "versión vieja", OLD);
    chmod(&dest.join("cambiado.txt"), 0o600);
    write(&dest.join("viejo-en-origen.txt"), "destino más nuevo", NEW);
    write(&dest.join("sobra.txt"), "sobra", OLD);
    write(&dest.join("restos/viejo.txt"), "restos", OLD);
    write(&dest.join("restos/mas/otro.txt"), "más restos", OLD);
    symlink(outside, dest.join("enlace-destino")).unwrap();
    // Lo que dejó a medias una sincronización anterior.
    write(
        &dest.join(format!("corte.bin{PART_SUFFIX}")),
        "a medias",
        NEW,
    );

    write(&outside.join("secreto.txt"), "no tocar", OLD);
}

/// Sincronización en un sentido, sin borrar y borrando.
fn sync_one_way(mode: SyncMode) {
    let Some(mut env) = Env::start() else {
        return;
    };
    let (local, remote) = (env.folder("local"), env.folder("remoto"));
    let outside = env.folder("fuera");
    let upload = mode == SyncMode::Upload;
    let (source, dest) = if upload {
        (&local, &remote)
    } else {
        (&remote, &local)
    };
    one_way_fixture(source, dest, &outside);
    let sides = |scan: &SyncScan| match upload {
        true => (scan.local.clone(), scan.remote.clone()),
        false => (scan.remote.clone(), scan.local.clone()),
    };
    let ignored =
        |names: &[&str]| -> BTreeSet<String> { names.iter().map(|n| n.to_string()).collect() };

    // --- Comparación: no toca nada y no sigue los enlaces -----------------
    let scan = env.scan(&local, &remote);
    let (from, to) = sides(&scan);
    assert_eq!(
        from.files.keys().collect::<Vec<_>>(),
        [
            "cambiado.txt",
            "igual.txt",
            "nuevo.txt",
            "sub/hondo/datos.bin",
            "viejo-en-origen.txt"
        ]
    );
    let dirs: Vec<_> = from.dirs.iter().collect();
    assert_eq!(dirs, ["sub", "sub/hondo", "vacia"]);
    assert_eq!(from.ignored, ignored(&["enlace-origen"]));
    let nuevo = SyncFile {
        size: 5,
        mtime: Some(NEW as i64),
    };
    assert_eq!(from.files["nuevo.txt"], nuevo);
    assert_eq!(
        to.files.keys().collect::<Vec<_>>(),
        [
            "cambiado.txt",
            "igual.txt",
            "restos/mas/otro.txt",
            "restos/viejo.txt",
            "sobra.txt",
            "viejo-en-origen.txt"
        ]
    );
    let part = format!("corte.bin{PART_SUFFIX}");
    assert_eq!(to.ignored, ignored(&["enlace-destino", &part]));
    assert_eq!(to.files["sobra.txt"].mtime, Some(OLD as i64));

    // --- Plan sin borrado -------------------------------------------------
    let plan = plan_sync(&scan, mode, false);
    let summary = SyncSummary::of(&plan);
    let copied_bytes = 5 + 300_000 + "versión nueva del origen".len() as u64;
    let (copies, bytes) = match upload {
        true => (summary.upload, summary.upload_bytes),
        false => (summary.download, summary.download_bytes),
    };
    assert_eq!((copies, bytes), (3, copied_bytes));
    assert_eq!(summary.upload + summary.download, 3);
    assert_eq!(summary.make_dirs, 3);
    assert_eq!((summary.delete_files, summary.delete_dirs), (0, 0));
    assert_eq!((summary.equal, summary.untouched), (1, 1));
    assert_eq!(summary.only_local + summary.only_remote, 2 + 3);
    // Los dos enlaces y el resto a medio copiar.
    assert_eq!(summary.ignored, 3);
    let before = files_under(dest);

    let done = env.sync(&local, &remote, &plan).ok();
    assert_eq!(
        done.stats,
        TransferStats {
            bytes: copied_bytes,
            copied: 3,
            ..Default::default()
        }
    );
    assert_eq!(done.progress, Some((copied_bytes, copied_bytes)));
    for rel in ["nuevo.txt", "sub/hondo/datos.bin", "cambiado.txt"] {
        let (from, to) = (source.join(rel), dest.join(rel));
        assert!(
            std::fs::read(&to).unwrap() == std::fs::read(&from).unwrap(),
            "{rel}"
        );
        // Con la fecha del origen: si no, la próxima vez se copiaría de nuevo.
        assert_eq!(mtime(&to), mtime(&from), "{rel}");
    }
    assert_eq!(mtime(&dest.join("nuevo.txt")), NEW);
    assert_eq!(mtime(&dest.join("sub/hondo/datos.bin")), OLD);
    // Al sustituir un fichero se conservan los permisos que tenía.
    assert_eq!(perms(&dest.join("cambiado.txt")), 0o600);
    assert!(dest.join("vacia").is_dir());
    // Lo más reciente del destino y lo que sobra siguen como estaban.
    assert_eq!(text(&dest.join("viejo-en-origen.txt")), "destino más nuevo");
    assert_eq!(text(&dest.join("sobra.txt")), "sobra");
    assert_eq!(text(&dest.join("restos/mas/otro.txt")), "más restos");
    // No queda nada a medio copiar (salvo lo que ya había) y el origen no
    // ha cambiado.
    let mut expected = before.clone();
    expected.extend(["nuevo.txt", "sub/hondo/datos.bin"].map(String::from));
    expected.sort();
    assert_eq!(files_under(dest), expected);
    assert_eq!(text(&dest.join(&part)), "a medias");
    assert_eq!(files_under(source).len(), 6);

    // --- Una segunda sincronización no encuentra nada que hacer ------------
    let scan = env.scan(&local, &remote);
    let plan = plan_sync(&scan, mode, false);
    let summary = SyncSummary::of(&plan);
    assert!(summary.nothing_to_do(), "{plan:#?}");
    assert_eq!((summary.equal, summary.untouched), (4, 1));

    // --- Con borrado: desaparece lo que sobra en el destino, y solo eso ----
    let plan = plan_sync(&scan, mode, true);
    let summary = SyncSummary::of(&plan);
    assert_eq!((summary.delete_files, summary.delete_dirs), (3, 2));
    assert_eq!(summary.upload + summary.download + summary.make_dirs, 0);
    let delete = if upload {
        SyncOp::DeleteRemote
    } else {
        SyncOp::DeleteLocal
    };
    let deleted: Vec<&str> = plan
        .iter()
        .filter(|item| item.op != SyncOp::None)
        .inspect(|item| assert_eq!(item.op, delete))
        .map(|item| item.path.as_str())
        .collect();
    assert_eq!(
        deleted,
        [
            "restos",
            "restos/mas",
            "restos/mas/otro.txt",
            "restos/viejo.txt",
            "sobra.txt"
        ]
    );
    let done = env.sync(&local, &remote, &plan).ok();
    assert_eq!(
        done.stats,
        TransferStats {
            deleted: 5,
            ..Default::default()
        }
    );
    assert!(!dest.join("sobra.txt").exists());
    assert!(!dest.join("restos").exists());
    // Los enlaces siguen ahí y lo de fuera está intacto.
    assert!(dest.join("enlace-destino").is_symlink());
    assert!(source.join("enlace-origen").is_symlink());
    assert_eq!(text(&outside.join("secreto.txt")), "no tocar");
    assert_eq!(files_under(&outside), ["secreto.txt"]);
    // El origen sigue completo.
    assert_eq!(files_under(source).len(), 6);
    assert_eq!(text(&source.join("viejo-en-origen.txt")), "origen viejo");

    let scan = env.scan(&local, &remote);
    assert!(SyncSummary::of(&plan_sync(&scan, mode, true)).nothing_to_do());
}

#[test]
fn sync_local_to_remote() {
    sync_one_way(SyncMode::Upload);
}

#[test]
fn sync_remote_to_local() {
    sync_one_way(SyncMode::Download);
}

#[test]
fn sync_both_ways() {
    let Some(mut env) = Env::start() else {
        return;
    };
    let (local, remote) = (env.folder("local"), env.folder("remoto"));
    write(&local.join("solo-local.txt"), "solo en local", OLD);
    write(&local.join("sub/l.txt"), "local hondo", NEW);
    write(&local.join("gana-local.txt"), "local más nuevo", NEW);
    write(&local.join("gana-remoto.txt"), "local viejo", OLD);
    write(&local.join("igual.txt"), "igual", OLD);
    write(&local.join("empate.txt"), "corto", OLD);
    std::fs::create_dir(local.join("vacia-local")).unwrap();

    write(&remote.join("solo-remoto.txt"), "solo en remoto", NEW);
    write(&remote.join("otra/r.txt"), "remoto hondo", OLD);
    write(&remote.join("gana-local.txt"), "remoto viejo", OLD);
    write(&remote.join("gana-remoto.txt"), "remoto más nuevo", NEW);
    write(&remote.join("igual.txt"), "igual", OLD);
    write(&remote.join("empate.txt"), "bastante más largo", OLD + 1);
    std::fs::create_dir(remote.join("vacia-remota")).unwrap();

    let scan = env.scan(&local, &remote);
    let plan = plan_sync(&scan, SyncMode::Both, false);
    // En los dos sentidos no se borra nada, aunque se pida.
    assert_eq!(plan_sync(&scan, SyncMode::Both, true), plan);
    let summary = SyncSummary::of(&plan);
    assert_eq!((summary.upload, summary.download), (3, 3));
    assert_eq!(summary.make_dirs, 4);
    assert_eq!((summary.equal, summary.untouched), (1, 1));
    assert_eq!((summary.delete_files, summary.delete_dirs), (0, 0));
    let empate = plan.iter().find(|i| i.path == "empate.txt").unwrap();
    assert_eq!((empate.state, empate.op), (SyncState::Differ, SyncOp::None));

    let done = env.sync(&local, &remote, &plan).ok();
    assert_eq!((done.stats.copied, done.stats.deleted), (6, 0));
    // Cada fichero ha ido hacia donde faltaba o era más antiguo…
    for (rel, content, date) in [
        ("solo-local.txt", "solo en local", OLD),
        ("sub/l.txt", "local hondo", NEW),
        ("solo-remoto.txt", "solo en remoto", NEW),
        ("otra/r.txt", "remoto hondo", OLD),
        ("gana-local.txt", "local más nuevo", NEW),
        ("gana-remoto.txt", "remoto más nuevo", NEW),
        ("igual.txt", "igual", OLD),
    ] {
        for side in [&local, &remote] {
            assert_eq!(text(&side.join(rel)), content, "{rel}");
            assert_eq!(mtime(&side.join(rel)), date, "{rel}");
        }
    }
    assert!(remote.join("vacia-local").is_dir());
    assert!(local.join("vacia-remota").is_dir());
    // …y el que no se sabe cuál es el bueno no se ha tocado.
    assert_eq!(text(&local.join("empate.txt")), "corto");
    assert_eq!(text(&remote.join("empate.txt")), "bastante más largo");
    assert_eq!(files_under(&local), files_under(&remote));
    assert_eq!(files_under(&local).len(), 8);

    // La segunda vez no hay nada que hacer.
    let scan = env.scan(&local, &remote);
    let summary = SyncSummary::of(&plan_sync(&scan, SyncMode::Both, false));
    assert!(summary.nothing_to_do());
    assert_eq!((summary.equal, summary.untouched), (7, 1));
}

#[test]
fn sync_never_leaves_the_two_folders() {
    let Some(mut env) = Env::start() else {
        return;
    };
    let (local, remote) = (env.folder("local"), env.folder("remoto"));
    let outside = env.folder("fuera");
    write(&outside.join("secreto.txt"), "no tocar", OLD);
    write(&env.dir.path().join("vecino.txt"), "no tocar", OLD);
    write(&local.join("valido.txt"), "válido", OLD);
    write(&remote.join("otro.txt"), "otro", OLD);
    let item = |path: &str, is_dir: bool, op: SyncOp| SyncItem {
        path: path.to_string(),
        is_dir,
        state: SyncState::OnlyLocal,
        op,
        size: 0,
    };
    let intact = |env: &Env| {
        assert_eq!(files_under(&outside), ["secreto.txt"]);
        assert_eq!(text(&outside.join("secreto.txt")), "no tocar");
        assert_eq!(text(&env.dir.path().join("vecino.txt")), "no tocar");
    };

    // Un plan con rutas que se salen se rechaza entero, antes de tocar nada.
    for path in [
        "../vecino.txt",
        "sub/../../vecino.txt",
        "/etc/passwd",
        "",
        "a//b",
        ".",
    ] {
        for op in [
            SyncOp::DeleteLocal,
            SyncOp::DeleteRemote,
            SyncOp::Upload,
            SyncOp::Download,
        ] {
            let plan = [
                item("valido.txt", false, SyncOp::Upload),
                item(path, false, op),
            ];
            let done = env.sync(&local, &remote, &plan);
            let error = done.error.expect("debe fallar");
            assert!(error.contains("Ruta no válida"), "{path:?}: {error}");
            assert!(!remote.join("valido.txt").exists());
            intact(&env);
        }
    }

    // Un enlace simbólico a otra carpeta no se atraviesa, ni para copiar ni
    // para borrar, en ninguno de los dos lados.
    symlink(&outside, local.join("enlace")).unwrap();
    symlink(&outside, remote.join("enlace")).unwrap();
    write(&local.join("subir/enlace/nuevo.txt"), "nuevo", OLD);
    symlink(&outside, remote.join("subir")).unwrap();
    write(&remote.join("bajar/nuevo.txt"), "nuevo", OLD);
    symlink(&outside, local.join("bajar")).unwrap();
    for plan in [
        [item("enlace/secreto.txt", false, SyncOp::DeleteLocal)],
        [item("enlace/secreto.txt", false, SyncOp::DeleteRemote)],
        [item("subir/enlace/nuevo.txt", false, SyncOp::Upload)],
        [item("bajar/nuevo.txt", false, SyncOp::Download)],
        [item("enlace/creada", true, SyncOp::Upload)],
        [item("enlace/creada", true, SyncOp::Download)],
        // Borrar el propio enlace como si fuera una carpeta tampoco entra.
        [item("enlace", true, SyncOp::DeleteLocal)],
        [item("enlace", true, SyncOp::DeleteRemote)],
    ] {
        let done = env.sync(&local, &remote, &plan);
        assert!(done.error.is_some(), "{plan:?}");
        intact(&env);
    }
    assert!(local.join("enlace").is_symlink() && remote.join("enlace").is_symlink());

    // Y el plan que sale de comparar esas carpetas ni lo intenta.
    for mode in [SyncMode::Upload, SyncMode::Download, SyncMode::Both] {
        let scan = env.scan(&local, &remote);
        let plan = plan_sync(&scan, mode, true);
        for item in &plan {
            let through_link = ["enlace", "subir", "bajar"]
                .iter()
                .any(|link| item.path == *link || item.path.starts_with(&format!("{link}/")));
            assert!(!through_link || item.op == SyncOp::None, "{item:?}");
        }
        env.sync(&local, &remote, &plan).ok();
        intact(&env);
    }
}

#[test]
fn cancelled_sync_leaves_no_partial_files() {
    let Some(mut env) = Env::start() else {
        return;
    };
    let (local, remote) = (env.folder("local"), env.folder("remoto"));
    let big = pattern(24_000_000);
    write(&local.join("grande.bin"), &big, OLD);
    write(&remote.join("grande.bin"), "versión anterior", OLD - 100);
    let scan = env.scan(&local, &remote);
    let plan = plan_sync(&scan, SyncMode::Upload, false);
    assert_eq!(SyncSummary::of(&plan).upload, 1);

    // Se cancela nada más empezar.
    let id = env.id();
    env.send(SftpCmd::Sync {
        id,
        local: local.clone(),
        remote: remote.display().to_string(),
        items: actions(&plan),
    });
    env.send(SftpCmd::Cancel(id));
    let done = env.finish(id, |_| panic!("una sincronización no pregunta"));
    assert!(done.error.is_some_and(|e| e.contains("Cancelado")));
    // El destino sigue con su versión anterior, entera, y no queda rastro
    // de la copia a medias.
    assert_eq!(files_under(&remote), ["grande.bin"]);
    assert_eq!(text(&remote.join("grande.bin")), "versión anterior");

    // Al repetirla, termina.
    let done = env.sync(&local, &remote, &plan).ok();
    assert_eq!(done.stats.bytes, big.len() as u64);
    assert!(std::fs::read(remote.join("grande.bin")).unwrap() == big);
    assert_eq!(mtime(&remote.join("grande.bin")), OLD);
    assert_eq!(files_under(&remote), ["grande.bin"]);
}

/// Un fichero soltado sobre la ventana, como lo entrega el sistema.
#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// Textos pintados en un fotograma.
fn painted(output: &egui::FullOutput) -> Vec<String> {
    fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(text) => out.push(text.galley.text().to_string()),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, out)),
            _ => {}
        }
    }
    let mut out = vec![];
    for clipped in &output.shapes {
        collect(&clipped.shape, &mut out);
    }
    out
}

/// La vista de verdad, sin ventana: ficheros que se arrastran y se sueltan
/// sobre ella acaban en la carpeta remota que se está viendo.
#[test]
fn files_dropped_on_the_view_are_uploaded() {
    let Some(env) = Env::start() else {
        return;
    };
    let remote = env.folder("remoto");
    let stuff = env.folder("cosas");
    write(&stuff.join("informe.txt"), "informe", OLD);
    write(&stuff.join("carpeta/dentro.txt"), "dentro", OLD);
    write(&stuff.join("carpeta/sub/hondo.bin"), pattern(200_000), OLD);

    let client = sftp::spawn(
        env.rt.handle(),
        SftpSource::Connect(Box::new(env.endpoint.clone())),
        prompter(answers(true, &[])),
    );
    let mut view = SftpView::new(client);
    let ctx = egui::Context::default();
    let frame = |view: &mut SftpView, hovered: &[&Path], dropped: &[&Path]| {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1100.0, 700.0),
            )),
            hovered_files: hovered
                .iter()
                .map(|path| egui::HoveredFile {
                    path: Some(path.to_path_buf()),
                    ..Default::default()
                })
                .collect(),
            dropped_files: dropped
                .iter()
                .map(|path| Arc::new(Dropped(path.to_path_buf())) as egui::DroppedFileHandle)
                .collect(),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| view.ui(ui));
        // No hay nadie que pinte: las texturas se descartan a propósito.
        output.textures_delta.clear();
        std::thread::sleep(Duration::from_millis(20));
        painted(&output)
    };
    let informe = stuff.join("informe.txt");
    let carpeta = stuff.join("carpeta");

    assert!(matches!(view.state, SftpState::Connecting(_)));
    let deadline = Instant::now() + Duration::from_secs(30);
    while view.state != SftpState::Ready {
        assert!(Instant::now() < deadline, "{:?}", view.state);
        frame(&mut view, &[], &[]);
    }

    // Se lleva la vista a la carpeta de pruebas (como al seguir a un
    // terminal) y, al arrastrar algo por encima, dice adónde iría.
    view.linked = true;
    view.follow_dir(&remote.display().to_string());
    let hint = format!("Suelta para subir a {}", remote.display());
    loop {
        assert!(Instant::now() < deadline, "la vista no cambió de carpeta");
        let texts = frame(&mut view, &[&informe], &[]);
        if texts.iter().any(|t| t.contains(&hint)) {
            break;
        }
    }
    // Sin nada encima, la indicación desaparece.
    let texts = frame(&mut view, &[], &[]);
    assert!(!texts.iter().any(|t| t.contains("Suelta para subir")));
    assert_eq!(files_under(&remote), [""; 0]);

    // Al soltar un fichero y una carpeta, se suben los dos.
    frame(&mut view, &[], &[&informe, &carpeta]);
    let expected = ["carpeta/dentro.txt", "carpeta/sub/hondo.bin", "informe.txt"];
    loop {
        assert!(Instant::now() < deadline, "{:?}", files_under(&remote));
        let texts = frame(&mut view, &[], &[]);
        let finished = texts.iter().filter(|t| t.starts_with("Completada")).count();
        if finished == 2 {
            break;
        }
    }
    assert_eq!(files_under(&remote), expected);
    assert_eq!(text(&remote.join("informe.txt")), "informe");
    assert_eq!(text(&remote.join("carpeta/dentro.txt")), "dentro");
    assert!(std::fs::read(remote.join("carpeta/sub/hondo.bin")).unwrap() == pattern(200_000));
}
