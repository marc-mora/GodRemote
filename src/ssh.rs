//! Conexión SSH: verificación de host, autenticación y shell interactiva.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use russh::client::{self, AuthResult, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::ssh_key::private::KeypairData;
use russh::keys::ssh_key::{Certificate, HashAlg, PrivateKey};
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect, MethodKind, MethodSet};
use tokio::sync::{mpsc, oneshot};

use crate::i18n::tr;
use crate::keys::{CertInfo, CertValidity};
use crate::model::{AuthMethod, Session};
use crate::terminal::{TermBackend, TermCmd, TermEvent};
use crate::totp::Totp;
use crate::trf;
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
    /// Agente SSH local que se reenvía a este equipo. Solo lo hay si el
    /// servidor aceptó el reenvío: sin él no se atienden los canales de
    /// agente que abra.
    pub agent_forward: Mutex<Option<crate::agent::AgentKind>>,
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

    /// La función que despierta la interfaz, para quien avise por su cuenta.
    pub fn waker(&self) -> Waker {
        self.waker.clone()
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
    /// Generador de códigos TOTP con el que contestar solo cuando el
    /// servidor pida un código de verificación (segundo factor).
    pub totp: Option<Totp>,
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

    /// Encadena una lista de saltos (el primero es el más cercano a
    /// nosotros). El resultado es el último salto, que lleva dentro a los
    /// anteriores; `None` si la lista está vacía.
    pub fn route(hops: impl IntoIterator<Item = (Session, Credentials)>) -> Option<Self> {
        let mut route: Option<Endpoint> = None;
        for (session, creds) in hops {
            route = Some(Endpoint {
                session,
                creds,
                via: route.map(Box::new),
            });
        }
        route
    }

    /// Destino al que se llega pasando, en orden, por `hops`.
    pub fn through(
        hops: impl IntoIterator<Item = (Session, Credentials)>,
        session: Session,
        creds: Credentials,
    ) -> Self {
        Self {
            session,
            creds,
            via: Self::route(hops).map(Box::new),
        }
    }
}

/// Flujo de bytes hacia un equipo: un socket TCP directo o un canal SSH
/// abierto desde el último salto.
///
/// Es un tipo concreto (y no un objeto `dyn`) a propósito: con un objeto
/// dinámico el compilador no logra demostrar que los futuros de IronRDP se
/// pueden mover entre hilos.
pub enum NetStream {
    Tcp(tokio::net::TcpStream),
    Ssh(russh::ChannelStream<client::Msg>),
}

