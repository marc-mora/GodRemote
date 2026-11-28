//! Gestión de claves SSH: listado, generación e importación.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use russh::keys::ssh_key::{
    self, Algorithm, Certificate, EcdsaCurve, HashAlg, LineEnding, PrivateKey,
};

use crate::i18n::tr;
use crate::trf;

#[derive(Debug, Clone, PartialEq)]
pub struct KeyInfo {
    pub path: PathBuf,
    pub algorithm: String,
    pub fingerprint: String,
    pub comment: String,
    pub encrypted: bool,
    /// Clave pública en formato `authorized_keys` (vacía si no se pudo leer).
    pub public_openssh: String,
    /// Certificado OpenSSH que la acompaña (`<clave>-cert.pub`), si lo hay.
    pub certificate: Option<CertInfo>,
    /// Es de una llave de seguridad FIDO2 (`sk-ssh-ed25519`, `sk-ecdsa`): el
    /// fichero solo guarda una referencia, y quien firma es la llave.
    pub security_key: bool,
}

/// `true` si el algoritmo es de una llave de seguridad FIDO2.
pub fn is_security_key(algorithm: &Algorithm) -> bool {
    matches!(
        algorithm,
        Algorithm::SkEd25519 | Algorithm::SkEcdsaSha2NistP256
    )
}

/// Nombre corto de un algoritmo de llave de seguridad: «Ed25519», «ECDSA
/// P-256». `None` si no es de una llave de seguridad.
pub fn security_key_label(algorithm: &str) -> Option<&'static str> {
    if algorithm.starts_with("sk-ssh-ed25519") {
        Some("Ed25519")
    } else if algorithm.starts_with("sk-ecdsa-sha2-nistp256") {
        Some("ECDSA P-256")
    } else {
        None
    }
}

/// Datos de un certificado OpenSSH (`…-cert.pub`), para mostrarlos.
#[derive(Debug, Clone, PartialEq)]
pub struct CertInfo {
    pub path: PathBuf,
    /// Identidad («key id») que le puso la autoridad al firmarlo.
    pub key_id: String,
    pub serial: u64,
    /// Es un certificado de host: no sirve para entrar como usuario.
    pub host: bool,
    /// Usuarios («principales») para los que vale; vacío si vale para todos.
    pub principals: Vec<String>,
    /// Validez, en segundos desde 1970 (`u64::MAX`: no caduca).
    pub valid_after: u64,
    pub valid_before: u64,
    /// Autoridad (CA) que lo firmó.
    pub ca_algorithm: String,
    pub ca_fingerprint: String,
    /// Huella de la clave que certifica.
    pub key_fingerprint: String,
}

/// Si un certificado se puede usar en un momento dado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertValidity {
    NotYetValid,
    Valid,
    Expired,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Fecha y hora locales de un instante en segundos desde 1970.
