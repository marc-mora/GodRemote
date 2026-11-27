//! Conexión SSH: verificación de host, autenticación y shell interactiva.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use russh::client::{self, AuthResult, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::ssh_key::{HashAlg, PrivateKey};
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect, MethodKind};
use tokio::sync::{mpsc, oneshot};

use crate::model::{AuthMethod, Session};
use crate::terminal::{TermBackend, TermCmd, TermEvent};
use crate::tunnels::{self, RemoteForwards};
use crate::x11::X11Forward;

/// Función que despierta la interfaz (pide un repintado).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Estado compartido entre una conexión y sus manejadores de canales.
#[derive(Default)]
pub struct ConnShared {
    /// Reenvíos remotos (`-R`) activos: puerto del servidor → destino local.
    pub remote_forwards: RemoteForwards,
    /// Reenvío X11 activo, si el servidor lo aceptó.
    pub x11: Mutex<Option<Arc<X11Forward>>>,
}

/// Conexión SSH autenticada. Mantiene vivo el salto (bastión) por el que
/// pasa, si lo hay.
pub struct SshConnection {
    handle: client::Handle<ClientHandler>,
    pub shared: Arc<ConnShared>,
    _jump: Option<SshHandle>,
}

impl std::ops::Deref for SshConnection {
    type Target = client::Handle<ClientHandler>;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

/// Conexión SSH autenticada, compartible entre terminal, SFTP y túneles.
pub type SshHandle = Arc<SshConnection>;

pub struct SecretAnswer {
    pub value: String,
    /// El usuario pidió guardar el secreto en el baúl.
    pub save: bool,
}

/// Preguntas que la conexión necesita hacer al usuario.
pub enum PromptRequest {
    /// Host desconocido: ¿confiar en esta clave?
    HostKey {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
        reply: oneshot::Sender<bool>,
    },
    /// Pedir una contraseña / passphrase / respuesta.
    Secret {
        title: String,
        message: String,
        echo: bool,
        allow_save: bool,
        reply: oneshot::Sender<Option<SecretAnswer>>,
    },
}

/// Canal hacia la interfaz para hacer preguntas al usuario.
#[derive(Clone)]
pub struct Prompter {
    tx: std::sync::mpsc::Sender<PromptRequest>,
    waker: Waker,
}

impl Prompter {
    pub fn new(tx: std::sync::mpsc::Sender<PromptRequest>, waker: Waker) -> Self {
        Self { tx, waker }
    }

    pub fn wake(&self) {
        (self.waker)();
    }

    /// Pregunta si se confía en la clave (o certificado) de un servidor
    /// desconocido. `algorithm` describe el tipo de clave o certificado.
    pub async fn confirm_host_key(
        &self,
        host: &str,
        port: u16,
        algorithm: String,
        fingerprint: String,
    ) -> bool {
        let (reply, rx) = oneshot::channel();
        let sent = self.tx.send(PromptRequest::HostKey {
            host: host.to_string(),
            port,
            algorithm,
            fingerprint,
            reply,
        });
        if sent.is_err() {
            return false;
        }
        self.wake();
        rx.await.unwrap_or(false)
    }

    /// Pide un secreto al usuario. `None` si cancela.
    pub async fn secret(
        &self,
        title: &str,
        message: &str,
        echo: bool,
        allow_save: bool,
    ) -> Option<SecretAnswer> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(PromptRequest::Secret {
                title: title.to_string(),
                message: message.to_string(),
                echo,
                allow_save,
                reply,
            })
            .ok()?;
        self.wake();
        rx.await.ok().flatten()
    }
}

/// Secretos ya conocidos (sacados del baúl) para una sesión.
#[derive(Default, Clone)]
pub struct Credentials {
    pub password: Option<String>,
    pub passphrase: Option<String>,
}

/// Sesión con todo lo necesario para conectar: sus secretos y, si hace
/// falta, el equipo intermedio (bastión) por el que hay que pasar.
#[derive(Clone)]
pub struct Endpoint {
    pub session: Session,
    pub creds: Credentials,
    pub via: Option<Box<Endpoint>>,
}

impl Endpoint {
    /// Conexión directa, sin saltos.
    pub fn direct(session: Session, creds: Credentials) -> Self {
        Self {
            session,
            creds,
            via: None,
        }
    }
}

pub struct ClientHandler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    prompter: Prompter,
    shared: Arc<ConnShared>,
}

