//! Panel de estado contra `sshd` reales: varios «equipos» (el mismo servidor
//! con nombres distintos), uno con el puerto cerrado, otro que cae y vuelve,
//! credenciales que faltan, servidores que no dan datos y el tope de
//! conexiones.
//!
//! Se omite si falta `sshd` o no se ejecuta como root.
#![cfg(unix)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::{Killed, answers, find_program, free_port, is_root, prompter, start_sshd};
use godremote::dashboard::{
    AlertKind, Dashboard, DashboardSettings, HostJob, HostRow, Link, Options, State, Thresholds,
    fullest_disk, summarize,
};
use godremote::keys::{self, KeyAlgorithm};
use godremote::model::{AuthMethod, Session};
use godremote::ssh::{Credentials, Endpoint};

/// Reenvía un puerto local al del `sshd`. Mientras está «caído» corta las
/// conexiones que tenía y cierra al momento las que le llegan: es un equipo
/// que deja de responder y luego vuelve, sin soltar el puerto.
struct Relay {
    port: u16,
    up: Arc<AtomicBool>,
    cut: tokio::sync::watch::Sender<u64>,
}

impl Relay {
    fn start(rt: &tokio::runtime::Runtime, target: u16) -> Self {
        let listener = rt
            .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let up = Arc::new(AtomicBool::new(true));
        let cut = tokio::sync::watch::channel(0u64).0;
        rt.spawn({
            let (up, cut) = (up.clone(), cut.clone());
            async move {
                while let Ok((mut inbound, _)) = listener.accept().await {
                    if !up.load(Ordering::SeqCst) {
                        continue;
                    }
                    let mut cut = cut.subscribe();
                    tokio::spawn(async move {
                        let Ok(mut outbound) =
                            tokio::net::TcpStream::connect(("127.0.0.1", target)).await
                        else {
                            return;
                        };
                        tokio::select! {
                            _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                            _ = cut.changed() => {}
                        }
                    });
                }
            }
        });
        Self { port, up, cut }
    }

    fn down(&self) {
        self.up.store(false, Ordering::SeqCst);
        self.cut.send_modify(|n| *n += 1);
    }

    fn up(&self) {
        self.up.store(true, Ordering::SeqCst);
    }
}

/// (pid, pid del padre, línea de órdenes) de todos los procesos.
fn processes() -> Vec<(u32, u32, String)> {
    let out = Command::new("ps")
        .args(["-eo", "pid=,ppid=,args="])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse().ok()?;
            let parent = parts.next()?.parse().ok()?;
            Some((pid, parent, parts.collect::<Vec<_>>().join(" ")))
        })
        .collect()
}

/// Líneas de órdenes de todo lo que cuelga de un proceso.
fn descendants(root: u32) -> Vec<String> {
    let all = processes();
    let mut family = BTreeSet::from([root]);
    let mut found = vec![];
    loop {
        let before = family.len();
        for (pid, parent, args) in &all {
            if family.contains(parent) && family.insert(*pid) {
                found.push(args.clone());
            }
        }
        if family.len() == before {
            return found;
        }
    }
}

/// Conexiones que el `sshd` tiene abiertas (un proceso hijo por cada una).
fn connections(sshd: &Killed) -> usize {
    let root = sshd.0.id();
    processes()
        .iter()
        .filter(|(_, parent, _)| *parent == root)
        .count()
}

/// Consultas del panel ejecutándose bajo un `sshd`, con ese intervalo.
fn probes(sshd: &Killed, interval: u32) -> usize {
    descendants(sshd.0.id())
        .iter()
        .filter(|args| {
            args.contains("/proc/mounts") && args.contains(&format!("s={interval}; done"))
        })
        .count()
}

/// Conexiones que el `sshd` ha aceptado desde que arrancó.
fn accepted(log: &Path) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("Accepted publickey"))
        .count()
}

/// Intentos de conexión que llegaron al `sshd` y se quedaron sin
/// autenticar.
fn unauthenticated(log: &Path) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("[preauth]"))
        .count()
}

