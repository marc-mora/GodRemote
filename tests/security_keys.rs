//! Llaves de seguridad FIDO2 (`sk-ssh-ed25519`, `sk-ecdsa`) a través del
//! agente SSH, contra un `sshd` real y sin hardware. Dos partes:
//!
//! 1. Un agente de mentira, en la propia prueba, con una llave simulada en
//!    software: comprueba que las identidades `sk-` del agente se entienden,
//!    que su firma (con las marcas y el contador de la llave) llega bien al
//!    servidor y qué pasa cuando la llave no firma (sin conectar, sin tocar).
//! 2. El `ssh-agent` de verdad con `sk-dummy`, el proveedor de pruebas que
//!    traen las fuentes de OpenSSH. Hay que compilarlo aparte e indicar
//!    dónde está; sin él, esta parte se omite:
//!
//! ```text
//! tar xf openssh-9.6p1.tar.gz && cd openssh-9.6p1 && ./configure
//! make regress-prep && make -C openbsd-compat
//! make regress/misc/sk-dummy/sk-dummy.so
//! GODREMOTE_SK_PROVIDER=$PWD/regress/misc/sk-dummy/sk-dummy.so cargo test --test security_keys
//! ```
//!
//! Todo se omite si falta `sshd` o no se ejecuta como root.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use common::{Killed, answers, find_program, is_root, prompter, start_sshd};
use godremote::keys;
use godremote::model::{AuthMethod, Session};
use godremote::ssh::{self, Credentials};
use russh::keys::PrivateKey;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Ejecuta una orden de OpenSSH con el proveedor de llaves de pruebas.
fn run(program: &str, provider: &Path, args: &[&str]) {
    let out = Command::new(program)
        .env("SSH_SK_PROVIDER", provider)
        .env_remove("SSH_ASKPASS")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("{program}: {e}"));
    assert!(
        out.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

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

/// Conecta y devuelve el resultado junto con los avisos emitidos.
fn connect(rt: &tokio::runtime::Runtime, session: &Session) -> (Result<(), String>, String) {
    let notes = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = notes.clone();
    let status = move |s: String| sink.lock().unwrap().push(s);
    let result = rt
        .block_on(ssh::connect(
            session,
            Credentials::default(),
            &prompter(answers(true, &[])),
            &status,
        ))
        .map(|_| ())
        .map_err(|e| format!("{e:#}"));
    let notes = notes.lock().unwrap().join("\n");
    (result, notes)
}

// --- Agente de mentira ------------------------------------------------------

/// Añade una cadena del protocolo SSH (longitud y datos).
fn put(out: &mut Vec<u8>, data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    out.extend(data);
}

/// Saca una cadena del protocolo SSH del principio de `rest`.
fn take<'a>(rest: &mut &'a [u8]) -> &'a [u8] {
    let len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
    let (data, tail) = rest[4..].split_at(len);
    *rest = tail;
    data
}

const SK_ED25519: &str = "sk-ssh-ed25519@openssh.com";

/// Una identidad del agente de mentira: una clave Ed25519 normal o la de una
/// «llave de seguridad» cuya firma se hace en software.
struct FakeKey {
    private: PrivateKey,
    security_key: bool,
    comment: &'static str,
    /// La llave no firma: como si no estuviera conectada o no se tocara.
    refuses: bool,
}

impl FakeKey {
    fn new(security_key: bool, comment: &'static str, refuses: bool) -> Self {
        let private = PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519);
        Self {
            private: private.unwrap(),
            security_key,
            comment,
            refuses,
        }
    }

    fn algorithm(&self) -> &'static str {
        if self.security_key {
            SK_ED25519
        } else {
            "ssh-ed25519"
        }
    }

    /// La clave pública tal como viaja en el protocolo.
    fn blob(&self) -> Vec<u8> {
        let public = self.private.public_key().key_data().ed25519().unwrap().0;
        let mut blob = vec![];
        put(&mut blob, self.algorithm().as_bytes());
        put(&mut blob, &public);
        if self.security_key {
            // La «aplicación» FIDO para la que se registró la llave.
            put(&mut blob, b"ssh:");
        }
        blob
    }

    fn authorized_line(&self) -> String {
        let blob = base64::engine::general_purpose::STANDARD.encode(self.blob());
        format!("{} {blob} {}\n", self.algorithm(), self.comment)
    }

    /// Firma como lo haría el agente. Una llave de seguridad no firma los
    /// datos tal cual, sino el resumen de la aplicación, sus marcas (aquí,
    /// «el usuario la tocó»), su contador y el resumen de los datos.
    fn sign(&self, data: &[u8]) -> Vec<u8> {
        use russh::keys::signature::Signer as _;
        let mut out = vec![];
        put(&mut out, self.algorithm().as_bytes());
        if self.security_key {
            let (flags, counter) = (0x01u8, 7u32);
            let mut signed = Sha256::digest(b"ssh:").to_vec();
            signed.push(flags);
            signed.extend(counter.to_be_bytes());
            signed.extend(Sha256::digest(data));
            let signature = self.private.key_data().try_sign(&signed).unwrap();
            put(&mut out, signature.as_bytes());
            out.push(flags);
            out.extend(counter.to_be_bytes());
        } else {
            let signature = self.private.key_data().try_sign(data).unwrap();
            put(&mut out, signature.as_bytes());
        }
        out
    }
}