impl client::Handler for ClientHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key,
            PublicKeyOrCertificate::Certificate(_) => {
                bail!("El servidor presentó un certificado de host; no está soportado")
            }
        };
        match russh::keys::known_hosts::check_known_hosts_path(
            &self.host,
            self.port,
            key,
            &self.known_hosts,
        ) {
            Ok(true) => Ok(true),
            Ok(false) => {
                let accepted = self
                    .prompter
                    .confirm_host_key(
                        &self.host,
                        self.port,
                        key.algorithm().to_string(),
                        key.fingerprint(HashAlg::Sha256).to_string(),
                    )
                    .await;
                if !accepted {
                    bail!("Clave de host rechazada por el usuario");
                }
                russh::keys::known_hosts::learn_known_hosts_path(
                    &self.host,
                    self.port,
                    key,
                    &self.known_hosts,
                )
                .with_context(|| format!("guardando en {}", self.known_hosts.display()))?;
                Ok(true)
            }
            Err(russh::keys::Error::KeyChanged { line }) => bail!(
                "¡LA CLAVE DEL HOST HA CAMBIADO! Puede ser un ataque de intermediario \
                 o que el servidor se haya reinstalado. Huella recibida: {}. \
                 Si el cambio es legítimo, elimina la línea {} de {} y vuelve a conectar.",
                key.fingerprint(HashAlg::Sha256),
                line,
                self.known_hosts.display()
            ),
            Err(e) => Err(anyhow!("error leyendo known_hosts: {e}")),
        }
    }

    /// Conexión entrante por un reenvío remoto (`-R`).
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        _connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let target = self
            .shared
            .remote_forwards
            .lock()
            .unwrap()
            .get(&connected_port)
            .cloned();
        // Solo se aceptan los reenvíos que hemos pedido; el resto se rechaza
        // al soltar `reply`.
        if let Some(target) = target {
            reply.accept().await;
            tokio::spawn(async move {
                let _ = tunnels::serve_remote(channel.into_stream(), target).await;
            });
        }
        Ok(())
    }

    /// Una aplicación gráfica remota abre una conexión X11.
    async fn server_channel_open_x11(
        &mut self,
        channel: russh::Channel<client::Msg>,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let forward = self.shared.x11.lock().unwrap().clone();
        if let Some(forward) = forward {
            reply.accept().await;
            tokio::spawn(async move {
                let _ = forward.bridge(channel.into_stream()).await;
            });
        }
        Ok(())
    }
}

/// Resultado de una conexión correcta.
pub struct Connected {
    pub handle: SshHandle,
    /// Secretos (clave del baúl, valor) que el usuario pidió guardar y que
    /// han sido validados por una autenticación correcta.
    pub to_save: Vec<(String, String)>,
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Máximo de saltos encadenados.
pub const MAX_JUMPS: usize = 8;

/// Conecta y autentica directamente (sin saltos). `status` recibe mensajes
/// de progreso.
pub async fn connect(
    session: &Session,
    creds: Credentials,
    prompter: &Prompter,
    status: &(dyn Fn(String) + Send + Sync),
) -> Result<Connected> {
    connect_via(session, creds, None, prompter, status).await
}

/// Conecta con un destino pasando por sus saltos (bastiones), si los tiene.
pub fn connect_endpoint<'a>(
    endpoint: &'a Endpoint,
    prompter: &'a Prompter,
    status: &'a (dyn Fn(String) + Send + Sync),
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Connected>> + Send + 'a>> {
    Box::pin(async move {
        let mut to_save = vec![];
        let jump = match &endpoint.via {
            Some(via) => {
                let connected = connect_endpoint(via, prompter, status)
                    .await
                    .with_context(|| format!("en el salto {}", via.session.display_name()))?;
                to_save.extend(connected.to_save);
                Some(connected.handle)
            }
            None => None,
        };
        let mut connected = connect_via(
            &endpoint.session,
            endpoint.creds.clone(),
            jump,
            prompter,
            status,
        )
        .await?;
        to_save.append(&mut connected.to_save);
        connected.to_save = to_save;
        Ok(connected)
    })
}

