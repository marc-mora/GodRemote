//! Copia de seguridad de toda la configuración en un solo fichero: equipos,
//! baúl (tal cual, cifrado), ajustes, snippets, temas y, si se quiere, las
//! claves guardadas en la carpeta de GodRemote.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

/// Ficheros sueltos de la carpeta de datos que entran en la copia.
const FILES: [&str; 6] = [
    "sessions.json",
    "vault.json",
    "settings.json",
    "macros.json",
    "tunnels.json",
    "tls_known_hosts.json",
];
/// Carpetas que entran en la copia (solo sus ficheros, sin subcarpetas).
const THEMES: &str = "themes";
const KEYS: &str = "keys";

const VERSION: u32 = 1;
const MAX_FILE: u64 = 32 * 1024 * 1024;
const MAX_FILES: usize = 2000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Ruta dentro de la carpeta de datos, con `/`.
    #[serde(rename = "ruta")]
    pub path: String,
    #[serde(rename = "texto", default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    /// Contenido de los ficheros que no son texto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base64: Option<String>,
}

impl Entry {
    fn new(path: String, bytes: Vec<u8>) -> Self {
        match String::from_utf8(bytes) {
            Ok(text) => Entry {
                path,
                text: Some(text),
                base64: None,
            },
            Err(e) => Entry {
                path,
                text: None,
                base64: Some(B64.encode(e.into_bytes())),
            },
        }
    }

    fn bytes(&self) -> Result<Vec<u8>> {
        match (&self.text, &self.base64) {
            (Some(text), _) => Ok(text.clone().into_bytes()),
            (None, Some(data)) => B64
                .decode(data)
                .map_err(|_| anyhow!("«{}» está dañado en la copia", self.path)),
            (None, None) => bail!("«{}» no tiene contenido en la copia", self.path),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Backup {
    #[serde(rename = "godremote_copia")]
    version: u32,
    #[serde(rename = "creada")]
    pub created: String,
    #[serde(rename = "ficheros")]
    pub files: Vec<Entry>,
}

/// Comprueba que una ruta de la copia es de las que se guardan y no puede
/// salirse de la carpeta de datos. Devuelve la ruta relativa.
fn safe_path(path: &str) -> Result<PathBuf> {
    let relative = Path::new(path);
    let parts: Vec<&str> = relative
        .components()
        .map(|c| match c {
            Component::Normal(part) => part.to_str().ok_or(()),
            _ => Err(()),
        })
        .collect::<Result<_, _>>()
        .map_err(|_| anyhow!("La copia contiene una ruta no admitida: {path}"))?;
    let allowed = match parts.as_slice() {
        [file] => FILES.contains(file),
        [dir, file] => (*dir == THEMES || *dir == KEYS) && !file.starts_with('.'),
        _ => false,
    };
    if !allowed || path.contains('\\') {
        bail!("La copia contiene una ruta no admitida: {path}");
    }
    Ok(parts.iter().collect())
}

fn read_limited(path: &Path) -> Result<Vec<u8>> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("No se pudo leer {}", path.display()))?;
    if meta.len() > MAX_FILE {
        bail!("{} es demasiado grande para la copia", path.display());
    }
    std::fs::read(path).with_context(|| format!("No se pudo leer {}", path.display()))
}

impl Backup {
    /// Reúne la configuración de la carpeta de datos.
    pub fn create(data_dir: &Path, include_keys: bool) -> Result<Backup> {
        let mut files = vec![];
        for name in FILES {
            let path = data_dir.join(name);
            if path.is_file() {
                files.push(Entry::new(name.to_string(), read_limited(&path)?));
            }
        }
        let mut folders = vec![THEMES];
        if include_keys {
            folders.push(KEYS);
        }
        for folder in folders {
            let mut paths: Vec<PathBuf> = std::fs::read_dir(data_dir.join(folder))
                .map(|rd| rd.flatten().map(|e| e.path()).collect())
                .unwrap_or_default();
            paths.sort();
            for path in paths.into_iter().filter(|p| p.is_file()) {
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                files.push(Entry::new(format!("{folder}/{name}"), read_limited(&path)?));
            }
        }
        if files.is_empty() {
            bail!("Todavía no hay nada que guardar");
        }
        Ok(Backup {
            version: VERSION,
            created: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            files,
        })
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_vec_pretty(self)?;
        // Lleva el baúl y puede llevar claves: solo para su dueño.
        crate::paths::write_private(path, &data)
            .with_context(|| format!("No se pudo escribir {}", path.display()))?;
        Ok(())
    }

