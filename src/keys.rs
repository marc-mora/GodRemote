//! Gestión de claves SSH: listado, generación e importación.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use russh::keys::ssh_key::{self, Algorithm, EcdsaCurve, HashAlg, LineEnding, PrivateKey};

#[derive(Debug, Clone, PartialEq)]
pub struct KeyInfo {
    pub path: PathBuf,
    pub algorithm: String,
    pub fingerprint: String,
    pub comment: String,
    pub encrypted: bool,
    /// Clave pública en formato `authorized_keys` (vacía si no se pudo leer).
    pub public_openssh: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAlgorithm {
    Ed25519,
    EcdsaP256,
    EcdsaP384,
}

impl KeyAlgorithm {
    pub const ALL: [KeyAlgorithm; 3] = [
        KeyAlgorithm::Ed25519,
        KeyAlgorithm::EcdsaP256,
        KeyAlgorithm::EcdsaP384,
    ];

    pub fn label(self) -> &'static str {
        match self {
            KeyAlgorithm::Ed25519 => "Ed25519 (recomendado)",
            KeyAlgorithm::EcdsaP256 => "ECDSA P-256",
            KeyAlgorithm::EcdsaP384 => "ECDSA P-384",
        }
    }

    fn to_ssh(self) -> Algorithm {
        match self {
            KeyAlgorithm::Ed25519 => Algorithm::Ed25519,
            KeyAlgorithm::EcdsaP256 => Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            },
            KeyAlgorithm::EcdsaP384 => Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP384,
            },
        }
    }
}

/// Directorios donde se buscan claves: `~/.ssh` y la carpeta propia.
pub fn search_dirs() -> Vec<PathBuf> {
    vec![
        crate::paths::home_dir().join(".ssh"),
        crate::paths::keys_dir(),
    ]
}

fn looks_like_private_key(text: &str) -> bool {
    let t = text.trim_start();
    (t.starts_with("-----BEGIN ") && t.contains("PRIVATE KEY-----"))
        || t.starts_with("PuTTY-User-Key-File-")
}

/// Lee la información de una clave privada sin necesidad de descifrarla.
pub fn inspect(path: &Path) -> Option<KeyInfo> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > 64 * 1024 {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    if !looks_like_private_key(&text) {
        return None;
    }

    // Formato OpenSSH: la parte pública es legible aunque esté cifrada.
    if let Ok(key) = PrivateKey::from_openssh(&text) {
        let public = key.public_key();
        return Some(KeyInfo {
            path: path.to_path_buf(),
            algorithm: key.algorithm().to_string(),
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
            comment: key.comment().to_string(),
            encrypted: key.is_encrypted(),
            public_openssh: public.to_openssh().unwrap_or_default(),
        });
    }

    // Otros formatos (PEM antiguo, PPK): se intenta sin passphrase.
    match russh::keys::decode_secret_key(&text, None) {
        Ok(key) => Some(KeyInfo {
            path: path.to_path_buf(),
            algorithm: key.algorithm().to_string(),
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
            comment: key.comment().to_string(),
            encrypted: false,
            public_openssh: key.public_key().to_openssh().unwrap_or_default(),
        }),
        Err(_) => {
            // Cifrada: se intenta completar con el `.pub` vecino.
            let mut pub_path = path.as_os_str().to_owned();
            pub_path.push(".pub");
            let public = std::fs::read_to_string(PathBuf::from(pub_path))
                .ok()
                .and_then(|s| ssh_key::PublicKey::from_openssh(s.trim()).ok());
            Some(KeyInfo {
                path: path.to_path_buf(),
                algorithm: public
                    .as_ref()
                    .map(|p| p.algorithm().to_string())
                    .unwrap_or_else(|| "desconocido".into()),
                fingerprint: public
                    .as_ref()
                    .map(|p| p.fingerprint(HashAlg::Sha256).to_string())
                    .unwrap_or_default(),
                comment: public
                    .as_ref()
                    .map(|p| p.comment().to_string())
                    .unwrap_or_default(),
                encrypted: true,
                public_openssh: public.and_then(|p| p.to_openssh().ok()).unwrap_or_default(),
            })
        }
    }
}

/// Lista todas las claves privadas encontradas en los directorios de búsqueda.
pub fn list() -> Vec<KeyInfo> {
    list_in(&search_dirs())
}

pub fn list_in(dirs: &[PathBuf]) -> Vec<KeyInfo> {
    let mut out = vec![];
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.extension().is_some_and(|e| e == "pub") {
                continue;
            }
            if let Some(info) = inspect(&path) {
                out.push(info);
            }
        }
    }
    out
}

fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && !name.ends_with(".pub")
}

/// Genera un par de claves en `dir/name` (+ `.pub`). No sobrescribe.
pub fn generate(
    dir: &Path,
    name: &str,
    algorithm: KeyAlgorithm,
    comment: &str,
    passphrase: Option<&str>,
) -> Result<KeyInfo> {
    if !valid_file_name(name) {
        bail!("Nombre de fichero no válido");
    }
    let path = dir.join(name);
    if path.exists() {
        bail!("Ya existe {}", path.display());
    }
    let mut rng = rand::rng();
    let mut key = PrivateKey::random(&mut rng, algorithm.to_ssh())
        .map_err(|e| anyhow::anyhow!("no se pudo generar la clave: {e}"))?;
    key.set_comment(comment);
    let public = key.public_key().to_openssh()?;
    let to_write = match passphrase.filter(|p| !p.is_empty()) {
        Some(pass) => key.encrypt(&mut rng, pass)?,
        None => key,
    };
    let pem = to_write.to_openssh(LineEnding::LF)?;
    crate::paths::write_private(&path, pem.as_bytes())
        .with_context(|| format!("escribiendo {}", path.display()))?;
    let mut pub_path = path.as_os_str().to_owned();
    pub_path.push(".pub");
    std::fs::write(PathBuf::from(pub_path), format!("{public}\n"))?;
    inspect(&path).context("la clave generada no se pudo releer")
}

/// Copia una clave existente a la carpeta de claves de GodRemote.
pub fn import(source: &Path, dest_dir: &Path) -> Result<KeyInfo> {
    if inspect(source).is_none() {
        bail!("{} no parece una clave privada SSH", source.display());
    }
    let name = source
        .file_name()
        .context("ruta sin nombre de fichero")?
        .to_owned();
    let dest = dest_dir.join(&name);
    if dest.exists() {
        bail!("Ya existe {}", dest.display());
    }
    let data = std::fs::read(source)?;
    crate::paths::write_private(&dest, &data)?;
    let mut src_pub = source.as_os_str().to_owned();
    src_pub.push(".pub");
    if let Ok(public) = std::fs::read(PathBuf::from(src_pub)) {
        let mut dest_pub = dest.as_os_str().to_owned();
        dest_pub.push(".pub");
        let _ = std::fs::write(PathBuf::from(dest_pub), public);
    }
    inspect(&dest).context("la clave importada no se pudo releer")
}

/// Borra una clave privada y su `.pub`.
pub fn delete(path: &Path) -> Result<()> {
    std::fs::remove_file(path).with_context(|| format!("borrando {}", path.display()))?;
    let mut pub_path = path.as_os_str().to_owned();
    pub_path.push(".pub");
    let _ = std::fs::remove_file(PathBuf::from(pub_path));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_list_import_delete() {
        let dir = tempfile::tempdir().unwrap();
        let plain = generate(dir.path(), "id_test", KeyAlgorithm::Ed25519, "yo@pc", None).unwrap();
        assert!(!plain.encrypted);
        assert_eq!(plain.comment, "yo@pc");
        assert!(plain.fingerprint.starts_with("SHA256:"));
        assert!(plain.public_openssh.starts_with("ssh-ed25519 "));

        let enc = generate(
            dir.path(),
            "id_enc",
            KeyAlgorithm::EcdsaP256,
            "",
            Some("frase"),
        )
        .unwrap();
        assert!(enc.encrypted);
        // La cifrada solo se abre con la passphrase correcta.
        assert!(russh::keys::load_secret_key(&enc.path, None).is_err());
        assert!(russh::keys::load_secret_key(&enc.path, Some("mala")).is_err());
        assert!(russh::keys::load_secret_key(&enc.path, Some("frase")).is_ok());

        // No se sobrescribe una clave existente.
        assert!(generate(dir.path(), "id_test", KeyAlgorithm::Ed25519, "", None).is_err());
        assert!(generate(dir.path(), "../x", KeyAlgorithm::Ed25519, "", None).is_err());

        std::fs::write(dir.path().join("notas.txt"), "hola").unwrap();
        let listed = list_in(&[dir.path().to_path_buf()]);
        assert_eq!(listed.len(), 2);

        let other = tempfile::tempdir().unwrap();
        let imported = import(&plain.path, other.path()).unwrap();
        assert_eq!(imported.fingerprint, plain.fingerprint);
        assert!(other.path().join("id_test.pub").exists());
        assert!(import(&dir.path().join("notas.txt"), other.path()).is_err());

        delete(&imported.path).unwrap();
        assert!(list_in(&[other.path().to_path_buf()]).is_empty());
    }
}
