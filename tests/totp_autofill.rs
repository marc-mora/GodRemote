//! Segundo factor con códigos TOTP: el código se rellena solo cuando el
//! servidor lo pide por keyboard-interactive, y se pregunta como siempre si
//! no hay secreto (baúl bloqueado) o si el código automático no vale.
//!
//! El servidor es uno de russh en el propio proceso, que comprueba los
//! códigos con la hora real y, como `pam_google_authenticator`, no admite
//! dos veces el mismo.

mod common;

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use godremote::keys::{self, KeyAlgorithm};
use godremote::model::{AuthMethod, Session};
use godremote::ssh::{self, Credentials, PromptRequest, Prompter, SecretAnswer};
use godremote::totp::{Totp, TotpAlgorithm};
use russh::server::{Auth, Response, Server as _};
use russh::{MethodKind, MethodSet};

const PASSWORD: &str = "c0ntraseña-buena";

/// Qué exige el servidor para dejar entrar.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Solo keyboard-interactive (PAM): contraseña y código en la misma ronda.
    Interactive,
    /// Clave pública y, después, el código (`publickey,keyboard-interactive`).
    KeyThenCode,
    /// Contraseña y, después, el código (`password,keyboard-interactive`).
    PasswordThenCode,
}

#[derive(Clone)]
struct TestServer {
    mode: Mode,
    totp: Totp,
    /// Códigos ya aceptados: no valen otra vez.
    used: Arc<Mutex<HashSet<String>>>,
    /// Clave autorizada (en `KeyThenCode`).
    authorized: Option<russh::keys::PublicKey>,
    /// Esta conexión ya ha pasado el primer factor.
    first_factor: bool,
}

fn only(method: MethodKind) -> Option<MethodSet> {
    Some(MethodSet::from(&[method][..]))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

impl TestServer {
    /// El código vale si es el de un periodo cercano y no se ha usado ya.
    fn code_is_good(&self, code: &str) -> bool {
        let now = self.totp.step_at(unix_now());
        let known =
            (now.saturating_sub(5)..=now + 5).any(|step| self.totp.code_for_step(step) == code);
        known && self.used.lock().unwrap().insert(code.to_string())
    }
}

impl russh::server::Server for TestServer {
    type Handler = TestServer;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
        self.clone()
    }
}

impl russh::server::Handler for TestServer {
    type Error = anyhow::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(match self.mode {
            Mode::PasswordThenCode if user == "ana" && password == PASSWORD => {
                self.first_factor = true;
                Auth::Reject {
                    proceed_with_methods: only(MethodKind::KeyboardInteractive),
                    partial_success: true,
                }
            }
            Mode::PasswordThenCode => Auth::Reject {
                proceed_with_methods: only(MethodKind::Password),
                partial_success: false,
            },
            _ => Auth::Reject {
                proceed_with_methods: only(MethodKind::KeyboardInteractive),
                partial_success: false,
            },
        })
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: russh::server::VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        let known = self
            .authorized
            .as_ref()
            .is_some_and(|k| k.key_data() == key.public_key.key_data());
        if self.mode == Mode::KeyThenCode && user == "ana" && known {
            self.first_factor = true;
            return Ok(Auth::Reject {
                proceed_with_methods: only(MethodKind::KeyboardInteractive),
                partial_success: true,
            });
        }
        Ok(Auth::reject())
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        let whole = self.mode == Mode::Interactive;
        if !whole && !self.first_factor {
            return Ok(Auth::reject());
        }
        let Some(response) = response else {
            let mut prompts = vec![];
            if whole {
                prompts.push(("Password: ".into(), false));
            }
            // Como PAM: tampoco el código se ve al teclearlo.
            prompts.push(("Verification code: ".into(), false));
            return Ok(Auth::Partial {
                name: "".into(),
                instructions: "".into(),
                prompts: prompts.into(),
            });
        };
        let mut got: Vec<String> = response
            .map(|b| String::from_utf8_lossy(&b).to_string())
            .collect();
        let code = got.pop().unwrap_or_default();
        let password_ok = !whole || got == [PASSWORD];
        Ok(
            if user == "ana" && password_ok && self.code_is_good(&code) {
                Auth::Accept
            } else {
                Auth::Reject {
                    proceed_with_methods: only(MethodKind::KeyboardInteractive),
                    partial_success: false,
                }
            },
        )
    }
}

