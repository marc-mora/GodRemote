//! Reenvío X11: las aplicaciones gráficas del servidor se muestran en el
//! servidor X local (el propio escritorio en Linux; VcXsrv, Xming o WSLg en
//! Windows; XQuartz en macOS).
//!
//! Al servidor remoto se le entrega una *cookie* falsa; cada conexión X11
//! entrante debe presentarla y aquí se sustituye por la auténtica antes de
//! pasarla al servidor X local, que nunca queda expuesto.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const COOKIE_PROTOCOL: &str = "MIT-MAGIC-COOKIE-1";
/// Longitud máxima razonable de un campo de autenticación X11.
const MAX_AUTH_FIELD: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum X11Target {
    Unix(PathBuf),
    Tcp(String, u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDisplay {
    pub target: X11Target,
    pub number: u32,
    pub screen: u32,
}

/// Interpreta una cadena `DISPLAY` (`:0`, `:1.0`, `host:0`, `unix:0`,
/// `/ruta/al/socket:0`).
pub fn parse_display(display: &str) -> Option<LocalDisplay> {
    let display = display.trim();
    let (host, rest) = display.rsplit_once(':')?;
    let (number, screen) = match rest.split_once('.') {
        Some((n, s)) => (n.parse().ok()?, s.parse().ok()?),
        None => (rest.parse().ok()?, 0),
    };
    // Evita desbordar el puerto TCP (6000 + número).
    if number > 59_000 {
        return None;
    }
    let target = if host.starts_with('/') {
        // XQuartz: la propia cadena (sin la pantalla) es la ruta del socket.
        X11Target::Unix(PathBuf::from(format!("{host}:{number}")))
    } else if host.is_empty() || host == "unix" {
        if cfg!(unix) {
            X11Target::Unix(PathBuf::from(format!("/tmp/.X11-unix/X{number}")))
        } else {
            X11Target::Tcp("127.0.0.1".into(), 6000 + number as u16)
        }
    } else {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        X11Target::Tcp(host.to_string(), 6000 + number as u16)
    };
    Some(LocalDisplay {
        target,
        number,
        screen,
    })
}

/// Pantalla X local: variable `DISPLAY` o, en Windows, `127.0.0.1:0`.
pub fn local_display() -> Result<LocalDisplay> {
    let value = match std::env::var("DISPLAY") {
        Ok(v) if !v.trim().is_empty() => v,
        _ if cfg!(windows) => "127.0.0.1:0".to_string(),
        _ => bail!("no hay servidor X local (variable DISPLAY sin definir)"),
    };
    parse_display(&value).ok_or_else(|| anyhow!("DISPLAY no válido: {value}"))
}

fn xauthority_path() -> PathBuf {
    match std::env::var_os("XAUTHORITY") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => crate::paths::home_dir().join(".Xauthority"),
    }
}

/// Busca la cookie `MIT-MAGIC-COOKIE-1` de una pantalla en el contenido de
/// un fichero `.Xauthority`.
pub fn find_cookie(xauthority: &[u8], display_number: u32) -> Option<Vec<u8>> {
    fn field<'a>(data: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
        let len = u16::from_be_bytes([*data.get(*pos)?, *data.get(*pos + 1)?]) as usize;
        let start = *pos + 2;
        let value = data.get(start..start + len)?;
        *pos = start + len;
        Some(value)
    }
    let wanted = display_number.to_string();
    let mut pos = 0;
    let mut fallback = None;
    while pos + 2 <= xauthority.len() {
        let family = u16::from_be_bytes([xauthority[pos], xauthority[pos + 1]]);
        pos += 2;
        let _address = field(xauthority, &mut pos)?;
        let number = field(xauthority, &mut pos)?;
        let name = field(xauthority, &mut pos)?;
        let data = field(xauthority, &mut pos)?;
        if name != COOKIE_PROTOCOL.as_bytes() || data.is_empty() {
            continue;
        }
        // Una entrada sin número vale para cualquier pantalla.
        if number == wanted.as_bytes() || number.is_empty() {
            // Se prefieren las entradas locales (256) o comodín (65535).
            if family == 256 || family == 65535 {
                return Some(data.to_vec());
            }
            fallback.get_or_insert_with(|| data.to_vec());
        }
    }
    fallback
}

/// Datos necesarios para atender conexiones X11 reenviadas.
pub struct X11Forward {
    pub display: LocalDisplay,
    real_cookie: Option<Vec<u8>>,
    fake_cookie: [u8; 16],
}

