//! Aviso de versión nueva. Al arrancar, y como mucho una vez al día, se
//! pregunta a GitHub cuál es la última versión publicada: una sola petición
//! HTTPS sin cuerpo ni datos del usuario (a través del proxy de salida, si
//! lo hay), cuya respuesta es la redirección a la página de esa versión. Si
//! es más nueva que la que se está usando, la barra de estado lo dice.
//!
//! Se puede desactivar en los ajustes, y solo existe en las ediciones
//! compiladas con `aviso-versiones` (la «SSH reforzada» no lo lleva: no hace
//! ninguna conexión que no se le pida).

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// ¿Lleva esta compilación el aviso de versiones?
pub const AVAILABLE: bool = cfg!(feature = "aviso-versiones");

/// Página que se abre desde el aviso.
pub const RELEASES_URL: &str = "https://github.com/marc-mora/GodRemote/releases/latest";

#[cfg(any(test, feature = "aviso-versiones"))]
const HOST: &str = "github.com";
#[cfg(any(test, feature = "aviso-versiones"))]
const PATH: &str = "/marc-mora/GodRemote/releases/latest";
/// Tiempo mínimo entre dos consultas.
const INTERVAL: Duration = Duration::from_secs(24 * 3600);
/// Tamaño máximo de la cabecera de respuesta que se lee.
#[cfg(any(test, feature = "aviso-versiones"))]
const MAX_HEAD: usize = 16 * 1024;

/// Función que despierta la interfaz (pide un repintado).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Lo que se recuerda entre arranques.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Record {
    /// Cuándo se consultó por última vez (segundos desde 1970).
    #[serde(rename = "consultado")]
    checked: u64,
    /// Última versión publicada que se vio.
    #[serde(rename = "ultima")]
    latest: String,
}

impl Record {
    fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) {
        if let Ok(data) = serde_json::to_vec_pretty(self) {
            let _ = crate::paths::write_private(path, &data);
        }
    }

    /// ¿Toca volver a preguntar?
    fn is_due(&self, now: u64) -> bool {
        // (Un reloj que ha ido hacia atrás no bloquea la consulta.)
        now < self.checked || now - self.checked >= INTERVAL.as_secs()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `(mayor, menor, parche)` de «v1.2.3» o «1.2.3». Nada más se acepta: lo
/// que llega de fuera no debe poder colar otra cosa en el aviso.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let text = text.trim();
    let text = text.strip_prefix('v').unwrap_or(text);
    let mut parts = text.split('.');
    let mut next = || -> Option<u64> {
        let part = parts.next()?;
        (!part.is_empty() && part.len() <= 6 && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())?
    };
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

/// ¿Es `latest` una versión posterior a `current`?
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// La versión a la que redirige la respuesta de «última versión»:
/// `Location: https://github.com/…/releases/tag/v1.2.3`.
#[cfg(any(test, feature = "aviso-versiones"))]
fn version_from_response(head: &str) -> Option<String> {
    let mut lines = head.lines();
    let status = lines.next()?.split_whitespace().nth(1)?;
    if !matches!(status, "301" | "302" | "303" | "307" | "308") {
        return None;
    }
    let location = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("location")
            .then(|| value.trim())
    })?;
    let tag = location.rsplit_once("/releases/tag/")?.1;
    let (major, minor, patch) = parse_version(tag)?;
    Some(format!("{major}.{minor}.{patch}"))
}

/// Hace la pregunta por una conexión ya abierta y devuelve la versión.
#[cfg(any(test, feature = "aviso-versiones"))]
async fn ask<S>(stream: &mut S) -> anyhow::Result<Option<String>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let request = format!(
        "HEAD {PATH} HTTP/1.1\r\nHost: {HOST}\r\nUser-Agent: GodRemote/{}\r\n\
         Accept: */*\r\nConnection: close\r\n\r\n",
        env!("CARGO_PKG_VERSION")
    );
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;
    let mut head = Vec::new();
    let mut chunk = [0u8; 2048];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < MAX_HEAD {
        match stream.read(&mut chunk).await? {
            0 => break,
            n => head.extend_from_slice(&chunk[..n]),
        }
    }
    Ok(version_from_response(&String::from_utf8_lossy(&head)))
}