/// Arranca un servidor con su propio secreto TOTP (del periodo indicado).
async fn start(
    mode: Mode,
    period: u64,
    authorized: Option<russh::keys::PublicKey>,
) -> (Session, Totp) {
    let mut secret = [0u8; 20];
    rand::fill(&mut secret);
    let totp = Totp::new(secret.to_vec(), TotpAlgorithm::Sha1, 6, period).unwrap();
    let methods: &[MethodKind] = match mode {
        Mode::Interactive => &[MethodKind::KeyboardInteractive],
        Mode::KeyThenCode => &[MethodKind::PublicKey, MethodKind::KeyboardInteractive],
        Mode::PasswordThenCode => &[MethodKind::Password, MethodKind::KeyboardInteractive],
    };
    let config = Arc::new(russh::server::Config {
        methods: MethodSet::from(methods),
        auth_rejection_time: Duration::from_millis(10),
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![
            russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)
                .unwrap(),
        ],
        ..Default::default()
    });
    let socket = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = socket.local_addr().unwrap().port();
    let mut server = TestServer {
        mode,
        totp: totp.clone(),
        used: Default::default(),
        authorized,
        first_factor: false,
    };
    tokio::spawn(async move {
        let _ = server.run_on_socket(config, &socket).await;
    });
    let session = Session {
        host: "127.0.0.1".into(),
        port,
        username: "ana".into(),
        auth: AuthMethod::Password,
        ..Default::default()
    };
    (session, totp)
}

/// Quien está al teclado: sabe la contraseña y, si tiene el generador a
/// mano, lee de él el código cuando se lo piden.
struct Human {
    totp: Option<Totp>,
    /// Veces que todavía tecleará un código equivocado.
    typos: AtomicUsize,
    /// Preguntas recibidas.
    asked: Mutex<Vec<String>>,
}