fn describe(state: &State) -> String {
    state
        .hosts
        .iter()
        .map(|h| {
            format!(
                "\n  {}: {:?} fallos={} sin_datos={:?} latencia={:?} muestra={:?} alertas={:?}",
                h.name,
                h.link,
                h.failures,
                h.probe_error,
                h.latency,
                h.updated.map(|u| u.format("%H:%M:%S%.3f").to_string()),
                h.alerts.active().collect::<Vec<_>>(),
            )
        })
        .collect()
}

fn host<'a>(state: &'a State, name: &str) -> &'a HostRow {
    state
        .hosts
        .iter()
        .find(|h| h.name == name)
        .unwrap_or_else(|| panic!("no hay equipo {name}"))
}

/// Espera a que el estado del panel cumpla la condición.
fn wait_for(dash: &Dashboard, what: &str, ready: impl Fn(&State) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        {
            let state = dash.state();
            if ready(&state) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "no se llegó a «{what}»:{}",
                describe(&state)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Espera a que cada equipo nombrado reciba `count` muestras más.
fn wait_samples(dash: &Dashboard, names: &[&str], count: usize) {
    let mut seen: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    {
        // Las que ya tenían no cuentan.
        let state = dash.state();
        for name in names {
            let first = host(&state, name).updated.map(|u| u.timestamp_micros());
            seen.insert(name.to_string(), first.into_iter().collect());
        }
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        {
            let state = dash.state();
            for name in names {
                if let Some(at) = host(&state, name).updated {
                    seen.get_mut(*name).unwrap().insert(at.timestamp_micros());
                }
            }
            if seen.values().all(|s| s.len() > count) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "no llegaron {count} muestras más de {names:?}:{}",
                describe(&state)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Espera a una condición de fuera del panel (procesos, registros).
fn wait_until(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready() {
        assert!(Instant::now() < deadline, "no se llegó a «{what}»");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Los fallos de autenticación no se reintentan (como en la aplicación).
fn retryable(error: &str) -> bool {
    !error.to_lowercase().contains("rechaz")
}

fn options() -> Options {
    Options {
        retry_delays: [300, 600, 1200, 2000]
            .into_iter()
            .map(Duration::from_millis)
            .collect(),
        retryable,
        ..Default::default()
    }
}

#[test]
fn a_folder_of_hosts_is_watched_over_persistent_connections() {
    if find_program("sshd").is_none() || !is_root() {
        eprintln!("OMITIDA: hace falta sshd y ejecutar como root");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: único test de este binario, antes de crear hilos.
    unsafe { std::env::set_var("GODREMOTE_KNOWN_HOSTS", dir.path().join("known_hosts")) };

    let key = keys::generate(dir.path(), "id", KeyAlgorithm::Ed25519, "", None).unwrap();
    let locked = keys::generate(
        dir.path(),
        "id_cifrada",
        KeyAlgorithm::Ed25519,
        "",
        Some("frase secreta"),
    )
    .unwrap();
    let stranger = keys::generate(dir.path(), "id_ajena", KeyAlgorithm::Ed25519, "", None).unwrap();
    let authorized = dir.path().join("authorized_keys");
    std::fs::write(
        &authorized,
        format!("{}\n{}\n", key.public_openssh, locked.public_openssh),
    )
    .unwrap();
    let (sshd, port, log) = start_sshd(dir.path(), &authorized, "MaxStartups 50\n");
    let user = String::from_utf8_lossy(&Command::new("id").arg("-un").output().unwrap().stdout)
        .trim()
        .to_string();
    let session = |name: &str, port: u16, key_path: &Path| Session {
        name: name.into(),
        host: "127.0.0.1".into(),
        port,
        username: user.clone(),
        auth: AuthMethod::Key,
        key_path: key_path.display().to_string(),
        ..Default::default()
    };
    let job_for = |session: Session, creds: Credentials| HostJob {
        id: session.id,
        name: session.name.clone(),
        target: session.target(),
        endpoint: Endpoint::direct(session, creds),
    };
    let job =
        |name: &str, port: u16| job_for(session(name, port, &key.path), Credentials::default());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let relay = Relay::start(&rt, port);
    // Lo ocupado que esté el equipo de pruebas no debe decidir el
    // resultado: aquí solo se vigila que los equipos respondan. (Los
    // umbrales tienen sus propias pruebas en `src/dashboard.rs`.)
    let settings = DashboardSettings {
        interval: DashboardSettings::MIN_INTERVAL,
        thresholds: Thresholds {
            cpu: 0.0,
            load: 0.0,
            memory: 0.0,
            disk: 0.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let interval = settings.interval;

    // --- 1. Varios equipos a la vez, y uno con el puerto cerrado ------------
    let closed = free_port();
    let asked = answers(true, &[]);
    let dash = Dashboard::start(
        rt.handle(),
        vec![
            job("uno", port),
            job("dos", port),
            job("tres", port),
            job("inestable", relay.port),
            job("apagado", closed),
        ],
        prompter(asked.clone()),
        &settings,
        options(),
    );
    let up = ["uno", "dos", "tres", "inestable"];
    wait_for(&dash, "cuatro con datos y uno sin conexión", |state| {
        up.iter().all(|name| {
            let h = host(state, name);
            h.link == Link::Online
                && h.latency.is_some()
                && h.stats.as_ref().is_some_and(|s| s.cpu.is_some())
        }) && host(state, "apagado").failures >= 3
    });
    {
        let state = dash.state();
        for name in up {
            let h = host(&state, name);
            let stats = h.stats.as_ref().unwrap();
            let cpu = stats.cpu.unwrap();
            assert!((0.0..=100.0).contains(&cpu), "{stats:?}");
            let (used, total) = stats.memory.expect("memoria");
            assert!(total > 100_000 && used > 0 && used < total, "{stats:?}");
            assert!(stats.load.is_some() && stats.cores.is_some(), "{stats:?}");
            assert!(stats.uptime_secs.is_some(), "{stats:?}");
            let disk = fullest_disk(stats).expect("algún disco");
            assert!(disk.total > 0 && disk.used_percent() <= 100.0, "{disk:?}");
            assert!(
                stats
                    .disks
                    .iter()
                    .any(|d| d.mount == "/" && !d.kind.is_empty()),
                "{stats:?}"
            );
            assert!(h.latency.unwrap() < Duration::from_secs(5), "{h:?}");
            assert!(h.updated.is_some() && !h.trend.is_empty(), "{h:?}");
            assert!(!h.is_stale(Instant::now(), interval), "{h:?}");
            assert_eq!(h.probe_error, None);
            assert!(!h.alerts.is_active(AlertKind::Unreachable));
            assert_eq!(h.state_label(), "en línea");
        }
        let off = host(&state, "apagado");
        assert!(matches!(off.link, Link::Down(_)), "{:?}", off.link);
        assert!(off.stats.is_none() && off.latency.is_none(), "{off:?}");
        assert!(off.alerts.is_active(AlertKind::Unreachable));
        assert!(off.retry_at.is_some() && off.has_problem());
        assert_eq!(state.online(), 4);
        // El aviso de «inalcanzable» sale una vez, no en cada intento.
        let unreachable: Vec<_> = state
            .history
            .iter()
            .filter(|e| e.kind == AlertKind::Unreachable)
            .collect();
        assert_eq!(unreachable.len(), 1, "{:?}", state.history);
        assert!(unreachable[0].raised && unreachable[0].name == "apagado");
    }
    let news = dash.take_news();
    assert!(
        news.iter()
            .any(|e| e.name == "apagado" && e.kind == AlertKind::Unreachable && e.raised),
        "{news:?}"
    );
    let (notice, bad) = summarize(&news).unwrap();
    assert!(bad && notice.contains("apagado"), "{notice}");

    // --- 2. Las conexiones se reutilizan entre refrescos ---------------------
    wait_samples(&dash, &up, 3);
    assert_eq!(accepted(&log), 4, "una conexión por equipo, no por muestra");
    assert_eq!(connections(&sshd), 4);
    assert!(probes(&sshd, interval) >= 4);
    assert!(
        !dash
            .take_news()
            .iter()
            .any(|e| e.kind == AlertKind::Unreachable),
        "no se avisa otra vez de lo mismo"
    );
    // Cambiar el intervalo relanza la consulta por la misma conexión.
    let slower = DashboardSettings {
        interval: 3,
        ..settings.clone()
    };
    dash.configure(&slower);
    wait_until("consultas con el intervalo nuevo", || {
        probes(&sshd, 3) >= 4 && probes(&sshd, interval) == 0
    });
    wait_samples(&dash, &up, 2);
    assert_eq!(accepted(&log), 4);
    dash.configure(&settings);
    wait_until("consultas con el intervalo de antes", || {
        probes(&sshd, interval) >= 4 && probes(&sshd, 3) == 0
    });

    // --- 3. Un equipo cae: reintentos cada vez más espaciados ----------------
    let attempts_before = host(&dash.state(), "apagado").failures;
    relay.down();
    let mut retries: BTreeMap<u32, chrono::DateTime<chrono::Local>> = BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while retries.keys().filter(|failures| **failures >= 1).count() < 3 {
        {
            let state = dash.state();
            let h = host(&state, "inestable");
            if let (Link::Down(_), Some(at)) = (&h.link, h.retry_at) {
                retries.entry(h.failures).or_insert(at);
            }
            assert!(
                Instant::now() < deadline,
                "sin reintentos:{}",
                describe(&state)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Entre un intento y el siguiente pasa, como poco, la espera que toca,
    // que crece con cada fallo.
    let delays = options();
    let seen: Vec<(u32, chrono::DateTime<chrono::Local>)> = retries.into_iter().collect();
    for pair in seen.windows(2) {
        let ((before, at_before), (after, at_after)) = (pair[0], pair[1]);
        if after == before + 1 {
            let gap = (at_after - at_before).to_std().unwrap();
            assert!(
                gap >= delays.retry_delay(after),
                "tras {after} fallos solo esperó {gap:?}"
            );
        }
    }
    assert!(delays.retry_delay(3) > delays.retry_delay(1));
    {
        let state = dash.state();
        let h = host(&state, "inestable");
        assert!(h.alerts.is_active(AlertKind::Unreachable), "{h:?}");
        assert!(h.stats.is_some(), "se conserva la última muestra");
        assert!(h.is_stale(Instant::now(), interval));
        assert_eq!(h.state_label(), "sin conexión");
        // Los demás siguen a lo suyo.
        assert!(
            ["uno", "dos", "tres"]
                .iter()
                .all(|name| host(&state, name).link == Link::Online)
        );
    }
    wait_samples(&dash, &["uno", "dos", "tres"], 1);
    let news = dash.take_news();
    assert_eq!(
        news.iter()
            .filter(|e| e.name == "inestable" && e.kind == AlertKind::Unreachable && e.raised)
            .count(),
        1,
        "{news:?}"
    );
    // …y vuelve: se reconecta solo y avisa de que se ha resuelto.
    relay.up();
    wait_for(&dash, "el equipo vuelve", |state| {
        let h = host(state, "inestable");
        h.link == Link::Online && h.latency.is_some() && !h.alerts.any()
    });
    wait_samples(&dash, &["inestable"], 2);
    let news = dash.take_news();
    let back: Vec<_> = news.iter().filter(|e| e.name == "inestable").collect();
    assert_eq!(back.len(), 1, "{news:?}");
    assert!(!back[0].raised && back[0].detail.contains("vuelve a responder"));
    assert_eq!(
        accepted(&log),
        5,
        "una sola conexión nueva: la del que volvió"
    );
    assert!(host(&dash.state(), "apagado").failures > attempts_before);
    {
        let state = dash.state();
        let names: Vec<&str> = state
            .order("", true, godremote::dashboard::Column::Name, false)
            .into_iter()
            .map(|i| state.hosts[i].name.as_str())
            .collect();
        assert_eq!(names, ["apagado"], "solo uno sigue con problemas");
    }

    // --- 4. Al cerrar el panel se cierran las conexiones ---------------------
    drop(dash);
    wait_until("ninguna conexión abierta", || connections(&sshd) == 0);
    wait_until("ninguna consulta en marcha", || {
        probes(&sshd, interval) == 0
    });

    // --- 5. Tope de conexiones: con más equipos, se turnan -------------------
    let before = accepted(&log);
    let names = ["a", "b", "c", "d", "e"];
    let dash = Dashboard::start(
        rt.handle(),
        names.iter().map(|name| job(name, port)).collect(),
        prompter(answers(true, &[])),
        &DashboardSettings {
            max_connections: 2,
            ..settings.clone()
        },
        options(),
    );
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        {
            let state = dash.state();
            let busy = state
                .hosts
                .iter()
                .filter(|h| matches!(h.link, Link::Connecting(_) | Link::Online))
                .count();
            assert!(busy <= 2, "más conexiones que el tope:{}", describe(&state));
            // Todos acaban teniendo datos, uso de CPU incluido.
            if state
                .hosts
                .iter()
                .all(|h| h.stats.as_ref().is_some_and(|s| s.cpu.is_some()))
                && accepted(&log) >= before + 7
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no se turnan:{}",
                describe(&state)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(dash.state().history.is_empty(), "turnarse no es una alerta");
    drop(dash);
    wait_until("ninguna conexión abierta", || connections(&sshd) == 0);

    // --- 6. Credenciales que faltan -------------------------------------------
    // (a) Se pregunta como en el resto de la aplicación; lo que el usuario
    // pide guardar sirve para las reconexiones, que ya no preguntan.
    let relay = Relay::start(&rt, port);
    let asked = answers(true, &["frase secreta"]);
    let secured = session("cifrada", relay.port, &locked.path);
    let (secured_id, passphrase_key) = (secured.id, secured.passphrase_key());
    let dash = Dashboard::start(
        rt.handle(),
        vec![
            job_for(secured.clone(), Credentials::default()),
            job("normal", port),
        ],
        prompter(asked.clone()),
        &settings,
        options(),
    );
    wait_for(&dash, "los dos con datos", |state| {
        state.hosts.iter().all(|h| h.stats.is_some())
    });
    assert_eq!(asked.secrets_asked(), 1);
    assert!(asked.messages.lock().unwrap()[0].contains("Passphrase"));
    assert_eq!(
        dash.take_secrets(),
        vec![(passphrase_key.clone(), "frase secreta".to_string())]
    );
    relay.down();
    wait_for(&dash, "la conexión se cae", |state| {
        matches!(host(state, "cifrada").link, Link::Down(_))
    });
    relay.up();
    wait_for(&dash, "reconecta sin preguntar", |state| {
        host(state, "cifrada").link == Link::Online
    });
    wait_samples(&dash, &["cifrada"], 1);
    assert_eq!(asked.secrets_asked(), 1, "la reconexión no pregunta");
    // (b) Con el baúl bloqueado se olvidan: la siguiente reconexión no
    // pregunta por su cuenta, queda en «falta credencial» y no insiste.
    dash.forget_credentials();
    let tries = unauthenticated(&log);
    relay.down();
    wait_for(&dash, "la conexión se cae otra vez", |state| {
        matches!(host(state, "cifrada").link, Link::Down(_))
    });
    relay.up();
    wait_for(&dash, "falta credencial", |state| {
        host(state, "cifrada").link == Link::NeedsCredential
    });
    // Un intento llegó al servidor y se quedó sin autenticar…
    wait_until("el intento sin credencial", || {
        unauthenticated(&log) == tries + 1
    });
    let attempts = accepted(&log);
    wait_samples(&dash, &["normal"], 2);
    {
        let state = dash.state();
        let h = host(&state, "cifrada");
        assert_eq!(h.link, Link::NeedsCredential);
        assert_eq!(h.state_label(), "falta credencial");
        // Se avisa (ya no se le vigila), pero no como «inalcanzable».
        assert!(h.has_problem());
        assert_eq!(
            h.alerts.active().collect::<Vec<_>>(),
            vec![AlertKind::Access]
        );
        assert_eq!(host(&state, "normal").link, Link::Online);
    }
    assert_eq!(asked.secrets_asked(), 1, "no se pregunta sin que lo pidan");
    // …y no hubo más: ni entra ni insiste.
    assert_eq!(accepted(&log), attempts);
    assert_eq!(unauthenticated(&log), tries + 1, "no se insiste");
    // (c) Reintentar sin permiso para preguntar (como al desbloquear el
    // baúl) lo intenta una vez y, como sigue faltando, queda igual.
    dash.retry(secured_id, None, false);
    wait_until("el reintento sin preguntar", || {
        unauthenticated(&log) == tries + 2
    });
    wait_for(&dash, "sigue faltando la credencial", |state| {
        host(state, "cifrada").link == Link::NeedsCredential
    });
    assert_eq!(asked.secrets_asked(), 1);
    // El usuario pide reintentar y cancela la pregunta: sigue faltando.
    dash.retry(secured_id, None, true);
    wait_until("se vuelve a preguntar", || asked.secrets_asked() == 2);
    wait_for(&dash, "sigue faltando la credencial", |state| {
        host(state, "cifrada").link == Link::NeedsCredential
    });
    // (d) Con la credencial ya en el baúl, reintentar la usa sin preguntar.
    let with_passphrase = Credentials {
        passphrase: Some("frase secreta".into()),
        ..Default::default()
    };
    dash.retry(
        secured_id,
        Some(Endpoint::direct(secured, with_passphrase)),
        false,
    );
    wait_for(&dash, "conecta con la credencial", |state| {
        host(state, "cifrada").link == Link::Online
    });
    wait_samples(&dash, &["cifrada"], 1);
    assert_eq!(asked.secrets_asked(), 2);
    assert!(dash.take_secrets().is_empty());
    {
        // «Sin acceso» saltó una vez y se resolvió una vez.
        let state = dash.state();
        assert!(!host(&state, "cifrada").alerts.any());
        let access: Vec<bool> = state
            .history
            .iter()
            .filter(|e| e.kind == AlertKind::Access)
            .map(|e| e.raised)
            .collect();
        assert_eq!(access, [true, false], "{:?}", state.history);
    }
    drop(dash);
    wait_until("ninguna conexión abierta", || connections(&sshd) == 0);

    // --- 7. Un acceso rechazado no se reintenta; servidores que no dan ---------
    // --- datos, o solo algunos; y un equipo al que se llega por un salto ------
    let mute_dir = dir.path().join("mudo");
    let partial_dir = dir.path().join("parcial");
    std::fs::create_dir_all(&mute_dir).unwrap();
    std::fs::create_dir_all(&partial_dir).unwrap();
    // Un servidor que no ejecuta lo que se le pide (no hay shell POSIX).
    let (mute_sshd, mute_port, mute_log) =
        start_sshd(&mute_dir, &authorized, "ForceCommand echo hola\n");
    // Y otro que no es Linux: sin /proc, solo sabe lo que ocupan sus discos.
    let script = partial_dir.join("consulta.sh");
    std::fs::write(
        &script,
        "while :; do echo @@begin; echo 'ncpu=2'; \
         echo 'disk=/dev/ada0p2 1000 950 50 95% /'; \
         echo 'disk=tmpfs 1000 999 1 100% /tmp'; echo @@end; sleep 1; done\n",
    )
    .unwrap();
    let (_partial_sshd, partial_port, _) = start_sshd(
        &partial_dir,
        &authorized,
        &format!("ForceCommand /bin/sh {}\n", script.display()),
    );
    let before = accepted(&log);
    let behind = session("tras-salto", port, &key.path);
    let hop = session("salto", port, &key.path);
    let dash = Dashboard::start(
        rt.handle(),
        vec![
            job_for(
                session("ajena", port, &stranger.path),
                Credentials::default(),
            ),
            job("mudo", mute_port),
            job("parcial", partial_port),
            job("normal", port),
            HostJob {
                id: behind.id,
                name: behind.name.clone(),
                target: behind.target(),
                endpoint: Endpoint::through(
                    [(hop, Credentials::default())],
                    behind,
                    Credentials::default(),
                ),
            },
        ],
        prompter(answers(true, &[])),
        &DashboardSettings {
            thresholds: Thresholds {
                disk: 90.0,
                ..settings.thresholds.clone()
            },
            ..settings.clone()
        },
        options(),
    );
    wait_for(&dash, "cada uno en su estado", |state| {
        matches!(host(state, "ajena").link, Link::Rejected(_))
            && host(state, "mudo").probe_error.is_some()
            && host(state, "mudo").latency.is_some()
            && host(state, "parcial").stats.is_some()
            && host(state, "normal").stats.is_some()
            && host(state, "tras-salto").stats.is_some()
            && host(state, "tras-salto").latency.is_some()
    });
    wait_samples(&dash, &["normal", "parcial", "tras-salto"], 3);
    {
        let state = dash.state();
        let rejected = host(&state, "ajena");
        match &rejected.link {
            Link::Rejected(error) => assert!(error.contains("rechazó la clave"), "{error}"),
            other => panic!("debía quedar rechazado: {other:?}"),
        }
        assert_eq!(rejected.state_label(), "acceso rechazado");
        assert!(rejected.alerts.is_active(AlertKind::Access) && rejected.retry_at.is_none());
        assert!(!rejected.alerts.is_active(AlertKind::Unreachable));
        // Conectado y con latencia, pero sin datos: se dice por qué, no se
        // da por inalcanzable y no se reconecta para insistir.
        let mute = host(&state, "mudo");
        assert_eq!(mute.link, Link::Online);
        assert_eq!(mute.state_label(), "sin datos");
        assert!(
            mute.probe_error.as_ref().unwrap().contains("shell POSIX"),
            "{mute:?}"
        );
        assert!(mute.stats.is_none() && !mute.alerts.any() && mute.has_problem());
        // Datos a medias: lo que hay se muestra (y se vigila) y lo demás
        // queda vacío, sin romper nada.
        let partial = host(&state, "parcial");
        assert_eq!(partial.link, Link::Online);
        let stats = partial.stats.as_ref().unwrap();
        assert_eq!(
            (stats.cpu, stats.memory, stats.load, stats.uptime_secs),
            (None, None, None, None)
        );
        assert_eq!(fullest_disk(stats).unwrap().mount, "/");
        assert_eq!(
            partial.alerts.active().collect::<Vec<_>>(),
            vec![AlertKind::Disk]
        );
        assert!(partial.latency.is_some() && !partial.trend.is_empty());
        let disk_alerts: Vec<_> = state
            .history
            .iter()
            .filter(|e| e.kind == AlertKind::Disk && e.name == "parcial")
            .collect();
        assert_eq!(disk_alerts.len(), 1, "{:?}", state.history);
        assert!(disk_alerts[0].detail.contains("disco / al 95 %"));
    }
    // Una conexión del equipo normal y dos del que va por un salto (la del
    // salto y, por ella, la suya).
    assert_eq!(
        accepted(&log),
        before + 3,
        "el rechazado no llega a entrar y no se reintenta"
    );
    assert_eq!(connections(&sshd), 3);
    assert_eq!(accepted(&mute_log), 1, "el mudo conserva su conexión");
    assert_eq!(connections(&mute_sshd), 1);
    // Al cerrar se cierran todas, también la del salto.
    drop(dash);
    wait_until("ninguna conexión abierta", || {
        connections(&sshd) == 0 && connections(&mute_sshd) == 0
    });
}