type FakeKeys = Arc<Mutex<Vec<Arc<FakeKey>>>>;

/// Respuesta del agente de mentira a un mensaje del protocolo de agente.
fn answer(keys: &FakeKeys, message: &[u8]) -> Vec<u8> {
    const FAILURE: u8 = 5;
    const REQUEST_IDENTITIES: u8 = 11;
    const IDENTITIES_ANSWER: u8 = 12;
    const SIGN_REQUEST: u8 = 13;
    const SIGN_RESPONSE: u8 = 14;
    let keys = keys.lock().unwrap().clone();
    match message.split_first() {
        Some((&REQUEST_IDENTITIES, _)) => {
            let mut out = vec![IDENTITIES_ANSWER];
            out.extend((keys.len() as u32).to_be_bytes());
            for key in &keys {
                put(&mut out, &key.blob());
                put(&mut out, key.comment.as_bytes());
            }
            out
        }
        Some((&SIGN_REQUEST, mut rest)) => {
            let blob = take(&mut rest);
            let data = take(&mut rest);
            match keys.iter().find(|key| key.blob() == blob) {
                Some(key) if !key.refuses => {
                    let mut out = vec![SIGN_RESPONSE];
                    put(&mut out, &key.sign(data));
                    out
                }
                _ => vec![FAILURE],
            }
        }
        _ => vec![FAILURE],
    }
}

/// Atiende el socket del agente de mentira.
async fn serve(listener: tokio::net::UnixListener, keys: FakeKeys) {
    while let Ok((mut stream, _)) = listener.accept().await {
        let keys = keys.clone();
        tokio::spawn(async move {
            loop {
                let mut len = [0u8; 4];
                if stream.read_exact(&mut len).await.is_err() {
                    return;
                }
                let mut message = vec![0u8; u32::from_be_bytes(len) as usize];
                if stream.read_exact(&mut message).await.is_err() {
                    return;
                }
                let reply = answer(&keys, &message);
                let mut out = (reply.len() as u32).to_be_bytes().to_vec();
                out.extend(reply);
                if stream.write_all(&out).await.is_err() {
                    return;
                }
            }
        });
    }
}

