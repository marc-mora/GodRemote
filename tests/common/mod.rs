//! Utilidades compartidas por las pruebas de integración.
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use godremote::ssh::{PromptRequest, Prompter, SecretAnswer};

/// Respuestas automáticas a las preguntas que hace una conexión.
pub struct Answers {
    pub accept_host: bool,
    pub secrets: Mutex<Vec<String>>,
    pub host_prompts: AtomicUsize,
    pub secret_prompts: AtomicUsize,
    /// Textos de las preguntas recibidas.
    pub messages: Mutex<Vec<String>>,
}

impl Answers {
    pub fn hosts(&self) -> usize {
        self.host_prompts.load(Ordering::SeqCst)
    }

    pub fn secrets_asked(&self) -> usize {
        self.secret_prompts.load(Ordering::SeqCst)
    }
}

pub fn answers(accept_host: bool, secrets: &[&str]) -> Arc<Answers> {
    Arc::new(Answers {
        accept_host,
        secrets: Mutex::new(secrets.iter().map(|s| s.to_string()).collect()),
        host_prompts: AtomicUsize::new(0),
        secret_prompts: AtomicUsize::new(0),
        messages: Mutex::new(vec![]),
    })
}

/// Crea un `Prompter` que contesta solo, como lo haría el usuario.
pub fn prompter(answers: Arc<Answers>) -> Prompter {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(req) = rx.recv() {
            match req {
                PromptRequest::HostKey {
                    reply, fingerprint, ..
                } => {
                    assert!(fingerprint.starts_with("SHA256:"));
                    answers.host_prompts.fetch_add(1, Ordering::SeqCst);
                    let _ = reply.send(answers.accept_host);
                }
                PromptRequest::Secret { reply, message, .. } => {
                    answers.secret_prompts.fetch_add(1, Ordering::SeqCst);
                    answers.messages.lock().unwrap().push(message);
                    let mut s = answers.secrets.lock().unwrap();
                    let answer = if s.is_empty() {
                        None
                    } else {
                        Some(SecretAnswer {
                            value: s.remove(0),
                            save: true,
                        })
                    };
                    let _ = reply.send(answer);
                }
            }
        }
    });
    Prompter::new(tx, Arc::new(|| {}))
}

