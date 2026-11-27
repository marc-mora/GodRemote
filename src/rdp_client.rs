//! Cliente RDP integrado, sobre IronRDP (Rust puro) y TLS de rustls.
//!
//! [`spawn`] lanza una tarea en segundo plano que conecta (X.224 → TLS →
//! NLA/CredSSP si el servidor lo pide → sesión activa), pinta el escritorio
//! remoto en un [`Framebuffer`] compartido y traduce los [`DesktopInput`] de
//! la interfaz a eventos RDP.
//!
//! El certificado TLS del servidor no se valida contra ninguna autoridad (en
//! RDP lo habitual son certificados autofirmados): la confianza es «a la
//! primera» (TOFU) por huella, con [`crate::trust`], y se decide siempre
//! antes de enviar ninguna credencial.

use std::sync::MutexGuard;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use ironrdp_cliprdr::CliprdrClient;
use ironrdp_connector::connection_activation::ConnectionActivationState;
use ironrdp_connector::credssp::CredsspSequence;
use ironrdp_connector::sspi::generator::NetworkRequest;
use ironrdp_connector::{
    BitmapConfig, ClientConnector, ClientConnectorState, Config, ConnectionResult, ConnectorError,
    ConnectorErrorKind, ConnectorResult, Credentials, DesktopSize, ServerName,
};
use ironrdp_core::WriteBuf;
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_pdu::gcc::{ConnectionType, KeyboardType};
use ironrdp_pdu::geometry::InclusiveRectangle;
use ironrdp_pdu::input::fast_path::{FastPathInputEvent, SynchronizeFlags};
use ironrdp_pdu::nego::{FailureCode, SecurityProtocol};
use ironrdp_pdu::rdp::capability_sets::{
    MajorPlatformType, RailSupportLevel, client_codecs_capabilities,
};
use ironrdp_pdu::rdp::client_info::{CompressionType, PerformanceFlags, TimezoneInfo};
use ironrdp_session::image::DecodedImage;
use ironrdp_session::{
    ActiveStageBuilder, ActiveStageOutput, GracefulDisconnectReason, SessionError,
};
use ironrdp_tokio::{FramedWrite as _, TokioFramed, single_sequence_step_read, split_tokio_framed};
use tokio::sync::mpsc;
use tokio_rustls::client::TlsStream;

use crate::desktop::{DesktopBackend, DesktopEvent, DesktopInput, Framebuffer, SharedFrame};
use crate::model::Session;
use crate::ssh::{Endpoint, NetStream, Prompter, Route};
use crate::trust::{self, TrustStatus};

mod clipboard;
mod input;
mod tls;

use self::clipboard::{ClipSignal, Clipboard, TextClipboard, with_cliprdr};
use self::input::InputEncoder;

/// Límite de cada fase posterior a la conexión TCP (negociación, TLS,
/// autenticación, reactivación).
const PHASE_TIMEOUT: Duration = Duration::from_secs(30);
/// Margen para que el servidor confirme un cierre ordenado.
const CLOSE_GRACE: Duration = Duration::from_secs(2);
const MAX_PASSWORD_ATTEMPTS: u32 = 3;
/// Tamaños de escritorio que admite el protocolo.
const MIN_SIZE: u16 = 200;
const MAX_SIZE: u16 = 8192;
/// Eventos de entrada que se agrupan como mucho en un mismo envío.
const MAX_INPUT_BATCH: usize = 128;
/// Identificador de la distribución de teclado «inglés (EE. UU.)».
const KEYBOARD_LAYOUT_EN_US: u32 = 0x0409;

type TlsFramed = TokioFramed<TlsStream<NetStream>>;

/// Envía avisos a la interfaz y la despierta.
#[derive(Clone)]
struct Emitter {
    tx: std::sync::mpsc::Sender<DesktopEvent>,
    prompter: Prompter,
}

impl Emitter {
    fn emit(&self, event: DesktopEvent) {
        let _ = self.tx.send(event);
        self.prompter.wake();
    }

    fn status(&self, text: impl Into<String>) {
        self.emit(DesktopEvent::Status(text.into()));
    }
}

/// Abre una sesión RDP en segundo plano.
///
/// `password` es la contraseña guardada en el baúl (si no hay, se pregunta al
/// usuario) y `size` el tamaño de escritorio deseado en píxeles. El servidor
/// puede imponer otro: el [`Framebuffer`] tiene el negociado al llegar
/// [`DesktopEvent::Connected`], y aún puede cambiar después si el servidor
/// reactiva la sesión con otra resolución (cambia entonces su `epoch`).
pub fn spawn(
    rt: &tokio::runtime::Handle,
    session: Session,
    password: Option<String>,
    size: (u16, u16),
    prompter: Prompter,
) -> DesktopBackend {
    spawn_via(rt, session, password, size, None, prompter)
}

