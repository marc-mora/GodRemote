//! El agente SSH de este equipo: conexión (el de OpenSSH o, en Windows,
//! Pageant), listado de sus claves y reenvío a un equipo remoto (`ssh -A`).

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::ssh_key::HashAlg;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::i18n::tr;
use crate::keys::CertInfo;
use crate::trf;

/// Qué agente SSH usar (ajuste; solo cambia algo en Windows, donde conviven
/// el agente de OpenSSH y Pageant, el de PuTTY).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AgentChoice {
    /// El de OpenSSH y, si no está o no tiene claves, Pageant.
    #[default]
    #[serde(rename = "automatico")]
    Auto,
    #[serde(rename = "openssh")]
    OpenSsh,
    #[serde(rename = "pageant")]
    Pageant,
}

impl AgentChoice {
    pub const ALL: [AgentChoice; 3] = [
        AgentChoice::Auto,
        AgentChoice::OpenSsh,
        AgentChoice::Pageant,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AgentChoice::Auto => tr("Automático (OpenSSH y, si no, Pageant)"),
            AgentChoice::OpenSsh => tr("Agente de OpenSSH"),
            AgentChoice::Pageant => "Pageant (PuTTY)",
        }
    }

    /// Agentes que se prueban, en orden.
    fn candidates(self) -> &'static [AgentKind] {
        // Fuera de Windows no hay Pageant: siempre el de `SSH_AUTH_SOCK`.
        if !cfg!(windows) {
            return &[AgentKind::OpenSsh];
        }
        match self {
            AgentChoice::Auto => &[AgentKind::OpenSsh, AgentKind::Pageant],
            AgentChoice::OpenSsh => &[AgentKind::OpenSsh],
            AgentChoice::Pageant => &[AgentKind::Pageant],
        }
    }
}

/// Ajuste en vigor. Lo fija la interfaz; las conexiones lo leen.
static CHOICE: AtomicU8 = AtomicU8::new(0);

pub fn set_choice(choice: AgentChoice) {
    CHOICE.store(choice as u8, Ordering::Relaxed);
}

pub fn choice() -> AgentChoice {
    AgentChoice::ALL
        .into_iter()
        .find(|c| *c as u8 == CHOICE.load(Ordering::Relaxed))
        .unwrap_or_default()
}

/// Un agente SSH concreto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// El agente de OpenSSH: `SSH_AUTH_SOCK` en Unix, su tubería en Windows.
    OpenSsh,
    /// Pageant, el agente de PuTTY (solo Windows).
    Pageant,
}

impl AgentKind {
    pub fn label(self) -> &'static str {
        match self {
            AgentKind::OpenSsh => tr("el agente de OpenSSH"),
            AgentKind::Pageant => "Pageant",
        }
    }
}

/// Cliente de un agente, sea cual sea su transporte.
pub type Client = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

/// Tubería del agente de OpenSSH para Windows.
#[cfg(windows)]
const OPENSSH_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

/// Lo que se espera a que un agente conteste al conectar y al listar (firmar
/// no tiene límite: puede estar esperando a que el usuario confirme).
const ANSWER_TIMEOUT: Duration = Duration::from_secs(15);

/// Abre una conexión con un agente concreto.
async fn open(kind: AgentKind) -> Result<Client> {
    // (La tubería de Windows se reintenta sin fin mientras esté ocupada.)
    tokio::time::timeout(ANSWER_TIMEOUT, open_now(kind))
        .await
        .map_err(|_| anyhow!("{}", trf!("{agent} no contesta", agent = kind.label())))?
}

async fn open_now(kind: AgentKind) -> Result<Client> {
    match kind {
        AgentKind::OpenSsh => {
            #[cfg(unix)]
            {
                let socket = std::env::var_os("SSH_AUTH_SOCK")
                    .filter(|path| !path.is_empty())
                    .context(tr("no hay agente SSH (variable SSH_AUTH_SOCK sin definir)"))?;
                let client = AgentClient::connect_uds(socket).await.context(tr(
                    "no se pudo conectar con el agente SSH (¿SSH_AUTH_SOCK?)",
                ))?;
                Ok(client.dynamic())
            }
            #[cfg(windows)]
            {
                let client = AgentClient::connect_named_pipe(OPENSSH_PIPE)
                    .await
                    .context(tr(
                        "no se pudo conectar con el agente de OpenSSH (¿servicio ssh-agent?)",
                    ))?;
                Ok(client.dynamic())
            }
            #[cfg(not(any(unix, windows)))]
            {
                Err(anyhow!("{}", tr("no hay agente SSH en este sistema")))
            }
        }
        AgentKind::Pageant => {
            #[cfg(windows)]
            {
                let client = AgentClient::connect_pageant()
                    .await
                    .context(tr("no se pudo conectar con Pageant (¿está en marcha?)"))?;
                Ok(client.dynamic())
            }
            #[cfg(not(windows))]
            {
                Err(anyhow!("{}", tr("Pageant solo existe en Windows")))
            }
        }
    }
}