async fn connect_via(
    session: &Session,
    creds: Credentials,
    jump: Option<SshHandle>,
    prompter: &Prompter,
    status: &(dyn Fn(String) + Send + Sync),
) -> Result<Connected> {
    let host = session.host.trim().to_string();
    let user = session.username.trim().to_string();

    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 4,
        ..Default::default()
    });
    let shared = Arc::new(ConnShared::default());
    let handler = ClientHandler {
        host: host.clone(),
        port: session.port,
        known_hosts: crate::paths::known_hosts_file(),
        prompter: prompter.clone(),
        shared: shared.clone(),
    };

    let mut handle = match &jump {
        // A través del bastión: un canal TCP directo hace de transporte.
        Some(jump) => {
            status(format!("Saltando hacia {host}:{}…", session.port));
            let channel = jump
                .channel_open_direct_tcpip(host.clone(), session.port as u32, "127.0.0.1", 0)
                .await
                .map_err(|e| {
                    anyhow!(
                        "El salto no pudo abrir la conexión con {host}:{}: {e}",
                        session.port
                    )
                })?;
            client::connect_stream(config, channel.into_stream(), handler).await?
        }
        None => {
            status(format!("Conectando a {host}:{}…", session.port));
            let stream = tokio::time::timeout(
                CONNECT_TIMEOUT,
                tokio::net::TcpStream::connect((host.as_str(), session.port)),
            )
            .await
            .map_err(|_| {
                anyhow!(
                    "Tiempo de espera agotado conectando a {host}:{}",
                    session.port
                )
            })?
            .with_context(|| format!("No se pudo conectar a {host}:{}", session.port))?;
            let _ = stream.set_nodelay(true);
            client::connect_stream(config, stream, handler).await?
        }
    };

    status(format!("Autenticando como {user}…"));
    let mut to_save = vec![];
    match session.auth {
        AuthMethod::Password => {
            auth_password(
                &mut handle,
                session,
                &user,
                creds,
                prompter,
                status,
                &mut to_save,
            )
            .await?
        }
        AuthMethod::Key => {
            auth_key(&mut handle, session, &user, creds, prompter, &mut to_save).await?
        }
        AuthMethod::Agent => auth_agent(&mut handle, &user).await?,
    }

    Ok(Connected {
        handle: Arc::new(SshConnection {
            handle,
            shared,
            _jump: jump,
        }),
        to_save,
    })
}

async fn auth_password(
    handle: &mut client::Handle<ClientHandler>,
    session: &Session,
    user: &str,
    creds: Credentials,
    prompter: &Prompter,
    status: &(dyn Fn(String) + Send + Sync),
    to_save: &mut Vec<(String, String)>,
) -> Result<()> {
    let mut stored = creds.password;
    let target = format!("{user}@{}", session.host.trim());
    for attempt in 0..3 {
        let (password, save) = match stored.take() {
            Some(p) => (p, false),
            None => {
                let msg = if attempt == 0 {
                    format!("Contraseña para {target}")
                } else {
                    format!("Contraseña incorrecta. Contraseña para {target}")
                };
                match prompter
                    .secret("Autenticación SSH", &msg, false, true)
                    .await
                {
                    Some(a) => (a.value, a.save),
                    None => bail!("Autenticación cancelada"),
                }
            }
        };

        let result = handle.authenticate_password(user, password.clone()).await?;
        let remaining = match result {
            AuthResult::Success => {
                if save {
                    to_save.push((session.password_key(), password));
                }
                return Ok(());
            }
            AuthResult::Failure {
                remaining_methods, ..
            } => remaining_methods,
        };

        // Muchos servidores (PAM) solo aceptan keyboard-interactive.
        if remaining.contains(&MethodKind::KeyboardInteractive)
            && keyboard_interactive(handle, user, &password, prompter).await?
        {
            if save {
                to_save.push((session.password_key(), password));
            }
            return Ok(());
        }
        if !remaining.contains(&MethodKind::Password)
            && !remaining.contains(&MethodKind::KeyboardInteractive)
        {
            bail!("El servidor no admite autenticación por contraseña");
        }
        if !save && attempt == 0 {
            status("La contraseña guardada fue rechazada".into());
        }
    }
    bail!("Autenticación fallida: contraseña incorrecta")
}