/// Como [`spawn`], llegando al servidor a través de saltos SSH (`via` es el
/// último salto de la cadena). El certificado y las credenciales se siguen
/// asociando al nombre y puerto reales del servidor RDP.
pub fn spawn_via(
    rt: &tokio::runtime::Handle,
    session: Session,
    password: Option<String>,
    size: (u16, u16),
    via: Option<Endpoint>,
    prompter: Prompter,
) -> DesktopBackend {
    let (input_tx, input_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    let size = clamp_size(size);
    let frame = Framebuffer::shared(u32::from(size.0), u32::from(size.1));
    let emit = Emitter {
        tx: ev_tx,
        prompter,
    };
    let task_frame = frame.clone();
    rt.spawn(async move {
        // La sesión corre en su propia tarea: así un fallo interno (un pánico
        // al decodificar datos del servidor) acaba como un cierre con error
        // y no como una pestaña que se queda muda.
        let inner = tokio::spawn(run(
            session,
            password,
            size,
            via,
            emit.clone(),
            input_rx,
            task_frame,
        ));
        let error = match inner.await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(format!("{e:#}")),
            Err(_) => Some("Error interno del cliente RDP".to_string()),
        };
        emit.emit(DesktopEvent::Closed(error));
    });
    DesktopBackend {
        input: input_tx,
        events: ev_rx,
        frame,
    }
}

/// El tamaño de escritorio que impone el servidor, si es admisible: uno
/// desmesurado obligaría a reservar gigas de memoria para la imagen.
fn checked_size(size: DesktopSize) -> Result<DesktopSize> {
    let valid = 1..=MAX_SIZE;
    if !valid.contains(&size.width) || !valid.contains(&size.height) {
        bail!(
            "El servidor anunció un tamaño de escritorio no válido ({}×{})",
            size.width,
            size.height
        );
    }
    Ok(size)
}

/// Ajusta el tamaño pedido a lo que admite RDP: entre 200 y 8192 píxeles y
/// con el ancho múltiplo de 4.
fn clamp_size((width, height): (u16, u16)) -> (u16, u16) {
    (
        width.clamp(MIN_SIZE, MAX_SIZE) & !3,
        height.clamp(MIN_SIZE, MAX_SIZE),
    )
}

async fn run(
    session: Session,
    password: Option<String>,
    size: (u16, u16),
    via: Option<Endpoint>,
    emit: Emitter,
    mut input_rx: mpsc::UnboundedReceiver<DesktopInput>,
    frame: SharedFrame,
) -> Result<()> {
    let mut clipboard = Clipboard::new(session.rdp.clipboard);
    let (clip_tx, mut clip_rx) = mpsc::unbounded_channel();
    let clip_tx = session.rdp.clipboard.then_some(clip_tx);

    // Mientras se conecta se atiende ya la entrada: cerrar la pestaña cancela
    // la conexión y el portapapeles local se recuerda para anunciarlo luego.
    let established = tokio::select! {
        result = connect(&session, password, size, via.as_ref(), &emit, clip_tx) => result?,
        () = wait_close(&mut input_rx, &mut clipboard) => return Ok(()),
    };

    let desktop = checked_size(established.result.desktop_size)?;
    lock(&frame).resize(u32::from(desktop.width), u32::from(desktop.height));
    if !established.to_save.is_empty() {
        emit.emit(DesktopEvent::SaveSecrets(established.to_save.clone()));
    }
    emit.emit(DesktopEvent::Connected);

    let mut close_requested = false;
    let result = active_session(
        established,
        &mut input_rx,
        &mut clip_rx,
        &mut clipboard,
        &frame,
        &emit,
        &mut close_requested,
    )
    .await;
    // Una vez pedido el cierre, da igual cómo termine la conexión (lo normal
    // es que el servidor la corte sin más).
    if close_requested { Ok(()) } else { result }
}

/// Espera a que la interfaz pida cerrar (o suelte el canal de entrada).
async fn wait_close(
    input_rx: &mut mpsc::UnboundedReceiver<DesktopInput>,
    clipboard: &mut Clipboard,
) {
    loop {
        match input_rx.recv().await {
            None | Some(DesktopInput::Close) => return,
            Some(DesktopInput::ClipboardText(text)) => clipboard.remember_local(text),
            Some(_) => {}
        }
    }
}

