//! Reenvío del agente SSH y conexión a través de un proxy de salida, contra
//! un `sshd` real.
//!
//! Se omite si falta `sshd`, `ssh-agent` o `ssh-add`, o si no se ejecuta
//! como root.
#![cfg(unix)]

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Killed, answers, find_program, is_root, prompter, start_sshd};
use godremote::keys::{self, KeyAlgorithm};
use godremote::model::{AuthMethod, Session};
use godremote::proxy::{self, ProxyConfig, ProxyKind, ProxySettings};
use godremote::ssh::{self, Credentials, Endpoint};
use godremote::terminal::{TermBackend, TermCmd, TermEvent};

/// Lee de la shell hasta que la salida contenga `needle`.
fn wait_output(backend: &TermBackend, output: &mut String, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !output.contains(needle) {
        match backend
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(TermEvent::Data(d)) => output.push_str(&String::from_utf8_lossy(&d)),
            Ok(TermEvent::Status(s)) => output.push_str(&format!("[{s}]\n")),
            Ok(TermEvent::Closed(e)) => panic!("sesión cerrada: {e:?}\n{output}"),
            Ok(_) => {}
            Err(_) => panic!("no apareció {needle:?} en:\n{output}"),
        }
    }
}

/// Envía una orden y espera su resultado. Lo que se teclea antes de que la
/// shell esté lista se pierde, así que se repite hasta que conteste.
fn ask(backend: &TermBackend, output: &mut String, line: &str, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !output.contains(needle) {
        assert!(
            Instant::now() < deadline,
            "no apareció {needle:?} en:\n{output}"
        );
        backend
            .cmd
            .send(TermCmd::Input(format!("{line}\n").into_bytes()))
            .unwrap();
        let retry = Instant::now() + Duration::from_millis(1500);
        while !output.contains(needle) {
            match backend
                .events
                .recv_timeout(retry.saturating_duration_since(Instant::now()))
            {
                Ok(TermEvent::Data(d)) => output.push_str(&String::from_utf8_lossy(&d)),
                Ok(TermEvent::Status(s)) => output.push_str(&format!("[{s}]\n")),
                Ok(TermEvent::Closed(e)) => panic!("sesión cerrada: {e:?}\n{output}"),
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
}

/// Proxy HTTP CONNECT de pruebas: lleva cualquier nombre a `127.0.0.1` y
/// anota a qué destinos le han pedido conectar.
fn http_proxy() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(vec![]));
    let log = seen.clone();
    std::thread::spawn(move || {
        for client in listener.incoming().flatten() {
            let log = log.clone();
            std::thread::spawn(move || {
                let mut client = client;
                let mut head = vec![];
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if client.read_exact(&mut byte).is_err() {
                        return;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let target = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                log.lock().unwrap().push(target.clone());
                let port: u16 = target
                    .rsplit(':')
                    .next()
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or(0);
                let Ok(upstream) = TcpStream::connect(("127.0.0.1", port)) else {
                    let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
                    return;
                };
                if client
                    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .is_err()
                {
                    return;
                }
                let (mut client_read, mut upstream_write) =
                    (client.try_clone().unwrap(), upstream.try_clone().unwrap());
                let (mut upstream_read, mut client_write) = (upstream, client);
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut client_read, &mut upstream_write);
                    let _ = upstream_write.shutdown(std::net::Shutdown::Write);
                });
                let _ = std::io::copy(&mut upstream_read, &mut client_write);
                let _ = client_write.shutdown(std::net::Shutdown::Both);
            });
        }
    });
    (port, seen)
}