fn format_time(secs: u64) -> String {
    i64::try_from(secs)
        .ok()
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
        .map(|utc| {
            utc.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| tr("una fecha fuera de rango").into())
}

impl CertInfo {
    /// Los datos de un certificado ya leído (de `path`, o del agente).
    pub fn from_certificate(path: &Path, cert: &Certificate) -> Self {
        Self {
            path: path.to_path_buf(),
            key_id: cert.key_id().to_string(),
            serial: cert.serial(),
            host: cert.cert_type().is_host(),
            principals: cert.valid_principals().to_vec(),
            valid_after: cert.valid_after(),
            valid_before: cert.valid_before(),
            ca_algorithm: cert.signature_key().algorithm().to_string(),
            ca_fingerprint: cert
                .signature_key()
                .fingerprint(HashAlg::Sha256)
                .to_string(),
            key_fingerprint: cert.public_key().fingerprint(HashAlg::Sha256).to_string(),
        }
    }

    /// Como OpenSSH: vale desde `valid_after` (incluido) hasta `valid_before`.
    pub fn validity_at(&self, now: u64) -> CertValidity {
        if now < self.valid_after {
            CertValidity::NotYetValid
        } else if now >= self.valid_before {
            CertValidity::Expired
        } else {
            CertValidity::Valid
        }
    }

    pub fn validity(&self) -> CertValidity {
        self.validity_at(unix_now())
    }

    /// «válido hasta el 2027-01-01 10:00», «caducó el…», «no caduca»…
    pub fn validity_text(&self) -> String {
        self.validity_text_at(unix_now())
    }

    pub fn validity_text_at(&self, now: u64) -> String {
        match self.validity_at(now) {
            CertValidity::NotYetValid => {
                trf!(
                    "no vale hasta el {date}",
                    date = format_time(self.valid_after)
                )
            }
            CertValidity::Expired => {
                trf!("caducó el {date}", date = format_time(self.valid_before))
            }
            CertValidity::Valid if self.valid_before == u64::MAX => tr("no caduca").into(),
            CertValidity::Valid => trf!(
                "válido hasta el {date}",
                date = format_time(self.valid_before)
            ),
        }
    }

    /// Los principales, separados por comas («cualquiera» si no los limita).
    pub fn principals_text(&self) -> String {
        if self.principals.is_empty() {
            tr("cualquiera").into()
        } else {
            self.principals.join(", ")
        }
    }
}

/// Ruta donde OpenSSH busca el certificado de una clave: `<clave>-cert.pub`.
pub fn cert_path_for(key: &Path) -> PathBuf {
    let mut path = key.as_os_str().to_owned();
    path.push("-cert.pub");
    PathBuf::from(path)
}

/// Niveles de certificado dentro de certificado que se toleran al leer uno.
/// Uno legítimo tiene un solo nivel (su CA es una clave normal); sin límite,
/// un fichero retorcido agotaría la pila al descodificarlo.
const MAX_CERT_NESTING: usize = 8;

/// Lee un certificado OpenSSH de un fichero `…-cert.pub`.
pub fn read_cert(path: &Path) -> Result<(Certificate, CertInfo)> {
    use base64::Engine;
    let meta = std::fs::metadata(path).with_context(|| could_not_read(path))?;
    if !meta.is_file() || meta.len() > 64 * 1024 {
        bail!("{}", not_a_certificate(path));
    }
    let text = std::fs::read_to_string(path).with_context(|| could_not_read(path))?;
    let text = text.trim();
    let blob = text
        .split_whitespace()
        .nth(1)
        .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
        .unwrap_or_default();
    const MARKER: &[u8] = b"-cert-v01@";
    let nesting = blob.windows(MARKER.len()).filter(|w| *w == MARKER).count();
    if nesting == 0 || nesting > MAX_CERT_NESTING {
        bail!("{}", not_a_certificate(path));
    }
    let cert = Certificate::from_openssh(text).map_err(|e| {
        anyhow::anyhow!(
            "{}",
            trf!(
                "{path} no es un certificado OpenSSH válido: {e}",
                path = path.display(),
                e
            )
        )
    })?;
    let info = CertInfo::from_certificate(path, &cert);
    Ok((cert, info))
}

fn could_not_read(path: &Path) -> String {
    trf!("No se pudo leer {path}", path = path.display())
}

fn not_a_certificate(path: &Path) -> String {
    trf!("{path} no es un certificado OpenSSH", path = path.display())
}

/// Datos del certificado que acompaña a una clave, si existe y se entiende.
fn neighbour_cert(key: &Path) -> Option<CertInfo> {
    let path = cert_path_for(key);
    read_cert(&path).ok().map(|(_, info)| info)
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
            KeyAlgorithm::Ed25519 => tr("Ed25519 (recomendado)"),
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
            certificate: neighbour_cert(path),
            security_key: is_security_key(&key.algorithm()),
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
            certificate: neighbour_cert(path),
            security_key: is_security_key(&key.algorithm()),
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
                    .unwrap_or_else(|| tr("desconocido").into()),
                fingerprint: public
                    .as_ref()
                    .map(|p| p.fingerprint(HashAlg::Sha256).to_string())
                    .unwrap_or_default(),
                comment: public
                    .as_ref()
                    .map(|p| p.comment().to_string())
                    .unwrap_or_default(),
                encrypted: true,
                security_key: public
                    .as_ref()
                    .is_some_and(|p| is_security_key(&p.algorithm())),
                public_openssh: public.and_then(|p| p.to_openssh().ok()).unwrap_or_default(),
                certificate: neighbour_cert(path),
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
        bail!("{}", tr("Nombre de fichero no válido"));
    }
    let path = dir.join(name);
    if path.exists() {
        bail!("{}", trf!("Ya existe {path}", path = path.display()));
    }
    let mut rng = rand::rng();
    let mut key = PrivateKey::random(&mut rng, algorithm.to_ssh())
        .map_err(|e| anyhow::anyhow!("{}", trf!("no se pudo generar la clave: {e}", e)))?;
    key.set_comment(comment);
    let public = key.public_key().to_openssh()?;
    let to_write = match passphrase.filter(|p| !p.is_empty()) {
        Some(pass) => key.encrypt(&mut rng, pass)?,
        None => key,
    };
    let pem = to_write.to_openssh(LineEnding::LF)?;
    crate::paths::write_private(&path, pem.as_bytes())
        .with_context(|| trf!("escribiendo {path}", path = path.display()))?;
    let mut pub_path = path.as_os_str().to_owned();
    pub_path.push(".pub");
    std::fs::write(PathBuf::from(pub_path), format!("{public}\n"))?;
    inspect(&path).context(tr("la clave generada no se pudo releer"))
}