    /// Lee y valida un fichero de copia.
    pub fn read(path: &Path) -> Result<Backup> {
        let meta = std::fs::metadata(path)
            .with_context(|| format!("No se pudo leer {}", path.display()))?;
        if meta.len() > 512 * 1024 * 1024 {
            bail!("{} es demasiado grande para ser una copia", path.display());
        }
        let data =
            std::fs::read(path).with_context(|| format!("No se pudo leer {}", path.display()))?;
        let backup: Backup = serde_json::from_slice(&data).map_err(|_| {
            anyhow!(
                "{} no es una copia de seguridad de GodRemote",
                path.display()
            )
        })?;
        if backup.version > VERSION {
            bail!("La copia es de una versión más reciente de GodRemote");
        }
        if backup.files.is_empty() || backup.files.len() > MAX_FILES {
            bail!("La copia no contiene ficheros válidos");
        }
        for entry in &backup.files {
            safe_path(&entry.path)?;
            entry.bytes()?;
        }
        Ok(backup)
    }

    /// `true` si la copia incluye claves privadas.
    pub fn has_keys(&self) -> bool {
        self.files.iter().any(|e| e.path.starts_with("keys/"))
    }

    /// Resumen legible de lo que contiene.
    pub fn summary(&self) -> String {
        let count = |prefix: &str| {
            self.files
                .iter()
                .filter(|e| e.path.starts_with(prefix))
                .count()
        };
        let has = |name: &str| self.files.iter().any(|e| e.path == name);
        let mut parts = vec![];
        if has("sessions.json") {
            parts.push("los equipos".to_string());
        }
        if has("vault.json") {
            parts.push("el baúl cifrado".to_string());
        }
        if has("settings.json") {
            parts.push("los ajustes".to_string());
        }
        if has("macros.json") {
            parts.push("los snippets".to_string());
        }
        if has("tunnels.json") {
            parts.push("los túneles".to_string());
        }
        if count("themes/") > 0 {
            parts.push(format!("{} tema(s)", count("themes/")));
        }
        if count("keys/") > 0 {
            parts.push(format!("{} fichero(s) de claves", count("keys/")));
        }
        format!("Copia del {}: {}", self.created, parts.join(", "))
    }

