//! Disparadores, grabación e historial de órdenes sobre una sesión SSH de
//! verdad: un terminal conectado a un `sshd` real, tecleando como lo haría
//! el usuario.
//!
//! Se omite si falta `sshd` o no se ejecuta como root.
#![cfg(unix)]

mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use common::{answers, find_program, is_root, prompter, start_sshd};
use godremote::cast::{Cast, Player, Step};
use godremote::keys::{self, KeyAlgorithm};
use godremote::model::{AuthMethod, Session};
use godremote::ssh::{self, Credentials, Endpoint};
use godremote::terminal::{INITIAL_COLS, INITIAL_ROWS, Signal, TermInput, TerminalView};
use godremote::triggers::{Compiled, Fired, LongCommand, Trigger, TriggerSettings};

/// Un terminal y lo que la aplicación recogería de él en cada fotograma.
struct Tab {
    view: TerminalView,
    rules: Compiled,
    fired: Vec<Fired>,
    signals: Vec<Signal>,
}

impl Tab {
    /// Lo que la aplicación hace en cada fotograma.
    fn frame(&mut self) {
        self.view.poll();
        self.fired
            .extend(self.view.run_triggers(&self.rules, &|_| true));
        self.signals.append(&mut self.view.signals);
    }

    fn wait(&mut self, what: &str, mut done: impl FnMut(&mut Tab) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.frame();
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "no ha ocurrido: {what}\n--- pantalla ---\n{}",
                self.view.contents()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Deja pasar un rato atendiendo al terminal.
    fn settle(&mut self, time: Duration) {
        let until = Instant::now() + time;
        while Instant::now() < until {
            self.frame();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Teclea una línea carácter a carácter, espera a verla y pulsa Intro.
    fn type_line(&mut self, line: &str) {
        for c in line.chars() {
            self.view
                .apply_input(&TermInput::Bytes(c.to_string().into_bytes()));
        }
        self.wait("el eco de lo tecleado", |tab| {
            tab.view.contents().contains(line)
        });
        self.enter();
    }

    fn enter(&mut self) {
        self.view
            .apply_input(&TermInput::Key(egui::Key::Enter, egui::Modifiers::NONE));
    }

    fn commands(&self) -> Vec<&str> {
        self.signals
            .iter()
            .filter_map(|signal| match signal {
                Signal::Command(command) => Some(command.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// Reproduce entera una grabación en un terminal nuevo.
fn play(path: &std::path::Path) -> TerminalView {
    let cast = Cast::load(path).unwrap();
    let mut copy = TerminalView::viewer(cast.cols, cast.rows);
    let mut player = Player::new(cast);
    player.seek(f64::MAX);
    let pending = player.drain(usize::MAX, |step| match step {
        Step::Output(bytes) => copy.viewer_feed(bytes),
        Step::Resize(cols, rows) => copy.viewer_resize(cols, rows),
    });
    assert!(!pending);
    copy
}

#[test]
fn triggers_recording_and_history_over_ssh() {
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
    let (_sshd, port, _log) = start_sshd(dir.path(), &authorized, "");
    let user = String::from_utf8_lossy(&Command::new("id").arg("-un").output().unwrap().stdout)
        .trim()
        .to_string();
    let session = Session {
        name: "pruebas".into(),
        host: "127.0.0.1".into(),
        port,
        username: user,
        auth: AuthMethod::Key,
        key_path: key.path.display().to_string(),
        ..Default::default()
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let backend = ssh::spawn_shell(
        rt.handle(),
        Endpoint::direct(session, Credentials::default()),
        prompter(answers(true, &[])),
        INITIAL_COLS,
        INITIAL_ROWS,
    );

    let answer = Trigger {
        name: "confirmar".into(),
        pattern: "[y/N]".into(),
        send: Some("y\n".into()),
        notify: false,
        ..Default::default()
    };
    let notice = Trigger {
        name: "fallo".into(),
        pattern: "fallo grave".into(),
        cooldown_secs: 0,
        ..Default::default()
    };
    let rules = Compiled::new(&TriggerSettings {
        rules: vec![answer.clone(), notice.clone()],
        long_command: LongCommand {
            enabled: true,
            seconds: LongCommand::MIN_SECONDS,
            bell: false,
        },
    });
    let mut tab = Tab {
        view: TerminalView::new(backend),
        rules,
        fired: vec![],
        signals: vec![],
    };
    tab.view.set_command_capture(true);
    tab.wait("el indicador de la shell remota", |tab| {
        tab.view.is_connected() && !tab.view.contents().trim().is_empty()
    });
    tab.settle(Duration::from_millis(500));
    let recording = tab.view.start_cast(dir.path(), "pruebas").unwrap();

    // --- 1. Una orden: llega al historial y su salida dispara un aviso ------
    // (Lo tecleado lleva dos espacios para que su eco, aunque se mirara, no
    // fuera el texto de la regla.)
    let first = "echo fallo  grave | tr -s ' '";
    tab.type_line(first);
    tab.wait("el aviso del disparador", |tab| {
        tab.fired.iter().any(|f| f.rule == notice.id)
    });
    tab.settle(Duration::from_millis(400));
    assert_eq!(tab.commands(), [first]);
    let fired: Vec<&Fired> = tab.fired.iter().filter(|f| f.rule == notice.id).collect();
    assert_eq!(fired.len(), 1, "{:?}", tab.fired);
    assert_eq!(fired[0].line, "fallo grave");
    assert!(fired[0].notify && fired[0].send.is_none());

    // --- 2. Una pregunta: el disparador contesta, una sola vez ---------------
    let question = r#"read -p "Continue? [y/N] " r; echo respuesta=$r"#;
    tab.type_line(question);
    tab.wait("la respuesta del disparador", |tab| {
        tab.view.contents().contains("respuesta=y")
    });
    tab.settle(Duration::from_millis(600));
    let answers: Vec<&Fired> = tab.fired.iter().filter(|f| f.rule == answer.id).collect();
    assert_eq!(answers.len(), 1, "{:?}", tab.fired);
    assert_eq!(answers[0].send.as_deref(), Some(&b"y\r"[..]));
    // Lo que contesta un disparador no es una orden del usuario.
    assert_eq!(tab.commands(), [first, question]);

    // --- 3. Una lectura sin eco: no llega al historial ni a la grabación -----
    let hidden = "stty -echo; read x; stty echo; echo leido$((1+1))";
    tab.type_line(hidden);
    tab.wait("que la orden empiece", |tab| {
        tab.commands().contains(&hidden)
    });
    tab.settle(Duration::from_millis(500));
    for c in "hunter2".chars() {
        tab.view
            .apply_input(&TermInput::Bytes(c.to_string().into_bytes()));
    }
    tab.enter();
    tab.wait("el final de la lectura", |tab| {
        tab.view.contents().contains("leido2")
    });
    tab.settle(Duration::from_millis(300));
    assert!(!tab.view.contents().contains("hunter2"));
    assert_eq!(tab.commands(), [first, question, hidden]);

    // --- 4. La grabación reproduce lo mismo que se ha visto -------------------
    assert_eq!(tab.view.stop_cast().as_deref(), Some(recording.as_path()));
    let copy = play(&recording);
    assert_eq!(copy.size(), tab.view.size());
    assert_eq!(copy.contents(), tab.view.contents());
    let text = std::fs::read_to_string(&recording).unwrap();
    assert!(text.contains("respuesta=y") && text.contains("leido2"));
    assert!(
        !text.contains("hunter2"),
        "lo tecleado sin eco está en la grabación"
    );

    // --- 5. Una orden larga, con los marcadores de la shell -------------------
    let long = r"printf '\033]133;C\007'; sleep 5.3; printf '\033]133;D;3\007'";
    tab.type_line(long);
    let started = Instant::now();
    tab.wait("el aviso de fin de orden larga", |tab| {
        tab.signals
            .iter()
            .any(|signal| matches!(signal, Signal::CommandDone(_)))
    });
    assert!(started.elapsed() >= Duration::from_secs(5));
    let done = tab
        .signals
        .iter()
        .find_map(|signal| match signal {
            Signal::CommandDone(done) => Some(*done),
            _ => None,
        })
        .unwrap();
    assert!(done.exact);
    assert_eq!(done.exit_code, Some(3));
    assert!(done.duration >= Duration::from_secs(5), "{done:?}");
    assert_eq!(tab.commands(), [first, question, hidden, long]);
}