/// Primera parte: una llave simulada en un agente de mentira.
fn simulated_security_key(dir: &Path, user: &str) {
    let socket = dir.join("simulado.sock");
    // SAFETY: único test de este binario; no hay conexiones en curso.
    unsafe { std::env::set_var("SSH_AUTH_SOCK", &socket) };

    let touched = Arc::new(FakeKey::new(true, "llave simulada", false));
    let unplugged = Arc::new(FakeKey::new(true, "llave sin conectar", true));
    let plain = Arc::new(FakeKey::new(false, "clave normal", false));
    let authorized = dir.join("authorized_keys");
    let lines: String = [&touched, &unplugged, &plain]
        .iter()
        .map(|key| key.authorized_line())
        .collect();
    std::fs::write(&authorized, lines).unwrap();
    let (_sshd, port, log) = start_sshd(dir, &authorized, "LogLevel VERBOSE\n");

    let rt = tokio::runtime::Runtime::new().unwrap();
    let keys: FakeKeys = Arc::new(Mutex::new(vec![touched.clone()]));
    let listener = rt.block_on(async { tokio::net::UnixListener::bind(&socket).unwrap() });
    rt.spawn(serve(listener, keys.clone()));
    let session = Session {
        name: "pruebas".into(),
        host: "127.0.0.1".into(),
        port,
        username: user.to_string(),
        auth: AuthMethod::Agent,
        ..Default::default()
    };

    // El gestor de claves la ve como llave de seguridad.
    let listing = godremote::agent::list_blocking().expect("claves del agente");
    assert_eq!(listing.keys.len(), 1, "{listing:?}");
    assert!(listing.keys[0].security_key);
    assert_eq!(listing.keys[0].algorithm, SK_ED25519);
    assert_eq!(listing.keys[0].comment, "llave simulada");
    assert_eq!(
        listing.keys[0].public_openssh.split(' ').nth(1),
        touched.authorized_line().split(' ').nth(1)
    );

    // Firma la llave: se avisa de que hay que tocarla y el servidor la acepta.
    let (result, notes) = connect(&rt, &session);
    result.expect("llave de seguridad simulada");
    assert!(
        notes.contains("Toca la llave de seguridad «llave simulada» para confirmar"),
        "{notes}"
    );
    wait_log(&log, "Accepted publickey");
    wait_log(&log, "ED25519-SK");

    // La llave no firma (sin conectar, o nadie la toca): el error lo explica.
    *keys.lock().unwrap() = vec![unplugged.clone()];
    let (result, notes) = connect(&rt, &session);
    let error = result.expect_err("la llave no firmó");
    assert!(
        error.contains("Ninguna clave del agente fue aceptada por el servidor"),
        "{error}"
    );
    assert!(
        error.contains(
            "la llave de seguridad «llave sin conectar» no firmó (¿está conectada y la has tocado?)"
        ),
        "{error}"
    );
    assert!(
        notes.contains("Toca la llave de seguridad «llave sin conectar»"),
        "{notes}"
    );

    // …y no impide probar las demás claves del agente.
    *keys.lock().unwrap() = vec![unplugged, plain.clone()];
    let (result, notes) = connect(&rt, &session);
    result.expect("la clave normal, tras la llave que no firma");
    assert!(notes.contains("«llave sin conectar» no firmó"), "{notes}");
    assert!(!notes.contains("«clave normal»"), "{notes}");
    let fingerprint = plain
        .private
        .public_key()
        .fingerprint(russh::keys::HashAlg::Sha256);
    wait_log(&log, &format!("ED25519 {fingerprint}"));
}