/// Un agente al que ya se le han pedido sus identidades.
pub struct Connected {
    pub client: Client,
    pub kind: AgentKind,
    pub identities: Vec<AgentIdentity>,
}

/// Conecta con un agente y le pide sus identidades. Es también la manera de
/// saber si está: conectar con Pageant no falla aunque no esté en marcha.
async fn probe(kind: AgentKind) -> Result<Connected> {
    let attempt = async {
        let mut client = open(kind).await?;
        let identities = client.request_identities().await.map_err(|e| {
            anyhow!(
                "{}",
                trf!(
                    "{agent} no devolvió sus claves: {e}",
                    agent = kind.label(),
                    e
                )
            )
        })?;
        Ok(Connected {
            client,
            kind,
            identities,
        })
    };
    tokio::time::timeout(ANSWER_TIMEOUT, attempt)
        .await
        .map_err(|_| anyhow!("{}", trf!("{agent} no contesta", agent = kind.label())))?
}

/// Conecta con el agente SSH que toque según el ajuste y le pide sus
/// identidades. En automático gana el primero que tenga alguna clave; si
/// ninguno tiene, el primero que conteste.
pub async fn connect() -> Result<Connected> {
    let mut empty = None;
    let mut errors = vec![];
    for kind in choice().candidates() {
        match probe(*kind).await {
            Ok(found) if !found.identities.is_empty() => return Ok(found),
            Ok(found) => {
                empty.get_or_insert(found);
            }
            Err(e) => errors.push(format!("{e:#}")),
        }
    }
    empty.ok_or_else(|| anyhow!("{}", errors.join("; ")))
}

/// Agente al que reenviar con `ssh -A`: el mismo que se usaría para
/// autenticarse.
pub async fn pick() -> Result<AgentKind> {
    Ok(connect().await?.kind)
}

/// Une un canal abierto por el servidor con el agente local. El protocolo
/// del agente pasa tal cual: aquí no se mira ni se guarda nada.
pub async fn bridge<S>(mut channel: S, kind: AgentKind) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut agent = open(kind).await?.into_inner();
    tokio::io::copy_bidirectional(&mut channel, &mut agent).await?;
    Ok(())
}

/// Una identidad cargada en el agente, para mostrarla.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentKey {
    pub algorithm: String,
    pub fingerprint: String,
    pub comment: String,
    /// Clave pública (o certificado) en formato `authorized_keys`.
    pub public_openssh: String,
    /// Datos del certificado, si la identidad es uno.
    pub certificate: Option<CertInfo>,
    /// Es de una llave de seguridad FIDO2: el agente firma con la llave.
    pub security_key: bool,
}

impl AgentKey {
    fn new(identity: &AgentIdentity) -> Self {
        match identity {
            AgentIdentity::PublicKey { key, comment } => Self {
                algorithm: key.algorithm().to_string(),
                fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
                comment: comment.clone(),
                public_openssh: key.to_openssh().unwrap_or_default(),
                certificate: None,
                security_key: crate::keys::is_security_key(&key.algorithm()),
            },
            AgentIdentity::Certificate {
                certificate,
                comment,
            } => Self {
                algorithm: certificate.algorithm().to_certificate_type(),
                fingerprint: certificate
                    .public_key()
                    .fingerprint(HashAlg::Sha256)
                    .to_string(),
                comment: comment.clone(),
                public_openssh: certificate.to_openssh().unwrap_or_default(),
                certificate: Some(CertInfo::from_certificate(
                    std::path::Path::new(""),
                    certificate,
                )),
                security_key: crate::keys::is_security_key(&certificate.algorithm()),
            },
        }
    }
}

/// Las claves de un agente.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentListing {
    pub kind: AgentKind,
    pub keys: Vec<AgentKey>,
}

/// Lista las claves cargadas en el agente SSH.
pub async fn list() -> Result<AgentListing> {
    let connected = connect().await?;
    Ok(AgentListing {
        kind: connected.kind,
        keys: connected.identities.iter().map(AgentKey::new).collect(),
    })
}

/// Como [`list`], para llamar desde un hilo que no es de tokio (bloquea).
pub fn list_blocking() -> Result<AgentListing> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context(tr("no se pudo preparar la consulta al agente"))?
        .block_on(list())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_is_remembered_and_serialised_in_spanish() {
        assert_eq!(choice(), AgentChoice::Auto);
        for wanted in AgentChoice::ALL {
            set_choice(wanted);
            assert_eq!(choice(), wanted);
        }
        set_choice(AgentChoice::Auto);
        assert_eq!(
            serde_json::to_string(&AgentChoice::ALL).unwrap(),
            r#"["automatico","openssh","pageant"]"#
        );
        // Pageant solo se prueba en Windows; fuera, siempre `SSH_AUTH_SOCK`.
        let expected: &[AgentKind] = if cfg!(windows) {
            &[AgentKind::Pageant]
        } else {
            &[AgentKind::OpenSsh]
        };
        assert_eq!(AgentChoice::Pageant.candidates(), expected);
        assert_eq!(AgentChoice::Auto.candidates()[0], AgentKind::OpenSsh);
    }
}
