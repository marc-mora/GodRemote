//! Prueba de extremo a extremo del cliente RDP integrado contra un servidor
//! real: `freerdp-shadow-cli3`, que comparte por RDP un escritorio X (aquí un
//! Xvfb propio, con un xterm de fondo conocido que recoge lo tecleado).
//!
//! Se omite si falta `freerdp-shadow-cli3`, `Xvfb` o alguna de las
//! utilidades de X que se usan para comprobar el resultado (`xterm`,
//! `xdotool`, `xev`).
#![cfg(unix)]

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Answers, answers, prompter};
use godremote::desktop::{DesktopBackend, DesktopEvent, DesktopInput, KeyCode, MouseButton};
use godremote::model::{Protocol, Session};
use godremote::{paths, rdp_client, trust};
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// Usuario y contraseña de la base SAM del servidor de prueba.
const USER: &str = "probador";
const PASSWORD: &str = "clave-Ñ-42";
/// Fondo del xterm: lo que debe verse en la imagen remota.
const BACKGROUND: [u8; 3] = [0x20, 0x60, 0xC0];
const SCREEN: (u32, u32) = (1024, 768);

/// Margen para lo que depende de otros procesos (la máquina puede ir cargada).
const PATIENCE: Duration = Duration::from_secs(30);

struct Proc(Child);