#[test]
fn security_keys_through_the_agent() {
    if find_program("sshd").is_none() || !is_root() {
        eprintln!("OMITIDA: hace falta root y sshd");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: único test de este binario, antes de crear hilos.
    unsafe { std::env::set_var("GODREMOTE_KNOWN_HOSTS", dir.path().join("known_hosts")) };
    let user = String::from_utf8_lossy(&Command::new("id").arg("-un").output().unwrap().stdout)
        .trim()
        .to_string();

    let simulated = dir.path().join("simulada");
    std::fs::create_dir_all(&simulated).unwrap();
    simulated_security_key(&simulated, &user);

    let real = dir.path().join("sk-dummy");
    std::fs::create_dir_all(&real).unwrap();
    openssh_agent_with_sk_dummy(&real, &user);
}

/// Segunda parte: el `ssh-agent` de OpenSSH con el proveedor `sk-dummy`.
fn openssh_agent_with_sk_dummy(dir: &Path, user: &str) {
    let provider = std::env::var_os("GODREMOTE_SK_PROVIDER").map(PathBuf::from);
    let Some(provider) = provider.filter(|p| p.is_file()) else {
        eprintln!(
            "OMITIDA EN PARTE: GODREMOTE_SK_PROVIDER no apunta al proveedor sk-dummy de OpenSSH"
        );
        return;
    };
    let tools = ["ssh-keygen", "ssh-agent", "ssh-add"];
    if tools.iter().any(|p| find_program(p).is_none()) {
        eprintln!("OMITIDA EN PARTE: hace falta {tools:?}");
        return;
    }
    let agent_socket = dir.join("agente.sock");
    // SAFETY: único test de este binario; no hay conexiones en curso.
    unsafe { std::env::set_var("SSH_AUTH_SOCK", &agent_socket) };

    // --- Dos «llaves»: una Ed25519 y otra ECDSA ------------------------------
    let ed = dir.join("id_ed25519_sk");
    let ecdsa = dir.join("id_ecdsa_sk");
    let provider_arg = provider.to_str().unwrap();
    for (kind, path, comment) in [
        ("ed25519-sk", &ed, "llave azul"),
        ("ecdsa-sk", &ecdsa, "llave roja"),
    ] {
        let path = path.to_str().unwrap();
        let args = [
            "-q",
            "-t",
            kind,
            "-w",
            provider_arg,
            "-N",
            "",
            "-C",
            comment,
            "-f",
            path,
        ];
        run("ssh-keygen", &provider, &args);
    }

    // El gestor de claves las reconoce como llaves de seguridad.
    let ed_info = keys::inspect(&ed).expect("clave de la llave Ed25519");
    let ecdsa_info = keys::inspect(&ecdsa).expect("clave de la llave ECDSA");
    assert!(ed_info.security_key && ecdsa_info.security_key);
    assert_eq!(ed_info.algorithm, "sk-ssh-ed25519@openssh.com");
    assert_eq!(
        keys::security_key_label(&ed_info.algorithm),
        Some("Ed25519")
    );
    assert_eq!(
        keys::security_key_label(&ecdsa_info.algorithm),
        Some("ECDSA P-256")
    );
    assert_eq!(ed_info.comment, "llave azul");
    assert!(
        ed_info
            .public_openssh
            .starts_with("sk-ssh-ed25519@openssh.com ")
    );

    // --- El servidor autoriza las dos ---------------------------------------
    let authorized = dir.join("authorized_keys");
    let both = format!(
        "{}\n{}\n",
        ed_info.public_openssh, ecdsa_info.public_openssh
    );
    std::fs::write(&authorized, &both).unwrap();
    let (_sshd, port, log) = start_sshd(dir, &authorized, "LogLevel VERBOSE\n");

    let rt = tokio::runtime::Runtime::new().unwrap();
    let session = |auth: AuthMethod, key: &Path| Session {
        name: "pruebas".into(),
        host: "127.0.0.1".into(),
        port,
        username: user.to_string(),
        auth,
        key_path: key.display().to_string(),
        ..Default::default()
    };
    let connect = |session: &Session| connect(&rt, session);

    // --- Sin agente: la clave de una llave no sirve por sí sola -------------
    let (result, _) = connect(&session(AuthMethod::Key, &ed));
    let error = result.expect_err("sin agente no se puede firmar");
    assert!(error.contains("es una llave de seguridad"), "{error}");
    assert!(error.contains("ssh-add"), "{error}");

    // --- Un agente con las dos llaves cargadas ------------------------------
    let _agent = Killed(
        Command::new("ssh-agent")
            .arg("-D")
            .arg("-a")
            .arg(&agent_socket)
            // El agente solo carga proveedores de las carpetas permitidas.
            .arg("-P")
            .arg(format!("{}/*", provider.parent().unwrap().display()))
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
    // Con el agente en marcha pero sin la llave cargada, se explica qué hacer.
    let (result, _) = connect(&session(AuthMethod::Key, &ed));
    let error = result.expect_err("la llave no está en el agente");
    assert!(error.contains("no tiene cargada esta clave"), "{error}");
    assert!(error.contains("ssh-add"), "{error}");

    run(
        "ssh-add",
        &provider,
        &["-q", "-S", provider_arg, ed.to_str().unwrap()],
    );
    run(
        "ssh-add",
        &provider,
        &["-q", "-S", provider_arg, ecdsa.to_str().unwrap()],
    );
    let listing = godremote::agent::list_blocking().expect("claves del agente");
    assert_eq!(listing.keys.len(), 2, "{listing:?}");
    assert!(listing.keys.iter().all(|key| key.security_key));
    assert_eq!(listing.keys[0].fingerprint, ed_info.fingerprint);
    assert_eq!(listing.keys[0].comment, "llave azul");

    // --- «Agente SSH»: firma la llave, y se avisa de que hay que tocarla ----
    let (result, notes) = connect(&session(AuthMethod::Agent, Path::new("")));
    result.expect("llave Ed25519 por el agente");
    assert!(
        notes.contains("Toca la llave de seguridad «llave azul»"),
        "{notes}"
    );
    wait_log(&log, "ED25519-SK");

    // --- «Clave privada» con el fichero de la llave: también por el agente --
    let (result, notes) = connect(&session(AuthMethod::Key, &ecdsa));
    result.expect("llave ECDSA elegida como clave privada");
    assert!(
        notes.contains("Toca la llave de seguridad «llave roja»"),
        "{notes}"
    );
    wait_log(&log, "ECDSA-SK");

    // --- Solo autorizada la segunda: se prueban en orden --------------------
    std::fs::write(&authorized, format!("{}\n", ecdsa_info.public_openssh)).unwrap();
    let (result, notes) = connect(&session(AuthMethod::Agent, Path::new("")));
    result.expect("la segunda llave del agente");
    // La primera no llegó a firmar: el servidor no la quiso.
    assert!(!notes.contains("llave azul"), "{notes}");
    assert!(notes.contains("llave roja"), "{notes}");
    // Y elegida a mano la que no vale, el error lo dice.
    let (result, _) = connect(&session(AuthMethod::Key, &ed));
    let error = result.expect_err("llave no autorizada");
    assert!(error.contains("rechazó la clave"), "{error}");
    assert!(error.contains("llave de seguridad"), "{error}");
}
