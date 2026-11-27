//! Barra de información del servidor contra un `sshd` real: la consulta
//! viaja por un canal aparte de la conexión, da datos coherentes con los de
//! este equipo y deja de ejecutarse en el servidor al soltarla.
//!
//! Se omite si no hay `sshd` o no se ejecuta como root.
#![cfg(unix)]

mod common;

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{answers, find_program, is_root, prompter, start_sshd};
use godremote::keys::{self, KeyAlgorithm};
use godremote::model::{AuthMethod, Session};
use godremote::monitor::{self, Monitor, ServerBar, Stats};
use godremote::ssh::{self, Credentials, Endpoint};

/// Espera a que la consulta entregue una muestra que cumpla la condición.
fn wait_stats(monitor: &mut Monitor, what: &str, ready: impl Fn(&Stats) -> bool) -> Stats {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        monitor.poll();
        if let Some(error) = &monitor.error {
            panic!("{what}: la consulta falló: {error}");
        }
        if let Some(stats) = monitor.stats.as_ref().filter(|s| ready(s)) {
            return stats.clone();
        }
        assert!(
            Instant::now() < deadline,
            "{what}: sin datos a tiempo; último: {:?}",
            monitor.stats
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Procesos del servidor que siguen ejecutando la consulta.
fn probes_running(marker: &str) -> usize {
    let out = Command::new("pgrep").args(["-f", marker]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).lines().count()
}

#[test]
fn server_info_over_a_real_connection() {
    if find_program("sshd").is_none() || !is_root() {
        eprintln!("OMITIDA: hace falta sshd y ejecutar como root");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: único test de este binario; aún no hay otros hilos.
    unsafe { std::env::set_var("GODREMOTE_KNOWN_HOSTS", dir.path().join("known_hosts")) };
    let key = keys::generate(dir.path(), "id", KeyAlgorithm::Ed25519, "", None).unwrap();
    let authorized = dir.path().join("authorized_keys");
    std::fs::write(&authorized, format!("{}\n", key.public_openssh)).unwrap();
    let (_sshd, port, _log) = start_sshd(dir.path(), &authorized, "");
    let user = String::from_utf8_lossy(&Command::new("id").arg("-un").output().unwrap().stdout)
        .trim()
        .to_string();
    let session = Session {
        host: "127.0.0.1".into(),
        port,
        username: user,
        auth: AuthMethod::Key,
        key_path: key.path.display().to_string(),
        ..Default::default()
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let connected = rt
        .block_on(ssh::connect_endpoint(
            &Endpoint::direct(session, Credentials::default()),
            &prompter(answers(true, &[])),
            &|_: String| {},
        ))
        .expect("conexión SSH");

    // Una ruta poco habitual sirve para reconocer el proceso de la consulta.
    let marker_dir = dir.path().join("disco-de-prueba");
    std::fs::create_dir(&marker_dir).unwrap();
    let marker = marker_dir.display().to_string();
    assert!(monitor::valid_disk(&marker), "{marker}");
    let config = ServerBar {
        interval: 1,
        disks: vec!["/".into(), marker.clone()],
        ..Default::default()
    };
    let waker: ssh::Waker = Arc::new(|| {});
    let mut probe = monitor::spawn(
        rt.handle(),
        connected.handle.clone(),
        &config,
        waker.clone(),
    );
    assert!(!probe.is_stale(&config));

    // Primera muestra: lo que no depende de comparar con otra.
    let first = wait_stats(&mut probe, "primera muestra", |_| true);
    let hostname = String::from_utf8_lossy(&Command::new("hostname").output().unwrap().stdout)
        .trim()
        .to_string();
    assert_eq!(first.host, hostname);
    assert!(first.kernel.starts_with("Linux"), "{first:?}");
    assert!(
        !first.time.is_empty() && !first.arch.is_empty(),
        "{first:?}"
    );
    let (used, total) = first.memory.expect("memoria");
    assert!(total > 100_000 && used > 0 && used < total, "{first:?}");
    let root = first
        .disks
        .iter()
        .find(|d| d.mount == "/")
        .expect("disco raíz");
    assert!(root.total > 0 && root.available <= root.total, "{root:?}");
    assert!(first.cores.is_some_and(|c| c >= 1), "{first:?}");
    assert!(
        first.uptime_secs.is_some() && first.load.is_some(),
        "{first:?}"
    );
    assert!(first.processes.is_some_and(|p| p > 3), "{first:?}");

    // Las siguientes ya traen el uso de CPU y el tráfico de red.
    let later = wait_stats(&mut probe, "segunda muestra", |s| s.cpu.is_some());
    let cpu = later.cpu.unwrap();
    assert!((0.0..=100.0).contains(&cpu), "{later:?}");
    if std::fs::read_to_string("/proc/net/dev").is_ok_and(|t| t.lines().count() > 3) {
        assert!(later.network.is_some(), "{later:?}");
    }
    eprintln!(
        "servidor: {} · {} · CPU {cpu:.0} % · memoria {}/{} · / libre {}",
        later.host,
        later.os,
        monitor::format_kb(used),
        monitor::format_kb(total),
        monitor::format_kb(root.available),
    );

    // Cambiar el intervalo o los discos obliga a relanzar la consulta.
    let changed = ServerBar {
        interval: 2,
        ..config.clone()
    };
    assert!(probe.is_stale(&changed));

    // Al soltarla, el bucle deja de ejecutarse en el servidor.
    assert!(
        probes_running(&marker) >= 1,
        "la consulta no está en marcha"
    );
    drop(probe);
    let deadline = Instant::now() + Duration::from_secs(15);
    while probes_running(&marker) > 0 {
        assert!(
            Instant::now() < deadline,
            "la consulta sigue ejecutándose en el servidor"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // La conexión sigue sirviendo para otra consulta (y para el terminal).
    let mut again = monitor::spawn(rt.handle(), connected.handle.clone(), &changed, waker);
    wait_stats(&mut again, "consulta relanzada", |s| !s.host.is_empty());
}
