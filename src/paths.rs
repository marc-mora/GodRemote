//! Rutas de configuración de la aplicación.

use std::path::{Path, PathBuf};

/// Directorio de datos. Se puede forzar con la variable `GODREMOTE_HOME`.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("GODREMOTE_HOME") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("godremote")
}

pub fn sessions_file() -> PathBuf {
    data_dir().join("sessions.json")
}

pub fn vault_file() -> PathBuf {
    data_dir().join("vault.json")
}

/// Carpeta donde GodRemote guarda las claves que genera.
pub fn keys_dir() -> PathBuf {
    data_dir().join("keys")
}

/// Huellas de certificados TLS aceptados (RDP).
pub fn tls_known_hosts_file() -> PathBuf {
    data_dir().join("tls_known_hosts.json")
}

/// Carpeta con los temas del usuario.
pub fn themes_dir() -> PathBuf {
    data_dir().join("themes")
}

/// Pestañas abiertas al cerrar la aplicación (para reabrirlas).
pub fn state_file() -> PathBuf {
    data_dir().join("state.json")
}

/// Copias locales de los ficheros remotos que se están editando.
pub fn edit_dir() -> PathBuf {
    data_dir().join("edit")
}

/// Registros de las sesiones de terminal.
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

/// Preferencias (tema elegido…).
pub fn settings_file() -> PathBuf {
    data_dir().join("settings.json")
}

/// Macros de terminal.
pub fn macros_file() -> PathBuf {
    data_dir().join("macros.json")
}

/// Túneles del gestor de túneles.
pub fn tunnels_file() -> PathBuf {
    data_dir().join("tunnels.json")
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Fichero `known_hosts` (el de OpenSSH; `GODREMOTE_KNOWN_HOSTS` lo cambia).
pub fn known_hosts_file() -> PathBuf {
    if let Some(p) = std::env::var_os("GODREMOTE_KNOWN_HOSTS") {
        return PathBuf::from(p);
    }
    home_dir().join(".ssh").join("known_hosts")
}

/// Escribe un fichero de forma atómica y, en Unix, solo legible por el usuario.
pub fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Expande un `~/` inicial al directorio personal.
pub fn expand_tilde(path: &str) -> PathBuf {
    let path = path.trim();
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return home_dir().join(rest);
    }
    PathBuf::from(path)
}

/// Abre un fichero o una carpeta con la aplicación que el sistema tenga
/// asociada (el explorador de archivos, el editor de texto…).
pub fn open_with_system(path: &Path) -> std::io::Result<()> {
    let mut command = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        // El primer argumento entre comillas de `start` es el título.
        c.args(["/C", "start", ""]).arg(path);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(path);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        c
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Abre una dirección web con el navegador del sistema. La dirección sale
/// de lo que el servidor escribe en el terminal, así que solo se admiten
/// http, https y ftp, y nunca pasa por un intérprete de órdenes.
pub fn open_url(url: &str) -> std::io::Result<()> {
    if !crate::highlight::is_safe_url(url) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "solo se abren direcciones http, https o ftp",
        ));
    }
    let mut command = if cfg!(windows) {
        // Sin `cmd /C start`: para él, «&» o «^» en la dirección serían
        // parte de una orden.
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler").arg(url);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Abre un fichero con el editor indicado en los ajustes (una orden como
/// `code -w` o la ruta de un programa) o, si no hay, con el del sistema.
pub fn open_in_editor(editor: &str, path: &Path) -> std::io::Result<()> {
    let editor = editor.trim();
    if editor.is_empty() {
        return open_with_system(path);
    }
    // Una ruta con espacios (C:\Program Files\…) es el programa entero;
    // si no existe como fichero, se separan programa y argumentos.
    let (program, arguments): (&str, Vec<&str>) = if Path::new(editor).is_file() {
        (editor, vec![])
    } else {
        let mut parts = editor.split_whitespace();
        (parts.next().unwrap_or(editor), parts.collect())
    };
    std::process::Command::new(program)
        .args(arguments)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}
