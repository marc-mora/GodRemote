//! Prueba de extremo a extremo contra un `sshd` real de OpenSSH: clave con
//! passphrase, verificación de host, shell interactiva y SFTP.
//!
//! Se omite si no hay `sshd` instalado o no se ejecuta como root (sshd
//! necesita root para abrir sesiones).
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{answers, prompter};
use godremote::keys::{self, KeyAlgorithm};
use godremote::model::{AuthMethod, Session};
use godremote::sftp::{self, SftpCmd, SftpEvent, SftpSource};
use godremote::ssh::{self, Credentials};
use godremote::terminal::{TermCmd, TermEvent};

struct Sshd(Child);
impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn find_sshd() -> Option<PathBuf> {
    ["/usr/sbin/sshd", "/usr/bin/sshd", "/sbin/sshd"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start_sshd(sshd: &Path, dir: &Path, port: u16, authorized: &Path) -> Sshd {
    keys::generate(dir, "host_key", KeyAlgorithm::Ed25519, "", None).unwrap();
    let sftp_server = [
        "/usr/lib/openssh/sftp-server",
        "/usr/libexec/openssh/sftp-server",
        "/usr/libexec/sftp-server",
    ]
    .iter()
    .find(|p| Path::new(p).exists())
    .copied()
    .unwrap_or("internal-sftp");
    let config = format!(
        "Port {port}\nListenAddress 127.0.0.1\nHostKey {host}\nPidFile {pid}\n\
         AuthorizedKeysFile {auth}\nStrictModes no\nPermitRootLogin prohibit-password\n\
         PubkeyAuthentication yes\nPasswordAuthentication no\n\
         KbdInteractiveAuthentication no\nUsePAM no\nSubsystem sftp {sftp_server}\n",
        host = dir.join("host_key").display(),
        pid = dir.join("sshd.pid").display(),
        auth = authorized.display(),
    );
    let cfg = dir.join("sshd_config");
    std::fs::write(&cfg, config).unwrap();
    std::fs::create_dir_all("/run/sshd").ok();
    let child = Command::new(sshd)
        .args(["-D", "-e", "-f"])
        .arg(&cfg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let guard = Sshd(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "sshd no arrancó");
        std::thread::sleep(Duration::from_millis(50));
    }
    guard
}

fn wait_sftp<T>(client: &sftp::SftpClient, mut pick: impl FnMut(SftpEvent) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match client.events.recv_timeout(remaining) {
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

#[test]
fn ssh_shell_and_sftp_end_to_end() {
    let Some(sshd) = find_sshd() else {
        eprintln!("OMITIDA: no hay sshd instalado");
        return;
    };
    let id = |flag: &str| {
        Command::new("id")
            .arg(flag)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    if id("-u") != "0" {
        eprintln!("OMITIDA: se necesita root para lanzar un sshd de prueba");
        return;
    }
    let user = id("-un");

    let dir = tempfile::tempdir().unwrap();
    let known_hosts = dir.path().join("known_hosts");
    // SAFETY: único test de este binario; aún no hay otros hilos.
    unsafe { std::env::set_var("GODREMOTE_KNOWN_HOSTS", &known_hosts) };

    let key = keys::generate(
        dir.path(),
        "id_cliente",
        KeyAlgorithm::Ed25519,
        "test",
        Some("frase secreta"),
    )
    .unwrap();
    let authorized = dir.path().join("authorized_keys");
    std::fs::write(&authorized, format!("{}\n", key.public_openssh)).unwrap();

    let port = free_port();
    let _sshd = start_sshd(&sshd, dir.path(), port, &authorized);

    let rt = tokio::runtime::Runtime::new().unwrap();
    let session = Session {
        name: "prueba".into(),
        host: "127.0.0.1".into(),
        port,
        username: user,
        auth: AuthMethod::Key,
        key_path: key.path.display().to_string(),
        ..Default::default()
    };
    let quiet = |_: String| {};

    // 1. Host desconocido rechazado por el usuario -> no conecta ni se guarda.
    let a = answers(false, &[]);
    let err = rt
        .block_on(ssh::connect(
            &session,
            Credentials::default(),
            &prompter(a.clone()),
            &quiet,
        ))
        .err()
        .expect("debe fallar");
    assert!(format!("{err:#}").contains("rechazada"), "{err:#}");
    assert_eq!(a.hosts(), 1);
    assert!(!known_hosts.exists());

    // 2. Passphrase incorrecta y luego correcta; host aceptado y recordado.
    let a = answers(true, &["mala", "frase secreta"]);
    let connected = rt
        .block_on(ssh::connect(
            &session,
            Credentials::default(),
            &prompter(a.clone()),
            &quiet,
        ))
        .expect("conexión con clave");
    assert_eq!(a.hosts(), 1);
    assert_eq!(a.secrets_asked(), 2);
    assert_eq!(
        connected.to_save,
        vec![(session.passphrase_key(), "frase secreta".to_string())]
    );
    assert!(
        std::fs::read_to_string(&known_hosts)
            .unwrap()
            .contains("ssh-ed25519")
    );
    let shared = connected.handle;

    // 3. Shell: host ya conocido (sin pregunta) y passphrase del baúl.
    let a = answers(false, &[]);
    let creds = Credentials {
        password: None,
        passphrase: Some("frase secreta".into()),
    };
    let backend = ssh::spawn_shell(
        rt.handle(),
        session.clone(),
        creds.clone(),
        prompter(a.clone()),
        80,
        24,
    );
    let mut output = String::new();
    let mut connected_seen = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    // Se espera a ver el eco *y* el tamaño aplicado al pty remoto.
    while !(output.contains("godremote-42") && output.contains("40 120")) {
        let ev = backend
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|_| panic!("sin respuesta de la shell: {output}"));
        match ev {
            TermEvent::Connected(handle) => {
                assert!(handle.is_some());
                connected_seen = true;
                backend
                    .cmd
                    .send(TermCmd::Resize {
                        cols: 120,
                        rows: 40,
                    })
                    .unwrap();
                backend
                    .cmd
                    .send(TermCmd::Input(
                        b"echo godremote-$((40+2)); stty size\n".to_vec(),
                    ))
                    .unwrap();
            }
            TermEvent::Data(d) => output.push_str(&String::from_utf8_lossy(&d)),
            TermEvent::Closed(e) => panic!("shell cerrada: {e:?} {output}"),
            _ => {}
        }
    }
    assert!(connected_seen);
    assert_eq!(a.hosts(), 0);
    assert_eq!(a.secrets_asked(), 0);
    // `exit` cierra la sesión limpiamente.
    backend
        .cmd
        .send(TermCmd::Input(b"exit\n".to_vec()))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match backend
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(TermEvent::Closed(err)) => {
                assert!(err.is_none(), "{err:?}");
                break;
            }
            Ok(_) => {}
            Err(_) => panic!("la shell no se cerró"),
        }
    }

    // 4. SFTP reutilizando la conexión ya autenticada.
    let work = dir.path().join("remoto");
    std::fs::create_dir(&work).unwrap();
    let work_remote = work.display().to_string();
    let client = sftp::spawn(
        rt.handle(),
        SftpSource::Existing(shared),
        prompter(answers(false, &[])),
    );
    let home = wait_sftp(&client, |ev| match ev {
        SftpEvent::Ready { home } => Some(home),
        _ => None,
    });
    assert!(home.starts_with('/'));

    // Subida de un árbol de directorios con un fichero grande.
    let local_src = dir.path().join("origen");
    std::fs::create_dir_all(local_src.join("sub/vacio")).unwrap();
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(local_src.join("grande.bin"), &big).unwrap();
    std::fs::write(local_src.join("sub/nota.txt"), "hola ñ").unwrap();
    client
        .cmd
        .send(SftpCmd::Upload {
            id: 1,
            local: local_src.clone(),
            remote: format!("{work_remote}/subido"),
        })
        .unwrap();
    let mut saw_progress = false;
    wait_sftp(&client, |ev| match ev {
        SftpEvent::Progress {
            id: 1, done, total, ..
        } => {
            assert!(done <= total);
            saw_progress = true;
            None
        }
        SftpEvent::TransferFinished { id: 1, error } => {
            assert!(error.is_none(), "{error:?}");
            Some(())
        }
        _ => None,
    });
    assert!(saw_progress);
    assert_eq!(std::fs::read(work.join("subido/grande.bin")).unwrap(), big);
    assert_eq!(
        std::fs::read_to_string(work.join("subido/sub/nota.txt")).unwrap(),
        "hola ñ"
    );
    assert!(work.join("subido/sub/vacio").is_dir());

    // Listado: carpetas primero, con tamaños.
    client
        .cmd
        .send(SftpCmd::List(format!("{work_remote}/subido")))
        .unwrap();
    let entries = wait_sftp(&client, |ev| match ev {
        SftpEvent::Listing { entries, .. } => Some(entries),
        _ => None,
    });
    assert_eq!(entries.len(), 2);
    assert!(entries[0].is_dir && entries[0].name == "sub");
    assert_eq!(entries[1].size, big.len() as u64);

    // mkdir + rename.
    client
        .cmd
        .send(SftpCmd::Mkdir(format!("{work_remote}/nueva")))
        .unwrap();
    wait_sftp(&client, |ev| matches!(ev, SftpEvent::Done(_)).then_some(()));
    client
        .cmd
        .send(SftpCmd::Rename {
            from: format!("{work_remote}/nueva"),
            to: format!("{work_remote}/renombrada"),
        })
        .unwrap();
    wait_sftp(&client, |ev| matches!(ev, SftpEvent::Done(_)).then_some(()));
    assert!(work.join("renombrada").is_dir());

    // Descarga recursiva.
    let local_dst = dir.path().join("descarga");
    client
        .cmd
        .send(SftpCmd::Download {
            id: 2,
            remote: format!("{work_remote}/subido"),
            local: local_dst.clone(),
        })
        .unwrap();
    wait_sftp(&client, |ev| match ev {
        SftpEvent::TransferFinished { id: 2, error } => {
            assert!(error.is_none(), "{error:?}");
            Some(())
        }
        _ => None,
    });
    assert_eq!(std::fs::read(local_dst.join("grande.bin")).unwrap(), big);
    assert_eq!(
        std::fs::read_to_string(local_dst.join("sub/nota.txt")).unwrap(),
        "hola ñ"
    );

    // Borrado recursivo: no sigue enlaces simbólicos fuera del árbol.
    let outside = dir.path().join("fuera.txt");
    std::fs::write(&outside, "intacto").unwrap();
    std::os::unix::fs::symlink(&outside, work.join("subido/enlace")).unwrap();
    std::os::unix::fs::symlink(&local_dst, work.join("subido/enlace_dir")).unwrap();
    client
        .cmd
        .send(SftpCmd::Remove {
            path: format!("{work_remote}/subido"),
            is_dir: true,
        })
        .unwrap();
    wait_sftp(&client, |ev| matches!(ev, SftpEvent::Done(_)).then_some(()));
    assert!(!work.join("subido").exists());
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "intacto");
    assert!(local_dst.join("grande.bin").exists());

    // Un error (listar algo inexistente) se notifica sin tumbar la sesión.
    client
        .cmd
        .send(SftpCmd::List(format!("{work_remote}/no-existe")))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match client
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(SftpEvent::Error(e)) => {
                assert!(e.contains("no-existe"));
                break;
            }
            Ok(_) => {}
            Err(_) => panic!("no llegó el error"),
        }
    }
    client.cmd.send(SftpCmd::List(work_remote.clone())).unwrap();
    wait_sftp(&client, |ev| {
        matches!(ev, SftpEvent::Listing { .. }).then_some(())
    });
    drop(client);

    // 5. SFTP con conexión propia.
    let client = sftp::spawn(
        rt.handle(),
        SftpSource::Connect {
            session: Box::new(session.clone()),
            creds: creds.clone(),
        },
        prompter(answers(false, &[])),
    );
    wait_sftp(&client, |ev| {
        matches!(ev, SftpEvent::Ready { .. }).then_some(())
    });
    drop(client);

    // 6. Clave no autorizada -> rechazo claro.
    let other = keys::generate(dir.path(), "id_otra", KeyAlgorithm::Ed25519, "", None).unwrap();
    let mut bad = session.clone();
    bad.key_path = other.path.display().to_string();
    let err = rt
        .block_on(ssh::connect(
            &bad,
            Credentials::default(),
            &prompter(answers(true, &[])),
            &quiet,
        ))
        .err()
        .expect("debe fallar");
    assert!(format!("{err:#}").contains("rechazó la clave"), "{err:#}");

    // 7. La clave del host cambia -> se rechaza la conexión sin preguntar.
    let impostor = keys::generate(dir.path(), "impostor", KeyAlgorithm::Ed25519, "", None).unwrap();
    let pub_parts: Vec<&str> = impostor.public_openssh.split_whitespace().collect();
    std::fs::write(
        &known_hosts,
        format!("[127.0.0.1]:{port} {} {}\n", pub_parts[0], pub_parts[1]),
    )
    .unwrap();
    let a = answers(true, &[]);
    let err = rt
        .block_on(ssh::connect(&session, creds, &prompter(a.clone()), &quiet))
        .err()
        .expect("debe fallar");
    assert!(format!("{err:#}").contains("HA CAMBIADO"), "{err:#}");
    assert_eq!(a.hosts(), 0);
}