/// Proceso hijo que se mata al salir del ámbito.
pub struct Killed(pub std::process::Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Espera a que un puerto local acepte conexiones.
pub fn wait_port(port: u16) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            std::time::Instant::now() < deadline,
            "el puerto {port} no se abrió"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Busca un ejecutable en las rutas habituales del sistema.
pub fn find_program(name: &str) -> Option<std::path::PathBuf> {
    ["/usr/sbin", "/usr/bin", "/sbin", "/bin", "/usr/local/bin"]
        .iter()
        .map(|dir| std::path::Path::new(dir).join(name))
        .find(|p| p.exists())
}

/// `true` si las pruebas se ejecutan como root (necesario para `sshd`).
#[cfg(unix)]
pub fn is_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

/// Arranca un `sshd` de OpenSSH de usar y tirar en 127.0.0.1. Devuelve el
/// proceso, el puerto y el fichero donde escribe su registro.
#[cfg(unix)]
pub fn start_sshd(
    dir: &std::path::Path,
    authorized_keys: &std::path::Path,
    extra_config: &str,
) -> (Killed, u16, std::path::PathBuf) {
    use godremote::keys::{self, KeyAlgorithm};
    let sshd = find_program("sshd").expect("sshd");
    keys::generate(dir, "host_key", KeyAlgorithm::Ed25519, "", None).unwrap();
    let port = free_port();
    let config = format!(
        "Port {port}\nListenAddress 127.0.0.1\nHostKey {host}\nPidFile {pid}\n\
         AuthorizedKeysFile {auth}\nStrictModes no\nPermitRootLogin prohibit-password\n\
         PubkeyAuthentication yes\nPasswordAuthentication no\n\
         KbdInteractiveAuthentication no\nUsePAM no\n\
         MaxStartups 200:30:400\nMaxAuthTries 20\nMaxSessions 100\n{extra_config}\n",
        host = dir.join("host_key").display(),
        pid = dir.join("sshd.pid").display(),
        auth = authorized_keys.display(),
    );
    let cfg = dir.join("sshd_config");
    std::fs::write(&cfg, config).unwrap();
    std::fs::create_dir_all("/run/sshd").ok();
    let log_path = dir.join("sshd.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let child = std::process::Command::new(sshd)
        .args(["-D", "-e", "-f"])
        .arg(&cfg)
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    let guard = Killed(child);
    wait_port(port);
    (guard, port, log_path)
}

/// Servidor X virtual (`Xvfb`) de usar y tirar. Al soltarlo se detiene y se
/// borran su socket y su fichero de bloqueo, para no ir agotando números de
/// pantalla entre ejecuciones.
#[cfg(unix)]
pub struct XDisplay {
    pub number: u32,
    _process: Killed,
}

#[cfg(unix)]
impl XDisplay {
    /// Valor para la variable `DISPLAY`.
    pub fn name(&self) -> String {
        format!(":{}", self.number)
    }
}

#[cfg(unix)]
impl Drop for XDisplay {
    fn drop(&mut self) {
        let _ = self._process.0.kill();
        let _ = self._process.0.wait();
        let _ = std::fs::remove_file(format!("/tmp/.X11-unix/X{}", self.number));
        let _ = std::fs::remove_file(format!("/tmp/.X{}-lock", self.number));
    }
}

/// Número de pantalla X libre (ni socket ni fichero de bloqueo).
#[cfg(unix)]
pub fn free_display() -> u32 {
    (60..100)
        .find(|n| {
            !std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
                && !std::path::Path::new(&format!("/tmp/.X{n}-lock")).exists()
        })
        .expect("no queda ningún número de pantalla libre")
}

/// Arranca un `Xvfb` en la pantalla indicada y espera a que acepte
/// conexiones. `extra` son argumentos adicionales (p. ej. `-auth fichero`).
#[cfg(unix)]
pub fn start_xvfb(number: u32, size: &str, extra: &[&str]) -> XDisplay {
    let child = std::process::Command::new("Xvfb")
        .arg(format!(":{number}"))
        // `-noreset`: sin él, el servidor se reinicia al irse su último
        // cliente y expulsa a los que estén conectándose en ese instante.
        .args(["-screen", "0", size, "-nolisten", "tcp", "-noreset"])
        .args(extra)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("Xvfb");
    let display = XDisplay {
        number,
        _process: Killed(child),
    };
    let socket = format!("/tmp/.X11-unix/X{number}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        assert!(std::time::Instant::now() < deadline, "Xvfb no arrancó");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    display
}

/// Lanza un `xterm` en la pantalla indicada y espera a que tenga ventana.
///
/// En entornos de pruebas muy cargados el xterm a veces muere al arrancar;
/// se reintenta unas pocas veces dejando constancia en la salida, y si no
/// hay manera se falla con todo el detalle disponible.
#[cfg(unix)]
pub fn start_xterm(display: &str, args: &[&str], log: &std::path::Path) -> Killed {
    let search = |display: &str| {
        std::process::Command::new("xdotool")
            .env("DISPLAY", display)
            .args(["search", "--class", "xterm"])
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false)
    };
    let mut last = String::new();
    for attempt in 1..=4 {
        let mut child = Killed(
            std::process::Command::new("xterm")
                .env("DISPLAY", display)
                .args(args)
                .stdout(std::process::Stdio::null())
                .stderr(std::fs::File::create(log).expect("registro del xterm"))
                .spawn()
                .expect("xterm"),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if search(display) {
                return child;
            }
            let exited = child.0.try_wait().ok().flatten();
            if exited.is_some() || std::time::Instant::now() > deadline {
                last = format!(
                    "intento {attempt}: estado {exited:?}: {}",
                    std::fs::read_to_string(log).unwrap_or_default().trim()
                );
                eprintln!(
                    "AVISO: el xterm de pruebas no arrancó en {display} ({last}); se reintenta"
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    panic!("el xterm no llegó a abrir su ventana en {display}: {last}");
}
