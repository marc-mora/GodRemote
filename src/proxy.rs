//! Proxy de salida: la primera conexión hacia un equipo puede hacerse a
//! través de un proxy HTTP (`CONNECT`) o SOCKS5, como en las redes de
//! empresa que no dejan salir directamente.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Clave del baúl donde se guarda la contraseña del proxy.
pub const PASSWORD_KEY: &str = "proxy/password";
/// Cabeceras de respuesta del proxy HTTP: más que esto no es un proxy.
const MAX_HTTP_REPLY: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ProxyKind {
    #[default]
    #[serde(rename = "ninguno")]
    None,
    #[serde(rename = "http")]
    Http,
    #[serde(rename = "socks5")]
    Socks5,
}

impl ProxyKind {
    pub const ALL: [ProxyKind; 3] = [ProxyKind::None, ProxyKind::Http, ProxyKind::Socks5];

    pub fn label(self) -> &'static str {
        match self {
            ProxyKind::None => "Sin proxy",
            ProxyKind::Http => "HTTP (CONNECT)",
            ProxyKind::Socks5 => "SOCKS5",
        }
    }
}

/// Proxy de salida tal como se guarda en los ajustes (la contraseña va al
/// baúl).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxySettings {
    #[serde(rename = "tipo")]
    pub kind: ProxyKind,
    #[serde(rename = "servidor")]
    pub host: String,
    #[serde(rename = "puerto")]
    pub port: u16,
    #[serde(rename = "usuario")]
    pub username: String,
    /// Destinos a los que se llega directamente, separados por comas:
    /// nombres, sufijos (`.interno`) o comienzos de dirección (`10.`).
    #[serde(rename = "excepciones")]
    pub exceptions: String,
}

impl Default for ProxySettings {
    fn default() -> Self {
        Self {
            kind: ProxyKind::None,
            host: String::new(),
            port: 8080,
            username: String::new(),
            exceptions: String::new(),
        }
    }
}

impl ProxySettings {
    /// Hay un proxy configurado de verdad.
    pub fn is_active(&self) -> bool {
        self.kind != ProxyKind::None && !self.host.trim().is_empty() && self.port != 0
    }
}

/// Proxy en vigor: los ajustes más su contraseña.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyConfig {
    pub settings: ProxySettings,
    pub password: Option<String>,
}

static CURRENT: RwLock<Option<Arc<ProxyConfig>>> = RwLock::new(None);

/// Pone en vigor el proxy de salida (o lo quita).
pub fn set(config: Option<ProxyConfig>) {
    let config = config.filter(|c| c.settings.is_active()).map(Arc::new);
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = config;
}

