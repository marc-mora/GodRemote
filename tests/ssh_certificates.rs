//! Autenticación con certificados OpenSSH contra un `sshd` real que confía
//! en una autoridad (`TrustedUserCAKeys`). Los certificados se firman con
//! `ssh-keygen -s`.
//!
//! Se omite si falta `sshd`, `ssh-keygen`, `ssh-agent` o `ssh-add`, o si no
//! se ejecuta como root.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Killed, answers, find_program, is_root, prompter, start_sshd};
use godremote::keys::{self, CertValidity, KeyAlgorithm};
use godremote::model::{AuthMethod, Session};
use godremote::ssh::{self, Credentials};

/// Ejecuta `ssh-keygen` y exige que termine bien.
fn ssh_keygen(args: &[&str]) {
    let out = Command::new("ssh-keygen")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("ssh-keygen");
    assert!(
        out.status.success(),
        "ssh-keygen {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Firma `<key>.pub` con la autoridad `ca`: deja `<key>-cert.pub`.
fn sign(ca: &Path, key: &Path, id: &str, principals: &str, validity: &str) -> PathBuf {
    ssh_keygen(&[
        "-q",
        "-s",
        ca.to_str().unwrap(),
        "-I",
        id,
        "-n",
        principals,
        "-V",
        validity,
        &format!("{}.pub", key.display()),
    ]);
    keys::cert_path_for(key)
}

/// Espera a que el registro del `sshd` contenga `needle`.
fn wait_log(log: &Path, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        if text.contains(needle) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "no apareció {needle:?} en el registro de sshd:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn certificate_authentication() {
    let tools = ["sshd", "ssh-keygen", "ssh-agent", "ssh-add"];
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
    let user = String::from_utf8_lossy(&Command::new("id").arg("-un").output().unwrap().stdout)
        .trim()
        .to_string();

    // --- La autoridad en la que confía el servidor, y otra en la que no -----
    let ca = keys::generate(
        dir.path(),
        "ca",
        KeyAlgorithm::Ed25519,
        "CA de pruebas",
        None,
    )
    .unwrap();
    let other_ca = keys::generate(dir.path(), "otra_ca", KeyAlgorithm::Ed25519, "", None).unwrap();
    let trusted = dir.path().join("ca_de_confianza.pub");
    std::fs::write(&trusted, format!("{}\n", ca.public_openssh)).unwrap();
    // Ninguna clave está autorizada directamente salvo `con_clave`: solo se
    // entra con un certificado de la autoridad.
    let plain = keys::generate(dir.path(), "con_clave", KeyAlgorithm::Ed25519, "", None).unwrap();
    let authorized = dir.path().join("authorized_keys");
    std::fs::write(&authorized, format!("{}\n", plain.public_openssh)).unwrap();
    let (_sshd, port, log) = start_sshd(
        dir.path(),
        &authorized,
        &format!(
            "TrustedUserCAKeys {}\nLogLevel VERBOSE\n",
            trusted.display()
        ),
    );

    // Si algo falla, lo que diga el servidor ayuda a entender por qué.
    let server_log = log.clone();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        if let Ok(text) = std::fs::read_to_string(&server_log) {
            let lines: Vec<&str> = text.lines().collect();
            eprintln!("--- final del registro de sshd ---");
            for line in &lines[lines.len().saturating_sub(40)..] {
                eprintln!("{line}");
            }
        }
    }));

    let rt = tokio::runtime::Runtime::new().unwrap();
    let session = |key: &Path| Session {
        name: "pruebas".into(),
        host: "127.0.0.1".into(),
        port,
        username: user.clone(),
        auth: AuthMethod::Key,
        key_path: key.display().to_string(),
        ..Default::default()
    };
    // Conecta y devuelve el resultado junto con los avisos emitidos.
    let connect = |session: &Session, secrets: &[&str]| {
        let notes = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = notes.clone();
        let status = move |s: String| sink.lock().unwrap().push(s);
        let result = rt
            .block_on(ssh::connect(
                session,
                Credentials::default(),
                &prompter(answers(true, secrets)),
                &status,
            ))
            .map(|_| ())
            .map_err(|e| format!("{e:#}"));
        let notes = notes.lock().unwrap().join("\n");
        (result, notes)
    };

    // --- 1. Certificado detectado junto a la clave (`<clave>-cert.pub`) -----
    let key = keys::generate(dir.path(), "id_ed25519", KeyAlgorithm::Ed25519, "", None).unwrap();
    assert!(key.certificate.is_none());
    let cert = sign(&ca.path, &key.path, "ana-ed25519", &user, "+1h");
    let info = keys::inspect(&key.path)
        .unwrap()
        .certificate
        .expect("certificado detectado");
    assert_eq!(info.path, cert);
    assert_eq!(info.key_id, "ana-ed25519");
    assert_eq!(info.principals, vec![user.clone()]);
    assert_eq!(info.ca_fingerprint, ca.fingerprint);
    assert_eq!(info.key_fingerprint, key.fingerprint);
    assert_eq!(info.validity(), CertValidity::Valid);
    assert!(!info.host);
    let (result, notes) = connect(&session(&key.path), &[]);
    result.expect("certificado Ed25519");
    assert!(!notes.contains("certificado"), "{notes}");
    // El servidor confirma que entró con el certificado, no con la clave.
    wait_log(&log, "ED25519-CERT");
    let text = wait_log(&log, "ID ana-ed25519");
    assert!(
        text.contains(&format!("CA ED25519 {}", ca.fingerprint)),
        "{text}"
    );

    // --- 2. ECDSA, RSA (firma rsa-sha2) y clave con passphrase --------------
    let ecdsa = keys::generate(dir.path(), "id_ecdsa", KeyAlgorithm::EcdsaP256, "", None).unwrap();
    sign(&ca.path, &ecdsa.path, "ana-ecdsa", &user, "+1h");
    connect(&session(&ecdsa.path), &[])
        .0
        .expect("certificado ECDSA");
    wait_log(&log, "ID ana-ecdsa");

    let rsa = dir.path().join("id_rsa");
    ssh_keygen(&[
        "-q",
        "-t",
        "rsa",
        "-b",
        "2048",
        "-N",
        "",
        "-f",
        rsa.to_str().unwrap(),
    ]);
    sign(&ca.path, &rsa, "ana-rsa", &user, "+1h");
    connect(&session(&rsa), &[]).0.expect("certificado RSA");
    wait_log(&log, "ID ana-rsa");

    let locked = keys::generate(
        dir.path(),
        "id_cifrada",
        KeyAlgorithm::Ed25519,
        "",
        Some("frase"),
    )
    .unwrap();
    sign(&ca.path, &locked.path, "ana-cifrada", &user, "+1h");
    connect(&session(&locked.path), &["frase"])
        .0
        .expect("certificado de una clave con passphrase");
    wait_log(&log, "ID ana-cifrada");

    // --- 3. Certificado en otra ruta, indicada en el equipo -----------------
    let apart = keys::generate(dir.path(), "id_aparte", KeyAlgorithm::Ed25519, "", None).unwrap();
    let signed = sign(&ca.path, &apart.path, "ana-aparte", &user, "+1h");
    let elsewhere = dir.path().join("certificados").join("el-mio.pub");
    std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
    std::fs::rename(&signed, &elsewhere).unwrap();
    // Sin indicarlo no hay certificado: la clave sola no está autorizada.
    let (result, _) = connect(&session(&apart.path), &[]);
    let error = result.expect_err("sin certificado no debe entrar");
    assert!(error.contains("rechazó la clave"), "{error}");
    assert!(!error.contains("ertificado"), "{error}");
    let with_cert = Session {
        cert_path: elsewhere.display().to_string(),
        ..session(&apart.path)
    };
    connect(&with_cert, &[])
        .0
        .expect("certificado indicado a mano");
    wait_log(&log, "ID ana-aparte");

    // Indicar uno que no es de esa clave, o que no existe, es un error claro.
    let wrong = Session {
        cert_path: cert.display().to_string(),
        ..session(&apart.path)
    };
    let error = connect(&wrong, &[])
        .0
        .expect_err("certificado de otra clave");
    assert!(error.contains("no corresponde a la clave"), "{error}");
    let missing = Session {
        cert_path: dir.path().join("no-existe-cert.pub").display().to_string(),
        ..session(&apart.path)
    };
    let error = connect(&missing, &[])
        .0
        .expect_err("certificado inexistente");
    assert!(error.contains("No se pudo leer"), "{error}");

    // --- 4. Certificado caducado --------------------------------------------
    let old = keys::generate(dir.path(), "id_caducada", KeyAlgorithm::Ed25519, "", None).unwrap();
    sign(
        &ca.path,
        &old.path,
        "ana-caducada",
        &user,
        "20200101:20200201",
    );
    let info = keys::inspect(&old.path).unwrap().certificate.unwrap();
    assert_eq!(info.validity(), CertValidity::Expired);
    assert!(
        info.validity_text().starts_with("caducó el 2020-"),
        "{}",
        info.validity_text()
    );
    let (result, notes) = connect(&session(&old.path), &[]);
    let error = result.expect_err("certificado caducado");
    assert!(error.contains("rechazó la clave"), "{error}");
    assert!(error.contains("caducó el 2020-"), "{error}");
    assert!(notes.contains("caducó el 2020-"), "{notes}");
    // No se llegó a presentar al servidor.
    assert!(
        !std::fs::read_to_string(&log)
            .unwrap()
            .contains("ana-caducada")
    );

    // Caducado, pero con la clave autorizada: entra con la clave y avisa.
    sign(
        &ca.path,
        &plain.path,
        "ana-con-clave",
        &user,
        "20200101:20200201",
    );
    let (result, notes) = connect(&session(&plain.path), &[]);
    result.expect("la clave sola sí está autorizada");
    assert!(notes.contains("caducó el 2020-"), "{notes}");
    assert!(
        notes.contains("se prueba la clave sin certificado"),
        "{notes}"
    );

    // Uno que todavía no vale.
    let early = keys::generate(dir.path(), "id_futura", KeyAlgorithm::Ed25519, "", None).unwrap();
    sign(
        &ca.path,
        &early.path,
        "ana-futura",
        &user,
        "20990101:20990201",
    );
    let info = keys::inspect(&early.path).unwrap().certificate.unwrap();
    assert_eq!(info.validity(), CertValidity::NotYetValid);
    let error = connect(&session(&early.path), &[])
        .0
        .expect_err("aún no vale");
    assert!(error.contains("no vale hasta el 2099-"), "{error}");

    // --- 5. Principal equivocado, y autoridad en la que no se confía --------
    let other = keys::generate(dir.path(), "id_otro", KeyAlgorithm::Ed25519, "", None).unwrap();
    sign(&ca.path, &other.path, "ana-otro", "invitado,copias", "+1h");
    let error = connect(&session(&other.path), &[])
        .0
        .expect_err("otro principal");
    assert!(error.contains("no aceptó el certificado"), "{error}");
    assert!(
        error.contains(&format!("vale para invitado, copias, no para «{user}»")),
        "{error}"
    );

    let stranger = keys::generate(dir.path(), "id_ajena", KeyAlgorithm::Ed25519, "", None).unwrap();
    sign(&other_ca.path, &stranger.path, "ana-ajena", &user, "+1h");
    let error = connect(&session(&stranger.path), &[])
        .0
        .expect_err("otra CA");
    assert!(error.contains("no aceptó el certificado"), "{error}");
    assert!(error.contains("autoridad"), "{error}");

    // Un fichero que no es un certificado, junto a la clave: se avisa y se
    // sigue con la clave (aquí, autorizada).
    std::fs::write(
        keys::cert_path_for(&plain.path),
        "esto no es un certificado\n",
    )
    .unwrap();
    let (result, notes) = connect(&session(&plain.path), &[]);
    result.expect("la clave sola sí está autorizada");
    assert!(notes.contains("no es un certificado OpenSSH"), "{notes}");

    // --- 6. Certificado cargado en el agente SSH ----------------------------
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
    let in_agent =
        keys::generate(dir.path(), "id_agente", KeyAlgorithm::Ed25519, "", None).unwrap();
    sign(&ca.path, &in_agent.path, "ana-agente", &user, "+1h");
    // `ssh-add` carga la clave y, al verlo al lado, también su certificado.
    let added = Command::new("ssh-add")
        .arg(&in_agent.path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(added.success(), "ssh-add");
    // El agente tiene dos identidades: la clave y su certificado.
    let listing = godremote::agent::list_blocking().expect("claves del agente");
    assert_eq!(listing.keys.len(), 2, "{listing:?}");
    let listed = listing
        .keys
        .iter()
        .find_map(|key| key.certificate.as_ref())
        .expect("certificado en el agente");
    assert_eq!(listed.key_id, "ana-agente");
    assert_eq!(listed.ca_fingerprint, ca.fingerprint);
    assert!(
        listing
            .keys
            .iter()
            .all(|key| key.fingerprint == in_agent.fingerprint)
    );
    let by_agent = Session {
        auth: AuthMethod::Agent,
        key_path: String::new(),
        ..session(Path::new(""))
    };
    connect(&by_agent, &[]).0.expect("certificado del agente");
    wait_log(&log, "ID ana-agente");
}
