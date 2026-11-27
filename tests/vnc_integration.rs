//! Cliente VNC contra un servidor real (`x11vnc` compartiendo un `Xvfb`):
//! autenticación, imagen con cada codificación, ratón, teclado y portapapeles.
//!
//! Se omite si falta alguno de los programas necesarios.
#![cfg(unix)]

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{Killed, answers, find_program, free_port, prompter, wait_port};
use godremote::desktop::{DesktopBackend, DesktopEvent, DesktopInput, KeyCode, MouseButton};
use godremote::model::{Protocol, Session};
use godremote::vnc;

const BG: [u8; 3] = [0x20, 0x40, 0x80];

/// Espera al evento `Connected` (o devuelve el error de cierre).
fn wait_connected(backend: &DesktopBackend) -> Result<Vec<(String, String)>, String> {
    let mut saved = vec![];
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match backend
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(DesktopEvent::Connected) => return Ok(saved),
            Ok(DesktopEvent::SaveSecrets(s)) => saved.extend(s),
            Ok(DesktopEvent::Closed(e)) => return Err(e.unwrap_or_default()),
            Ok(_) => {}
            Err(_) => return Err("tiempo de espera agotado".into()),
        }
    }
}

/// Espera a que un píxel del escritorio tenga el color esperado.
fn wait_pixel(backend: &DesktopBackend, x: u32, y: u32, rgb: [u8; 3]) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let got = {
            let fb = backend.frame.lock().unwrap();
            let i = ((y * fb.width() + x) * 4) as usize;
            fb.pixels.get(i..i + 4).map(|p| [p[0], p[1], p[2], p[3]])
        };
        if got == Some([rgb[0], rgb[1], rgb[2], 255]) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "píxel ({x},{y}) = {got:?}, se esperaba {rgb:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Cierra la sesión y espera a que su tarea termine del todo.
fn close(backend: DesktopBackend) {
    let _ = backend.input.send(DesktopInput::Close);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match backend
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(DesktopEvent::Closed(e)) => {
                assert!(e.is_none(), "{e:?}");
                return;
            }
            Ok(_) => {}
            Err(_) => panic!("la sesión no se cerró"),
        }
    }
}