fn lock(frame: &SharedFrame) -> MutexGuard<'_, Framebuffer> {
    // Un pánico de la interfaz con el cerrojo tomado no debe tumbar la sesión.
    frame
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// Conexión
// ---------------------------------------------------------------------------

/// Sesión establecida, lista para la fase activa.
struct Established {
    framed: TlsFramed,
    result: ConnectionResult,
    /// Secretos validados que el usuario pidió guardar.
    to_save: Vec<(String, String)>,
    /// Cadena de saltos SSH (si la hay): debe vivir mientras dure la sesión.
    _route: Option<Route>,
}

/// Resultado de un intento de conexión.
enum Attempt {
    Connected(Box<Established>),
    /// Hubo que esperar al usuario: se reconecta con una conexión nueva para
    /// no depender de los tiempos de espera del servidor.
    Retry,
    /// El servidor rechazó las credenciales (con el detalle que dio).
    AuthRejected(String),
}

/// Datos fijos de la conexión.
struct Target<'a> {
    session: &'a Session,
    host: String,
    port: u16,
    user: String,
    domain: Option<String>,
    size: (u16, u16),
    emit: &'a Emitter,
    clip_tx: Option<mpsc::UnboundedSender<ClipSignal>>,
    /// Camino hasta el servidor: directo o por una cadena de saltos SSH.
    route: Route,
}

/// Contraseña en uso y si el usuario pidió guardarla.
struct Secret {
    password: Option<String>,
    save: bool,
    /// Veces que el servidor la ha rechazado.
    rejected: u32,
}

async fn connect(
    session: &Session,
    password: Option<String>,
    size: (u16, u16),
    via: Option<&Endpoint>,
    emit: &Emitter,
    clip_tx: Option<mpsc::UnboundedSender<ClipSignal>>,
) -> Result<Established> {
    // Primero la cadena de saltos SSH (si la hay); se reutiliza en cada
    // intento de conexión con el servidor RDP.
    let status = |text: String| emit.status(text);
    let (route, hop_secrets) = Route::establish(via, &emit.prompter, &status).await?;
    let route = route.without_proxy(session.bypass_proxy);
    if !hop_secrets.is_empty() {
        emit.emit(DesktopEvent::SaveSecrets(hop_secrets));
    }
    let target = Target {
        session,
        host: session.host.trim().to_string(),
        port: session.port,
        user: session.username.trim().to_string(),
        domain: Some(session.rdp.domain.trim())
            .filter(|d| !d.is_empty())
            .map(str::to_string),
        size,
        emit,
        clip_tx,
        route,
    };
    let mut secret = Secret {
        password,
        save: false,
        rejected: 0,
    };
    // Como mucho: una reconexión tras preguntar al usuario y otra por cada
    // contraseña rechazada. El tope es solo una salvaguarda.
    for _ in 0..(MAX_PASSWORD_ATTEMPTS + 2) {
        match attempt(&target, &mut secret).await? {
            Attempt::Connected(mut established) => {
                established._route = Some(target.route);
                return Ok(*established);
            }
            Attempt::Retry => {}
            Attempt::AuthRejected(detail) => {
                secret.rejected += 1;
                if secret.rejected >= MAX_PASSWORD_ATTEMPTS {
                    bail!("Autenticación fallida: usuario o contraseña incorrectos ({detail})");
                }
                emit.status("El servidor rechazó la contraseña");
                // El servidor ya es de confianza: se pregunta aquí mismo y el
                // siguiente intento va directo a autenticarse.
                secret.password = None;
                secret.save = false;
                ask_password(&target, &mut secret).await?;
            }
        }
    }
    bail!("No se pudo completar la conexión")
}

/// Pide la contraseña al usuario (solo tras confiar en el servidor).
async fn ask_password(target: &Target<'_>, secret: &mut Secret) -> Result<()> {
    let Target { user, host, .. } = target;
    let message = if secret.rejected == 0 {
        format!("Contraseña de {user} en {host}")
    } else {
        format!("Contraseña incorrecta. Contraseña de {user} en {host}")
    };
    let answer = target
        .emit
        .prompter
        .secret("Escritorio remoto", &message, false, true)
        .await
        .ok_or_else(|| anyhow!("Autenticación cancelada"))?;
    secret.password = Some(answer.value);
    secret.save = answer.save;
    Ok(())
}

/// Aplica el límite de tiempo de una fase de la conexión.
async fn phase<T>(what: &'static str, fut: impl Future<Output = T>) -> Result<T> {
    tokio::time::timeout(PHASE_TIMEOUT, fut)
        .await
        .map_err(|_| anyhow!("Tiempo de espera agotado {what}"))
}

