//! GodRemote: gestor de conexiones remotas (SSH, SFTP, RDP).

pub mod backdrop;
pub mod desktop;
pub mod keys;
pub mod macros;
pub mod model;
pub mod monitor;
pub mod paths;
pub mod rdp;
#[cfg(feature = "rdp-integrado")]
pub mod rdp_client;
pub mod serial;
pub mod sftp;
pub mod ssh;
pub mod telnet;
pub mod terminal;
pub mod theme;
pub mod trust;
pub mod tunnels;
pub mod ui;
pub mod vault;
pub mod vnc;
pub mod x11;

/// `true` si esta compilación incluye el cliente RDP integrado.
pub const HAS_EMBEDDED_RDP: bool = cfg!(feature = "rdp-integrado");

/// Versión de la librería SSH (russh) con la que se ha compilado.
pub const RUSSH_VERSION: &str = env!("GODREMOTE_RUSSH_VERSION");

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    /// Raíz del repositorio, se compile la edición que se compile.
    fn root() -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        if dir.join("reforzado").is_dir() {
            dir.to_path_buf()
        } else {
            dir.parent().expect("carpeta del repositorio").to_path_buf()
        }
    }

    /// Dependencias de un manifiesto: nombre → especificación.
    fn dependencies(manifest: &str) -> BTreeMap<String, String> {
        let section = manifest
            .split("[dependencies]")
            .nth(1)
            .expect("sección [dependencies]");
        section
            .lines()
            .take_while(|l| !l.starts_with('['))
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| l.split_once('='))
            .map(|(name, spec)| (name.trim().to_string(), spec.trim().to_string()))
            .collect()
    }

    fn version(manifest: &str) -> &str {
        manifest
            .lines()
            .find_map(|l| l.strip_prefix("version = "))
            .expect("versión")
    }

    /// La edición «SSH reforzada» tiene su propio manifiesto: debe llevar las
    /// mismas dependencias que la completa, salvo las del RDP integrado.
    #[test]
    fn editions_share_dependencies() {
        let root = root();
        let full = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let hardened = std::fs::read_to_string(root.join("reforzado/Cargo.toml")).unwrap();
        assert_eq!(version(&full), version(&hardened), "versiones distintas");

        let mut expected = dependencies(&full);
        // Las opcionales son las del RDP integrado.
        expected.retain(|_, spec| !spec.contains("optional = true"));
        assert_eq!(
            dependencies(&hardened),
            expected,
            "reforzado/Cargo.toml no coincide con Cargo.toml"
        );

        // Y su Cargo.lock no arrastra nada del RDP integrado.
        let lock = std::fs::read_to_string(root.join("reforzado/Cargo.lock")).unwrap();
        for name in ["ironrdp", "sspi", "picky"] {
            assert!(
                !lock.contains(&format!("name = \"{name}")),
                "reforzado/Cargo.lock incluye {name}"
            );
        }
    }

    #[test]
    fn build_reports_the_ssh_library_version() {
        let parts: Vec<&str> = super::RUSSH_VERSION.split('.').collect();
        assert_eq!(parts.len(), 3, "{}", super::RUSSH_VERSION);
        assert!(parts.iter().all(|p| p.parse::<u32>().is_ok()));
        if !super::HAS_EMBEDDED_RDP {
            // La razón de ser de la edición sin RDP integrado.
            let patch: u32 = parts[2].parse().unwrap();
            assert!(
                (parts[0], parts[1]) != ("0", "65") || patch >= 4,
                "se esperaba russh 0.65.4 o posterior, no {}",
                super::RUSSH_VERSION
            );
        }
    }
}