    /// Escribe los ficheros de la copia en la carpeta de datos, sustituyendo
    /// los que haya. Devuelve cuántos se han escrito.
    pub fn restore(&self, data_dir: &Path) -> Result<usize> {
        // Primero se valida todo: o se restaura entera o no se toca nada.
        let mut writes = vec![];
        for entry in &self.files {
            writes.push((data_dir.join(safe_path(&entry.path)?), entry.bytes()?));
        }
        for (path, bytes) in &writes {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            crate::paths::write_private(path, bytes)
                .with_context(|| format!("No se pudo escribir {}", path.display()))?;
        }
        Ok(writes.len())
    }
}

/// Nombre sugerido para una copia nueva.
pub fn suggested_name() -> String {
    format!(
        "godremote-copia-{}.json",
        chrono::Local::now().format("%Y%m%d-%H%M")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(dir: &Path) {
        std::fs::create_dir_all(dir.join("themes")).unwrap();
        std::fs::create_dir_all(dir.join("keys")).unwrap();
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::write(dir.join("sessions.json"), r#"{"nodes":[]}"#).unwrap();
        std::fs::write(dir.join("vault.json"), "{\"cifrado\":\"ñ\"}").unwrap();
        std::fs::write(dir.join("settings.json"), "{}").unwrap();
        std::fs::write(dir.join("themes/Mío.json"), "{\"nombre\":\"Mío\"}").unwrap();
        std::fs::write(
            dir.join("keys/id_demo"),
            b"-----BEGIN OPENSSH PRIVATE KEY-----\n",
        )
        .unwrap();
        std::fs::write(dir.join("keys/binario.ppk"), [0u8, 159, 146, 150, 255]).unwrap();
        // Lo que no es configuración no entra.
        std::fs::write(dir.join("logs/sesion.log"), "salida").unwrap();
        std::fs::write(dir.join("state.json"), "{}").unwrap();
    }

    #[test]
    fn backup_and_restore_roundtrip() {
        let source = tempfile::tempdir().unwrap();
        sample(source.path());
        let backup = Backup::create(source.path(), true).unwrap();
        let paths: Vec<&str> = backup.files.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "sessions.json",
                "vault.json",
                "settings.json",
                "themes/Mío.json",
                "keys/binario.ppk",
                "keys/id_demo"
            ]
        );
        assert!(backup.has_keys());
        let summary = backup.summary();
        assert!(
            summary.contains("los equipos") && summary.contains("el baúl cifrado"),
            "{summary}"
        );
        assert!(summary.contains("1 tema(s)") && summary.contains("2 fichero(s) de claves"));

        // A disco, de vuelta, y a una carpeta vacía.
        let file = source.path().join("copias/copia.json");
        backup.write(&file).unwrap();
        let read = Backup::read(&file).unwrap();
        assert_eq!(read, backup);
        let target = tempfile::tempdir().unwrap();
        assert_eq!(read.restore(target.path()).unwrap(), 6);
        for path in paths {
            assert_eq!(
                std::fs::read(target.path().join(path)).unwrap(),
                std::fs::read(source.path().join(path)).unwrap(),
                "{path}"
            );
        }
        assert!(!target.path().join("logs").exists());

        // Sin claves, si no se piden.
        let without = Backup::create(source.path(), false).unwrap();
        assert!(!without.has_keys() && without.files.len() == 4);
        // Una carpeta vacía no da una copia.
        let empty = tempfile::tempdir().unwrap();
        assert!(Backup::create(empty.path(), true).is_err());
    }

    #[test]
    fn hostile_backups_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let write = |json: &str| {
            let path = dir.path().join("copia.json");
            std::fs::write(&path, json).unwrap();
            path
        };
        let with_path = |path: &str| {
            format!(
                r#"{{"godremote_copia":1,"creada":"x","ficheros":[{{"ruta":{},"texto":"x"}}]}}"#,
                serde_json::to_string(path).unwrap()
            )
        };
        // Rutas que se salen de la carpeta o que no son configuración.
        for bad in [
            "../fuera.json",
            "/etc/passwd",
            "themes/../../x",
            "keys/sub/x",
            "otra/x.json",
            "autoexec.bat",
            "themes\\x.json",
            "keys/.oculto",
        ] {
            let e = Backup::read(&write(&with_path(bad))).unwrap_err();
            assert!(
                format!("{e:#}").contains("ruta no admitida"),
                "{bad}: {e:#}"
            );
        }
        assert!(Backup::read(&write(&with_path("themes/bien.json"))).is_ok());
        // Otros ficheros, versiones futuras y copias vacías.
        assert!(Backup::read(&write("{}")).is_err());
        assert!(Backup::read(&write("no es json")).is_err());
        let future =
            with_path("sessions.json").replace("\"godremote_copia\":1", "\"godremote_copia\":99");
        assert!(
            format!("{:#}", Backup::read(&write(&future)).unwrap_err()).contains("más reciente")
        );
        let empty = r#"{"godremote_copia":1,"creada":"x","ficheros":[]}"#;
        assert!(Backup::read(&write(empty)).is_err());
        assert!(suggested_name().starts_with("godremote-copia-"));
    }
}