async fn attempt(target: &Target<'_>, secret: &mut Secret) -> Result<Attempt> {
    let Target {
        host, port, emit, ..
    } = target;
    let port = *port;

    emit.status(format!("Conectando a {host}:{port}…"));
    let connection = target.route.connect_with_addr(host, port).await?;
    let stream = connection.stream;
    // A través de un salto no hay socket propio: se usan direcciones neutras.
    let client_addr = connection
        .local_addr
        .unwrap_or_else(|| std::net::SocketAddr::from(([127, 0, 0, 1], 0)));
    let peer_ip = connection
        .peer_addr
        .map_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), |a| {
            a.ip()
        });

    emit.status("Negociando seguridad…");
    // La negociación va en claro y antes de saber con quién se habla: el
    // conector se crea sin credenciales (ni siquiera el usuario, que si no
    // viajaría en la petición inicial) y se le dan al confiar en el servidor.
    let mut connector = ClientConnector::new(build_config(target)?, client_addr);
    if let Some(tx) = &target.clip_tx {
        connector
            .attach_static_channel(CliprdrClient::new(Box::new(TextClipboard::new(tx.clone()))));
    }
    let mut framed = TokioFramed::new(stream);
    let should_upgrade = phase(
        "negociando con el servidor",
        ironrdp_tokio::connect_begin(&mut framed, &mut connector),
    )
    .await?
    .map_err(|e| connector_error("Fallo al negociar la conexión", &e))?;
    // ¿Ha elegido el servidor autenticación de red (NLA/CredSSP)?
    let nla = matches!(
        connector.state,
        ClientConnectorState::EnhancedSecurityUpgrade { selected_protocol }
            if selected_protocol.intersects(SecurityProtocol::HYBRID | SecurityProtocol::HYBRID_EX)
    );
    let (stream, leftover) = framed.into_inner();
    if !leftover.is_empty() {
        bail!("El servidor envió datos inesperados antes de la negociación TLS");
    }

    let tls = phase(
        "negociando TLS",
        tls::upgrade(stream, host.clone(), peer_ip),
    )
    .await??;
    let certificate = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|chain| chain.first())
        .map(|cert| cert.as_ref().to_vec())
        .ok_or_else(|| anyhow!("El servidor no presentó ningún certificado TLS"))?;

    // Confianza en el certificado: siempre antes de enviar credenciales.
    let fingerprint = trust::fingerprint(&certificate);
    let known_hosts = crate::paths::tls_known_hosts_file();
    let mut asked = false;
    match trust::check(&known_hosts, host, port, &fingerprint)? {
        TrustStatus::Trusted => {}
        TrustStatus::Unknown => {
            let accepted = emit
                .prompter
                .confirm_host_key(host, port, "certificado TLS".into(), fingerprint.clone())
                .await;
            if !accepted {
                bail!("Certificado rechazado por el usuario");
            }
            trust::remember(&known_hosts, host, port, &fingerprint)
                .with_context(|| format!("guardando en {}", known_hosts.display()))?;
            asked = true;
        }
        TrustStatus::Changed { expected } => bail!(
            "¡EL CERTIFICADO TLS DE {host}:{port} HA CAMBIADO! Puede ser un intento de \
             suplantación (ataque de intermediario) o que el servidor haya renovado su \
             certificado. Huella recibida: {fingerprint}. Huella recordada: {expected}. \
             Si el cambio es legítimo, elimina la entrada «{}:{port}» de {} y vuelve a conectar.",
            host.to_lowercase(),
            known_hosts.display()
        ),
    }

    let user = &target.user;
    if user.is_empty() {
        // Sin usuario no hay nada que autenticar desde aquí: con NLA es un
        // error y, si no, será el servidor quien lo pida en su pantalla.
        if nla {
            bail!("El servidor exige autenticación de red (NLA): indica el usuario en la sesión");
        }
    } else if secret.password.is_none() {
        ask_password(target, secret).await?;
        asked = true;
    }
    if asked {
        // El usuario puede haber tardado más de lo que el servidor espera.
        return Ok(Attempt::Retry);
    }
    let password = secret.password.clone().unwrap_or_default();

    emit.status("Autenticando…");
    // Con contraseña, el servidor inicia la sesión sin volver a preguntar.
    connector.config.autologon = !user.is_empty() && !password.is_empty();
    connector.config.credentials = Credentials::UsernamePassword {
        username: user.clone(),
        password: password.clone(),
    };
    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let mut framed = TokioFramed::new(tls);
    let server_name = ServerName::new(host.as_str());
    let mut public_key = Vec::new();

    if connector.should_perform_credssp() {
        // NLA: las credenciales viajan dentro de CredSSP, ligadas a la clave
        // pública del certificado que se acaba de aceptar.
        let ClientConnectorState::Credssp {
            selected_protocol, ..
        } = connector.state
        else {
            bail!("Estado inesperado del conector RDP antes de CredSSP");
        };
        public_key = tls::server_public_key(&certificate)?;
        let (sequence, ts_request) = CredsspSequence::init(
            connector.config.credentials.clone(),
            connector.config.domain.as_deref(),
            selected_protocol,
            server_name.clone(),
            public_key.clone(),
            None,
        )
        .map_err(|e| connector_error("No se pudo iniciar la autenticación (NLA)", &e))?;
        let mut buf = WriteBuf::new();
        let outcome = phase(
            "autenticando",
            ironrdp_tokio::perform_credssp(
                &mut framed,
                &mut NoNetwork,
                &mut buf,
                sequence,
                ts_request,
            ),
        )
        .await?;
        if let Err(e) = outcome {
            return match credssp_rejection(&e) {
                Some(detail) => Ok(Attempt::AuthRejected(detail)),
                None => Err(connector_error("Fallo en la autenticación (NLA)", &e)),
            };
        }
        connector.mark_credssp_as_done();
    }

    let outcome = phase(
        "estableciendo la sesión",
        ironrdp_tokio::connect_finalize(
            upgraded,
            connector,
            &mut framed,
            &mut NoNetwork,
            server_name,
            public_key,
            None,
        ),
    )
    .await?
    .map_err(|e| connector_error("No se pudo establecer la sesión", &e))?;
    let result = outcome
        .into_connected()
        .map_err(|_| anyhow!("El servidor pidió una redirección a otro equipo, no soportada"))?;

    let to_save = if secret.save {
        vec![(target.session.password_key(), password)]
    } else {
        vec![]
    };
    Ok(Attempt::Connected(Box::new(Established {
        framed,
        result,
        to_save,
        _route: None,
    })))
}

