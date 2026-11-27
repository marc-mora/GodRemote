//! Rutas de configuración de la aplicación.

use std::path::PathBuf;

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