impl Proc {
    /// Mata el proceso sin miramientos, como si se hubiera caído.
    fn crash(mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        // Si ya terminó (y se recogió) no hay a quién avisar.
        if !matches!(self.0.try_wait(), Ok(None)) {
            return;
        }
        // Primero por las buenas: así Xvfb retira su cerrojo y su socket.
        let _ = Command::new("kill").arg(self.0.id().to_string()).status();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.0.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn find(names: &[&str]) -> Option<PathBuf> {
    let dirs = ["/usr/bin", "/usr/local/bin", "/bin"];
    names
        .iter()
        .flat_map(|name| dirs.iter().map(move |dir| Path::new(dir).join(name)))
        .find(|path| path.exists())
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Arranca un Xvfb en el primer número de pantalla libre (lo elige él).
fn start_xvfb(xvfb: &Path) -> (Proc, String) {
    let mut child = Command::new(xvfb)
        .args(["-displayfd", "1", "-screen", "0"])
        .arg(format!("{}x{}x24", SCREEN.0, SCREEN.1))
        // `-noreset`: sin él, el servidor se reinicia al irse su último
        // cliente (p. ej. un `xdotool` de comprobación) y expulsa al xterm
        // que esté arrancando en ese instante.
        .args(["-nolisten", "tcp", "-noreset"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Escribe el número de pantalla cuando ya admite conexiones.
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let guard = Proc(child);
    let number: u32 = line.trim().parse().expect("Xvfb no arrancó");
    (guard, format!(":{number}"))
}

/// Hash NT (MD4 del UTF-16LE) de una contraseña, como lo guarda un SAM.
fn nt_hash(password: &str) -> String {
    use md4::{Digest, Md4};
    let utf16: Vec<u8> = password
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    Md4::digest(&utf16)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect()
}

/// Arranca el servidor de escritorio compartido sobre `display`.
fn start_shadow(shadow: &Path, display: &str, dir: &Path, port: u16, args: &[&str]) -> Proc {
    // Cada servidor genera su propio certificado en su carpeta de configuración.
    std::fs::create_dir_all(dir.join("config")).unwrap();
    let sam = dir.join("sam");
    std::fs::write(&sam, format!("{USER}:::{}:::\n", nt_hash(PASSWORD))).unwrap();
    let log = std::fs::File::create(dir.join("shadow.log")).unwrap();
    let child = Command::new(shadow)
        .arg(format!("/port:{port}"))
        .arg("/bind-address:127.0.0.1")
        .arg(format!("/sam-file:{}", sam.display()))
        .args(args)
        .env("DISPLAY", display)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    let guard = Proc(child);
    let deadline = Instant::now() + PATIENCE;
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "el servidor RDP no arrancó");
        std::thread::sleep(Duration::from_millis(50));
    }
    guard
}

fn xdotool(display: &str, args: &[&str]) -> String {
    let out = Command::new("xdotool")
        .args(args)
        .env("DISPLAY", display)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Espera a que se cumpla una condición.
fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !check() {
        assert!(Instant::now() < deadline, "no ocurrió a tiempo: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Presenta siempre el mismo certificado, sea de quien sea la clave.
#[derive(Debug)]
struct FixedCertificate(Arc<rustls::sign::CertifiedKey>);

impl rustls::server::ResolvesServerCert for FixedCertificate {
    fn resolve(
        &self,
        _hello: rustls::server::ClientHello<'_>,
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.clone())
    }
}

/// Un «servidor RDP» mínimo: acepta la negociación eligiendo TLS, hace el
/// intercambio TLS presentando `certificate` y firmando con `key` (que puede
/// no ser la suya) y corta. Devuelve su puerto.
fn tls_endpoint(certificate: CertificateDer<'static>, key: PrivateKeyDer<'static>) -> u16 {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let signer = provider.key_provider.load_private_key(key).unwrap();
    let certified = rustls::sign::CertifiedKey::new(vec![certificate], signer);
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(FixedCertificate(Arc::new(certified)))),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
            let mut buf = [0u8; 1024];
            // Petición de conexión X.224 → confirmación con PROTOCOL_SSL.
            if stream.read(&mut buf).unwrap_or(0) == 0 {
                continue;
            }
            let confirm = [
                0x03, 0x00, 0x00, 0x13, // TPKT, 19 bytes
                0x0E, 0xD0, 0x00, 0x00, 0x12, 0x34, 0x00, // X.224 Connection Confirm
                0x02, 0x00, 0x08, 0x00, 0x01, 0x00, 0x00, 0x00, // RDP_NEG_RSP: TLS
            ];
            if stream.write_all(&confirm).is_err() {
                continue;
            }
            let Ok(connection) = rustls::ServerConnection::new(config.clone()) else {
                continue;
            };
            // Leer completa el intercambio TLS (y lo primero que mande el
            // cliente, si llega a mandar algo); después se corta.
            let mut tls = rustls::StreamOwned::new(connection, stream);
            let _ = tls.read(&mut buf);
        }
    });
    port
}

/// Lo que cuenta una sesión hasta que se establece o se cierra.
#[derive(Default)]
struct Seen {
    status: Vec<String>,
    saved: Vec<(String, String)>,
    connected: bool,
    closed: Option<Option<String>>,
}

/// Consume avisos hasta `Connected` o `Closed`.
fn wait_outcome(backend: &DesktopBackend) -> Seen {
    let mut seen = Seen::default();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let event = backend
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|_| panic!("la sesión no respondió; estado: {:?}", seen.status));
        match event {
            DesktopEvent::Status(text) => seen.status.push(text),
            DesktopEvent::SaveSecrets(secrets) => {
                assert!(!seen.connected, "los secretos se avisan antes de Connected");
                seen.saved.extend(secrets);
            }
            DesktopEvent::Connected => {
                seen.connected = true;
                return seen;
            }
            DesktopEvent::Closed(error) => {
                seen.closed = Some(error);
                return seen;
            }
            DesktopEvent::ClipboardText(_) => {}
        }
    }
}

/// Espera el cierre de una sesión ya establecida y devuelve su error.
fn wait_closed(backend: &DesktopBackend) -> Option<String> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        match backend
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(DesktopEvent::Closed(error)) => return error,
            Ok(_) => {}
            Err(_) => panic!("la sesión no se cerró"),
        }
    }
}

