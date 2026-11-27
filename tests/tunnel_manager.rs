//! Gestor de túneles contra un `sshd` real de OpenSSH: cada túnel abre su
//! propia conexión, sin terminal, y se puede parar y volver a iniciar.
//!
//! Se omite si falta `sshd` o no se ejecuta como root.
#![cfg(unix)]

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::time::{Duration, Instant};

use common::{answers, find_program, free_port, is_root, prompter, start_sshd};
use godremote::keys::{self, KeyAlgorithm};
use godremote::model::{AuthMethod, Session, Tunnel, TunnelKind};
use godremote::ssh::{Credentials, Endpoint};
use godremote::tunnel_manager::{self, RunState, RunningTunnel};

/// Servidor de eco: responde `eco:` + lo recibido.
fn echo_server() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut buf = [0u8; 256];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut reply = b"eco:".to_vec();
                    reply.extend_from_slice(&buf[..n]);
                    if stream.write_all(&reply).is_err() {
                        break;
                    }
                }
            });
        }
    });
    port
}

fn roundtrip(mut stream: TcpStream, message: &[u8]) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(message).unwrap();
    let mut buf = vec![0u8; 4 + message.len()];
    stream.read_exact(&mut buf).expect("respuesta del eco");
    buf
}

/// Espera a que el túnel deje de conectar y devuelve cómo ha quedado.
fn settle(running: &mut RunningTunnel) -> RunState {
    wait_until(running, |state| !matches!(state, RunState::Connecting(_)))
}