/// Copia una clave existente a la carpeta de claves de GodRemote.
pub fn import(source: &Path, dest_dir: &Path) -> Result<KeyInfo> {
    if inspect(source).is_none() {
        bail!(
            "{}",
            trf!(
                "{path} no parece una clave privada SSH",
                path = source.display()
            )
        );
    }
    let name = source
        .file_name()
        .context(tr("ruta sin nombre de fichero"))?
        .to_owned();
    let dest = dest_dir.join(&name);
    if dest.exists() {
        bail!("{}", trf!("Ya existe {path}", path = dest.display()));
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
    // Y su certificado, si lo tiene al lado.
    if let Ok(cert) = std::fs::read(cert_path_for(source)) {
        let _ = std::fs::write(cert_path_for(&dest), cert);
    }
    inspect(&dest).context(tr("la clave importada no se pudo releer"))
}

/// Borra una clave privada, su `.pub` y su certificado (`-cert.pub`).
pub fn delete(path: &Path) -> Result<()> {
    std::fs::remove_file(path).with_context(|| trf!("borrando {path}", path = path.display()))?;
    let mut pub_path = path.as_os_str().to_owned();
    pub_path.push(".pub");
    let _ = std::fs::remove_file(PathBuf::from(pub_path));
    let _ = std::fs::remove_file(cert_path_for(path));
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

    /// Firma un certificado de usuario para `key` con la autoridad `ca`.
    fn issue(ca: &Path, key: &KeyInfo, valid_after: u64, valid_before: u64) -> Certificate {
        use russh::keys::ssh_key::certificate::{Builder, CertType};
        let ca = russh::keys::load_secret_key(ca, None).unwrap();
        let public = ssh_key::PublicKey::from_openssh(&key.public_openssh).unwrap();
        let mut builder = Builder::new_with_random_nonce(
            &mut rand::rng(),
            public.key_data().clone(),
            valid_after,
            valid_before,
        )
        .unwrap();
        builder.serial(7).unwrap();
        builder.key_id("ana@empresa").unwrap();
        builder.cert_type(CertType::User).unwrap();
        builder.valid_principal("ana").unwrap();
        builder.valid_principal("root").unwrap();
        builder.sign(&ca).unwrap()
    }

    #[test]
    fn certificate_next_to_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let ca = generate(dir.path(), "ca", KeyAlgorithm::Ed25519, "", None).unwrap();
        let key = generate(dir.path(), "id", KeyAlgorithm::Ed25519, "", None).unwrap();
        assert_eq!(key.certificate, None);
        let cert_path = cert_path_for(&key.path);
        assert_eq!(cert_path, dir.path().join("id-cert.pub"));

        let now = unix_now();
        let cert = issue(&ca.path, &key, now - 60, now + 3600);
        std::fs::write(&cert_path, format!("{}\n", cert.to_openssh().unwrap())).unwrap();

        // El certificado no cuenta como clave, pero acompaña a la suya.
        let listed = list_in(&[dir.path().to_path_buf()]);
        assert_eq!(listed.len(), 2);
        let info = inspect(&key.path).unwrap().certificate.unwrap();
        assert_eq!(info.path, cert_path);
        assert_eq!(info.key_id, "ana@empresa");
        assert_eq!(info.serial, 7);
        assert!(!info.host);
        assert_eq!(info.principals_text(), "ana, root");
        assert_eq!(info.key_fingerprint, key.fingerprint);
        assert_eq!(info.ca_fingerprint, ca.fingerprint);
        assert_eq!(info.ca_algorithm, "ssh-ed25519");

        // Validez: desde `valid_after` (incluido) hasta `valid_before`.
        assert_eq!(info.validity(), CertValidity::Valid);
        assert_eq!(info.validity_at(now - 61), CertValidity::NotYetValid);
        assert_eq!(info.validity_at(now - 60), CertValidity::Valid);
        assert_eq!(info.validity_at(now + 3600), CertValidity::Expired);
        assert!(info.validity_text().starts_with("válido hasta el 20"));
        assert!(
            info.validity_text_at(now + 7200)
                .starts_with("caducó el 20")
        );
        assert!(info.validity_text_at(0).starts_with("no vale hasta el 20"));
        let forever = CertInfo {
            valid_after: 0,
            valid_before: u64::MAX,
            principals: vec![],
            ..info.clone()
        };
        assert_eq!(forever.validity_text(), "no caduca");
        assert_eq!(forever.principals_text(), "cualquiera");

        // Importar y borrar la clave se llevan consigo el certificado.
        let other = tempfile::tempdir().unwrap();
        let imported = import(&key.path, other.path()).unwrap();
        let copy = imported.certificate.unwrap();
        assert_eq!(copy.path, other.path().join("id-cert.pub"));
        assert_eq!(copy.key_id, info.key_id);
        delete(&imported.path).unwrap();
        assert!(!copy.path.exists());

        // Lo que no es un certificado se rechaza sin más.
        let public = dir.path().join("id.pub");
        assert!(read_cert(&public).is_err());
        std::fs::write(&cert_path, "ssh-ed25519-cert-v01@openssh.com AAAA\n").unwrap();
        assert!(read_cert(&cert_path).is_err());
        assert_eq!(inspect(&key.path).unwrap().certificate, None);
        std::fs::write(&cert_path, vec![b'A'; 100 * 1024]).unwrap();
        assert!(read_cert(&cert_path).is_err());
        assert!(read_cert(&dir.path().join("no-existe")).is_err());
    }

    /// Clave de una llave de seguridad de mentira (generada con `ssh-keygen
    /// -t ed25519-sk` y el proveedor `sk-dummy` de las pruebas de OpenSSH).
    const SK_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAASgAAABpzay1zc2
gtZWQyNTUxOUBvcGVuc3NoLmNvbQAAACBuSsG8frqk4CcXjqW2rpECsyRNMy8M77SfBd73
LY9o/wAAAARzc2g6AAAAsFarvLdWq7y3AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY2
9tAAAAIG5Kwbx+uqTgJxeOpbaukQKzJE0zLwzvtJ8F3vctj2j/AAAABHNzaDoBAAAAQOeA
I4MWOhWKcH7VyPohMXbkwyBzxZ/Y6hsvQRF/xoBPbkrBvH66pOAnF46ltq6RArMkTTMvDO
+0nwXe9y2PaP8AAAAAAAAAEGxsYXZlIGRlIHBydWViYXMB
-----END OPENSSH PRIVATE KEY-----
";

    #[test]
    fn security_keys_are_recognised() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_ed25519_sk");
        std::fs::write(&path, SK_KEY).unwrap();
        let info = inspect(&path).expect("clave de una llave de seguridad");
        assert!(info.security_key && !info.encrypted);
        assert_eq!(info.algorithm, "sk-ssh-ed25519@openssh.com");
        assert_eq!(security_key_label(&info.algorithm), Some("Ed25519"));
        assert_eq!(
            info.fingerprint,
            "SHA256:EQ0ButGpKfQtj87Zxg5CIPZ17Yqb/AVAsblpQo7Plg4"
        );
        assert_eq!(info.comment, "llave de pruebas");
        assert!(
            info.public_openssh
                .starts_with("sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29t")
        );
        // Las claves corrientes no lo son.
        let plain = generate(dir.path(), "id", KeyAlgorithm::Ed25519, "", None).unwrap();
        assert!(!plain.security_key);
        assert_eq!(security_key_label(&plain.algorithm), None);
        assert_eq!(
            security_key_label("sk-ecdsa-sha2-nistp256@openssh.com"),
            Some("ECDSA P-256")
        );
    }
}