/// Fracción de píxeles de la imagen remota con el color dado (con un pequeño
/// margen: el códec RemoteFX tiene pérdidas).
fn share_of(backend: &DesktopBackend, rgb: [u8; 3]) -> f64 {
    let fb = backend.frame.lock().unwrap();
    let total = fb.pixels.len() / 4;
    let hits = fb
        .pixels
        .chunks_exact(4)
        .filter(|px| (0..3).all(|i| px[i].abs_diff(rgb[i]) <= 6))
        .count();
    hits as f64 / total.max(1) as f64
}

struct Rig {
    rt: tokio::runtime::Runtime,
    session: Session,
}

impl Rig {
    fn connect(&self, password: Option<&str>, a: &Arc<Answers>) -> DesktopBackend {
        rdp_client::spawn(
            self.rt.handle(),
            self.session.clone(),
            password.map(str::to_string),
            (800, 600),
            prompter(a.clone()),
        )
    }

    fn with_port(&self, port: u16) -> Session {
        Session {
            port,
            ..self.session.clone()
        }
    }
}

fn close(backend: &DesktopBackend) {
    backend.input.send(DesktopInput::Close).unwrap();
    assert_eq!(wait_closed(backend), None, "el cierre pedido es limpio");
}

#[test]
fn rdp_embedded_client_end_to_end() {
    let (Some(shadow), Some(xvfb)) = (find(&["freerdp-shadow-cli3"]), find(&["Xvfb"])) else {
        eprintln!("OMITIDA: hacen falta freerdp-shadow-cli3 y Xvfb");
        return;
    };
    if let Some(missing) = ["xterm", "xdotool", "xev", "stdbuf"]
        .into_iter()
        .find(|tool| find(&[tool]).is_none())
    {
        eprintln!("OMITIDA: hace falta {missing}");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    // SAFETY: único test de este binario; aún no hay otros hilos.
    unsafe {
        std::env::set_var("GODREMOTE_HOME", dir.path().join("godremote"));
        std::env::set_var("GODREMOTE_KNOWN_HOSTS", dir.path().join("known_hosts"));
    }
    let known = paths::tls_known_hosts_file();

    let (_xvfb, display) = start_xvfb(&xvfb);
    // Un xterm que ocupa casi toda la pantalla, con un fondo reconocible y
    // que guarda en un fichero todo lo que se le teclea.
    let typed = dir.path().join("tecleado.txt");
    let _xterm = common::start_xterm(
        &display,
        &[
            "-bg",
            "#2060c0",
            "-fg",
            "white",
            "-b",
            "0",
            "-bw",
            "0",
            "-geometry",
            "160x55+0+0",
            "-e",
            &format!("cat > {}", typed.display()),
        ],
        &dir.path().join("xterm.log"),
    );

    let port = free_port();
    let _server = start_shadow(
        &shadow,
        &display,
        &dir.path().join("nla"),
        port,
        &["/sec:nla"],
    );
    let rig = Rig {
        rt: tokio::runtime::Runtime::new().unwrap(),
        session: Session {
            name: "prueba".into(),
            protocol: Protocol::Rdp,
            host: "127.0.0.1".into(),
            port,
            username: USER.into(),
            ..Default::default()
        },
    };

    // 1. Certificado desconocido rechazado: ni conecta ni se recuerda, y no
    //    se llega a pedir la contraseña.
    let a = answers(false, &[PASSWORD]);
    let seen = wait_outcome(&rig.connect(None, &a));
    let error = seen.closed.flatten().expect("debe fallar");
    assert!(error.contains("Certificado rechazado"), "{error}");
    assert_eq!((a.hosts(), a.secrets_asked()), (1, 0));
    assert!(!known.exists());

    // 2. Primera conexión: una pregunta por el certificado, contraseña del
    //    baúl (no se pregunta ni se pide guardar nada).
    let a = answers(true, &[]);
    let backend = rig.connect(Some(PASSWORD), &a);
    let seen = wait_outcome(&backend);
    assert!(seen.connected, "cierre inesperado: {:?}", seen.closed);
    assert_eq!((a.hosts(), a.secrets_asked()), (1, 0));
    assert!(seen.saved.is_empty());
    assert!(
        seen.status[0].starts_with(&format!("Conectando a 127.0.0.1:{port}")),
        "{:?}",
        seen.status
    );
    for expected in ["Negociando seguridad…", "Autenticando…"] {
        assert!(
            seen.status.iter().any(|s| s == expected),
            "{:?}",
            seen.status
        );
    }
    let remembered = std::fs::read_to_string(&known).unwrap();
    assert!(
        remembered.contains(&format!("127.0.0.1:{port}")) && remembered.contains("SHA256:"),
        "{remembered}"
    );
    // 3. Gráficos: llega la imagen, con el fondo del xterm y alfa opaco. El
    //    servidor impone el tamaño de la pantalla que comparte (reactivando
    //    la sesión), no el que se le pidió.
    eventually("imagen del escritorio remoto", || {
        share_of(&backend, BACKGROUND) > 0.5
    });
    {
        let fb = backend.frame.lock().unwrap();
        assert_eq!((fb.width(), fb.height()), SCREEN);
        assert!(fb.pixels.chunks_exact(4).all(|px| px[3] == 255));
    }
    assert!(backend.frame.lock().unwrap().take_dirty().is_some());

    // 4. Ratón: el puntero se mueve en el servidor X.
    backend
        .input
        .send(DesktopInput::PointerMove { x: 321, y: 123 })
        .unwrap();
    eventually("movimiento del puntero", || {
        xdotool(&display, &["getmouselocation"]).starts_with("x:321 y:123 ")
    });

    // 5. Teclado por códigos de exploración: «Hola» (con Mayús) y Enter.
    let key = |key, pressed| {
        backend
            .input
            .send(DesktopInput::Key { key, pressed })
            .unwrap()
    };
    let tap = |k| {
        key(k, true);
        key(k, false);
    };
    key(KeyCode::Shift, true);
    tap(KeyCode::Letter(b'h'));
    key(KeyCode::Shift, false);
    for letter in *b"ola" {
        tap(KeyCode::Letter(letter));
    }
    tap(KeyCode::Digit(7));
    tap(KeyCode::Enter);
    let typed_text = || std::fs::read_to_string(&typed).unwrap_or_default();
    eventually("texto tecleado con códigos de exploración", || {
        typed_text().contains("Hola7\n")
    });

    // 6. Texto ya compuesto. Lo que existe en un teclado estadounidense se
    //    teclea (con su Mayús) y llega tal cual a la aplicación…
    let ascii = "Texto: a-Z_0/9 (ok) ~!@#$%^&*+=[]{};'\\|,.<>?\"`\n";
    backend
        .input
        .send(DesktopInput::Text(ascii.into()))
        .unwrap();
    eventually("texto ASCII enviado como Text", || {
        typed_text().contains(ascii)
    });
    //    …respetando la Mayús que la interfaz mantenga pulsada.
    key(KeyCode::Shift, true);
    backend
        .input
        .send(DesktopInput::Text("Mayus".into()))
        .unwrap();
    key(KeyCode::Shift, false);
    backend
        .input
        .send(DesktopInput::Text(" suelta\n".into()))
        .unwrap();
    eventually("texto con Mayús pulsada", || {
        typed_text().contains("Mayus suelta\n")
    });
    //    El resto viaja como eventos de teclado Unicode (pulsar y soltar por
    //    cada unidad UTF-16; «😀» ocupa dos). El servidor de escritorio
    //    compartido de FreeRDP los recibe pero no sabe inyectarlos en X
    //    («not implemented»), así que aquí solo se puede comprobar que
    //    llegan todos: o aparece el texto, o constan en su registro.
    let unicode_events = || {
        std::fs::read_to_string(dir.path().join("nla/shadow.log"))
            .unwrap_or_default()
            .matches("unicode_keyboard_event")
            .count()
    };
    assert_eq!(unicode_events(), 0);
    backend
        .input
        .send(DesktopInput::Text("ñ€😀".into()))
        .unwrap();
    eventually("eventos de teclado Unicode", || {
        typed_text().contains("ñ€😀") || unicode_events() == 2 * 4
    });

    // 7. Botones y rueda, sobre la zona de la pantalla sin ventanas (donde
    //    `xev` puede ver los eventos de la ventana raíz). El primer clic va
    //    a una posición a la que no se había movido el puntero.
    let xev_log = dir.path().join("xev.log");
    let _xev = Proc(
        Command::new("stdbuf")
            .args(["-oL", "xev", "-root", "-event", "button"])
            .env("DISPLAY", &display)
            .stdout(std::fs::File::create(&xev_log).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    std::thread::sleep(Duration::from_millis(500));
    let (x, y) = (1000, 750);
    let click = |button| {
        for pressed in [true, false] {
            backend
                .input
                .send(DesktopInput::PointerButton {
                    button,
                    pressed,
                    x,
                    y,
                })
                .unwrap();
        }
    };
    click(MouseButton::Left);
    click(MouseButton::Right);
    click(MouseButton::Middle);
    for delta in [-3, 1] {
        backend
            .input
            .send(DesktopInput::Wheel {
                x,
                y,
                delta,
                horizontal: false,
            })
            .unwrap();
    }
    // Cada evento de xev es un párrafo; se cuentan por botón de X (4 y 5
    // son la rueda hacia arriba y hacia abajo).
    let buttons = |kind: &str, button: u8| {
        let log = std::fs::read_to_string(&xev_log).unwrap_or_default();
        log.split("\n\n")
            .filter(|event| {
                event.contains(kind)
                    && event.contains(&format!("({x},{y})"))
                    && event.contains(&format!("button {button},"))
            })
            .count()
    };
    eventually("botones y rueda en el servidor X", || {
        (1..=5).all(|b| buttons("ButtonRelease", b) >= 1) && buttons("ButtonRelease", 5) == 3
    });
    for (button, times) in [(1, 1), (2, 1), (3, 1), (4, 1), (5, 3)] {
        assert_eq!(buttons("ButtonPress", button), times, "botón {button}");
        assert_eq!(buttons("ButtonRelease", button), times, "botón {button}");
    }

    //    El servidor no tiene canal de portapapeles: avisarle de una copia
    //    local no hace nada (ni rompe nada).
    backend
        .input
        .send(DesktopInput::ClipboardText("copiado en local".into()))
        .unwrap();
    backend
        .input
        .send(DesktopInput::PointerMove { x: 5, y: 6 })
        .unwrap();
    eventually("sesión viva tras el aviso de portapapeles", || {
        xdotool(&display, &["getmouselocation"]).starts_with("x:5 y:6 ")
    });

    // 8. Cierre pedido por la interfaz.
    close(&backend);

    // 9. Segunda conexión: el certificado ya es de confianza. Sin contraseña
    //    en el baúl se pregunta una vez y, como se pidió guardarla, se avisa
    //    solo cuando la sesión está establecida.
    let a = answers(false, &[PASSWORD]);
    let backend = rig.connect(None, &a);
    let seen = wait_outcome(&backend);
    assert!(seen.connected, "cierre inesperado: {:?}", seen.closed);
    assert_eq!((a.hosts(), a.secrets_asked()), (0, 1));
    assert_eq!(
        a.messages.lock().unwrap()[0],
        format!("Contraseña de {USER} en 127.0.0.1")
    );
    assert_eq!(
        seen.saved,
        vec![(rig.session.password_key(), PASSWORD.to_string())]
    );
    // Soltar el canal de entrada también termina la sesión.
    let DesktopBackend { input, events, .. } = backend;
    drop(input);
    let deadline = Instant::now() + PATIENCE;
    loop {
        match events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(DesktopEvent::Closed(error)) => {
                assert_eq!(error, None);
                break;
            }
            Ok(_) => {}
            Err(_) => panic!("la sesión no terminó al soltar la entrada"),
        }
    }

    // 10. Contraseña incorrecta (la del baúl): el servidor la rechaza, se
    //     pregunta de nuevo y, si el usuario cancela, no hay sesión ni nada
    //     que guardar.
    let a = answers(false, &[]);
    let seen = wait_outcome(&rig.connect(Some("incorrecta"), &a));
    let error = seen.closed.flatten().expect("debe fallar");
    assert!(error.contains("Autenticación cancelada"), "{error}");
    assert!(
        seen.status
            .iter()
            .any(|s| s.contains("rechazó la contraseña")),
        "{:?}",
        seen.status
    );
    assert_eq!((a.hosts(), a.secrets_asked()), (0, 1));
    assert!(
        a.messages.lock().unwrap()[0].starts_with("Contraseña incorrecta."),
        "{:?}",
        a.messages.lock().unwrap()
    );
    assert!(seen.saved.is_empty());

    //     Incorrecta y después la buena: conecta y guarda solo la buena.
    let a = answers(false, &["otra mala", PASSWORD]);
    let backend = rig.connect(Some("incorrecta"), &a);
    let seen = wait_outcome(&backend);
    assert!(seen.connected, "cierre inesperado: {:?}", seen.closed);
    assert_eq!((a.hosts(), a.secrets_asked()), (0, 2));
    assert_eq!(
        seen.saved,
        vec![(rig.session.password_key(), PASSWORD.to_string())]
    );
    close(&backend);

    //     Siempre incorrecta: se desiste al tercer rechazo.
    let a = answers(false, &["mala 2", "mala 3", "sobra"]);
    let seen = wait_outcome(&rig.connect(Some("mala 1"), &a));
    let error = seen.closed.flatten().expect("debe fallar");
    assert!(error.contains("Autenticación fallida"), "{error}");
    assert_eq!(a.secrets_asked(), 2);
    assert!(seen.saved.is_empty());

    // 11. El certificado recordado es otro: se rechaza sin preguntar nada
    //     (ni confianza ni contraseña) y sin tocar lo recordado.
    let genuine = std::fs::read_to_string(&known).unwrap();
    let forged = format!("SHA256:{}", "0".repeat(64));
    trust::remember(&known, "127.0.0.1", port, &forged).unwrap();
    let a = answers(true, &[PASSWORD]);
    let seen = wait_outcome(&rig.connect(None, &a));
    let error = seen.closed.flatten().expect("debe fallar");
    assert!(
        error.contains("HA CAMBIADO") && error.contains("tls_known_hosts.json"),
        "{error}"
    );
    assert!(error.contains(&forged), "{error}");
    assert_eq!((a.hosts(), a.secrets_asked()), (0, 0));
    assert!(std::fs::read_to_string(&known).unwrap().contains(&forged));
    std::fs::write(&known, genuine).unwrap();

    //     Sin usuario no hay con qué autenticarse ante un servidor con NLA:
    //     se dice sin preguntar nada.
    let anonymous = Session {
        username: String::new(),
        ..rig.session.clone()
    };
    let a = answers(true, &[PASSWORD]);
    let backend = rdp_client::spawn(
        rig.rt.handle(),
        anonymous.clone(),
        None,
        (800, 600),
        prompter(a.clone()),
    );
    let error = wait_outcome(&backend)
        .closed
        .flatten()
        .expect("debe fallar");
    assert!(error.contains("indica el usuario"), "{error}");
    assert_eq!((a.hosts(), a.secrets_asked()), (0, 0));

    // 12. Servidor sin NLA (solo TLS, sin autenticación propia): mismo
    //     circuito de confianza y sesión sin CredSSP.
    let tls_port = free_port();
    let tls_server = start_shadow(
        &shadow,
        &display,
        &dir.path().join("tls"),
        tls_port,
        &["/sec:tls", "-auth"],
    );
    let a = answers(true, &[]);
    let backend = rdp_client::spawn(
        rig.rt.handle(),
        rig.with_port(tls_port),
        Some("da igual".into()),
        (800, 600),
        prompter(a.clone()),
    );
    let seen = wait_outcome(&backend);
    assert!(seen.connected, "cierre inesperado: {:?}", seen.closed);
    assert_eq!((a.hosts(), a.secrets_asked()), (1, 0));
    eventually("imagen por el servidor solo TLS", || {
        share_of(&backend, BACKGROUND) > 0.5
    });
    close(&backend);

    //     Sin usuario ni contraseña tampoco se pregunta: en un servidor así
    //     la sesión se inicia en su propia pantalla.
    let a = answers(false, &[]);
    let backend = rdp_client::spawn(
        rig.rt.handle(),
        Session {
            port: tls_port,
            ..anonymous
        },
        None,
        (800, 600),
        prompter(a.clone()),
    );
    let seen = wait_outcome(&backend);
    assert!(seen.connected, "cierre inesperado: {:?}", seen.closed);
    assert_eq!((a.hosts(), a.secrets_asked()), (0, 0));
    assert!(seen.saved.is_empty());

    // 13. El servidor desaparece en plena sesión: cierre con error.
    eventually("imagen antes de la caída del servidor", || {
        share_of(&backend, BACKGROUND) > 0.5
    });
    tls_server.crash();
    let error = wait_closed(&backend).expect("debe acabar con error");
    assert!(error.contains("servidor"), "{error}");

    // 14. Lo primero que sale por la red (la petición de conexión, en claro
    //     y antes de saber quién es el servidor) no lleva ni el usuario ni,
    //     por supuesto, la contraseña.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let backend = rdp_client::spawn(
        rig.rt.handle(),
        rig.with_port(listener.local_addr().unwrap().port()),
        Some(PASSWORD.into()),
        (800, 600),
        prompter(answers(true, &[])),
    );
    let (mut peer, _) = listener.accept().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut request = [0u8; 512];
    let received = peer.read(&mut request).unwrap();
    let request = String::from_utf8_lossy(&request[..received]).to_lowercase();
    assert!(received > 0);
    assert!(
        !request.contains(USER) && !request.contains("mstshash") && !request.contains("clave"),
        "{request:?}"
    );
    drop(peer);
    let error = wait_outcome(&backend)
        .closed
        .flatten()
        .expect("debe fallar");
    assert!(
        error.contains("Fallo al negociar la conexión: el servidor cerró la conexión"),
        "{error}"
    );

    // 15. Suplantación con el certificado auténtico: alguien que presenta un
    //     certificado ya recordado pero no tiene su clave privada (firma el
    //     intercambio TLS con otra) no pasa de TLS, aunque la huella coincida.
    //     Los certificados y claves son los que generaron los dos servidores.
    let identity = |name: &str| {
        let base = dir.path().join(name).join("config/freerdp/shadow");
        (
            CertificateDer::from_pem_file(base.join("shadow.crt")).unwrap(),
            PrivateKeyDer::from_pem_file(base.join("shadow.key")).unwrap(),
        )
    };
    let (genuine_cert, genuine_key) = identity("nla");
    let (_, other_key) = identity("tls");
    //     Control: con su propia clave, TLS y la confianza funcionan y el
    //     fallo llega después (el servidor de pega no sabe seguir).
    let honest = tls_endpoint(genuine_cert.clone(), genuine_key);
    let a = answers(true, &[]);
    let backend = rdp_client::spawn(
        rig.rt.handle(),
        rig.with_port(honest),
        Some(PASSWORD.into()),
        (800, 600),
        prompter(a.clone()),
    );
    let seen = wait_outcome(&backend);
    let error = seen.closed.flatten().expect("debe fallar");
    assert!(
        error.contains("No se pudo establecer la sesión") && !error.contains("TLS"),
        "{error}"
    );
    assert!(seen.status.iter().any(|s| s == "Autenticando…"));
    assert_eq!(a.hosts(), 1);
    //     El impostor: mismo certificado (ya de confianza para su puerto),
    //     clave ajena.
    let impostor = tls_endpoint(genuine_cert.clone(), other_key);
    trust::remember(
        &known,
        "127.0.0.1",
        impostor,
        &trust::fingerprint(&genuine_cert),
    )
    .unwrap();
    let a = answers(true, &[PASSWORD]);
    let backend = rdp_client::spawn(
        rig.rt.handle(),
        rig.with_port(impostor),
        Some(PASSWORD.into()),
        (800, 600),
        prompter(a.clone()),
    );
    let seen = wait_outcome(&backend);
    let error = seen.closed.flatten().expect("debe fallar");
    assert!(error.contains("Fallo en la negociación TLS"), "{error}");
    assert!(!seen.status.iter().any(|s| s == "Autenticando…"));
    assert_eq!((a.hosts(), a.secrets_asked()), (0, 0));

    // 16. Nadie escucha: error de conexión claro.
    let dead = free_port();
    let backend = rdp_client::spawn(
        rig.rt.handle(),
        rig.with_port(dead),
        Some(PASSWORD.into()),
        (800, 600),
        prompter(answers(true, &[])),
    );
    let error = wait_outcome(&backend)
        .closed
        .flatten()
        .expect("debe fallar");
    assert!(error.contains("No se pudo conectar"), "{error}");

    // 17. A través de un salto SSH: el certificado y la contraseña siguen
    //     asociados al servidor RDP real, y la conexión sale del bastión.
    if common::find_program("sshd").is_some() && common::is_root() {
        use godremote::keys::{self, KeyAlgorithm};
        use godremote::model::AuthMethod;
        use godremote::ssh::{Credentials, Endpoint};
        let ssh_dir = dir.path().join("ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        let key = keys::generate(&ssh_dir, "id", KeyAlgorithm::Ed25519, "", None).unwrap();
        let authorized = ssh_dir.join("authorized_keys");
        std::fs::write(&authorized, format!("{}\n", key.public_openssh)).unwrap();
        let (_sshd, ssh_port, sshd_log) = common::start_sshd(
            &ssh_dir,
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
        // El certificado del servidor RDP ya es de confianza (pasos previos):
        // solo debe preguntarse la clave SSH del bastión.
        let a = answers(true, &[]);
        let backend = rdp_client::spawn_via(
            rig.rt.handle(),
            rig.session.clone(),
            Some(PASSWORD.into()),
            (800, 600),
            Endpoint::route([(bastion, Credentials::default())]),
            prompter(a.clone()),
        );
        let seen = wait_outcome(&backend);
        assert!(
            seen.connected,
            "RDP por salto: {:?} {:?}",
            seen.status, seen.closed
        );
        assert_eq!((a.hosts(), a.secrets_asked()), (1, 0));
        eventually("la imagen llega a través del salto", || {
            share_of(&backend, BACKGROUND) > 0.5
        });
        close(&backend);
        let log = std::fs::read_to_string(&sshd_log).unwrap();
        assert!(
            log.lines()
                .any(|l| l.contains("direct") && l.contains(&format!("port {port}"))),
            "el bastión no abrió la conexión RDP:\n{log}"
        );
        eprintln!("RDP a través de un salto SSH: sesión e imagen correctas");
    } else {
        eprintln!("RDP por salto: OMITIDO (hace falta sshd y root)");
    }
}