fn wait_until(running: &mut RunningTunnel, done: impl Fn(&RunState) -> bool) -> RunState {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        running.poll();
        if done(&running.state) {
            return running.state.clone();
        }
        assert!(
            Instant::now() < deadline,
            "el túnel se quedó en {:?}",
            running.state
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Espera a que nadie escuche ya en el puerto.
fn wait_closed(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_ok() {
        assert!(Instant::now() < deadline, "el puerto {port} sigue abierto");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn standalone_tunnels_against_real_sshd() {
    if find_program("sshd").is_none() || !is_root() {
        eprintln!("OMITIDA: hace falta sshd y ejecutar como root");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: único test de este binario, antes de crear hilos.
    unsafe { std::env::set_var("GODREMOTE_KNOWN_HOSTS", dir.path().join("known_hosts")) };

    let key = keys::generate(dir.path(), "id", KeyAlgorithm::Ed25519, "", None).unwrap();
    let authorized = dir.path().join("authorized_keys");
    std::fs::write(&authorized, format!("{}\n", key.public_openssh)).unwrap();
    let (sshd, port, _log) = start_sshd(dir.path(), &authorized, "AllowTcpForwarding yes\n");
    let user = String::from_utf8_lossy(&Command::new("id").arg("-un").output().unwrap().stdout)
        .trim()
        .to_string();
    let session = Session {
        name: "destino".into(),
        host: "127.0.0.1".into(),
        port,
        username: user,
        auth: AuthMethod::Key,
        key_path: key.path.display().to_string(),
        ..Default::default()
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let echo = echo_server();
    let direct = || Endpoint::direct(session.clone(), Credentials::default());
    let start = |endpoint: Endpoint, tunnel: Tunnel| {
        tunnel_manager::spawn(rt.handle(), endpoint, tunnel, prompter(answers(true, &[])))
    };

    // --- 1. Local (-L): funciona sin ningún terminal abierto ---------------
    let local = free_port();
    let tunnel = Tunnel {
        kind: TunnelKind::Local,
        listen_host: "127.0.0.1".into(),
        listen_port: local,
        target_host: "127.0.0.1".into(),
        target_port: echo,
    };
    let mut running = start(direct(), tunnel.clone());
    assert!(matches!(running.state, RunState::Connecting(_)));
    assert_eq!(settle(&mut running), RunState::Active);
    assert!(running.was_active && running.is_running());
    let stream = TcpStream::connect(("127.0.0.1", local)).unwrap();
    assert_eq!(roundtrip(stream, b"hola"), b"eco:hola");

    // Al pararlo deja de escuchar y queda como detenido, no como fallo.
    running.stop();
    assert_eq!(
        wait_until(&mut running, |state| *state != RunState::Active),
        RunState::Stopped
    );
    wait_closed(local);

    // Se puede volver a iniciar en el mismo puerto.
    let mut running = start(direct(), tunnel.clone());
    assert_eq!(settle(&mut running), RunState::Active);
    let stream = TcpStream::connect(("127.0.0.1", local)).unwrap();
    assert_eq!(roundtrip(stream, b"otra"), b"eco:otra");

    // --- 2. El puerto ya está ocupado: falla y lo explica ------------------
    let mut clash = start(direct(), tunnel.clone());
    match settle(&mut clash) {
        RunState::Failed(error) => {
            assert!(error.contains("no se pudo escuchar"), "{error}");
        }
        other => panic!("debía fallar: {other:?}"),
    }
    assert!(!clash.was_active && !clash.is_running());
    // El primero sigue funcionando.
    let stream = TcpStream::connect(("127.0.0.1", local)).unwrap();
    assert_eq!(roundtrip(stream, b"sigue"), b"eco:sigue");
    // Soltarlo también lo cierra.
    drop(running);
    wait_closed(local);

    // --- 3. Dinámico (-D) pasando por un salto -----------------------------
    let socks = free_port();
    let bastion = Session {
        name: "bastion".into(),
        ..session.clone()
    };
    let through = Endpoint::through(
        [(bastion, Credentials::default())],
        session.clone(),
        Credentials::default(),
    );
    let mut dynamic = start(
        through,
        Tunnel {
            kind: TunnelKind::Dynamic,
            listen_host: "127.0.0.1".into(),
            listen_port: socks,
            ..Default::default()
        },
    );
    assert_eq!(settle(&mut dynamic), RunState::Active);
    let mut s = TcpStream::connect(("127.0.0.1", socks)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(&[5, 1, 0]).unwrap();
    let mut resp = [0u8; 2];
    s.read_exact(&mut resp).unwrap();
    assert_eq!(resp, [5, 0]);
    let mut request = vec![5, 1, 0, 3, 9];
    request.extend_from_slice(b"localhost");
    request.extend_from_slice(&echo.to_be_bytes());
    s.write_all(&request).unwrap();
    let mut resp = [0u8; 10];
    s.read_exact(&mut resp).unwrap();
    assert_eq!(&resp[..2], &[5, 0], "SOCKS debe aceptar");
    assert_eq!(roundtrip(s, b"socks"), b"eco:socks");

    // --- 4. Remoto (-R) -----------------------------------------------------
    let remote_port = free_port();
    let mut remote = start(
        direct(),
        Tunnel {
            kind: TunnelKind::Remote,
            listen_host: "127.0.0.1".into(),
            listen_port: remote_port,
            target_host: "127.0.0.1".into(),
            target_port: echo,
        },
    );
    assert_eq!(settle(&mut remote), RunState::Active);
    let stream = TcpStream::connect(("127.0.0.1", remote_port)).unwrap();
    assert_eq!(roundtrip(stream, b"remoto"), b"eco:remoto");
    remote.stop();
    assert_eq!(
        wait_until(&mut remote, |state| *state != RunState::Active),
        RunState::Stopped
    );
    wait_closed(remote_port);

    // --- 5. Se cae la conexión: queda como fallo tras haber funcionado ------
    // Se matan los procesos de sshd que atienden las conexiones abiertas.
    if find_program("pkill").is_some() {
        let killed = Command::new("pkill")
            .args(["-P", &sshd.0.id().to_string()])
            .status()
            .unwrap();
        assert!(killed.success(), "pkill no encontró las sesiones de sshd");
        match wait_until(&mut dynamic, |state| *state != RunState::Active) {
            RunState::Failed(error) => assert!(error.contains("perdió la conexión"), "{error}"),
            other => panic!("debía notar la caída: {other:?}"),
        }
        assert!(dynamic.was_active);
        wait_closed(socks);
    }

    // --- 6. Un servidor que no contesta ------------------------------------
    drop(sshd);
    let mut dead = start(direct(), tunnel);
    assert!(matches!(settle(&mut dead), RunState::Failed(_)));
    assert!(!dead.was_active);
}