/// Configuración del conector, aún sin credenciales (se añaden al confiar
/// en el servidor).
fn build_config(target: &Target<'_>) -> Result<Config> {
    let codecs = client_codecs_capabilities(&[])
        .map_err(|e| anyhow!("No se pudieron preparar los códecs gráficos: {e}"))?;
    let platform = if cfg!(windows) {
        MajorPlatformType::WINDOWS
    } else if cfg!(target_os = "macos") {
        MajorPlatformType::MACINTOSH
    } else {
        MajorPlatformType::UNIX
    };
    Ok(Config {
        desktop_size: DesktopSize {
            width: target.size.0,
            height: target.size.1,
        },
        monitor_layout: None,
        desktop_scale_factor: 0,
        desktop_physical_size: None,
        desktop_orientation: None,
        device_scale_factor: None,
        monitor_extended_layout: None,
        relative_mouse_input: false,
        // El servidor elige: NLA si la admite, TLS con inicio de sesión
        // gráfico si no.
        enable_tls: true,
        enable_credssp: true,
        enable_standard_rdp_security: false,
        autologon: false,
        credentials: Credentials::UsernamePassword {
            username: String::new(),
            password: String::new(),
        },
        domain: target.domain.clone(),
        client_build: 0,
        client_name: "GodRemote".into(),
        keyboard_type: KeyboardType::IBM_ENHANCED,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        // Distribución estadounidense: es con la que se teclea (véase
        // `InputEncoder`) y la que corresponde a los códigos de `KeyCode`.
        keyboard_layout: KEYBOARD_LAYOUT_EN_US,
        connection_type: ConnectionType::Lan,
        ime_file_name: String::new(),
        bitmap: Some(BitmapConfig {
            lossy_compression: true,
            color_depth: 32,
            codecs,
        }),
        dig_product_id: String::new(),
        client_dir: "C:\\Windows\\System32\\mstscax.dll".into(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        remote_application_mode: false,
        rail_support_level: RailSupportLevel::empty(),
        platform,
        hardware_id: None,
        request_data: None,
        enable_audio_playback: false,
        enable_audio_capture: false,
        performance_flags: PerformanceFlags::default(),
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        compression_type: Some(CompressionType::K64),
        // El puntero remoto se dibuja dentro de la propia imagen.
        enable_server_pointer: true,
        pointer_software_rendering: true,
        multitransport_flags: None,
        support_dyn_vc_gfx_protocol: false,
    })
}

/// CredSSP solo usa NTLM aquí: no hay peticiones de red a un KDC (Kerberos).
struct NoNetwork;

impl ironrdp_tokio::NetworkClient for NoNetwork {
    async fn send(&mut self, _request: &NetworkRequest) -> ConnectorResult<Vec<u8>> {
        Err(ironrdp_connector::general_err!(
            "Kerberos no está soportado"
        ))
    }
}

/// Si el fallo de CredSSP es un rechazo de las credenciales, lo describe.
///
/// Unos servidores responden con un código de estado y otros simplemente
/// cortan la conexión; cualquier otro fallo (p. ej. una comprobación
/// criptográfica) no se trata como «contraseña incorrecta».
fn credssp_rejection(error: &ConnectorError) -> Option<String> {
    match error.kind() {
        ConnectorErrorKind::Credssp(e) if e.nstatus.is_some() => Some(e.to_string()),
        ConnectorErrorKind::AccessDenied => None,
        _ => connection_lost(error).then(|| "el servidor cerró la conexión".to_string()),
    }
}

/// ¿Se debe el error a que el servidor ha cortado la conexión?
fn connection_lost(error: &ConnectorError) -> bool {
    use std::io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};

    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return matches!(
                io.kind(),
                UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe
            );
        }
        source = cause.source();
    }
    false
}