#[cfg(feature = "aviso-versiones")]
async fn fetch() -> anyhow::Result<Option<String>> {
    use tokio_rustls::rustls;
    const TIMEOUT: Duration = Duration::from_secs(15);
    let work = async {
        let tcp = crate::proxy::dial(HOST, 443, false, TIMEOUT).await?;
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(HOST)?;
        let mut tls = tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(name, tcp)
            .await?;
        ask(&mut tls).await
    };
    tokio::time::timeout(TIMEOUT, work).await?
}

#[cfg(not(feature = "aviso-versiones"))]
async fn fetch() -> anyhow::Result<Option<String>> {
    Ok(None)
}

/// El aviso: lo que se sabía de la última consulta y, si toca, una nueva.
pub struct UpdateCheck {
    newer: Arc<Mutex<Option<String>>>,
}

impl UpdateCheck {
    /// Con `enabled` a falso no se consulta ni se avisa de nada.
    pub fn start(rt: &tokio::runtime::Handle, enabled: bool, waker: Waker) -> Self {
        let check = Self {
            newer: Arc::default(),
        };
        if !enabled || !AVAILABLE {
            return check;
        }
        let path = crate::paths::update_file();
        let record = Record::load(&path);
        check.note(&record.latest);
        if record.is_due(now()) {
            let newer = check.newer.clone();
            rt.spawn(async move {
                // Si no hay red (o no hay versiones) no pasa nada: se
                // volverá a intentar en el próximo arranque.
                let Ok(Some(latest)) = fetch().await else {
                    return;
                };
                Record {
                    checked: now(),
                    latest: latest.clone(),
                }
                .save(&path);
                if is_newer(&latest, env!("CARGO_PKG_VERSION")) {
                    *newer.lock().unwrap_or_else(PoisonError::into_inner) = Some(latest);
                    waker();
                }
            });
        }
        check
    }

    fn note(&self, latest: &str) {
        if is_newer(latest, env!("CARGO_PKG_VERSION")) {
            *self.newer.lock().unwrap_or_else(PoisonError::into_inner) = Some(latest.to_string());
        }
    }