impl Human {
    fn new(totp: Option<&Totp>, typos: usize) -> Arc<Self> {
        Arc::new(Self {
            totp: totp.cloned(),
            typos: AtomicUsize::new(typos),
            asked: Mutex::new(vec![]),
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

fn prompter(human: Arc<Human>) -> Prompter {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(request) = rx.recv() {
            match request {
                PromptRequest::HostKey { reply, .. } => {
                    let _ = reply.send(true);
                }
                PromptRequest::Secret { reply, message, .. } => {
                    human.asked.lock().unwrap().push(message.clone());
                    let value = if message.contains("Verification code") {
                        if human.typos.load(Ordering::SeqCst) > 0 {
                            human.typos.fetch_sub(1, Ordering::SeqCst);
                            Some("000000".to_string())
                        } else {
                            human.totp.as_ref().map(Totp::code)
                        }
                    } else {
                        Some(PASSWORD.to_string())
                    };
                    let _ = reply.send(value.map(|value| SecretAnswer { value, save: true }));
                }
            }
        }
    });
    Prompter::new(tx, Arc::new(|| {}))
}

/// Conecta y devuelve el resultado, lo que hay que guardar y los avisos.
async fn connect(
    session: &Session,
    creds: Credentials,
    human: &Arc<Human>,
) -> (Result<Vec<(String, String)>, String>, String) {
    let notes = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = notes.clone();
    let status = move |s: String| sink.lock().unwrap().push(s);
    let result = ssh::connect(session, creds, &prompter(human.clone()), &status)
        .await
        .map(|connected| connected.to_save)
        .map_err(|e| format!("{e:#}"));
    let notes = notes.lock().unwrap().join("\n");
    (result, notes)
}

fn creds(password: bool, totp: Option<&Totp>) -> Credentials {
    Credentials {
        password: password.then(|| PASSWORD.to_string()),
        totp: totp.cloned(),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn totp_second_factor() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: único test de este binario.
    unsafe { std::env::set_var("GODREMOTE_KNOWN_HOSTS", dir.path().join("known_hosts")) };

    // --- 1. Contraseña y código en la misma ronda (PAM) ----------------------
    let (session, totp) = start(Mode::Interactive, 30, None).await;
    // Con la contraseña y el secreto del baúl no se pregunta nada.
    let human = Human::new(None, 0);
    let (result, notes) = connect(&session, creds(true, Some(&totp)), &human).await;
    assert_eq!(result, Ok(vec![]), "{notes}");
    assert_eq!(human.asked(), Vec::<String>::new());
    assert!(
        notes.contains("Código de verificación TOTP enviado"),
        "{notes}"
    );
    // Ni el código ni el secreto salen en los avisos.
    assert!(!notes.contains(&totp.code()), "{notes}");

    // Sin el secreto (baúl bloqueado) se pregunta todo, como siempre.
    let (session, totp) = start(Mode::Interactive, 30, None).await;
    let human = Human::new(Some(&totp), 0);
    let (result, notes) = connect(&session, creds(false, None), &human).await;
    assert_eq!(
        result,
        Ok(vec![(session.password_key(), PASSWORD.to_string())]),
        "{notes}"
    );
    let asked = human.asked();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(
        asked[0].contains("Contraseña para ana@127.0.0.1"),
        "{asked:?}"
    );
    assert_eq!(asked[1], "Verification code: ");
    assert!(!notes.contains("TOTP"), "{notes}");

    // --- 2. El código automático no vale: se pide al usuario ----------------
    let (session, totp) = start(Mode::Interactive, 30, None).await;
    let stale = Totp::parse("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ").unwrap();
    let human = Human::new(Some(&totp), 0);
    let (result, notes) = connect(&session, creds(true, Some(&stale)), &human).await;
    assert_eq!(
        result,
        Ok(vec![(session.password_key(), PASSWORD.to_string())]),
        "{notes}"
    );
    assert!(
        notes.contains("La contraseña guardada o el código de verificación fueron rechazados"),
        "{notes}"
    );
    let asked = human.asked();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(
        asked[0].starts_with("Contraseña o código de verificación incorrectos."),
        "{asked:?}"
    );
    assert!(
        asked[1].contains("El código TOTP automático ya se envió"),
        "{asked:?}"
    );
    assert!(asked[1].ends_with("Verification code: "), "{asked:?}");

    // Y si tampoco acierta el usuario, el error lo dice.
    let (session, _totp) = start(Mode::Interactive, 30, None).await;
    let human = Human::new(None, 99);
    let (result, _) = connect(&session, creds(true, None), &human).await;
    assert_eq!(
        result,
        Err("Autenticación fallida: contraseña o código de verificación incorrectos".into())
    );

    // --- 3. Clave pública y, después, el código ------------------------------
    let key = keys::generate(dir.path(), "id", KeyAlgorithm::Ed25519, "", None).unwrap();
    let public = russh::keys::PublicKey::from_openssh(&key.public_openssh).unwrap();
    let (session, totp) = start(Mode::KeyThenCode, 30, Some(public.clone())).await;
    let with_key = Session {
        auth: AuthMethod::Key,
        key_path: key.path.display().to_string(),
        ..session
    };
    let human = Human::new(None, 0);
    let (result, notes) = connect(&with_key, creds(false, Some(&totp)), &human).await;
    assert_eq!(result, Ok(vec![]), "{notes}");
    assert_eq!(human.asked(), Vec::<String>::new());
    assert!(
        notes.contains("Código de verificación TOTP enviado"),
        "{notes}"
    );

    // Sin secreto: se teclea; un fallo se puede corregir.
    let (session, totp) = start(Mode::KeyThenCode, 30, Some(public.clone())).await;
    let with_key = Session {
        auth: AuthMethod::Key,
        key_path: key.path.display().to_string(),
        ..session
    };
    let human = Human::new(Some(&totp), 1);
    let (result, notes) = connect(&with_key, creds(false, None), &human).await;
    assert_eq!(result, Ok(vec![]), "{notes}");
    let asked = human.asked();
    assert_eq!(asked[0], "Verification code: ");
    assert_eq!(
        asked[1..],
        ["El servidor no aceptó el código anterior.\nVerification code: "]
    );
    assert!(
        notes.contains("El servidor no aceptó el código de verificación"),
        "{notes}"
    );

    // Con un secreto que no es el del servidor, el código automático falla
    // y el siguiente se pide a mano, diciendo por qué.
    let (session, totp) = start(Mode::KeyThenCode, 30, Some(public.clone())).await;
    let with_key = Session {
        auth: AuthMethod::Key,
        key_path: key.path.display().to_string(),
        ..session
    };
    let human = Human::new(Some(&totp), 0);
    // (Otro secreto que el de antes: aquel ya gastó su código de ahora.)
    let wrong = Totp::new(b"secreto equivocado".to_vec(), TotpAlgorithm::Sha1, 6, 30).unwrap();
    let (result, notes) = connect(&with_key, creds(false, Some(&wrong)), &human).await;
    assert_eq!(result, Ok(vec![]), "{notes}");
    let asked = human.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        asked[0].starts_with("El código TOTP automático ya se envió y no bastó"),
        "{asked:?}"
    );

    // Tres códigos malos: se desiste con un mensaje claro.
    let human = Human::new(None, 99);
    let (result, _) = connect(&with_key, creds(false, None), &human).await;
    assert_eq!(
        result,
        Err("Autenticación fallida: el servidor rechazó el código de verificación".into())
    );
    assert_eq!(human.asked().len(), 3);

    // --- 4. Contraseña y, después, el código ---------------------------------
    let (session, totp) = start(Mode::PasswordThenCode, 30, None).await;
    let human = Human::new(None, 0);
    let (result, notes) = connect(&session, creds(true, Some(&totp)), &human).await;
    assert_eq!(result, Ok(vec![]), "{notes}");
    assert_eq!(human.asked(), Vec::<String>::new());
    // La contraseña tecleada se da por buena en cuanto el servidor la acepta.
    let (session, totp) = start(Mode::PasswordThenCode, 30, None).await;
    let human = Human::new(None, 0);
    let (result, notes) = connect(&session, creds(false, Some(&totp)), &human).await;
    assert_eq!(
        result,
        Ok(vec![(session.password_key(), PASSWORD.to_string())]),
        "{notes}"
    );
    assert_eq!(human.asked().len(), 1);

    // --- 5. Dos conexiones seguidas no repiten código ------------------------
    // (Periodo corto para no alargar la prueba; se empieza al principio de
    // uno para que las dos conexiones caigan en el mismo.)
    const PERIOD: u64 = 4;
    let (session, totp) = start(Mode::Interactive, PERIOD, None).await;
    let human = Human::new(None, 0);
    loop {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        if now.as_millis() % (u128::from(PERIOD) * 1000) < 300 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (first, notes) = connect(&session, creds(true, Some(&totp)), &human).await;
    assert_eq!(first, Ok(vec![]), "{notes}");
    assert!(!notes.contains("esperando"), "{notes}");
    let started = Instant::now();
    let (second, notes) = connect(&session, creds(true, Some(&totp)), &human).await;
    assert_eq!(second, Ok(vec![]), "{notes}");
    assert!(notes.contains("esperando al siguiente"), "{notes}");
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(human.asked(), Vec::<String>::new());
}
