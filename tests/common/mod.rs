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
         KbdInteractiveAuthentication no\nUsePAM no\n{extra_config}\n",
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