impl X11Forward {
    /// Localiza el servidor X local y prepara una cookie falsa.
    pub fn prepare() -> Result<Self> {
        let display = local_display()?;
        let real_cookie = std::fs::read(xauthority_path())
            .ok()
            .and_then(|data| find_cookie(&data, display.number));
        let mut fake_cookie = [0u8; 16];
        rand::fill(&mut fake_cookie);
        Ok(Self {
            display,
            real_cookie,
            fake_cookie,
        })
    }

    /// Cookie (falsa) que se anuncia al servidor remoto, en hexadecimal.
    pub fn fake_cookie_hex(&self) -> String {
        self.fake_cookie
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Atiende una conexión X11 llegada por el canal SSH.
    pub async fn bridge<S>(&self, mut channel: S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        // Cabecera de conexión: se valida y se cambia la cookie.
        let mut head = [0u8; 12];
        channel.read_exact(&mut head).await?;
        let (name_len, data_len) = setup_lengths(&head)?;
        let mut rest = vec![0u8; pad4(name_len) + pad4(data_len)];
        channel.read_exact(&mut rest).await?;
        let setup = rewrite_setup(&head, &rest, &self.fake_cookie, self.real_cookie.as_deref())?;

        match &self.display.target {
            #[cfg(unix)]
            X11Target::Unix(path) => {
                let mut local = tokio::net::UnixStream::connect(path)
                    .await
                    .with_context(|| format!("conectando con {}", path.display()))?;
                local.write_all(&setup).await?;
                tokio::io::copy_bidirectional(&mut channel, &mut local).await?;
            }
            #[cfg(not(unix))]
            X11Target::Unix(path) => {
                bail!(
                    "socket X11 no soportado en este sistema: {}",
                    path.display()
                )
            }
            X11Target::Tcp(host, port) => {
                let mut local = tokio::net::TcpStream::connect((host.as_str(), *port))
                    .await
                    .with_context(|| format!("conectando con el servidor X {host}:{port}"))?;
                let _ = local.set_nodelay(true);
                local.write_all(&setup).await?;
                tokio::io::copy_bidirectional(&mut channel, &mut local).await?;
            }
        }
        Ok(())
    }
}

fn pad4(len: usize) -> usize {
    len.div_ceil(4) * 4
}

/// Longitudes de nombre y datos de autenticación de una cabecera X11.
fn setup_lengths(head: &[u8; 12]) -> Result<(usize, usize)> {
    let read = |a: u8, b: u8| -> usize {
        match head[0] {
            b'B' => u16::from_be_bytes([a, b]) as usize,
            _ => u16::from_le_bytes([a, b]) as usize,
        }
    };
    if head[0] != b'B' && head[0] != b'l' {
        bail!("cabecera X11 no válida");
    }
    let (name_len, data_len) = (read(head[6], head[7]), read(head[8], head[9]));
    if name_len > MAX_AUTH_FIELD || data_len > MAX_AUTH_FIELD {
        bail!("cabecera X11 demasiado grande");
    }
    Ok((name_len, data_len))
}

/// Comprueba que la conexión presenta la cookie falsa y devuelve la cabecera
/// con la cookie auténtica (o sin autenticación si no hay ninguna).
pub fn rewrite_setup(
    head: &[u8; 12],
    rest: &[u8],
    fake: &[u8],
    real: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let (name_len, data_len) = setup_lengths(head)?;
    let name = rest.get(..name_len).context("cabecera X11 truncada")?;
    let data = rest
        .get(pad4(name_len)..pad4(name_len) + data_len)
        .context("cabecera X11 truncada")?;
    if name != COOKIE_PROTOCOL.as_bytes() || data != fake {
        bail!("conexión X11 rechazada: cookie incorrecta");
    }
    let (new_name, new_data): (&[u8], &[u8]) = match real {
        Some(cookie) => (COOKIE_PROTOCOL.as_bytes(), cookie),
        None => (&[], &[]),
    };
    let put = |v: usize| -> [u8; 2] {
        match head[0] {
            b'B' => (v as u16).to_be_bytes(),
            _ => (v as u16).to_le_bytes(),
        }
    };
    let mut out = head.to_vec();
    out[6..8].copy_from_slice(&put(new_name.len()));
    out[8..10].copy_from_slice(&put(new_data.len()));
    for part in [new_name, new_data] {
        out.extend_from_slice(part);
        out.resize(out.len() + pad4(part.len()) - part.len(), 0);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_strings() {
        let d = parse_display(":1").unwrap();
        assert_eq!((d.number, d.screen), (1, 0));
        if cfg!(unix) {
            assert_eq!(d.target, X11Target::Unix("/tmp/.X11-unix/X1".into()));
        }
        let d = parse_display("localhost:10.2").unwrap();
        assert_eq!(d.target, X11Target::Tcp("localhost".into(), 6010));
        assert_eq!(d.screen, 2);
        let d = parse_display("/private/tmp/com.apple.launchd.x/org.xquartz:0").unwrap();
        assert_eq!(
            d.target,
            X11Target::Unix("/private/tmp/com.apple.launchd.x/org.xquartz:0".into())
        );
        assert!(parse_display("sin-numero").is_none());
        assert!(parse_display(":abc").is_none());
        assert!(parse_display("host:99999999").is_none());
    }

    fn xauth_entry(family: u16, addr: &[u8], number: &[u8], name: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = family.to_be_bytes().to_vec();
        for f in [addr, number, name, data] {
            out.extend_from_slice(&(f.len() as u16).to_be_bytes());
            out.extend_from_slice(f);
        }
        out
    }

    #[test]
    fn xauthority_lookup() {
        let mut file = xauth_entry(0, b"\x7f\0\0\x01", b"0", b"MIT-MAGIC-COOKIE-1", &[1; 16]);
        file.extend(xauth_entry(256, b"pc", b"7", b"OTRO-PROTOCOLO", &[9; 16]));
        file.extend(xauth_entry(
            256,
            b"pc",
            b"7",
            b"MIT-MAGIC-COOKIE-1",
            &[7; 16],
        ));
        assert_eq!(find_cookie(&file, 7), Some(vec![7; 16]));
        assert_eq!(find_cookie(&file, 0), Some(vec![1; 16]));
        assert_eq!(find_cookie(&file, 3), None);
        // Un fichero truncado o basura no provoca pánico.
        assert_eq!(find_cookie(&file[..file.len() - 5], 3), None);
        assert_eq!(find_cookie(&[0xff; 7], 0), None);
    }

    fn setup(order: u8, name: &[u8], data: &[u8]) -> ([u8; 12], Vec<u8>) {
        let put = |v: usize| match order {
            b'B' => (v as u16).to_be_bytes(),
            _ => (v as u16).to_le_bytes(),
        };
        let mut head = [0u8; 12];
        head[0] = order;
        head[2..4].copy_from_slice(&put(11));
        head[6..8].copy_from_slice(&put(name.len()));
        head[8..10].copy_from_slice(&put(data.len()));
        let mut rest = vec![];
        for part in [name, data] {
            rest.extend_from_slice(part);
            rest.resize(rest.len() + pad4(part.len()) - part.len(), 0);
        }
        (head, rest)
    }

    #[test]
    fn cookie_substitution() {
        let fake = [0xAA; 16];
        let real = [0x55; 16];
        for order in *b"Bl" {
            let (head, rest) = setup(order, COOKIE_PROTOCOL.as_bytes(), &fake);
            let out = rewrite_setup(&head, &rest, &fake, Some(&real)).unwrap();
            let (h2, r2) = setup(order, COOKIE_PROTOCOL.as_bytes(), &real);
            assert_eq!(out, [h2.to_vec(), r2].concat());
            // Sin cookie local: se entra sin autenticación.
            let out = rewrite_setup(&head, &rest, &fake, None).unwrap();
            assert_eq!(out.len(), 12);
            assert_eq!(&out[6..10], &[0, 0, 0, 0]);
        }
        // Cookie equivocada, protocolo distinto o cabecera rota: rechazo.
        let (head, rest) = setup(b'l', COOKIE_PROTOCOL.as_bytes(), &[0xBB; 16]);
        assert!(rewrite_setup(&head, &rest, &fake, Some(&real)).is_err());
        let (head, rest) = setup(b'l', b"XDM-AUTHORIZATION-1", &fake);
        assert!(rewrite_setup(&head, &rest, &fake, Some(&real)).is_err());
        let (head, rest) = setup(b'l', COOKIE_PROTOCOL.as_bytes(), &fake);
        assert!(rewrite_setup(&head, &rest[..10], &fake, Some(&real)).is_err());
        let mut bad = head;
        bad[0] = b'?';
        assert!(rewrite_setup(&bad, &rest, &fake, Some(&real)).is_err());
    }
}