    /// La versión publicada más nueva que esta, si se sabe de alguna.
    pub fn newer(&self) -> Option<String> {
        self.newer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_parsed_strictly_and_compared() {
        assert_eq!(parse_version("v0.8.0"), Some((0, 8, 0)));
        assert_eq!(parse_version(" 1.20.3\n"), Some((1, 20, 3)));
        for bad in [
            "",
            "v",
            "1.2",
            "1.2.3.4",
            "1.2.x",
            "v1.2.3-beta",
            "1..3",
            "-1.2.3",
            "1.2.3 4",
            "9999999.1.1",
            "<b>1.2.3</b>",
        ] {
            assert_eq!(parse_version(bad), None, "{bad:?}");
        }
        assert!(is_newer("0.8.1", "0.8.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.8.0", "0.8.0"));
        assert!(!is_newer("0.7.9", "0.8.0"));
        // Lo que no es una versión nunca es «más nuevo».
        assert!(!is_newer("ultima", "0.8.0"));
        assert!(!is_newer("9.9.9", "desconocida"));
    }

    #[test]
    fn the_redirect_gives_the_version() {
        let ok = "HTTP/1.1 302 Found\r\nServer: github.com\r\n\
                  Location: https://github.com/marc-mora/GodRemote/releases/tag/v1.4.2\r\n\r\n";
        assert_eq!(version_from_response(ok).as_deref(), Some("1.4.2"));
        // Cabeceras en minúsculas (HTTP/2 traducido) y otros códigos de redirección.
        let lower =
            "HTTP/1.1 301 Moved\r\nlocation: /marc-mora/GodRemote/releases/tag/0.9.0\r\n\r\n";
        assert_eq!(version_from_response(lower).as_deref(), Some("0.9.0"));
        // Sin versiones publicadas se redirige a la lista: no hay nada.
        let none = "HTTP/1.1 302 Found\r\nLocation: https://github.com/marc-mora/GodRemote/releases\r\n\r\n";
        assert_eq!(version_from_response(none), None);
        // Ni un 200, ni una etiqueta rara, ni basura cuentan.
        for bad in [
            "HTTP/1.1 200 OK\r\nLocation: https://x/releases/tag/v1.2.3\r\n\r\n",
            "HTTP/1.1 302 Found\r\nLocation: https://x/releases/tag/v1.2.3-rc1\r\n\r\n",
            "HTTP/1.1 302 Found\r\nLocation: https://x/releases/tag/<script>\r\n\r\n",
            "HTTP/1.1 302 Found\r\n\r\n",
            "",
            "\u{0}\u{1}\u{2}",
        ] {
            assert_eq!(version_from_response(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_question_is_one_small_request() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let (mut client, mut server) = tokio::io::duplex(64 * 1024);
            let served = tokio::spawn(async move {
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    server.read_exact(&mut byte).await.unwrap();
                    request.push(byte[0]);
                }
                server
                    .write_all(
                        b"HTTP/1.1 302 Found\r\nLocation: https://github.com/marc-mora/GodRemote/releases/tag/v9.9.9\r\nContent-Length: 0\r\n\r\n",
                    )
                    .await
                    .unwrap();
                String::from_utf8(request).unwrap()
            });
            assert_eq!(ask(&mut client).await.unwrap().as_deref(), Some("9.9.9"));
            let request = served.await.unwrap();
            assert!(request.starts_with("HEAD /marc-mora/GodRemote/releases/latest HTTP/1.1\r\n"));
            assert!(request.contains("\r\nHost: github.com\r\n"));
            // Nada más que la versión del programa viaja en la petición.
            let lines: Vec<&str> = request.trim_end().split("\r\n").collect();
            assert_eq!(lines.len(), 5, "{request}");
            assert!(lines.iter().all(|l| !l.to_lowercase().contains("cookie")));

            // Una respuesta enorme o que no termina no se lee sin límite.
            let (mut client, mut server) = tokio::io::duplex(64 * 1024);
            tokio::spawn(async move {
                let junk = vec![b'x'; 8 * 1024];
                while server.write_all(&junk).await.is_ok() {}
            });
            assert_eq!(ask(&mut client).await.unwrap(), None);
        });
    }

    /// La consulta de verdad, contra github.com. No corre con las demás
    /// (depende de la red): `cargo test --lib live_query -- --ignored`.
    #[cfg(feature = "aviso-versiones")]
    #[test]
    #[ignore = "necesita salir a Internet"]
    fn live_query_reaches_github() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let latest = rt
            .block_on(fetch())
            .expect("la consulta falló")
            .expect("no hay ninguna versión publicada");
        assert!(parse_version(&latest).is_some(), "{latest}");
        eprintln!("última versión publicada: {latest}");
    }

    #[test]
    fn checks_are_spaced_a_day_apart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("version.json");
        // Sin fichero (o con uno roto) toca preguntar.
        assert!(Record::load(&path).is_due(1_000_000));
        std::fs::write(&path, "{").unwrap();
        assert_eq!(Record::load(&path), Record::default());
        let record = Record {
            checked: 1_000_000,
            latest: "0.9.0".into(),
        };
        record.save(&path);
        assert_eq!(Record::load(&path), record);
        assert!(!record.is_due(1_000_000 + 3600));
        assert!(record.is_due(1_000_000 + 24 * 3600));
        // Si el reloj va hacia atrás, también.
        assert!(record.is_due(999_999));
    }
}
