//! RDP: lanza el cliente de escritorio remoto del sistema.
//!
//! - Linux/macOS: FreeRDP (`xfreerdp3`, `sdl-freerdp3`, `xfreerdp`, …).
//! - Windows: `mstsc.exe` con un fichero `.rdp` temporal.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::model::Session;

/// Clientes FreeRDP conocidos, por orden de preferencia. El booleano indica
/// si es FreeRDP 3 (admite `/args-from:stdin`).
const FREERDP_CLIENTS: [(&str, bool); 6] = [
    ("xfreerdp3", true),
    ("sdl-freerdp3", true),
    ("wlfreerdp3", true),
    ("sdl-freerdp", true),
    ("xfreerdp", false),
    ("wlfreerdp", false),
];

fn find_in_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Busca un cliente FreeRDP instalado.
pub fn find_freerdp() -> Option<(PathBuf, bool)> {
    FREERDP_CLIENTS
        .iter()
        .find_map(|(name, v3)| find_in_path(name).map(|p| (p, *v3)))
}

/// Nombre del cliente que se usará, para mostrarlo en la interfaz.
pub fn client_description() -> String {
    if cfg!(windows) {
        return "mstsc.exe".into();
    }
    match find_freerdp() {
        Some((path, _)) => path.display().to_string(),
        None => "FreeRDP no encontrado (instala freerdp3-x11 / freerdp)".into(),
    }
}

/// Argumentos de FreeRDP para una sesión (sin el nombre del programa).
pub fn freerdp_args(session: &Session, password: Option<&str>) -> Vec<String> {
    let host = session.host.trim();
    let target = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{}", session.port) // IPv6
    } else {
        format!("{host}:{}", session.port)
    };
    let mut args = vec![format!("/v:{target}")];
    if !session.username.trim().is_empty() {
        args.push(format!("/u:{}", session.username.trim()));
    }
    if !session.rdp.domain.trim().is_empty() {
        args.push(format!("/d:{}", session.rdp.domain.trim()));
    }
    if let Some(p) = password {
        args.push(format!("/p:{p}"));
    }
    if session.rdp.fullscreen {
        args.push("/f".into());
    } else {
        args.push(format!("/size:{}x{}", session.rdp.width, session.rdp.height));
        args.push("/dynamic-resolution".into());
    }
    args.push(if session.rdp.clipboard { "+clipboard" } else { "-clipboard" }.into());
    // Confiar en el certificado la primera vez y avisar si cambia.
    args.push("/cert:tofu".into());
    args.push(format!("/t:{} - GodRemote", session.display_name()));
    args
}

/// Contenido de un fichero `.rdp` para `mstsc`.
pub fn rdp_file(session: &Session) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "full address:s:{}:{}\r\n",
        session.host.trim(),
        session.port
    ));
    let user = if session.rdp.domain.trim().is_empty() {
        session.username.trim().to_string()
    } else {
        format!("{}\\{}", session.rdp.domain.trim(), session.username.trim())
    };
    if !user.is_empty() {
        out.push_str(&format!("username:s:{user}\r\n"));
    }
    if session.rdp.fullscreen {
        out.push_str("screen mode id:i:2\r\n");
    } else {
        out.push_str("screen mode id:i:1\r\n");
        out.push_str(&format!("desktopwidth:i:{}\r\n", session.rdp.width));
        out.push_str(&format!("desktopheight:i:{}\r\n", session.rdp.height));
    }
    out.push_str(&format!(
        "redirectclipboard:i:{}\r\n",
        session.rdp.clipboard as u8
    ));
    out.push_str("prompt for credentials:i:1\r\n");
    out
}

/// Lanza el cliente RDP. Devuelve el proceso hijo.
pub fn launch(session: &Session, password: Option<&str>) -> Result<Child> {
    if cfg!(windows) {
        return launch_mstsc(session);
    }
    let Some((program, v3)) = find_freerdp() else {
        bail!(
            "No se encontró ningún cliente FreeRDP en el PATH. \
             Instala `freerdp3-x11` (Debian/Ubuntu), `freerdp` (Fedora/Arch) \
             o `brew install freerdp` (macOS)."
        );
    };
    let args = freerdp_args(session, password);
    let mut cmd = Command::new(&program);
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());

    if v3 {
        // FreeRDP 3: los argumentos (incluida la contraseña) viajan por stdin
        // y no quedan visibles en la lista de procesos.
        use std::io::Write;
        cmd.arg("/args-from:stdin").stdin(Stdio::piped());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("No se pudo ejecutar {}", program.display()))?;
        if let Some(mut stdin) = child.stdin.take() {
            let mut payload = String::new();
            for a in &args {
                // Un salto de línea rompería el formato de un argumento por línea.
                if a.contains(['\n', '\r']) {
                    let _ = child.kill();
                    bail!("Los datos de la sesión contienen saltos de línea");
                }
                payload.push_str(a);
                payload.push('\n');
            }
            stdin.write_all(payload.as_bytes())?;
        }
        Ok(child)
    } else {
        // FreeRDP 2 no admite /args-from: la contraseña va en la línea de
        // órdenes (visible para otros usuarios locales mientras dure).
        cmd.args(&args).stdin(Stdio::null());
        cmd.spawn()
            .with_context(|| format!("No se pudo ejecutar {}", program.display()))
    }
}

fn launch_mstsc(session: &Session) -> Result<Child> {
    let dir = crate::paths::data_dir().join("rdp");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}.rdp", session.id));
    std::fs::write(&file, rdp_file(session))?;
    Command::new("mstsc.exe")
        .arg(&file)
        .spawn()
        .context("No se pudo ejecutar mstsc.exe")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Protocol, RdpOptions};

    fn session() -> Session {
        Session {
            name: "srv".into(),
            protocol: Protocol::Rdp,
            host: "10.0.0.5".into(),
            port: 3389,
            username: "admin".into(),
            rdp: RdpOptions {
                domain: "CORP".into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn freerdp_arguments() {
        let mut s = session();
        let args = freerdp_args(&s, Some("pw"));
        assert!(args.contains(&"/v:10.0.0.5:3389".to_string()));
        assert!(args.contains(&"/u:admin".to_string()));
        assert!(args.contains(&"/d:CORP".to_string()));
        assert!(args.contains(&"/p:pw".to_string()));
        assert!(args.contains(&"/size:1600x900".to_string()));
        assert!(args.contains(&"/cert:tofu".to_string()));

        s.rdp.fullscreen = true;
        s.host = "fe80::1".into();
        let args = freerdp_args(&s, None);
        assert!(args.contains(&"/v:[fe80::1]:3389".to_string()));
        assert!(args.contains(&"/f".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("/p:")));
    }

    #[test]
    fn rdp_file_contents() {
        let text = rdp_file(&session());
        assert!(text.contains("full address:s:10.0.0.5:3389"));
        assert!(text.contains("username:s:CORP\\admin"));
        assert!(text.contains("desktopwidth:i:1600"));
    }
}
