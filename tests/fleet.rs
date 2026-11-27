//! Ejecución de una orden en varios equipos contra un `sshd` real.
//!
//! Se omite si falta `sshd` o no se ejecuta como root.
#![cfg(unix)]

mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use common::{answers, find_program, free_port, is_root, prompter, start_sshd};
use godremote::fleet::{self, FleetRun, HostJob, HostState, MAX_OUTPUT};
use godremote::keys::{self, KeyAlgorithm};
use godremote::model::{AuthMethod, Session};
use godremote::ssh::{Credentials, Endpoint};

fn wait_finished(run: &mut FleetRun, limit: Duration) {
    let deadline = Instant::now() + limit;
    loop {
        run.poll();
        if run.is_finished() {
            return;
        }
        let states: Vec<&HostState> = run.hosts.iter().map(|h| &h.state).collect();
        assert!(Instant::now() < deadline, "no terminó: {states:?}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn a_command_runs_on_several_hosts() {
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
    let (_sshd, port, _log) = start_sshd(dir.path(), &authorized, "MaxStartups 50\n");
    let user = String::from_utf8_lossy(&Command::new("id").arg("-un").output().unwrap().stdout)
        .trim()
        .to_string();
    let session = |name: &str, port: u16| Session {
        name: name.into(),
        host: "127.0.0.1".into(),
        port,
        username: user.clone(),
        auth: AuthMethod::Key,
        key_path: key.path.display().to_string(),
        ..Default::default()
    };
    let job = |name: &str, port: u16, command: &str| {
        let session = session(name, port);
        HostJob {
            id: session.id,
            name: name.into(),
            endpoint: Endpoint::direct(session, Credentials::default()),
            command: command.into(),
        }
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let start = |jobs: Vec<HostJob>, parallel: usize, timeout: u64| {
        fleet::run(
            rt.handle(),
            jobs,
            prompter(answers(true, &[])),
            parallel,
            Duration::from_secs(timeout),
        )
    };

    // --- 1. Salida, errores y códigos de cada equipo ------------------------
    let closed = free_port();
    let mut run = start(
        vec![
            job("uno", port, "echo hola-$((6*7)); echo aviso >&2"),
            job("dos", port, "echo antes; exit 3"),
            job("tres", port, "head -c 700000 /dev/zero | tr '\\0' 'x'"),
            job("apagado", closed, "echo nunca"),
            job("eñe", port, "printf 'caf\\303\\251 \\342\\202\\254\\n'"),
        ],
        4,
        30,
    );
    assert!(run.hosts.iter().all(|h| h.state == HostState::Waiting));
    wait_finished(&mut run, Duration::from_secs(40));
    let host = |name: &str| run.hosts.iter().find(|h| h.name == name).unwrap();
    assert_eq!(host("uno").state, HostState::Done(Some(0)));
    assert!(host("uno").output.contains("hola-42") && host("uno").output.contains("aviso"));
    assert_eq!(host("dos").state, HostState::Done(Some(3)));
    assert_eq!(host("dos").output, "antes\n");
    // La salida larga se recorta en lugar de comerse la memoria.
    assert_eq!(host("tres").state, HostState::Done(Some(0)));
    assert!(host("tres").truncated && host("tres").output.len() == MAX_OUTPUT);
    assert!(matches!(host("apagado").state, HostState::Failed(_)));
    assert!(host("apagado").output.is_empty());
    // Los caracteres de varios bytes llegan enteros.
    assert_eq!(host("eñe").output, "café €\n");
    assert_eq!(run.summary(), (3, 2, 0));
    let report = run.report();
    assert!(report.contains("===== dos (código 3) ====="), "{report}");
    assert!(report.contains("===== apagado (error: "), "{report}");
    assert!(report.contains("[salida recortada]"));

    // --- 2. No más de N a la vez ---------------------------------------------
    let began = Instant::now();
    let mut run = start(
        (0..4)
            .map(|i| job(&format!("turno-{i}"), port, "sleep 1; echo listo"))
            .collect(),
        2,
        30,
    );
    wait_finished(&mut run, Duration::from_secs(40));
    assert!(run.hosts.iter().all(|h| h.state.is_success()));
    let took = began.elapsed();
    assert!(
        took >= Duration::from_millis(1900),
        "cuatro de un segundo, de dos en dos, no pueden tardar {took:?}"
    );

    // --- 3. Tiempo máximo por equipo -----------------------------------------
    let mut run = start(
        vec![
            job("lento", port, "sleep 30"),
            job("rapido", port, "echo ya"),
        ],
        4,
        2,
    );
    wait_finished(&mut run, Duration::from_secs(20));
    match &run.hosts[0].state {
        HostState::Failed(error) => assert!(error.contains("no terminó"), "{error}"),
        other => panic!("debía agotar el tiempo: {other:?}"),
    }
    assert!(run.hosts[1].state.is_success());

    // --- 4. Cancelar ----------------------------------------------------------
    let mut run = start(
        vec![
            job("a", port, "echo empiezo; sleep 30"),
            job("b", port, "sleep 30"),
            job("en-cola", port, "sleep 30"),
        ],
        2,
        60,
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while !run.hosts[0].output.contains("empiezo") {
        run.poll();
        assert!(Instant::now() < deadline, "no llegó a empezar");
        std::thread::sleep(Duration::from_millis(30));
    }
    run.cancel();
    wait_finished(&mut run, Duration::from_secs(10));
    assert!(run.hosts.iter().all(|h| h.state == HostState::Cancelled));
    // Lo que ya había escrito se conserva.
    assert!(run.hosts[0].output.contains("empiezo"));
}
