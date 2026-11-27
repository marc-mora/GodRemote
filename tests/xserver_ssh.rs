//! Servidor X integrado de extremo a extremo: una aplicación gráfica lanzada
//! en una sesión SSH con reenvío X11 (contra un `sshd` real) acaba dibujada
//! en la pantalla del servidor X de GodRemote.
//!
//! Se omite si falta `sshd`, `xauth` o los clientes X de prueba, o si no se
//! ejecuta como root.
#![cfg(unix)]

mod common;

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{answers, find_program, is_root, prompter, start_sshd};
use godremote::desktop::{DesktopEvent, DesktopInput};
use godremote::keys::{self, KeyAlgorithm};
use godremote::model::{AuthMethod, Session};
use godremote::ssh::{self, Credentials, Endpoint};
use godremote::terminal::{TermBackend, TermCmd, TermEvent};
use godremote::xhost::{self, XHost};

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

fn wait_for(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "no ocurrió: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn remote_x_clients_show_up_in_the_embedded_server() {
    let tools = ["sshd", "xauth", "xdpyinfo", "xlogo"];
    if tools.iter().any(|p| find_program(p).is_none()) || !is_root() {
        eprintln!("OMITIDA: hace falta root y {tools:?}");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: único test de este binario, antes de crear hilos.
    unsafe {
        std::env::set_var("GODREMOTE_KNOWN_HOSTS", dir.path().join("known_hosts"));
        // Sin servidor X del sistema: solo queda el integrado.
        std::env::remove_var("DISPLAY");
    }

    let key = keys::generate(dir.path(), "id", KeyAlgorithm::Ed25519, "", None).unwrap();
    let authorized = dir.path().join("authorized_keys");
    std::fs::write(&authorized, format!("{}\n", key.public_openssh)).unwrap();
    let (_sshd, port, _log) = start_sshd(
        dir.path(),
        &authorized,
        "X11Forwarding yes\nXAuthLocation /usr/bin/xauth\n",
    );
    let user = String::from_utf8_lossy(&Command::new("id").arg("-un").output().unwrap().stdout)
        .trim()
        .to_string();
    let session = Session {
        name: "graficos".into(),
        host: "127.0.0.1".into(),
        port,
        username: user,
        auth: AuthMethod::Key,
        key_path: key.path.display().to_string(),
        x11_forwarding: true,
        ..Default::default()
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let host = XHost::new(rt.handle().clone(), Arc::new(|| {}));
    xhost::install(host.clone());
    let open_shell = || {
        ssh::spawn_shell(
            rt.handle(),
            Endpoint::direct(session.clone(), Credentials::default()),
            prompter(answers(true, &[])),
            100,
            30,
        )
    };
    let send = |backend: &TermBackend, line: &str| {
        backend
            .cmd
            .send(TermCmd::Input(format!("{line}\n").into_bytes()))
            .unwrap();
    };

    // --- 1. Desactivado y sin DISPLAY no hay a dónde reenviar ---------------
    assert!(xhost::active().is_none());
    let backend = open_shell();
    let mut output = String::new();
    wait_output(&backend, &mut output, "Reenvío X11 no disponible");
    drop(backend);

    // --- 2. Activado: el reenvío va al servidor integrado -------------------
    host.configure(true, false);
    assert!(xhost::active().is_some());
    let backend = open_shell();
    let mut output = String::new();
    wait_output(&backend, &mut output, "Reenvío X11 activo");
    assert!(output.contains("Servidor X"), "{output}");
    // Conectar no arranca nada: el servidor espera a la primera aplicación.
    assert!(host.running().is_none() && host.take_backend().is_none());

    // Una consulta sin ventanas: el servidor arranca y contesta.
    send(
        &backend,
        "xdpyinfo | grep -c 'GodRemote\\|dimensions' ; echo consulta-$?-hecha",
    );
    // (El texto de la orden, que la shell repite, no contiene el resultado.)
    wait_output(&backend, &mut output, "consulta-0-hecha");
    let server = host.running().expect("el servidor debía arrancar");
    let screen = host
        .take_backend()
        .expect("la pantalla es para la interfaz");
    assert!(host.take_backend().is_none(), "solo se entrega una vez");
    assert!(matches!(
        screen.events.recv_timeout(Duration::from_secs(5)),
        Ok(DesktopEvent::Connected)
    ));
    wait_for("xdpyinfo se desconecta", || server.client_count() == 0);
    let background = screen.frame.lock().unwrap().pixels[..4].to_vec();

    // Una ventana de verdad: se dibuja en la pantalla del servidor.
    send(&backend, "xlogo -geometry 200x160+40+60 &");
    wait_for("xlogo conecta", || server.client_count() == 1);
    wait_for("xlogo se dibuja", || {
        let frame = screen.frame.lock().unwrap();
        frame
            .pixels
            .chunks_exact(4)
            .filter(|pixel| *pixel != &background[..])
            .count()
            > 2_000
    });

    // --- 3. La pantalla cambia de tamaño con la pestaña ---------------------
    server.resize(900, 500);
    wait_for("cambio de tamaño", || {
        let frame = screen.frame.lock().unwrap();
        (frame.width(), frame.height()) == (900, 500)
    });
    assert_eq!(server.client_count(), 1, "la aplicación sigue viva");

    // --- 4. Cerrar la pestaña para el servidor y sus aplicaciones -----------
    screen.input.send(DesktopInput::Close).unwrap();
    host.stop();
    wait_for("las aplicaciones se cierran", || server.client_count() == 0);
    assert!(host.running().is_none());
    send(&backend, "wait; echo logo-$((1+1))-terminado");
    wait_output(&backend, &mut output, "logo-2-terminado");

    // --- 5. La siguiente aplicación lo vuelve a arrancar ---------------------
    send(&backend, "xlogo &");
    wait_for("vuelve a arrancar", || {
        host.running()
            .is_some_and(|server| server.client_count() == 1)
    });
    assert!(host.take_backend().is_some(), "hay pantalla nueva");

    // --- 6. Al desactivarlo en los ajustes se para ---------------------------
    host.configure(false, false);
    assert!(host.running().is_none() && xhost::active().is_none());
    send(&backend, "wait; echo todo-$((1+1))-cerrado");
    wait_output(&backend, &mut output, "todo-2-cerrado");
}