fn x(display: u32, program: &str, args: &[&str]) -> String {
    let out = Command::new(program)
        .env("DISPLAY", format!(":{display}"))
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn vnc_against_x11vnc() {
    let needed = ["Xvfb", "x11vnc", "xterm", "xdotool", "xprop"];
    if let Some(missing) = needed.iter().find(|p| find_program(p).is_none()) {
        eprintln!("OMITIDA: falta {missing}");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: único test de este binario; aún no hay otros hilos.
    unsafe { std::env::set_var("GODREMOTE_KNOWN_HOSTS", dir.path().join("known_hosts")) };
    let xvfb = common::start_xvfb(common::free_display(), "640x480x24", &[]);
    let display = xvfb.number;
    // Un xterm de color conocido que guarda en un fichero lo que se teclea.
    let typed = dir.path().join("tecleado.txt");
    let _xterm = common::start_xterm(
        &format!(":{display}"),
        &[
            "-geometry",
            "80x24+0+0",
            "-bg",
            "#204080",
            "-fg",
            "white",
            "-e",
            "sh",
            "-c",
            &format!("cat > {}", typed.display()),
        ],
        &dir.path().join("xterm.log"),
    );
    let port = free_port();
    let _vnc = Killed(
        Command::new("x11vnc")
            .args(["-display", &format!(":{display}")])
            .args(["-rfbport", &port.to_string(), "-localhost"])
            .args(["-passwd", "secreto1", "-forever", "-shared", "-quiet"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_port(port);

    let rt = tokio::runtime::Runtime::new().unwrap();
    let session = Session {
        protocol: Protocol::Vnc,
        host: "127.0.0.1".into(),
        port,
        ..Default::default()
    };

    // --- Autenticación ------------------------------------------------------
    let a = answers(false, &["incorrecta"]);
    let backend = vnc::spawn(rt.handle(), session.clone(), None, prompter(a.clone()));
    let err = wait_connected(&backend).expect_err("debe fallar");
    assert!(err.contains("contraseña incorrecta"), "{err}");
    assert_eq!(a.secrets_asked(), 1);

    // Cancelar el diálogo aborta.
    let backend = vnc::spawn(
        rt.handle(),
        session.clone(),
        None,
        prompter(answers(false, &[])),
    );
    let err = wait_connected(&backend).expect_err("debe fallar");
    assert!(err.contains("cancelada"), "{err}");

    // Contraseña correcta escrita por el usuario: se pide guardarla.
    let a = answers(false, &["secreto1"]);
    let backend = vnc::spawn(rt.handle(), session.clone(), None, prompter(a.clone()));
    let saved = wait_connected(&backend).expect("conexión VNC");
    assert_eq!(
        saved,
        vec![(session.password_key(), "secreto1".to_string())]
    );
    {
        let fb = backend.frame.lock().unwrap();
        assert_eq!((fb.width(), fb.height()), (640, 480));
    }
    // Imagen (ZRLE por defecto): el fondo del xterm tiene el color esperado.
    wait_pixel(&backend, 300, 200, BG);
    close(backend);

    // --- Cada codificación por separado, con la contraseña del baúl ---------
    let count = |i: usize| vnc::RECTS_BY_ENCODING[i].load(std::sync::atomic::Ordering::Relaxed);
    for (name, index, encodings) in [
        ("Raw", 0, vec![vnc::ENC_RAW]),
        ("Hextile", 2, vec![vnc::ENC_HEXTILE]),
        ("ZRLE", 3, vec![vnc::ENC_ZRLE]),
    ] {
        let before: Vec<u64> = (0..4).map(count).collect();
        let a = answers(false, &[]);
        let backend = vnc::spawn_with_encodings(
            rt.handle(),
            session.clone(),
            Some("secreto1".into()),
            prompter(a.clone()),
            encodings,
        );
        wait_connected(&backend).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(a.secrets_asked(), 0);
        wait_pixel(&backend, 300, 200, BG);
        // La esquina inferior derecha (fuera del xterm) es el fondo negro.
        wait_pixel(&backend, 630, 470, [0, 0, 0]);
        close(backend);
        // El servidor usó de verdad la codificación pedida, y solo esa.
        for i in [0, 2, 3] {
            let received = count(i) - before[i];
            assert_eq!(
                received > 0,
                i == index,
                "{name}: rectángulos con la codificación {i}: {received}"
            );
        }
        eprintln!("codificación {name}: imagen correcta");
    }

    // --- Ratón y teclado -----------------------------------------------------
    let backend = vnc::spawn(
        rt.handle(),
        session.clone(),
        Some("secreto1".into()),
        prompter(answers(false, &[])),
    );
    wait_connected(&backend).unwrap();
    let send = |input: DesktopInput| backend.input.send(input).unwrap();
    send(DesktopInput::PointerMove { x: 123, y: 77 });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let location = x(display, "xdotool", &["getmouselocation"]);
        if location.contains("x:123 y:77") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "el puntero no se movió: {location}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // Un clic da el foco al xterm; después se teclea texto y teclas sueltas.
    for pressed in [true, false] {
        send(DesktopInput::PointerButton {
            button: MouseButton::Left,
            pressed,
            x: 123,
            y: 77,
        });
    }
    std::thread::sleep(Duration::from_millis(300));
    send(DesktopInput::Text("Hola Mundo 123".into()));
    for (key, pressed) in [
        (KeyCode::Enter, true),
        (KeyCode::Enter, false),
        (KeyCode::Letter(b'x'), true),
        (KeyCode::Letter(b'x'), false),
        (KeyCode::Backspace, true),
        (KeyCode::Backspace, false),
        // Ctrl+D: fin de fichero para `cat`.
        (KeyCode::Control, true),
        (KeyCode::Letter(b'd'), true),
        (KeyCode::Letter(b'd'), false),
        (KeyCode::Control, false),
    ] {
        send(DesktopInput::Key { key, pressed });
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(&typed).unwrap_or_default();
        if text == "Hola Mundo 123\n" {
            break;
        }
        assert!(Instant::now() < deadline, "tecleado: {text:?}");
        std::thread::sleep(Duration::from_millis(50));
    }

    // --- Portapapeles: del cliente al servidor --------------------------------
    send(DesktopInput::ClipboardText("desde-godremote".into()));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let prop = x(display, "xprop", &["-root", "CUT_BUFFER0"]);
        if prop.contains("desde-godremote") {
            break;
        }
        assert!(Instant::now() < deadline, "portapapeles remoto: {prop}");
        std::thread::sleep(Duration::from_millis(100));
    }

    // --- Cierre limpio ---------------------------------------------------------
    close(backend);

    // --- A través de un salto SSH ------------------------------------------------
    if find_program("sshd").is_some() && common::is_root() {
        use godremote::keys::{self, KeyAlgorithm};
        use godremote::model::AuthMethod;
        use godremote::ssh::{Credentials, Endpoint};
        let key = keys::generate(dir.path(), "id", KeyAlgorithm::Ed25519, "", None).unwrap();
        let authorized = dir.path().join("authorized_keys");
        std::fs::write(&authorized, format!("{}\n", key.public_openssh)).unwrap();
        let (_sshd, ssh_port, sshd_log) = common::start_sshd(
            dir.path(),
            &authorized,
            "LogLevel DEBUG1\nAllowTcpForwarding yes\n",
        );
        let user = String::from_utf8_lossy(&Command::new("id").arg("-un").output().unwrap().stdout)
            .trim()
            .to_string();
        let bastion = Session {
            name: "bastion".into(),
            host: "127.0.0.1".into(),
            port: ssh_port,
            username: user,
            auth: AuthMethod::Key,
            key_path: key.path.display().to_string(),
            ..Default::default()
        };
        // El primer xterm se cerró con el Ctrl+D de antes: se abre otro, de
        // otro color, para comprobar que la imagen que llega es la de ahora.
        const BG_JUMP: [u8; 3] = [0x80, 0x60, 0x20];
        let _xterm = common::start_xterm(
            &format!(":{display}"),
            &[
                "-geometry",
                "80x24+0+0",
                "-bg",
                "#806020",
                "-e",
                "sleep",
                "600",
            ],
            &dir.path().join("xterm2.log"),
        );
        let a = answers(true, &[]);
        let backend = vnc::spawn_via(
            rt.handle(),
            session.clone(),
            Some("secreto1".into()),
            Endpoint::route([(bastion, Credentials::default())]),
            prompter(a.clone()),
        );
        wait_connected(&backend).expect("VNC a través del salto");
        // Solo se preguntó por la clave del bastión (host nuevo).
        assert_eq!((a.hosts(), a.secrets_asked()), (1, 0));
        wait_pixel(&backend, 300, 200, BG_JUMP);
        close(backend);
        // La conexión VNC la abrió el bastión.
        let log = std::fs::read_to_string(&sshd_log).unwrap();
        assert!(
            log.lines()
                .any(|l| l.contains("direct") && l.contains(&format!("port {port}"))),
            "el bastión no abrió la conexión VNC:\n{log}"
        );
        eprintln!("VNC a través de un salto SSH: imagen correcta");
    } else {
        eprintln!("VNC por salto: OMITIDO (hace falta sshd y root)");
    }

    // --- Un servidor que no es VNC da un error claro -----------------------------
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let fake_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::Write;
        if let Ok((mut s, _)) = listener.accept() {
            let _ = s.write_all(b"SSH-2.0-OpenSSH_9.6\r\n");
            std::thread::sleep(Duration::from_secs(2));
        }
    });
    let mut not_vnc = session.clone();
    not_vnc.port = fake_port;
    let backend = vnc::spawn(rt.handle(), not_vnc, None, prompter(answers(false, &[])));
    let err = wait_connected(&backend).expect_err("debe fallar");
    assert!(err.contains("no habla el protocolo VNC"), "{err}");
}
