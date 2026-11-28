//! Autenticación por contraseña y keyboard-interactive contra un servidor
//! SSH mínimo en el propio proceso (no necesita usuarios del sistema).

mod common;

use std::sync::Arc;

use common::{answers, prompter};
use godremote::model::{AuthMethod, Session};
use godremote::ssh::{self, Credentials};
use russh::server::{Auth, Response, Server as _};
use russh::{MethodKind, MethodSet};

const PASSWORD: &str = "c0ntraseña-buena";
const OTP: &str = "123456";

#[derive(Clone)]
struct TestServer {
    /// Si es `false`, el servidor solo admite keyboard-interactive.
    allow_password: bool,
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
        if !self.allow_password {
            return Ok(Auth::Reject {
                proceed_with_methods: Some(MethodSet::from(&[MethodKind::KeyboardInteractive][..])),
                partial_success: false,
            });
        }
        Ok(if user == "ana" && password == PASSWORD {
            Auth::Accept
        } else {
            // Como OpenSSH: tras un fallo se puede volver a intentar.
            Auth::Reject {
                proceed_with_methods: Some(MethodSet::from(&[MethodKind::Password][..])),
                partial_success: false,
            }
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        let Some(response) = response else {
            return Ok(Auth::Partial {
                name: "Acceso".into(),
                instructions: "".into(),
                prompts: vec![("Password: ".into(), false), ("Código: ".into(), true)].into(),
            });
        };
        let got: Vec<String> = response
            .map(|b| String::from_utf8_lossy(&b).to_string())
            .collect();
        Ok(if user == "ana" && got == [PASSWORD, OTP] {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }
}

/// Arranca el servidor de prueba con los métodos indicados; devuelve el puerto.
async fn start(methods: &[MethodKind]) -> u16 {
    let allow_password = methods.contains(&MethodKind::Password);
    let config = Arc::new(russh::server::Config {
        methods: MethodSet::from(methods),
        auth_rejection_time: std::time::Duration::from_millis(10),
        auth_rejection_time_initial: Some(std::time::Duration::ZERO),
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
    tokio::spawn(async move {
        let mut server = TestServer { allow_password };
        let _ = server.run_on_socket(config, &socket).await;
    });
    port
}

fn session(port: u16) -> Session {
    Session {
        host: "127.0.0.1".into(),
        port,
        username: "ana".into(),
        auth: AuthMethod::Password,
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn password_and_keyboard_interactive() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: único test de este binario.
    unsafe { std::env::set_var("GODREMOTE_KNOWN_HOSTS", dir.path().join("known_hosts")) };
    let quiet = |_: String| {};

    // --- Servidor con contraseña ---------------------------------------
    let s = session(start(&[MethodKind::Password]).await);

    // Incorrecta y luego correcta: se pregunta dos veces y solo se guarda la buena.
    let a = answers(true, &["mala", PASSWORD]);
    let ok = ssh::connect(&s, Credentials::default(), &prompter(a.clone()), &quiet)
        .await
        .expect("contraseña correcta al segundo intento");
    assert_eq!(a.secrets_asked(), 2);
    assert_eq!(a.hosts(), 1);
    assert_eq!(ok.to_save, vec![(s.password_key(), PASSWORD.to_string())]);
    assert!(a.messages.lock().unwrap()[1].contains("incorrecta"));

    // Con la contraseña del baúl no se pregunta nada (ni el host, ya conocido).
    let a = answers(false, &[]);
    let creds = Credentials {
        password: Some(PASSWORD.into()),
        ..Default::default()
    };
    let ok = ssh::connect(&s, creds, &prompter(a.clone()), &quiet)
        .await
        .expect("contraseña guardada");
    assert_eq!((a.secrets_asked(), a.hosts()), (0, 0));
    assert!(ok.to_save.is_empty());

    // Contraseña guardada obsoleta: se pide una nueva.
    let a = answers(false, &[PASSWORD]);
    let creds = Credentials {
        password: Some("antigua".into()),
        ..Default::default()
    };
    let ok = ssh::connect(&s, creds, &prompter(a.clone()), &quiet)
        .await
        .expect("contraseña nueva tras fallar la guardada");
    assert_eq!(a.secrets_asked(), 1);
    assert_eq!(ok.to_save.len(), 1);

    // Tres fallos -> error, nada que guardar.
    let a = answers(false, &["a", "b", "c"]);
    let err = ssh::connect(&s, Credentials::default(), &prompter(a.clone()), &quiet)
        .await
        .err()
        .expect("debe fallar");
    assert!(
        format!("{err:#}").contains("contraseña incorrecta"),
        "{err:#}"
    );
    assert_eq!(a.secrets_asked(), 3);

    // Cancelar el diálogo aborta la conexión.
    let a = answers(false, &[]);
    let err = ssh::connect(&s, Credentials::default(), &prompter(a.clone()), &quiet)
        .await
        .err()
        .expect("debe fallar");
    assert!(format!("{err:#}").contains("cancelada"), "{err:#}");

    // --- Servidor solo keyboard-interactive (contraseña + código) -------
    let s = session(start(&[MethodKind::KeyboardInteractive]).await);
    // La contraseña guardada rellena el campo oculto; el código se pregunta.
    let a = answers(true, &[OTP]);
    let creds = Credentials {
        password: Some(PASSWORD.into()),
        ..Default::default()
    };
    ssh::connect(&s, creds, &prompter(a.clone()), &quiet)
        .await
        .expect("keyboard-interactive");
    assert_eq!(a.secrets_asked(), 1);
    assert!(a.messages.lock().unwrap()[0].contains("Código"));
}