#[test]
fn agent_forwarding_and_outbound_proxy() {
    let tools = ["sshd", "ssh-agent", "ssh-add"];
    if tools.iter().any(|p| find_program(p).is_none()) || !is_root() {
        eprintln!("OMITIDA: hace falta root y {tools:?}");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let agent_socket = dir.path().join("agente.sock");
    // SAFETY: único test de este binario, antes de crear hilos.
    unsafe {
        std::env::set_var("GODREMOTE_KNOWN_HOSTS", dir.path().join("known_hosts"));
        std::env::set_var("SSH_AUTH_SOCK", &agent_socket);
    }

    // --- Un agente SSH local con una clave cargada --------------------------
    let _agent = Killed(
        Command::new("ssh-agent")
            .arg("-D")
            .arg("-a")
            .arg(&agent_socket)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !agent_socket.exists() {
        assert!(Instant::now() < deadline, "el agente no arrancó");
        std::thread::sleep(Duration::from_millis(30));
    }
    let agent_key =
        keys::generate(dir.path(), "en_el_agente", KeyAlgorithm::Ed25519, "", None).unwrap();
    let added = Command::new("ssh-add")
        .arg(&agent_key.path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(added.success(), "ssh-add");
    // Huella de la clave tal como la lista el agente.
    let listed = Command::new("ssh-add").arg("-l").output().unwrap();
    let listed = String::from_utf8_lossy(&listed.stdout).to_string();
    let fingerprint = listed
        .split_whitespace()
        .find(|word| word.starts_with("SHA256:"))
        .expect("huella en ssh-add -l")
        .to_string();

    // --- Servidor al que se entra con OTRA clave (un fichero) ---------------
    let key = keys::generate(dir.path(), "id", KeyAlgorithm::Ed25519, "", None).unwrap();
    let authorized = dir.path().join("authorized_keys");
    std::fs::write(&authorized, format!("{}\n", key.public_openssh)).unwrap();
    let (_sshd, port, _log) = start_sshd(dir.path(), &authorized, "AllowAgentForwarding yes\n");
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
    let open_shell = |session: &Session| {
        ssh::spawn_shell(
            rt.handle(),
            Endpoint::direct(session.clone(), Credentials::default()),
            prompter(answers(true, &[])),
            100,
            30,
        )
    };
    // --- 1. Sin pedirlo, el equipo remoto no ve nuestro agente --------------
    let backend = open_shell(&session);
    let mut output = String::new();
    // (Dentro de sshd no se hereda el agente de la prueba: lo fija sshd.)
    ask(
        &backend,
        &mut output,
        "echo sock=${SSH_AUTH_SOCK:-ninguno}=",
        "sock=ninguno=",
    );
    assert!(
        !output.contains("Reenvío del agente SSH activo"),
        "{output}"
    );
    drop(backend);

    // --- 2. Con el reenvío, lista la clave del agente local -----------------
    let forwarding = Session {
        agent_forwarding: true,
        ..session.clone()
    };
    let backend = open_shell(&forwarding);
    let mut output = String::new();
    wait_output(&backend, &mut output, "Reenvío del agente SSH activo");
    ask(
        &backend,
        &mut output,
        "ssh-add -l; echo listado-$?-hecho",
        "listado-0-hecho",
    );
    assert!(
        output.contains(&fingerprint),
        "{output}\n(esperaba {fingerprint})"
    );
    drop(backend);

    // --- 3. A través de un proxy HTTP ---------------------------------------
    // El nombre del destino solo lo sabe resolver el proxy: si la conexión
    // no pasara por él, fallaría.
    let (proxy_port, seen) = http_proxy();
    let config = ProxyConfig {
        settings: ProxySettings {
            kind: ProxyKind::Http,
            host: "127.0.0.1".into(),
            port: proxy_port,
            username: String::new(),
            exceptions: "directo.prueba".into(),
        },
        password: None,
    };
    proxy::set(Some(config));
    let hidden = Session {
        host: "sshd.solo-el-proxy.prueba".into(),
        ..session.clone()
    };
    let backend = open_shell(&hidden);
    let mut output = String::new();
    ask(
        &backend,
        &mut output,
        "echo por-el-proxy-$((6*7))",
        "por-el-proxy-42",
    );
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [format!("sshd.solo-el-proxy.prueba:{port}")]
    );
    drop(backend);

    // Las direcciones de este equipo nunca pasan por el proxy.
    let backend = open_shell(&session);
    let mut output = String::new();
    ask(&backend, &mut output, "echo directo-$((6*7))", "directo-42");
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "127.0.0.1 no debía ir al proxy"
    );
    drop(backend);

    // Un equipo marcado «sin proxy», o que esté en las excepciones, conecta
    // directamente: con un nombre que no existe, falla sin tocar el proxy.
    for direct in [
        Session {
            bypass_proxy: true,
            ..hidden.clone()
        },
        Session {
            host: "directo.prueba".into(),
            ..session.clone()
        },
    ] {
        let backend = open_shell(&direct);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match backend
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(TermEvent::Closed(Some(error))) => {
                    assert!(error.contains("No se pudo conectar"), "{error}");
                    break;
                }
                Ok(TermEvent::Closed(None)) => panic!("debía fallar"),
                Ok(_) => {}
                Err(_) => panic!("no terminó"),
            }
        }
    }
    assert_eq!(seen.lock().unwrap().len(), 1);

    // Sin proxy, el nombre inventado deja de ser alcanzable.
    proxy::set(None);
    let backend = open_shell(&hidden);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match backend
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(TermEvent::Closed(Some(_))) => break,
            Ok(TermEvent::Closed(None)) => panic!("debía fallar"),
            Ok(_) => {}
            Err(_) => panic!("no terminó"),
        }
    }
}