/// Autenticación keyboard-interactive. La contraseña conocida se usa una vez
/// para el primer campo oculto; el resto se pregunta al usuario.
async fn keyboard_interactive(
    handle: &mut client::Handle<ClientHandler>,
    user: &str,
    password: &str,
    prompter: &Prompter,
) -> Result<bool> {
    let mut password = Some(password.to_string());
    let mut response = handle
        .authenticate_keyboard_interactive_start(user, None::<String>)
        .await?;
    // Límite defensivo de rondas.
    for _ in 0..10 {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let mut answers = Vec::with_capacity(prompts.len());
                for p in prompts {
                    if !p.echo
                        && let Some(pw) = password.take()
                    {
                        answers.push(pw);
                        continue;
                    }
                    let title = if name.is_empty() {
                        "Autenticación SSH".to_string()
                    } else {
                        name.clone()
                    };
                    let msg = if instructions.is_empty() {
                        p.prompt.clone()
                    } else {
                        format!("{instructions}\n{}", p.prompt)
                    };
                    match prompter.secret(&title, &msg, p.echo, false).await {
                        Some(a) => answers.push(a.value),
                        None => bail!("Autenticación cancelada"),
                    }
                }
                response = handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await?;
            }
        }
    }
    Ok(false)
}

fn key_is_encrypted(text: &str) -> bool {
    if let Ok(key) = PrivateKey::from_openssh(text) {
        return key.is_encrypted();
    }
    text.contains("ENCRYPTED")
        || text
            .lines()
            .any(|l| l.starts_with("Encryption:") && !l.contains("none"))
}

async fn auth_key(
    handle: &mut client::Handle<ClientHandler>,
    session: &Session,
    user: &str,
    creds: Credentials,
    prompter: &Prompter,
    to_save: &mut Vec<(String, String)>,
) -> Result<()> {
    let path = crate::paths::expand_tilde(&session.key_path);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("No se pudo leer la clave {}", path.display()))?;

    let mut pending_save = None;
    let key = if key_is_encrypted(&text) {
        let mut stored = creds.passphrase;
        let mut found = None;
        for attempt in 0..3 {
            let (pass, save) = match stored.take() {
                Some(p) => (p, false),
                None => {
                    let msg = if attempt == 0 {
                        format!("Passphrase de la clave {}", path.display())
                    } else {
                        format!("Passphrase incorrecta. Passphrase de {}", path.display())
                    };
                    match prompter.secret("Clave privada", &msg, false, true).await {
                        Some(a) => (a.value, a.save),
                        None => bail!("Autenticación cancelada"),
                    }
                }
            };
            if let Ok(k) = russh::keys::decode_secret_key(&text, Some(&pass)) {
                if save {
                    pending_save = Some((session.passphrase_key(), pass));
                }
                found = Some(k);
                break;
            }
        }
        found.ok_or_else(|| anyhow!("No se pudo descifrar la clave privada"))?
    } else {
        russh::keys::decode_secret_key(&text, None)
            .map_err(|e| anyhow!("Clave privada no válida ({}): {e}", path.display()))?
    };

    let hash = handle.best_supported_rsa_hash().await?.flatten();
    let result = handle
        .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
        .await?;
    if !result.success() {
        bail!("El servidor rechazó la clave {}", path.display());
    }
    to_save.extend(pending_save);
    Ok(())
}

async fn auth_agent(handle: &mut client::Handle<ClientHandler>, user: &str) -> Result<()> {
    #[cfg(unix)]
    let mut agent = AgentClient::connect_env()
        .await
        .context("No se pudo conectar con el agente SSH (¿SSH_AUTH_SOCK?)")?
        .dynamic();
    #[cfg(windows)]
    let mut agent = match AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
        Ok(a) => a.dynamic(),
        Err(_) => AgentClient::connect_pageant()
            .await
            .context("No se pudo conectar con el agente SSH ni con Pageant")?
            .dynamic(),
    };

    let identities = agent
        .request_identities()
        .await
        .context("El agente no devolvió identidades")?;
    if identities.is_empty() {
        bail!("El agente SSH no tiene ninguna clave cargada");
    }
    let hash = handle.best_supported_rsa_hash().await?.flatten();
    for identity in identities {
        let AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        match handle
            .authenticate_publickey_with(user, key, hash, &mut agent)
            .await
        {
            Ok(AuthResult::Success) => return Ok(()),
            Ok(AuthResult::Failure { .. }) => continue,
            Err(e) => bail!("Error firmando con el agente: {e}"),
        }
    }
    bail!("Ninguna clave del agente fue aceptada por el servidor")
}