pub fn current() -> Option<Arc<ProxyConfig>> {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// El destino es este mismo equipo: nunca pasa por el proxy.
fn is_loopback(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// El destino está entre las excepciones del proxy.
fn is_exception(host: &str, exceptions: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    exceptions
        .split([',', ';', ' ', '\n'])
        .map(|rule| rule.trim().trim_start_matches('*').to_ascii_lowercase())
        .filter(|rule| !rule.is_empty())
        .any(|rule| {
            let rule = rule.trim_end_matches('*');
            if let Some(domain) = rule.strip_prefix('.') {
                // Sufijo de dominio: «.interno» vale para «bd.interno».
                host.ends_with(rule) || host == domain
            } else if rule.ends_with('.') {
                // Comienzo de dirección: «10.» vale para «10.1.2.3».
                host.starts_with(rule)
            } else {
                host == rule
            }
        })
}

/// A través de qué proxy hay que ir a `host`, si de alguno.
fn proxy_for(host: &str, bypass: bool) -> Option<Arc<ProxyConfig>> {
    if bypass || is_loopback(host) {
        return None;
    }
    current().filter(|config| !is_exception(host, &config.settings.exceptions))
}

/// Abre una conexión TCP con `host:port`, directa o a través del proxy de
/// salida. `bypass` fuerza la conexión directa (lo pide el equipo).
pub async fn dial(host: &str, port: u16, bypass: bool, timeout: Duration) -> Result<TcpStream> {
    let proxy = proxy_for(host, bypass);
    let work = async {
        match &proxy {
            None => TcpStream::connect((host, port))
                .await
                .with_context(|| format!("No se pudo conectar a {host}:{port}")),
            Some(config) => through(config, host, port).await,
        }
    };
    let stream = tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| anyhow!("Tiempo de espera agotado conectando a {host}:{port}"))??;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

/// Conecta con el proxy y le pide abrir el camino hasta `host:port`.
pub async fn through(config: &ProxyConfig, host: &str, port: u16) -> Result<TcpStream> {
    let settings = &config.settings;
    let proxy = format!("{}:{}", settings.host.trim(), settings.port);
    let mut stream = TcpStream::connect((settings.host.trim(), settings.port))
        .await
        .with_context(|| format!("No se pudo conectar con el proxy {proxy}"))?;
    let user = settings.username.trim();
    let password = config.password.as_deref().unwrap_or("");
    let result = match settings.kind {
        ProxyKind::Http => http_connect(&mut stream, host, port, user, password).await,
        ProxyKind::Socks5 => socks5_connect(&mut stream, host, port, user, password).await,
        ProxyKind::None => Ok(()),
    };
    result.with_context(|| format!("a través del proxy {proxy}"))?;
    Ok(stream)
}

async fn http_connect<S>(
    stream: &mut S,
    host: &str,
    port: u16,
    user: &str,
    password: &str,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if host.chars().any(|c| c.is_control() || c.is_whitespace()) {
        bail!("nombre de equipo no válido");
    }
    // IPv6 literal: entre corchetes.
    let target = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if !user.is_empty() {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
        request.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    request.push_str("Proxy-Connection: Keep-Alive\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;

    // La respuesta se lee byte a byte hasta la línea en blanco: lo que venga
    // después ya es del destino.
    let mut reply = Vec::with_capacity(256);
    while !reply.ends_with(b"\r\n\r\n") {
        if reply.len() >= MAX_HTTP_REPLY {
            bail!("la respuesta del proxy no tiene fin");
        }
        let byte = stream
            .read_u8()
            .await
            .map_err(|_| anyhow!("el proxy cerró la conexión sin contestar"))?;
        reply.push(byte);
    }
    let text = String::from_utf8_lossy(&reply);
    let status_line = text.lines().next().unwrap_or_default().trim();
    let mut parts = status_line.split_whitespace();
    let (version, code) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if !version.starts_with("HTTP/") {
        bail!("lo que contesta no es un proxy HTTP");
    }
    match code {
        "200" => Ok(()),
        "407" => bail!(
            "el proxy pide usuario y contraseña (o no acepta los indicados; si la contraseña \
             está en el baúl, desbloquéalo)"
        ),
        "403" => bail!("el proxy no permite conectar con {target}"),
        _ => bail!("el proxy contestó «{status_line}»"),
    }
}

async fn socks5_connect<S>(
    stream: &mut S,
    host: &str,
    port: u16,
    user: &str,
    password: &str,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let with_auth = !user.is_empty();
    if with_auth && (user.len() > 255 || password.len() > 255) {
        bail!("usuario o contraseña demasiado largos para SOCKS5");
    }
    // Saludo: métodos de autenticación que ofrecemos.
    let methods: &[u8] = if with_auth { &[0, 2] } else { &[0] };
    let mut hello = vec![5, methods.len() as u8];
    hello.extend_from_slice(methods);
    stream.write_all(&hello).await?;
    let mut choice = [0u8; 2];
    stream
        .read_exact(&mut choice)
        .await
        .map_err(|_| anyhow!("el proxy cerró la conexión sin contestar"))?;
    if choice[0] != 5 {
        bail!("lo que contesta no es un proxy SOCKS5");
    }
    match choice[1] {
        0 => {}
        2 if with_auth => {
            let mut auth = vec![1, user.len() as u8];
            auth.extend_from_slice(user.as_bytes());
            auth.push(password.len() as u8);
            auth.extend_from_slice(password.as_bytes());
            stream.write_all(&auth).await?;
            let mut result = [0u8; 2];
            stream.read_exact(&mut result).await?;
            if result[1] != 0 {
                bail!("el proxy no acepta el usuario o la contraseña");
            }
        }
        _ => bail!("el proxy pide una autenticación que no se le puede dar"),
    }
    // Petición CONNECT; el nombre lo resuelve el proxy.
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    let mut request = vec![5, 1, 0];
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            request.push(1);
            request.extend_from_slice(&ip.octets());
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            request.push(4);
            request.extend_from_slice(&ip.octets());
        }
        Err(_) => {
            if host.is_empty() || host.len() > 255 {
                bail!("nombre de equipo no válido");
            }
            request.push(3);
            request.push(host.len() as u8);
            request.extend_from_slice(host.as_bytes());
        }
    }
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).await?;
    let mut head = [0u8; 4];
    stream
        .read_exact(&mut head)
        .await
        .map_err(|_| anyhow!("el proxy cerró la conexión sin contestar"))?;
    if head[0] != 5 {
        bail!("respuesta SOCKS5 no válida");
    }
    if head[1] != 0 {
        bail!(match head[1] {
            2 => "el proxy no permite esa conexión".to_string(),
            3 => "red inalcanzable desde el proxy".to_string(),
            4 => "equipo inalcanzable desde el proxy".to_string(),
            5 => "el destino rechazó la conexión".to_string(),
            6 => "la conexión tardó demasiado".to_string(),
            code => format!("el proxy no pudo conectar (código {code})"),
        });
    }
    // Dirección a la que se ha enlazado el proxy: se lee y se descarta.
    let rest = match head[3] {
        1 => 4 + 2,
        4 => 16 + 2,
        3 => stream.read_u8().await? as usize + 2,
        _ => bail!("respuesta SOCKS5 no válida"),
    };
    let mut discard = vec![0u8; rest];
    stream.read_exact(&mut discard).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn exceptions_and_loopback() {
        assert!(is_loopback("localhost") && is_loopback("127.0.0.1") && is_loopback("[::1]"));
        assert!(is_loopback("127.8.9.10") && !is_loopback("10.0.0.1") && !is_loopback("equipo"));
        let rules = "intranet, .interno; 10. , *.lab.ejemplo.com 192.168.*";
        for host in [
            "intranet",
            "bd.interno",
            "interno",
            "10.1.2.3",
            "x.lab.ejemplo.com",
            "192.168.1.9",
            "BD.Interno",
        ] {
            assert!(is_exception(host, rules), "{host}");
        }
        for host in [
            "intranet2",
            "interno.com",
            "110.1.2.3",
            "ejemplo.com",
            "172.16.0.1",
        ] {
            assert!(!is_exception(host, rules), "{host}");
        }
        assert!(!is_exception("cualquiera", ""));
    }

    #[test]
    fn settings_round_trip() {
        let settings = ProxySettings {
            kind: ProxyKind::Socks5,
            host: "proxy.empresa".into(),
            port: 1080,
            username: "ana".into(),
            exceptions: ".interno".into(),
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(
            json.contains("\"socks5\"") && !json.contains("contras"),
            "{json}"
        );
        assert_eq!(
            serde_json::from_str::<ProxySettings>(&json).unwrap(),
            settings
        );
        let empty: ProxySettings = serde_json::from_str("{}").unwrap();
        assert!(!empty.is_active());
        assert!(
            !ProxySettings {
                kind: ProxyKind::Http,
                ..Default::default()
            }
            .is_active()
        );
    }

    /// Servidor de eco: devuelve `eco:` + lo recibido.
    async fn echo_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 64];
                    while let Ok(n) = socket.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        let mut reply = b"eco:".to_vec();
                        reply.extend_from_slice(&buf[..n]);
                        if socket.write_all(&reply).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        port
    }

    /// Proxy HTTP de pruebas: exige (o no) credenciales y lleva cualquier
    /// nombre a `127.0.0.1`. Devuelve su puerto y las peticiones recibidas.
    async fn http_proxy(
        credentials: Option<&'static str>,
    ) -> (u16, Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(std::sync::Mutex::new(vec![]));
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let log = log.clone();
                tokio::spawn(async move {
                    let mut head = vec![];
                    while !head.ends_with(b"\r\n\r\n") {
                        let Ok(byte) = client.read_u8().await else {
                            return;
                        };
                        head.push(byte);
                    }
                    let head = String::from_utf8_lossy(&head).to_string();
                    let request = head.lines().next().unwrap_or_default().to_string();
                    log.lock().unwrap().push(request.clone());
                    if let Some(expected) = credentials {
                        let token = base64::engine::general_purpose::STANDARD.encode(expected);
                        if !head.contains(&format!("Proxy-Authorization: Basic {token}")) {
                            let _ = client
                                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                                .await;
                            return;
                        }
                    }
                    let target = request.split_whitespace().nth(1).unwrap_or_default();
                    let port: u16 = target
                        .rsplit(':')
                        .next()
                        .unwrap_or("0")
                        .parse()
                        .unwrap_or(0);
                    if target.starts_with("prohibido") {
                        let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await;
                        return;
                    }
                    let Ok(mut upstream) = TcpStream::connect(("127.0.0.1", port)).await else {
                        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
                        return;
                    };
                    let _ = client
                        .write_all(b"HTTP/1.1 200 Connection established\r\nVia: prueba\r\n\r\n")
                        .await;
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
        (port, seen)
    }

    /// Proxy SOCKS5 de pruebas, con usuario y contraseña opcionales.
    async fn socks_proxy(credentials: Option<(&'static str, &'static str)>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut hello = [0u8; 2];
                    client.read_exact(&mut hello).await.ok()?;
                    let mut methods = vec![0u8; hello[1] as usize];
                    client.read_exact(&mut methods).await.ok()?;
                    match credentials {
                        Some((user, pass)) => {
                            if !methods.contains(&2) {
                                client.write_all(&[5, 0xff]).await.ok()?;
                                return None;
                            }
                            client.write_all(&[5, 2]).await.ok()?;
                            let mut head = [0u8; 2];
                            client.read_exact(&mut head).await.ok()?;
                            let mut name = vec![0u8; head[1] as usize];
                            client.read_exact(&mut name).await.ok()?;
                            let len = client.read_u8().await.ok()? as usize;
                            let mut secret = vec![0u8; len];
                            client.read_exact(&mut secret).await.ok()?;
                            let ok = name == user.as_bytes() && secret == pass.as_bytes();
                            client.write_all(&[1, u8::from(!ok)]).await.ok()?;
                            if !ok {
                                return None;
                            }
                        }
                        None => client.write_all(&[5, 0]).await.ok()?,
                    }
                    let mut request = [0u8; 4];
                    client.read_exact(&mut request).await.ok()?;
                    let host = match request[3] {
                        1 => {
                            let mut ip = [0u8; 4];
                            client.read_exact(&mut ip).await.ok()?;
                            std::net::Ipv4Addr::from(ip).to_string()
                        }
                        3 => {
                            let len = client.read_u8().await.ok()? as usize;
                            let mut name = vec![0u8; len];
                            client.read_exact(&mut name).await.ok()?;
                            String::from_utf8(name).ok()?
                        }
                        _ => return None,
                    };
                    let port = client.read_u16().await.ok()?;
                    if host == "cerrado.prueba" {
                        client
                            .write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0])
                            .await
                            .ok()?;
                        return None;
                    }
                    let mut upstream = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
                    // Respuesta con dirección de nombre, para probar su lectura.
                    client
                        .write_all(&[5, 0, 0, 3, 4, b'h', b'o', b'l', b'a', 0, 80])
                        .await
                        .ok()?;
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                    Some(())
                });
            }
        });
        port
    }

    async fn say(mut stream: TcpStream, text: &[u8]) -> Vec<u8> {
        stream.write_all(text).await.unwrap();
        let mut reply = vec![0u8; 4 + text.len()];
        stream.read_exact(&mut reply).await.unwrap();
        reply
    }

    fn config(kind: ProxyKind, port: u16, user: &str, password: Option<&str>) -> ProxyConfig {
        ProxyConfig {
            settings: ProxySettings {
                kind,
                host: "127.0.0.1".into(),
                port,
                username: user.into(),
                exceptions: String::new(),
            },
            password: password.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn http_connect_reaches_the_target() {
        let echo = echo_server().await;
        // Sin credenciales: el nombre lo resuelve el proxy, no nosotros.
        let (proxy, seen) = http_proxy(None).await;
        let stream = through(
            &config(ProxyKind::Http, proxy, "", None),
            "equipo.prueba",
            echo,
        )
        .await
        .unwrap();
        assert_eq!(say(stream, b"hola").await, b"eco:hola");
        assert_eq!(
            seen.lock().unwrap()[0],
            format!("CONNECT equipo.prueba:{echo} HTTP/1.1")
        );
        // Destino prohibido y destino apagado: el error lo explica.
        let error = through(
            &config(ProxyKind::Http, proxy, "", None),
            "prohibido.prueba",
            echo,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("no permite"), "{error:#}");
        let closed = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let error = through(
            &config(ProxyKind::Http, proxy, "", None),
            "equipo.prueba",
            closed,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("502"), "{error:#}");

        // Con credenciales: sin ellas o con otras, 407.
        let (proxy, _) = http_proxy(Some("ana:secreta")).await;
        let ok = config(ProxyKind::Http, proxy, "ana", Some("secreta"));
        assert_eq!(
            say(through(&ok, "equipo.prueba", echo).await.unwrap(), b"auth").await,
            b"eco:auth"
        );
        for bad in [
            config(ProxyKind::Http, proxy, "", None),
            config(ProxyKind::Http, proxy, "ana", Some("otra")),
            config(ProxyKind::Http, proxy, "ana", None),
        ] {
            let error = through(&bad, "equipo.prueba", echo).await.unwrap_err();
            assert!(
                format!("{error:#}").contains("usuario y contraseña"),
                "{error:#}"
            );
        }
        // Un nombre con saltos de línea no puede colar cabeceras.
        let error = through(&ok, "equipo\r\nX-Colada: 1", echo)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("no válido"), "{error:#}");
        // Lo que no es un proxy HTTP se reconoce.
        let error = through(
            &config(ProxyKind::Http, echo, "", None),
            "equipo.prueba",
            22,
        )
        .await
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("no es un proxy HTTP"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn socks5_reaches_the_target() {
        let echo = echo_server().await;
        let proxy = socks_proxy(None).await;
        let plain = config(ProxyKind::Socks5, proxy, "", None);
        assert_eq!(
            say(
                through(&plain, "equipo.prueba", echo).await.unwrap(),
                b"hola"
            )
            .await,
            b"eco:hola"
        );
        // También con una dirección IP.
        assert_eq!(
            say(through(&plain, "127.0.0.1", echo).await.unwrap(), b"ip").await,
            b"eco:ip"
        );
        let error = through(&plain, "cerrado.prueba", echo).await.unwrap_err();
        assert!(format!("{error:#}").contains("rechazó"), "{error:#}");

        let proxy = socks_proxy(Some(("ana", "secreta"))).await;
        let ok = config(ProxyKind::Socks5, proxy, "ana", Some("secreta"));
        assert_eq!(
            say(through(&ok, "equipo.prueba", echo).await.unwrap(), b"auth").await,
            b"eco:auth"
        );
        let error = through(
            &config(ProxyKind::Socks5, proxy, "ana", Some("otra")),
            "x.prueba",
            echo,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("no acepta"), "{error:#}");
        let error = through(
            &config(ProxyKind::Socks5, proxy, "", None),
            "x.prueba",
            echo,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("autenticación"), "{error:#}");
    }
}