/// Convierte un error del conector de IronRDP en uno legible.
fn connector_error(what: &str, error: &ConnectorError) -> anyhow::Error {
    match error.kind() {
        ConnectorErrorKind::Negotiation(failure) => {
            let reason = match failure.code() {
                FailureCode::SSL_REQUIRED_BY_SERVER => "el servidor exige TLS o NLA".to_string(),
                FailureCode::SSL_NOT_ALLOWED_BY_SERVER => {
                    "el servidor solo admite la seguridad RDP estándar (sin TLS), \
                     que no está soportada"
                        .to_string()
                }
                FailureCode::SSL_CERT_NOT_ON_SERVER => {
                    "el servidor no tiene un certificado válido".to_string()
                }
                FailureCode::HYBRID_REQUIRED_BY_SERVER => "el servidor exige NLA".to_string(),
                FailureCode::SSL_WITH_USER_AUTH_REQUIRED_BY_SERVER => {
                    "el servidor exige un certificado de cliente, no soportado".to_string()
                }
                FailureCode::ENTRA_AUTH_REQUIRED_BY_SERVER => {
                    "el servidor exige autenticación con Microsoft Entra, no soportada".to_string()
                }
                _ => failure.to_string(),
            };
            anyhow!("{what}: {reason}")
        }
        ConnectorErrorKind::AccessDenied => {
            anyhow!("{what}: el servidor denegó el acceso a este usuario")
        }
        _ if connection_lost(error) => anyhow!("{what}: el servidor cerró la conexión"),
        _ => anyhow!("{what}: {}", error.report()),
    }
}

fn session_error(error: SessionError) -> anyhow::Error {
    anyhow!("Error en la sesión RDP: {}", error.report())
}

// ---------------------------------------------------------------------------
// Sesión activa
// ---------------------------------------------------------------------------