/// Abre una conexión y una shell interactiva en segundo plano.
pub fn spawn_shell(
    rt: &tokio::runtime::Handle,
    endpoint: Endpoint,
    prompter: Prompter,
    cols: u16,
    rows: u16,
) -> TermBackend {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    let waker = prompter.waker.clone();
    rt.spawn(async move {
        let emit: Emit = Arc::new(move |ev: TermEvent| {
            let _ = ev_tx.send(ev);
            waker();
        });
        let result = run_shell(&endpoint, &prompter, cmd_rx, cols, rows, emit.clone()).await;
        emit(TermEvent::Closed(result.err().map(|e| format!("{e:#}"))));
    });
    TermBackend {
        cmd: cmd_tx,
        events: ev_rx,
    }
}

type Emit = Arc<dyn Fn(TermEvent) + Send + Sync>;

/// Pide el reenvío X11 en el canal y espera la respuesta del servidor.
async fn request_x11(
    channel: &mut russh::Channel<client::Msg>,
    handle: &SshHandle,
    emit: &Emit,
) -> Result<()> {
    let forward = match X11Forward::prepare() {
        Ok(f) => f,
        Err(e) => {
            emit(TermEvent::Status(format!(
                "Reenvío X11 no disponible: {e:#}"
            )));
            return Ok(());
        }
    };
    channel
        .request_x11(
            true,
            false,
            crate::x11::COOKIE_PROTOCOL,
            forward.fake_cookie_hex(),
            forward.display.screen,
        )
        .await?;
    let accepted = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Success) => break true,
                Some(ChannelMsg::Failure) | Some(ChannelMsg::Close) | None => break false,
                Some(_) => {}
            }
        }
    })
    .await
    .unwrap_or(false);
    if accepted {
        *handle.shared.x11.lock().unwrap() = Some(Arc::new(forward));
        emit(TermEvent::Status("Reenvío X11 activo".into()));
    } else {
        emit(TermEvent::Status(
            "El servidor rechazó el reenvío X11 (¿X11Forwarding yes y xauth instalado?)".into(),
        ));
    }
    Ok(())
}

async fn run_shell(
    endpoint: &Endpoint,
    prompter: &Prompter,
    mut cmd_rx: mpsc::UnboundedReceiver<TermCmd>,
    cols: u16,
    rows: u16,
    emit: Emit,
) -> Result<()> {
    let session = &endpoint.session;
    let status = {
        let emit = emit.clone();
        move |s: String| emit(TermEvent::Status(s))
    };
    let connected = connect_endpoint(endpoint, prompter, &status).await?;
    if !connected.to_save.is_empty() {
        emit(TermEvent::SaveSecrets(connected.to_save));
    }
    let handle = connected.handle;

    // Túneles: viven mientras dure esta shell.
    let tunnel_set = {
        let emit = emit.clone();
        let report: tunnels::Report =
            Arc::new(move |index, status| emit(TermEvent::Tunnel { index, status }));
        tunnels::start(&handle, &session.tunnels, report).await
    };

    let mut channel = handle.channel_open_session().await?;
    if session.x11_forwarding {
        request_x11(&mut channel, &handle, &emit).await?;
    }
    // El tamaño puede haber cambiado mientras se conectaba.
    let (mut cols, mut rows) = (cols, rows);
    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            TermCmd::Resize { cols: c, rows: r } => (cols, rows) = (c, r),
            TermCmd::Close => return Ok(()),
            TermCmd::Input(_) => {}
        }
    }
    channel
        .request_pty(true, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
        .await?;
    channel.request_shell(true).await?;
    emit(TermEvent::Connected(Some(handle.clone())));

    let mut exit_status = None;
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(TermCmd::Input(data)) => channel.data(&data[..]).await?,
                Some(TermCmd::Resize { cols, rows }) => {
                    channel.window_change(cols as u32, rows as u32, 0, 0).await?
                }
                Some(TermCmd::Close) | None => break,
            },
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data }) => emit(TermEvent::Data(data.to_vec())),
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    emit(TermEvent::Data(data.to_vec()))
                }
                Some(ChannelMsg::ExitStatus { exit_status: code }) => exit_status = Some(code),
                Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
        }
    }
    let _ = channel.close().await;
    // Los túneles dejan de aceptar conexiones y sueltan su referencia.
    drop(tunnel_set);
    tokio::task::yield_now().await;
    // Si SFTP comparte la conexión, sigue viva; si no, se cierra aquí.
    if Arc::strong_count(&handle) == 1 {
        let _ = handle.disconnect(Disconnect::ByApplication, "", "es").await;
    }
    match exit_status {
        Some(0) | None => Ok(()),
        Some(code) => {
            emit(TermEvent::Status(format!(
                "La shell terminó con código {code}"
            )));
            Ok(())
        }
    }
}