impl tokio::io::AsyncRead for NetStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            NetStream::Tcp(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            NetStream::Ssh(s) => std::pin::Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for NetStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            NetStream::Tcp(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            NetStream::Ssh(s) => std::pin::Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            NetStream::Tcp(s) => std::pin::Pin::new(s).poll_flush(cx),
            NetStream::Ssh(s) => std::pin::Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            NetStream::Tcp(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            NetStream::Ssh(s) => std::pin::Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// Camino hasta un equipo que no habla SSH (RDP, VNC, Telnet): directo o a
/// través de una cadena de saltos SSH que se mantiene abierta mientras viva.
pub struct Route {
    last_hop: Option<SshHandle>,
    /// Conectar directamente aunque haya un proxy de salida.
    bypass_proxy: bool,
}

impl Route {
    /// Abre la cadena de saltos (si la hay). Devuelve también los secretos
    /// validados que el usuario pidió guardar.
    pub async fn establish(
        via: Option<&Endpoint>,
        prompter: &Prompter,
        status: &(dyn Fn(String) + Send + Sync),
    ) -> Result<(Self, Vec<(String, String)>)> {
        match via {
            None => Ok((
                Self {
                    last_hop: None,
                    bypass_proxy: false,
                },
                vec![],
            )),
            Some(endpoint) => {
                let connected = connect_endpoint(endpoint, prompter, status)
                    .await
                    .with_context(|| {
                        trf!("en el salto {name}", name = endpoint.session.display_name())
                    })?;
                Ok((
                    Self {
                        last_hop: Some(connected.handle),
                        bypass_proxy: false,
                    },
                    connected.to_save,
                ))
            }
        }
    }

    pub fn is_direct(&self) -> bool {
        self.last_hop.is_none()
    }

    /// Pide que la conexión directa no pase por el proxy de salida.
    pub fn without_proxy(mut self, bypass: bool) -> Self {
        self.bypass_proxy = bypass;
        self
    }

    /// Abre una conexión con `host:port`: directa, o desde el último salto.
    pub async fn connect(&self, host: &str, port: u16) -> Result<NetStream> {
        Ok(self.connect_with_addr(host, port).await?.stream)
    }

    /// Como [`Route::connect`], devolviendo además las direcciones del
    /// socket cuando la conexión es directa.
    ///
    /// El futuro no toma prestado nada (copia lo que necesita), de modo que
    /// se puede esperar dentro de tareas que saltan entre hilos.
    pub fn connect_with_addr(
        &self,
        host: &str,
        port: u16,
    ) -> impl Future<Output = Result<RouteStream>> + Send + 'static {
        let hop = self.last_hop.clone();
        let host = host.to_string();
        let bypass = self.bypass_proxy;
        async move {
            match hop {
                None => {
                    let stream = crate::proxy::dial(&host, port, bypass, CONNECT_TIMEOUT).await?;
                    Ok(RouteStream {
                        local_addr: stream.local_addr().ok(),
                        peer_addr: stream.peer_addr().ok(),
                        stream: NetStream::Tcp(stream),
                    })
                }
                Some(hop) => {
                    let channel = tokio::time::timeout(
                        CONNECT_TIMEOUT,
                        hop.channel_open_direct_tcpip(host.clone(), port as u32, "127.0.0.1", 0),
                    )
                    .await
                    .map_err(|_| {
                        anyhow!(
                            "{}",
                            trf!(
                                "Tiempo de espera agotado saltando hacia {host}:{port}",
                                host,
                                port
                            )
                        )
                    })?
                    .map_err(|e| {
                        anyhow!(
                            "{}",
                            trf!(
                                "El último salto no pudo abrir la conexión con {host}:{port}: {e}",
                                host,
                                port,
                                e
                            )
                        )
                    })?;
                    Ok(RouteStream {
                        local_addr: None,
                        peer_addr: None,
                        stream: NetStream::Ssh(channel.into_stream()),
                    })
                }
            }
        }
    }
}

/// Conexión abierta por una [`Route`].
pub struct RouteStream {
    pub stream: NetStream,
    /// Dirección local del socket (solo en conexiones directas).
    pub local_addr: Option<std::net::SocketAddr>,
    /// Dirección del equipo remoto (solo en conexiones directas).
    pub peer_addr: Option<std::net::SocketAddr>,
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
                bail!(
                    "{}",
                    tr("El servidor presentó un certificado de host; no está soportado")
                )
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
                    bail!("{}", tr("Clave de host rechazada por el usuario"));
                }
                russh::keys::known_hosts::learn_known_hosts_path(
                    &self.host,
                    self.port,
                    key,
                    &self.known_hosts,
                )
                .with_context(|| trf!("guardando en {path}", path = self.known_hosts.display()))?;
                Ok(true)
            }
            Err(russh::keys::Error::KeyChanged { line }) => bail!(
                "{}",
                trf!(
                    "¡LA CLAVE DEL HOST HA CAMBIADO! Puede ser un ataque de intermediario \
                     o que el servidor se haya reinstalado. Huella recibida: {fingerprint}. \
                     Si el cambio es legítimo, elimina la línea {line} de {file} y vuelve a \
                     conectar.",
                    fingerprint = key.fingerprint(HashAlg::Sha256),
                    line,
                    file = self.known_hosts.display()
                )
            ),
            Err(e) => Err(anyhow!("{}", trf!("error leyendo known_hosts: {e}", e))),
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

    /// El equipo remoto quiere hablar con nuestro agente SSH. Solo se le
    /// deja si el reenvío se pidió para esta conexión y él lo aceptó.
    async fn server_channel_open_agent_forward(
        &mut self,
        channel: russh::Channel<client::Msg>,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let forwarded = *self.shared.agent_forward.lock().unwrap();
        if let Some(kind) = forwarded {
            reply.accept().await;
            tokio::spawn(async move {
                let _ = crate::agent::bridge(channel.into_stream(), kind).await;
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

/// Pide el reenvío del agente en el canal y espera la respuesta.
async fn request_agent(
    channel: &mut russh::Channel<client::Msg>,
    handle: &SshHandle,
    emit: &Emit,
) -> Result<()> {
    // El mismo agente con el que se autenticaría: el de OpenSSH o Pageant.
    let kind = match crate::agent::pick().await {
        Ok(kind) => kind,
        Err(e) => {
            emit(TermEvent::Status(trf!(
                "Reenvío del agente no disponible: {error}",
                error = format!("{e:#}")
            )));
            return Ok(());
        }
    };
    channel.agent_forward(true).await?;
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
        *handle.shared.agent_forward.lock().unwrap() = Some(kind);
        emit(TermEvent::Status(
            match kind {
                crate::agent::AgentKind::OpenSsh => tr("Reenvío del agente SSH activo"),
                crate::agent::AgentKind::Pageant => tr("Reenvío del agente SSH activo (Pageant)"),
            }
            .into(),
        ));
    } else {
        emit(TermEvent::Status(
            tr("El servidor rechazó el reenvío del agente (¿AllowAgentForwarding yes?)").into(),
        ));
    }
    Ok(())
}

/// Resultado de una conexión correcta.
pub struct Connected {
    pub handle: SshHandle,
    /// Secretos (clave del baúl, valor) que el usuario pidió guardar y que
    /// han sido validados por una autenticación correcta.
    pub to_save: Vec<(String, String)>,
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

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
                let connected =
                    connect_endpoint(via, prompter, status)
                        .await
                        .with_context(|| {
                            trf!("en el salto {name}", name = via.session.display_name())
                        })?;
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

/// Nombre de un equipo para los mensajes: «nombre (host:puerto)».
fn describe(session: &Session) -> String {
    let address = format!("{}:{}", session.host.trim(), session.port);
    let name = session.name.trim();
    if name.is_empty() || name == session.host.trim() {
        address
    } else {
        format!("{name} ({address})")
    }
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
            status(trf!("Saltando hacia {host}…", host = describe(session)));
            let channel = jump
                .channel_open_direct_tcpip(host.clone(), session.port as u32, "127.0.0.1", 0)
                .await
                .map_err(|e| {
                    anyhow!(
                        "{}",
                        trf!(
                            "El salto no pudo abrir la conexión con {host}:{port}: {e}",
                            host,
                            port = session.port,
                            e
                        )
                    )
                })?;
            client::connect_stream(config, channel.into_stream(), handler).await?
        }
        None => {
            status(trf!("Conectando a {host}…", host = describe(session)));
            // Directa o, si lo hay, a través del proxy de salida.
            let stream =
                crate::proxy::dial(&host, session.port, session.bypass_proxy, CONNECT_TIMEOUT)
                    .await?;
            client::connect_stream(config, stream, handler).await?
        }
    };

    status(trf!("Autenticando como {user}…", user));
    let mut to_save = vec![];
    let mut asker = Asker {
        session,
        user: &user,
        prompter,
        status,
        password: creds.password,
        totp: creds.totp,
        totp_sent: false,
        code_typed: false,
        code_rejected: false,
    };
    let outcome = match session.auth {
        AuthMethod::Password => auth_password(&mut handle, &mut asker, &mut to_save).await?,
        AuthMethod::Key => {
            auth_key(&mut handle, &mut asker, creds.passphrase, &mut to_save).await?
        }
        AuthMethod::Agent => auth_agent(&mut handle, &mut asker).await?,
    };
    // El servidor dio por bueno el primer paso y exige otro más.
    if let Outcome::Partial(remaining) = outcome {
        second_factor(&mut handle, &mut asker, remaining, &mut to_save).await?;
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

/// Cómo termina un intento de autenticación con un método.
enum Outcome {
    Success,
    /// Aceptado, pero el servidor exige además otro método (segundo factor).
    Partial(MethodSet),
    Rejected(MethodSet),
}

impl From<AuthResult> for Outcome {
    fn from(result: AuthResult) -> Self {
        match result {
            AuthResult::Success => Outcome::Success,
            AuthResult::Failure {
                remaining_methods,
                partial_success: true,
            } => Outcome::Partial(remaining_methods),
            AuthResult::Failure {
                remaining_methods, ..
            } => Outcome::Rejected(remaining_methods),
        }
    }
}

/// Lo necesario para contestar a lo que pregunte el servidor al autenticar:
/// con lo que ya se sabe (contraseña, TOTP) o preguntando al usuario.
struct Asker<'a> {
    session: &'a Session,
    user: &'a str,
    prompter: &'a Prompter,
    status: &'a (dyn Fn(String) + Send + Sync),
    /// Contraseña conocida (del baúl): vale para una sola pregunta.
    password: Option<String>,
    /// Generador TOTP del equipo. Contesta una sola vez por conexión: si el
    /// servidor no acepta su código, el siguiente se le pide al usuario.
    totp: Option<Totp>,
    totp_sent: bool,
    /// El usuario ya ha tecleado algún código en esta conexión.
    code_typed: bool,
    /// El servidor acaba de rechazar un código de verificación.
    code_rejected: bool,
}

impl Asker<'_> {
    fn target(&self) -> String {
        format!("{}@{}", self.user, self.session.host.trim())
    }

    /// Código TOTP para contestar al servidor, si este equipo tiene generador
    /// y aún no se ha usado en esta conexión.
    async fn totp_code(&mut self) -> Option<String> {
        if self.totp_sent {
            return None;
        }
        let code = fresh_code(self.totp.as_ref()?, self.status).await?;
        self.totp_sent = true;
        (self.status)(tr("Código de verificación TOTP enviado").into());
        Some(code)
    }
}

/// Último periodo cuyo código TOTP se ha enviado, por secreto. Muchos
/// servidores no admiten dos veces el mismo código (`pam_google_authenticator`
/// lo impide de fábrica): si el de ahora ya se envió —al abrir otra pestaña
/// del mismo equipo, por ejemplo— se espera al siguiente.
static TOTP_SENT: Mutex<BTreeMap<[u8; 32], u64>> = Mutex::new(BTreeMap::new());

/// Espera máxima al siguiente código; si hiciera falta más, se pregunta.
const TOTP_MAX_WAIT: Duration = Duration::from_secs(45);

/// Un código TOTP que no se haya enviado ya desde este programa. `None` si
/// habría que esperar demasiado al siguiente.
async fn fresh_code(totp: &Totp, status: &(dyn Fn(String) + Send + Sync)) -> Option<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    // Se reserva el periodo antes de esperar: dos conexiones simultáneas no
    // deben acabar enviando el mismo código.
    let (step, wait) = {
        let mut sent = TOTP_SENT.lock().unwrap();
        let id = totp.fingerprint();
        let current = totp.step_at(now.as_secs());
        let step = match sent.get(&id) {
            Some(last) if *last >= current => last + 1,
            _ => current,
        };
        let wait = Duration::from_secs(step.saturating_mul(totp.period)).saturating_sub(now);
        if wait > TOTP_MAX_WAIT {
            return None;
        }
        sent.insert(id, step);
        (step, wait)
    };
    if !wait.is_zero() {
        status(trf!(
            "El código TOTP de ahora ya se ha usado: esperando al siguiente ({secs} s)…",
            secs = wait.as_secs() + 1
        ));
        tokio::time::sleep(wait + Duration::from_millis(100)).await;
    }
    Some(totp.code_for_step(step))
}

/// `true` si lo que pregunta el servidor parece un código de verificación de
/// un solo uso (TOTP) y no una contraseña.
fn asks_for_code(prompt: &str) -> bool {
    let prompt = prompt.to_lowercase();
    // Palabras sueltas, para no ver «otp» dentro de otra.
    let words: Vec<&str> = prompt
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let has_word = |wanted: &[&str]| words.iter().any(|word| wanted.contains(word));
    if has_word(&[
        "otp",
        "totp",
        "2fa",
        "mfa",
        "token",
        "passcode",
        "authenticator",
        "autenticador",
    ]) {
        return true;
    }
    let phrases = [
        "one-time",
        "one time",
        "verification",
        "verificación",
        "verificacion",
        "two-factor",
        "two factor",
        "segundo factor",
        "doble factor",
        "un solo uso",
    ];
    if phrases.iter().any(|phrase| prompt.contains(phrase)) {
        return true;
    }
    // «Code» a secas, salvo que hable de una contraseña.
    has_word(&["code", "código", "codigo"])
        && !prompt.contains("password")
        && !prompt.contains("contraseña")
}

async fn auth_password(
    handle: &mut client::Handle<ClientHandler>,
    asker: &mut Asker<'_>,
    to_save: &mut Vec<(String, String)>,
) -> Result<Outcome> {
    let session = asker.session;
    let mut stored = asker.password.take();
    let target = asker.target();
    // En el último intento fallido el servidor pidió también un código: no
    // se puede saber cuál de las dos cosas estaba mal.
    let mut code_involved = false;
    for attempt in 0..3 {
        let (password, save, from_vault) = match stored.take() {
            Some(p) => (p, false, true),
            None => {
                let msg = if code_involved {
                    trf!(
                        "Contraseña o código de verificación incorrectos. \
                         Contraseña para {target}",
                        target
                    )
                } else if attempt == 0 {
                    trf!("Contraseña para {target}", target)
                } else {
                    trf!("Contraseña incorrecta. Contraseña para {target}", target)
                };
                match asker
                    .prompter
                    .secret(tr("Autenticación SSH"), &msg, false, true)
                    .await
                {
                    Some(a) => (a.value, a.save, false),
                    None => bail!("{}", tr("Autenticación cancelada")),
                }
            }
        };

        let result = handle
            .authenticate_password(asker.user, password.clone())
            .await?;
        let mut outcome = Outcome::from(result);
        // Muchos servidores (PAM) solo aceptan keyboard-interactive.
        if let Outcome::Rejected(remaining) = &outcome {
            if remaining.contains(&MethodKind::KeyboardInteractive) {
                let round = keyboard_interactive(handle, asker, Some(&password)).await?;
                code_involved = round.code_asked;
                outcome = round.outcome;
            } else if !remaining.contains(&MethodKind::Password) {
                bail!(
                    "{}",
                    tr("El servidor no admite autenticación por contraseña")
                );
            }
        }
        if !matches!(outcome, Outcome::Rejected(_)) {
            // La contraseña ha valido (aunque el servidor pida algo más).
            if save {
                to_save.push((session.password_key(), password));
            }
            return Ok(outcome);
        }
        if from_vault {
            (asker.status)(
                if code_involved {
                    tr("La contraseña guardada o el código de verificación fueron rechazados")
                } else {
                    tr("La contraseña guardada fue rechazada")
                }
                .into(),
            );
        }
    }
    if code_involved {
        bail!(
            "{}",
            tr("Autenticación fallida: contraseña o código de verificación incorrectos")
        )
    }
    bail!("{}", tr("Autenticación fallida: contraseña incorrecta"))
}

/// Segundo factor: el servidor aceptó el primer método y exige otro más
/// (por ejemplo, `AuthenticationMethods publickey,keyboard-interactive`).
async fn second_factor(
    handle: &mut client::Handle<ClientHandler>,
    asker: &mut Asker<'_>,
    mut remaining: MethodSet,
    to_save: &mut Vec<(String, String)>,
) -> Result<()> {
    let target = asker.target();
    let mut failures = 0;
    // Límite defensivo de pasos encadenados.
    for _ in 0..8 {
        let (outcome, what) = if remaining.contains(&MethodKind::KeyboardInteractive) {
            let password = asker.password.take();
            let round = keyboard_interactive(handle, asker, password.as_deref()).await?;
            let what = if round.code_asked {
                tr("el código de verificación")
            } else {
                tr("la respuesta")
            };
            asker.code_rejected = round.code_asked && matches!(round.outcome, Outcome::Rejected(_));
            (round.outcome, what)
        } else if remaining.contains(&MethodKind::Password) {
            let (password, save) = match asker.password.take() {
                Some(p) => (p, false),
                None => {
                    let msg = if failures == 0 {
                        trf!("Contraseña para {target}", target)
                    } else {
                        trf!("Contraseña incorrecta. Contraseña para {target}", target)
                    };
                    match asker
                        .prompter
                        .secret(tr("Autenticación SSH"), &msg, false, true)
                        .await
                    {
                        Some(a) => (a.value, a.save),
                        None => bail!("{}", tr("Autenticación cancelada")),
                    }
                }
            };
            let result = handle
                .authenticate_password(asker.user, password.clone())
                .await?;
            let outcome = Outcome::from(result);
            if save && !matches!(outcome, Outcome::Rejected(_)) {
                to_save.push((asker.session.password_key(), password));
            }
            (outcome, tr("la contraseña"))
        } else {
            let methods: Vec<&str> = remaining.iter().map(<&str>::from).collect();
            bail!(
                "{}",
                trf!(
                    "Autenticación fallida: el servidor exige además un método que no se puede \
                     ofrecer aquí ({methods})",
                    methods = methods.join(", ")
                )
            );
        };
        match outcome {
            Outcome::Success => return Ok(()),
            Outcome::Partial(more) => {
                remaining = more;
                failures = 0;
            }
            Outcome::Rejected(more) => {
                failures += 1;
                if failures >= 3 {
                    bail!(
                        "{}",
                        trf!("Autenticación fallida: el servidor rechazó {what}", what)
                    );
                }
                (asker.status)(trf!("El servidor no aceptó {what}", what));
                if !more.is_empty() {
                    remaining = more;
                }
            }
        }
    }
    bail!(
        "{}",
        tr("Autenticación fallida: el servidor encadena demasiados pasos")
    )
}

/// Cómo acabó una ronda de preguntas keyboard-interactive.
struct Interactive {
    outcome: Outcome,
    /// Entre las preguntas había un código de verificación.
    code_asked: bool,
}

/// Autenticación keyboard-interactive. A cada pregunta se contesta con lo
/// que se sabe: el código TOTP si pide un código de verificación, y
/// `password` (una vez) para el primer campo oculto. El resto se pregunta al
/// usuario.
async fn keyboard_interactive(
    handle: &mut client::Handle<ClientHandler>,
    asker: &mut Asker<'_>,
    password: Option<&str>,
) -> Result<Interactive> {
    let mut password = password.map(str::to_string);
    let mut code_asked = false;
    let mut response = handle
        .authenticate_keyboard_interactive_start(asker.user, None::<String>)
        .await?;
    // Límite defensivo de rondas.
    for _ in 0..10 {
        match response {
            KeyboardInteractiveAuthResponse::Success => {
                return Ok(Interactive {
                    outcome: Outcome::Success,
                    code_asked,
                });
            }
            KeyboardInteractiveAuthResponse::Failure {
                remaining_methods,
                partial_success,
            } => {
                let outcome = if partial_success {
                    Outcome::Partial(remaining_methods)
                } else {
                    Outcome::Rejected(remaining_methods)
                };
                return Ok(Interactive {
                    outcome,
                    code_asked,
                });
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let mut answers = Vec::with_capacity(prompts.len());
                for p in prompts {
                    let wants_code = asks_for_code(&p.prompt);
                    let mut msg = if instructions.is_empty() {
                        p.prompt.clone()
                    } else {
                        format!("{instructions}\n{}", p.prompt)
                    };
                    if wants_code {
                        code_asked = true;
                        if let Some(code) = asker.totp_code().await {
                            answers.push(code);
                            continue;
                        }
                        let rejected = std::mem::take(&mut asker.code_rejected);
                        if asker.totp_sent && !asker.code_typed {
                            msg = trf!(
                                "El código TOTP automático ya se envió y no bastó: escribe el \
                                 siguiente.\n{msg}",
                                msg
                            );
                        } else if rejected {
                            msg = trf!("El servidor no aceptó el código anterior.\n{msg}", msg);
                        }
                        asker.code_typed = true;
                    } else if !p.echo
                        && let Some(pw) = password.take()
                    {
                        answers.push(pw);
                        continue;
                    }
                    let title = if name.is_empty() {
                        tr("Autenticación SSH").to_string()
                    } else {
                        name.clone()
                    };
                    match asker.prompter.secret(&title, &msg, p.echo, false).await {
                        Some(a) => answers.push(a.value),
                        None => bail!("{}", tr("Autenticación cancelada")),
                    }
                }
                response = handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await?;
            }
        }
    }
    Ok(Interactive {
        outcome: Outcome::Rejected(MethodSet::empty()),
        code_asked,
    })
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

/// Firma con una clave privada local lo que russh pide firmar al presentar
/// un certificado. Se usa en lugar de `authenticate_openssh_cert` porque este
/// no deja elegir el algoritmo de firma de las claves RSA: anuncia `ssh-rsa`
/// (SHA-1), que los servidores actuales rechazan.
struct KeySigner(Arc<PrivateKey>);

impl russh::Signer for KeySigner {
    type Error = anyhow::Error;

    fn auth_sign(
        &mut self,
        _identity: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        mut to_sign: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send {
        use russh::keys::signature::Signer as _;
        use russh::keys::ssh_encoding::Encode as _;
        let signature = match self.0.key_data() {
            KeypairData::Rsa(rsa) => (rsa, hash_alg).try_sign(&to_sign),
            other => other.try_sign(&to_sign),
        };
        async move {
            let signature = signature.map_err(|e| {
                anyhow!("{}", trf!("no se pudo firmar con la clave privada: {e}", e))
            })?;
            // La firma va a continuación de lo firmado, como cadena SSH.
            let blob = signature.encode_vec()?;
            to_sign.extend_from_slice(&(blob.len() as u32).to_be_bytes());
            to_sign.extend_from_slice(&blob);
            Ok(to_sign)
        }
    }
}

/// Busca el certificado con el que acompañar a una clave: el indicado en el
/// equipo o, si no se indica ninguno, el que haya junto a ella
/// (`<clave>-cert.pub`), como hace OpenSSH.
///
/// Un certificado indicado a mano que no se puede usar con esta clave es un
/// error. Uno detectado solo, o uno caducado, no impide intentarlo con la
/// clave sin certificado: `problem` recibe el motivo para contarlo.
fn find_certificate(
    session: &Session,
    key_path: &std::path::Path,
    key: &PrivateKey,
    problem: &mut Option<String>,
) -> Result<Option<(Certificate, CertInfo)>> {
    let explicit = !session.cert_path.trim().is_empty();
    let path = if explicit {
        crate::paths::expand_tilde(&session.cert_path)
    } else {
        crate::keys::cert_path_for(key_path)
    };
    if !explicit && !path.exists() {
        return Ok(None);
    }
    let (cert, info) = match crate::keys::read_cert(&path) {
        Ok(found) => found,
        Err(e) if explicit => return Err(e),
        Err(e) => {
            *problem = Some(format!("{e:#}"));
            return Ok(None);
        }
    };
    // En los mensajes basta el nombre del fichero: la ruta ya se conoce.
    let name = file_name(&path);
    let unusable = if cert.public_key() != key.public_key().key_data() {
        Some(trf!(
            "El certificado {name} no corresponde a la clave {key}",
            name,
            key = file_name(key_path)
        ))
    } else if info.host {
        Some(trf!(
            "{name} es un certificado de host, no de usuario",
            name
        ))
    } else {
        None
    };
    if let Some(unusable) = unusable {
        if explicit {
            bail!(unusable);
        }
        *problem = Some(unusable);
        return Ok(None);
    }
    if info.validity() != CertValidity::Valid {
        *problem = Some(trf!(
            "El certificado {name} {validity}",
            name,
            validity = info.validity_text()
        ));
        return Ok(None);
    }
    Ok(Some((cert, info)))
}

/// Nombre de un fichero, sin su carpeta, para los mensajes.
fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Por qué pudo rechazar el servidor un certificado, para el mensaje.
fn cert_rejection(info: &CertInfo, user: &str) -> String {
    let name = file_name(&info.path);
    if !info.principals.is_empty() && !info.principals.iter().any(|p| p == user) {
        trf!(
            "El servidor no aceptó el certificado {name} (vale para {principals}, no para \
             «{user}»)",
            name,
            principals = info.principals_text(),
            user
        )
    } else {
        trf!(
            "El servidor no aceptó el certificado {name} (¿confía el servidor en la autoridad \
             que lo firmó?)",
            name
        )
    }
}

async fn auth_key(
    handle: &mut client::Handle<ClientHandler>,
    asker: &mut Asker<'_>,
    passphrase: Option<String>,
    to_save: &mut Vec<(String, String)>,
) -> Result<Outcome> {
    let (session, user, prompter, status) =
        (asker.session, asker.user, asker.prompter, asker.status);
    let path = crate::paths::expand_tilde(&session.key_path);
    let target = asker.target();
    let text = std::fs::read_to_string(&path)
        .with_context(|| trf!("No se pudo leer la clave {path}", path = path.display()))?;

    // Una llave de seguridad FIDO2 no se puede usar desde su fichero (ni
    // hace falta su passphrase): firma la llave, a través del agente.
    if let Ok(reference) = PrivateKey::from_openssh(&text)
        && crate::keys::is_security_key(&reference.algorithm())
    {
        return auth_security_key(handle, asker, &path, reference.public_key()).await;
    }

    let mut pending_save = None;
    let key = if key_is_encrypted(&text) {
        let mut stored = passphrase;
        let mut found = None;
        for attempt in 0..3 {
            let (pass, save) = match stored.take() {
                Some(p) => (p, false),
                None => {
                    let msg = if attempt == 0 {
                        trf!(
                            "Passphrase de la clave {path} (para {target})",
                            path = path.display(),
                            target
                        )
                    } else {
                        trf!(
                            "Passphrase incorrecta. Passphrase de {path} (para {target})",
                            path = path.display(),
                            target
                        )
                    };
                    match prompter
                        .secret(tr("Clave privada"), &msg, false, true)
                        .await
                    {
                        Some(a) => (a.value, a.save),
                        None => bail!("{}", tr("Autenticación cancelada")),
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
        found.ok_or_else(|| anyhow!("{}", tr("No se pudo descifrar la clave privada")))?
    } else {
        russh::keys::decode_secret_key(&text, None).map_err(|e| {
            anyhow!(
                "{}",
                trf!(
                    "Clave privada no válida ({path}): {e}",
                    path = path.display(),
                    e
                )
            )
        })?
    };
    let key = Arc::new(key);

    let mut cert_problem = None;
    let cert = find_certificate(session, &path, &key, &mut cert_problem)?;
    if let Some(problem) = &cert_problem {
        status(trf!(
            "{problem}: se prueba la clave sin certificado",
            problem
        ));
    }
    let hash = handle.best_supported_rsa_hash().await?.flatten();

    // Primero con el certificado; si el servidor no lo quiere, la clave sola.
    if let Some((cert, info)) = cert {
        let result = handle
            .authenticate_certificate_with(user, cert, hash, &mut KeySigner(key.clone()))
            .await?;
        let outcome = Outcome::from(result);
        if !matches!(outcome, Outcome::Rejected(_)) {
            to_save.extend(pending_save);
            return Ok(outcome);
        }
        let why = cert_rejection(&info, user);
        status(trf!(
            "{problem}: se prueba la clave sin certificado",
            problem = why
        ));
        cert_problem = Some(why);
    }
    let result = handle
        .authenticate_publickey(user, PrivateKeyWithHashAlg::new(key, hash))
        .await?;
    let outcome = Outcome::from(result);
    if matches!(outcome, Outcome::Rejected(_)) {
        match cert_problem {
            Some(problem) => bail!(
                "{}",
                trf!(
                    "El servidor rechazó la clave {path}. {problem}",
                    path = path.display(),
                    problem
                )
            ),
            None => bail!(
                "{}",
                trf!("El servidor rechazó la clave {path}", path = path.display())
            ),
        }
    }
    to_save.extend(pending_save);
    Ok(outcome)
}

/// Firma con el agente SSH y, si la clave es de una llave de seguridad
/// FIDO2, avisa antes: el agente esperará a que el usuario la toque.
struct AgentSigner<'a> {
    agent: &'a mut crate::agent::Client,
    status: &'a (dyn Fn(String) + Send + Sync),
    /// Nombre de la identidad con la que se va a firmar (russh no lo pasa).
    name: String,
}

impl russh::Signer for AgentSigner<'_> {
    type Error = russh::AgentAuthError;

    async fn auth_sign(
        &mut self,
        identity: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        to_sign: Vec<u8>,
    ) -> Result<Vec<u8>, Self::Error> {
        if crate::keys::is_security_key(&identity.public_key().algorithm()) {
            (self.status)(trf!(
                "Toca la llave de seguridad{name} para confirmar…",
                name = self.name
            ));
        }
        self.agent
            .sign_request(identity, hash_alg, to_sign)
            .await
            .map_err(Into::into)
    }
}

/// « «nombre»», o nada si no tiene nombre (para intercalar en una frase).
fn quoted(name: &str) -> String {
    if name.is_empty() {
        String::new()
    } else {
        trf!(" «{name}»", name)
    }
}

/// Ofrece al servidor, una a una, identidades del agente. Devuelve cómo
/// acabó la que el servidor aceptó, o `None` si no aceptó ninguna; `notes`
/// recibe lo que no se pudo ofrecer (certificados caducados, llaves que no
/// firmaron) para explicarlo.
async fn offer_agent_identities(
    handle: &mut client::Handle<ClientHandler>,
    asker: &Asker<'_>,
    agent: &mut crate::agent::Client,
    identities: Vec<AgentIdentity>,
    notes: &mut Vec<String>,
) -> Result<Option<Outcome>> {
    let (user, status) = (asker.user, asker.status);
    let hash = handle.best_supported_rsa_hash().await?.flatten();
    for identity in identities {
        let name = quoted(identity.comment());
        let security_key = crate::keys::is_security_key(&identity.public_key().algorithm());
        let mut signer = AgentSigner {
            agent: &mut *agent,
            status,
            name: name.clone(),
        };
        let result = match identity {
            AgentIdentity::PublicKey { key, .. } => {
                handle
                    .authenticate_publickey_with(user, key, hash, &mut signer)
                    .await
            }
            // Un certificado cargado en el agente (`ssh-add` lo añade si
            // está junto a la clave).
            AgentIdentity::Certificate { certificate, .. } => {
                let info = CertInfo::from_certificate(std::path::Path::new(""), &certificate);
                if info.host {
                    continue;
                }
                if info.validity() != CertValidity::Valid {
                    let note = trf!(
                        "el certificado «{id}» {validity}",
                        id = info.key_id,
                        validity = info.validity_text()
                    );
                    status(trf!(
                        "El agente tiene un certificado que no vale: {note}",
                        note
                    ));
                    notes.push(note);
                    continue;
                }
                handle
                    .authenticate_certificate_with(user, certificate, hash, &mut signer)
                    .await
            }
        };
        match result.map(Outcome::from) {
            Ok(Outcome::Rejected(_)) => continue,
            Ok(accepted) => return Ok(Some(accepted)),
            // El agente no firmó (llave sin conectar o sin tocar, clave que
            // pide confirmación y se denegó…): se sigue con las demás.
            Err(russh::AgentAuthError::Key(e)) => {
                let note = if security_key {
                    trf!(
                        "la llave de seguridad{name} no firmó (¿está conectada y la has tocado?)",
                        name
                    )
                } else {
                    trf!("el agente no firmó con la clave{name}: {e}", name, e)
                };
                status(trf!("Aviso: {note}", note));
                notes.push(note);
            }
            Err(e) => bail!(
                "{}",
                trf!("Se perdió la conexión mientras firmaba el agente: {e}", e)
            ),
        }
    }
    Ok(None)
}

async fn auth_agent(
    handle: &mut client::Handle<ClientHandler>,
    asker: &mut Asker<'_>,
) -> Result<Outcome> {
    // El agente de OpenSSH o, en Windows, Pageant, según el ajuste.
    let crate::agent::Connected {
        client: mut agent,
        kind,
        identities,
    } = crate::agent::connect().await.map_err(|e| {
        anyhow!(
            "{}",
            trf!(
                "No se pudo usar el agente SSH: {error}",
                error = format!("{e:#}")
            )
        )
    })?;
    if identities.is_empty() {
        match kind {
            crate::agent::AgentKind::OpenSsh => {
                bail!("{}", tr("El agente SSH no tiene ninguna clave cargada"))
            }
            crate::agent::AgentKind::Pageant => {
                bail!("{}", tr("Pageant no tiene ninguna clave cargada"))
            }
        }
    }
    let mut notes = vec![];
    match offer_agent_identities(handle, asker, &mut agent, identities, &mut notes).await? {
        Some(outcome) => Ok(outcome),
        None if notes.is_empty() => {
            bail!(
                "{}",
                tr("Ninguna clave del agente fue aceptada por el servidor")
            )
        }
        None => bail!(
            "{}",
            trf!(
                "Ninguna clave del agente fue aceptada por el servidor ({notes})",
                notes = notes.join("; ")
            )
        ),
    }
}

/// Autenticación con una llave de seguridad FIDO2 elegida como «clave
/// privada». Su fichero solo guarda una referencia: quien firma es la llave,
/// y se llega a ella a través del agente SSH, que debe tenerla cargada.
async fn auth_security_key(
    handle: &mut client::Handle<ClientHandler>,
    asker: &Asker<'_>,
    path: &std::path::Path,
    public: &russh::keys::PublicKey,
) -> Result<Outcome> {
    let name = file_name(path);
    let how = trf!(
        "{name} es una llave de seguridad: firma la propia llave, a través del agente SSH. \
         Cárgala en el agente (ssh-add {path}) y vuelve a conectar",
        name,
        path = path.display()
    );
    let crate::agent::Connected {
        client: mut agent,
        identities,
        ..
    } = crate::agent::connect().await.map_err(|e| {
        anyhow!(
            "{}",
            trf!(
                "No se pudo usar el agente SSH ({error}). {how}",
                error = format!("{e:#}"),
                how
            )
        )
    })?;
    // Solo las identidades de esta llave (la clave y, si lo hay, su
    // certificado, que va primero).
    let (mut certs, mut plain): (Vec<_>, Vec<_>) = identities
        .into_iter()
        .filter(|identity| identity.public_key().key_data() == public.key_data())
        .partition(|identity| matches!(identity, AgentIdentity::Certificate { .. }));
    certs.append(&mut plain);
    if certs.is_empty() {
        bail!(
            "{}",
            trf!("El agente SSH no tiene cargada esta clave. {how}", how)
        );
    }
    let mut notes = vec![];
    match offer_agent_identities(handle, asker, &mut agent, certs, &mut notes).await? {
        Some(outcome) => Ok(outcome),
        None if notes.is_empty() => {
            bail!(
                "{}",
                trf!(
                    "El servidor rechazó la clave {path} (llave de seguridad)",
                    path = path.display()
                )
            )
        }
        None => bail!(
            "{}",
            trf!(
                "No se pudo entrar con la clave {path}: {notes}",
                path = path.display(),
                notes = notes.join("; ")
            )
        ),
    }
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
            emit(TermEvent::Status(trf!(
                "Reenvío X11 no disponible: {error}",
                error = format!("{e:#}")
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
            forward.screen(),
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
        let message = if forward.is_embedded() {
            tr(
                "Reenvío X11 activo: las aplicaciones gráficas se abrirán en la pestaña \
                 «Servidor X»",
            )
        } else {
            tr("Reenvío X11 activo (servidor X del sistema)")
        };
        *handle.shared.x11.lock().unwrap() = Some(Arc::new(forward));
        emit(TermEvent::Status(message.into()));
    } else {
        emit(TermEvent::Status(
            tr("El servidor rechazó el reenvío X11 (¿X11Forwarding yes y xauth instalado?)").into(),
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
    if session.agent_forwarding {
        request_agent(&mut channel, &handle, &emit).await?;
    }
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
    // La pestaña se cerró desde aquí (no es una caída de la conexión).
    let mut closed_by_user = false;
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(TermCmd::Input(data)) => channel.data(&data[..]).await?,
                Some(TermCmd::Resize { cols, rows }) => {
                    channel.window_change(cols as u32, rows as u32, 0, 0).await?
                }
                Some(TermCmd::Close) | None => {
                    closed_by_user = true;
                    break;
                }
            },
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data }) => emit(TermEvent::Data(data.to_vec())),
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    emit(TermEvent::Data(data.to_vec()))
                }
                Some(ChannelMsg::ExitStatus { exit_status: code }) => exit_status = Some(code),
                // La shell murió por una señal: también es un final ordenado.
                Some(ChannelMsg::ExitSignal { .. }) => exit_status = Some(128),
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
        // Sin código de salida y sin haberlo cerrado nosotros, el canal se
        // ha cortado: la conexión se ha caído (red, reinicio del servidor…).
        None if !closed_by_user => bail!("{}", tr("Se perdió la conexión con el servidor")),
        Some(0) | None => Ok(()),
        Some(code) => {
            emit(TermEvent::Status(trf!(
                "La shell terminó con código {code}",
                code
            )));
            Ok(())
        }
    }
}

/// ¿Merece reintentar sola una conexión que ha acabado con este error? Un
/// corte de red, sí. Un fallo de autenticación o de confianza en el servidor,
/// no: repetirlo no lo arregla y varios intentos fallidos seguidos pueden
/// bloquear la cuenta.
///
/// Se decide por palabras del mensaje, que es lo único que le llega a quien
/// pregunta (la reconexión de los terminales, el gestor de túneles, el panel
/// de estado). El mensaje está en el idioma en que se creó, que puede no ser
/// el de ahora, así que se buscan siempre las palabras de todos los idiomas:
/// el resultado no depende del idioma en uso. Una prueba, junto a la tabla
/// de traducciones (`i18n/en/backend.rs`), vigila que cada mensaje se
/// clasifique igual en todos.
pub fn worth_retrying(error: &str) -> bool {
    const PERMANENT: [&str; 15] = [
        // En español…
        "autenticación",
        "cancelad",
        "contraseña",
        "clave",
        "agente",
        "certificado",
        "known_hosts",
        "rechaz",
        // …y en inglés.
        "authentication",
        "cancelled",
        "password",
        "key",
        "agent",
        "certificate",
        "reject",
    ];
    let error = error.to_lowercase();
    !PERMANENT.iter().any(|word| error.contains(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_network_failures_are_worth_retrying() {
        // Cortes de red, en cualquier idioma (y los del sistema, en inglés).
        for error in [
            "Connection reset by peer",
            "Tiempo de espera agotado conectando a 10.0.0.5:22",
            "Timed out connecting to 10.0.0.5:22",
            "El último salto no pudo abrir la conexión con x:22: refused",
            "The last jump host could not open the connection to x:22: refused",
            "Se perdió la conexión con el servidor",
            "The connection to the server was lost",
            "No se pudo conectar a x:22: Connection refused (os error 111)",
        ] {
            assert!(worth_retrying(error), "{error}");
        }
        // Autenticación o confianza: insistir puede bloquear la cuenta.
        for error in [
            "Autenticación fallida: contraseña incorrecta",
            "Authentication failed: wrong password",
            "Autenticación cancelada",
            "Authentication cancelled",
            "El servidor rechazó la clave /home/u/.ssh/id",
            "The server rejected the key /home/u/.ssh/id",
            "Clave de host rechazada por el usuario",
            "Host key rejected by the user",
            "Ninguna clave del agente fue aceptada por el servidor",
            "The server accepted none of the agent's keys",
            "error leyendo known_hosts: permiso denegado",
            "The server presented no TLS certificate",
        ] {
            assert!(!worth_retrying(error), "{error}");
        }
    }

    #[test]
    fn verification_code_prompts_are_recognised() {
        for prompt in [
            "Verification code: ",
            "Código de verificación: ",
            "Codigo de verificacion:",
            "One-time password (OATH) for `ana': ",
            "Enter your OTP:",
            "TOTP: ",
            "Código: ",
            "Authenticator code",
            "2FA token:",
            "Duo two-factor login for ana\nPasscode or option (1-3): ",
            "Segundo factor:",
        ] {
            assert!(asks_for_code(prompt), "{prompt:?}");
        }
        for prompt in [
            "Password: ",
            "Contraseña: ",
            "ana@servidor's password: ",
            "(current) UNIX password: ",
            "Passphrase for key: ",
            "Password code word: ",
            "Laptop serial number: ",
            "Choose an option: ",
            "",
        ] {
            assert!(!asks_for_code(prompt), "{prompt:?}");
        }
    }

    #[tokio::test]
    async fn totp_codes_are_never_sent_twice() {
        // Periodo de un segundo: el segundo código llega enseguida.
        let totp = Totp::new(
            b"no-repetir-codigos".to_vec(),
            crate::totp::TotpAlgorithm::Sha1,
            6,
            1,
        )
        .unwrap();
        let notes = Mutex::new(Vec::<String>::new());
        let status = |s: String| notes.lock().unwrap().push(s);
        let first_step = totp.step_at(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
        let first = fresh_code(&totp, &status).await.unwrap();
        assert!(notes.lock().unwrap().is_empty());
        let second = fresh_code(&totp, &status).await.unwrap();
        let third = fresh_code(&totp, &status).await.unwrap();
        // Cada uno es de un periodo posterior al anterior.
        assert_eq!(first, totp.code_for_step(first_step));
        assert_eq!(second, totp.code_for_step(first_step + 1));
        assert_eq!(third, totp.code_for_step(first_step + 2));
        let last = TOTP_SENT.lock().unwrap()[&totp.fingerprint()];
        assert!(last >= first_step + 2, "{last} frente a {first_step}");
        assert!(
            notes
                .lock()
                .unwrap()
                .iter()
                .any(|n| n.contains("esperando"))
        );

        // Con un periodo largo no se espera: se pregunta al usuario.
        let slow = Totp::new(
            b"periodo-largo".to_vec(),
            crate::totp::TotpAlgorithm::Sha1,
            6,
            3600,
        )
        .unwrap();
        assert!(fresh_code(&slow, &status).await.is_some());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        if slow.seconds_left_at(now) > 60 {
            assert!(fresh_code(&slow, &status).await.is_none());
        }
    }
}