async fn active_session(
    established: Established,
    input_rx: &mut mpsc::UnboundedReceiver<DesktopInput>,
    clip_rx: &mut mpsc::UnboundedReceiver<ClipSignal>,
    clipboard: &mut Clipboard,
    frame: &SharedFrame,
    emit: &Emitter,
    close_requested: &mut bool,
) -> Result<()> {
    let ConnectionResult {
        io_channel_id,
        user_channel_id,
        message_channel_id,
        share_id,
        static_channels,
        desktop_size,
        input_flags,
        enable_server_pointer,
        pointer_software_rendering,
        activation_factory,
        compression_type,
        ..
    } = established.result;
    let (mut reader, mut writer) = split_tokio_framed(established.framed);
    let mut image = DecodedImage::new(PixelFormat::RgbA32, desktop_size.width, desktop_size.height);
    let mut stage = ActiveStageBuilder {
        static_channels,
        user_channel_id,
        io_channel_id,
        message_channel_id,
        share_id,
        compression_type,
        enable_server_pointer,
        pointer_software_rendering,
    }
    .build();
    let mut encoder = InputEncoder::new(desktop_size, input_flags);

    // Vencimientos internos del canal de portapapeles.
    let mut ticker = tokio::time::interval(Duration::from_secs(5));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Desde que se pide el cierre ordenado: hasta cuándo se espera al servidor.
    let mut closing: Option<tokio::time::Instant> = None;
    // Lo primero, los bloqueos del teclado remoto en un estado conocido (sin
    // Bloq Mayús): el texto se teclea contando con ello.
    let sync = FastPathInputEvent::SyncEvent(SynchronizeFlags::NUM_LOCK);
    for output in stage
        .process_fastpath_input(&mut image, &[sync])
        .map_err(session_error)?
    {
        if let ActiveStageOutput::ResponseFrame(data) = output {
            writer
                .write_all(&data)
                .await
                .context("Se perdió la conexión con el servidor")?;
        }
    }

    loop {
        let grace = async {
            match closing {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        let outputs = tokio::select! {
            pdu = reader.read_pdu() => match pdu {
                Ok((action, payload)) => stage
                    .process(&mut image, action, &payload, reader.last_read_at())
                    .map_err(session_error)?,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    bail!("El servidor cerró la conexión")
                }
                Err(e) => return Err(anyhow!(e).context("Se perdió la conexión con el servidor")),
            },
            input = input_rx.recv(), if closing.is_none() => {
                let mut events = Vec::new();
                let mut outputs = Vec::new();
                let mut close = false;
                let mut next = input;
                // Se agrupa lo que haya pendiente (ráfagas de movimiento…).
                loop {
                    match next {
                        None | Some(DesktopInput::Close) => {
                            close = true;
                            break;
                        }
                        Some(DesktopInput::ClipboardText(text)) => {
                            outputs.extend(with_cliprdr(&mut stage, |cliprdr| {
                                clipboard.local_copy(text, cliprdr)
                            }));
                        }
                        Some(other) => encoder.push(other, &mut events),
                    }
                    if events.len() >= MAX_INPUT_BATCH {
                        break;
                    }
                    match input_rx.try_recv() {
                        Ok(more) => next = Some(more),
                        Err(_) => break,
                    }
                }
                if !events.is_empty() {
                    let mut sent = stage
                        .process_fastpath_input(&mut image, &events)
                        .map_err(session_error)?;
                    sent.append(&mut outputs);
                    outputs = sent;
                }
                if close {
                    *close_requested = true;
                    closing = Some(tokio::time::Instant::now() + CLOSE_GRACE);
                    outputs.extend(stage.graceful_shutdown().map_err(session_error)?);
                }
                outputs
            },
            signal = clip_rx.recv(), if clipboard.enabled => match signal {
                Some(signal) => {
                    let mut text = None;
                    let outputs = with_cliprdr(&mut stage, |cliprdr| {
                        let reply = clipboard.on_signal(signal, cliprdr);
                        text = reply.text;
                        reply.messages
                    });
                    if let Some(text) = text {
                        emit.emit(DesktopEvent::ClipboardText(text));
                    }
                    outputs
                }
                None => {
                    clipboard.enabled = false;
                    Vec::new()
                }
            },
            _ = ticker.tick() => with_cliprdr(&mut stage, |cliprdr| cliprdr.drive_timeouts().ok()),
            () = grace => return Ok(()),
        };

        let mut painted = false;
        for output in outputs {
            match output {
                ActiveStageOutput::ResponseFrame(data) => {
                    if !data.is_empty() {
                        writer
                            .write_all(&data)
                            .await
                            .context("Se perdió la conexión con el servidor")?;
                    }
                }
                ActiveStageOutput::GraphicsUpdate(region) => {
                    blit(frame, &image, &region);
                    painted = true;
                }
                ActiveStageOutput::Terminate(reason) => {
                    return match reason {
                        GracefulDisconnectReason::UserInitiated
                        | GracefulDisconnectReason::ServerInitiated => Ok(()),
                        // IronRDP solo da el texto del motivo. El más común,
                        // que el usuario cierre su sesión en el remoto, no es
                        // un error.
                        GracefulDisconnectReason::Other(description)
                            if description.contains("user logging off") =>
                        {
                            Ok(())
                        }
                        GracefulDisconnectReason::Other(description) => {
                            Err(anyhow!("El servidor cerró la sesión: {description}"))
                        }
                    };
                }
                ActiveStageOutput::DeactivateAll => {
                    // Secuencia de desactivación-reactivación: el servidor
                    // renegocia capacidades (p. ej. al cambiar de resolución).
                    let mut sequence = activation_factory.create();
                    let mut buf = WriteBuf::new();
                    let size = loop {
                        let written = phase(
                            "reactivando la sesión",
                            single_sequence_step_read(&mut reader, &mut sequence, &mut buf),
                        )
                        .await?
                        .map_err(|e| connector_error("Fallo al reactivar la sesión", &e))?;
                        if written.size().is_some() {
                            writer
                                .write_all(buf.filled())
                                .await
                                .context("Se perdió la conexión con el servidor")?;
                        }
                        match sequence.connection_activation_state() {
                            ConnectionActivationState::Finalized {
                                desktop_size,
                                share_id,
                                input_flags,
                                static_channel_chunk_size,
                                enable_server_pointer,
                                pointer_software_rendering,
                                ..
                            } => {
                                if !stage.reactivate(
                                    sequence.io_channel_id(),
                                    sequence.user_channel_id(),
                                    share_id,
                                    enable_server_pointer,
                                    pointer_software_rendering,
                                    static_channel_chunk_size,
                                ) {
                                    bail!("El servidor negoció parámetros no válidos al reactivar");
                                }
                                encoder.set_capabilities(input_flags);
                                break checked_size(desktop_size)?;
                            }
                            ConnectionActivationState::Redirected { .. } => bail!(
                                "El servidor pidió una redirección a otro equipo, no soportada"
                            ),
                            _ => {}
                        }
                    };
                    image = DecodedImage::new(PixelFormat::RgbA32, size.width, size.height);
                    encoder.set_size(size);
                    lock(frame).resize(u32::from(size.width), u32::from(size.height));
                    painted = true;
                }
                // El puntero se dibuja en la imagen; el resto son avisos que
                // esta interfaz no usa.
                _ => {}
            }
        }
        if painted {
            emit.prompter.wake();
        }
    }
}

/// Copia a la imagen compartida la zona que ha cambiado (con alfa opaco).
fn blit(frame: &SharedFrame, image: &DecodedImage, region: &InclusiveRectangle) {
    let width = usize::from(image.width());
    let height = usize::from(image.height());
    let left = usize::from(region.left);
    let top = usize::from(region.top);
    if left >= width || top >= height || region.right < region.left || region.bottom < region.top {
        return;
    }
    let right = usize::from(region.right).min(width - 1);
    let bottom = usize::from(region.bottom).min(height - 1);
    let source = image.data();

    let mut fb = lock(frame);
    if fb.width() as usize != width || fb.height() as usize != height {
        return;
    }
    let stride = width * 4;
    let len = (right - left + 1) * 4;
    for row in top..=bottom {
        let start = row * stride + left * 4;
        let (Some(src), Some(dst)) = (
            source.get(start..start + len),
            fb.pixels.get_mut(start..start + len),
        ) else {
            return;
        };
        dst.copy_from_slice(src);
        for pixel in dst.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }
    // Las coordenadas caben en u32: la imagen mide como mucho 65535 píxeles.
    fb.mark_dirty(
        left as u32,
        top as u32,
        (right - left + 1) as u32,
        (bottom - top + 1) as u32,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_clamped() {
        assert_eq!(clamp_size((1280, 720)), (1280, 720));
        assert_eq!(clamp_size((1283, 721)), (1280, 721));
        assert_eq!(clamp_size((10, 10)), (200, 200));
        assert_eq!(clamp_size((u16::MAX, u16::MAX)), (8192, 8192));
    }

    #[test]
    fn server_sizes_are_validated() {
        let size = |width, height| DesktopSize { width, height };
        assert!(checked_size(size(1024, 768)).is_ok());
        assert!(checked_size(size(8192, 8192)).is_ok());
        for bad in [
            size(0, 768),
            size(1024, 0),
            size(8193, 768),
            size(u16::MAX, u16::MAX),
        ] {
            let error = checked_size(bad).unwrap_err().to_string();
            assert!(error.contains("no válido"), "{error}");
        }
    }

    #[test]
    fn blit_copies_only_the_region_with_opaque_alpha() {
        let frame = Framebuffer::shared(4, 3);
        lock(&frame).take_dirty();
        let image = DecodedImage::new(PixelFormat::RgbA32, 4, 3);
        // Región que se sale por la derecha y por abajo: se recorta.
        blit(
            &frame,
            &image,
            &InclusiveRectangle {
                left: 2,
                top: 1,
                right: 9,
                bottom: 9,
            },
        );
        let mut fb = lock(&frame);
        let dirty = fb.take_dirty().unwrap();
        assert_eq!((dirty.x, dirty.y, dirty.width, dirty.height), (2, 1, 2, 2));
        assert!(fb.pixels.chunks_exact(4).all(|px| px[3] == 255));
        drop(fb);
        // Región totalmente fuera: no se toca nada.
        blit(
            &frame,
            &image,
            &InclusiveRectangle {
                left: 4,
                top: 0,
                right: 5,
                bottom: 0,
            },
        );
        assert!(lock(&frame).take_dirty().is_none());
    }
}
