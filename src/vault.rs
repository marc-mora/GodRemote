//! Baúl de credenciales cifrado con una contraseña maestra.
//!
//! Las contraseñas y passphrases se guardan en un único fichero cifrado con
//! XChaCha20-Poly1305; la clave se deriva de la contraseña maestra con
//! Argon2id. Nada sensible se escribe en claro en disco.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::i18n::tr;
use crate::trf;

const AAD: &[u8] = b"godremote-vault-v1";
const KDF_M_COST: u32 = 64 * 1024; // 64 MiB
const KDF_T_COST: u32 = 3;
const KDF_P_COST: u32 = 1;

#[derive(Serialize, Deserialize)]
struct VaultFile {
    version: u32,
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
    salt: String,
    nonce: String,
    ciphertext: String,
}

/// Baúl desbloqueado: mantiene la clave derivada en memoria.
pub struct Vault {
    path: PathBuf,
    key: Zeroizing<[u8; 32]>,
    salt: [u8; 16],
    params: (u32, u32, u32),
    secrets: BTreeMap<String, String>,
}

impl Drop for Vault {
    fn drop(&mut self) {
        for v in self.secrets.values_mut() {
            v.zeroize();
        }
    }
}

fn derive_key(master: &str, salt: &[u8], params: (u32, u32, u32)) -> Result<Zeroizing<[u8; 32]>> {
    let params = Params::new(params.0, params.1, params.2, Some(32))
        .map_err(|e| anyhow!("{}", trf!("parámetros KDF no válidos: {e}", e)))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(master.as_bytes(), salt, key.as_mut())
        .map_err(|e| anyhow!("{}", trf!("no se pudo derivar la clave: {e}", e)))?;
    Ok(key)
}

impl Vault {
    pub fn exists(path: &Path) -> bool {
        path.exists()
    }

    /// Crea un baúl nuevo (vacío) protegido por `master`.
    pub fn create(path: &Path, master: &str) -> Result<Self> {
        if master.is_empty() {
            bail!("{}", tr("La contraseña maestra no puede estar vacía"));
        }
        if path.exists() {
            bail!(
                "{}",
                trf!("Ya existe un baúl en {path}", path = path.display())
            );
        }
        let mut salt = [0u8; 16];
        rand::fill(&mut salt);
        let params = (KDF_M_COST, KDF_T_COST, KDF_P_COST);
        let vault = Self {
            path: path.to_path_buf(),
            key: derive_key(master, &salt, params)?,
            salt,
            params,
            secrets: BTreeMap::new(),
        };
        vault.persist()?;
        Ok(vault)
    }

    /// Abre un baúl existente. Falla si la contraseña maestra es incorrecta.
    pub fn unlock(path: &Path, master: &str) -> Result<Self> {
        let raw =
            std::fs::read(path).with_context(|| trf!("leyendo {path}", path = path.display()))?;
        let file: VaultFile = serde_json::from_slice(&raw).context(tr("baúl dañado"))?;
        if file.version != 1 {
            bail!(
                "{}",
                trf!(
                    "Versión de baúl no soportada: {version}",
                    version = file.version
                )
            );
        }
        let salt: [u8; 16] = B64
            .decode(&file.salt)?
            .try_into()
            .map_err(|_| anyhow!("{}", tr("baúl dañado (salt)")))?;
        let nonce: [u8; 24] = B64
            .decode(&file.nonce)?
            .try_into()
            .map_err(|_| anyhow!("{}", tr("baúl dañado (nonce)")))?;
        let ciphertext = B64.decode(&file.ciphertext)?;
        let params = (file.m_cost, file.t_cost, file.p_cost);
        let key = derive_key(master, &salt, params)?;
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
            .map_err(|_| anyhow!("{}", tr("clave no válida")))?;
        let plain = Zeroizing::new(
            cipher
                .decrypt(
                    &XNonce::from(nonce),
                    Payload {
                        msg: &ciphertext,
                        aad: AAD,
                    },
                )
                .map_err(|_| anyhow!("{}", tr("Contraseña maestra incorrecta")))?,
        );
        let secrets: BTreeMap<String, String> =
            serde_json::from_slice(&plain).context(tr("baúl dañado (contenido)"))?;
        Ok(Self {
            path: path.to_path_buf(),
            key,
            salt,
            params,
            secrets,
        })
    }

    fn persist(&self) -> Result<()> {
        let plain = Zeroizing::new(serde_json::to_vec(&self.secrets)?);
        let mut nonce = [0u8; 24];
        rand::fill(&mut nonce);
        let cipher = XChaCha20Poly1305::new_from_slice(self.key.as_ref())
            .map_err(|_| anyhow!("{}", tr("clave no válida")))?;
        let ciphertext = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &plain,
                    aad: AAD,
                },
            )
            .map_err(|_| anyhow!("{}", tr("fallo al cifrar el baúl")))?;
        let file = VaultFile {
            version: 1,
            m_cost: self.params.0,
            t_cost: self.params.1,
            p_cost: self.params.2,
            salt: B64.encode(self.salt),
            nonce: B64.encode(nonce),
            ciphertext: B64.encode(ciphertext),
        };
        crate::paths::write_private(&self.path, &serde_json::to_vec_pretty(&file)?)?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.secrets.get(key).map(String::as_str)
    }

    /// Nombres de todos los secretos guardados.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.secrets.keys().map(String::as_str)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.secrets.contains_key(key)
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        if self.secrets.get(key).map(String::as_str) == Some(value) {
            return Ok(());
        }
        if let Some(mut old) = self.secrets.insert(key.to_string(), value.to_string()) {
            old.zeroize();
        }
        self.persist()
    }

    pub fn remove(&mut self, key: &str) -> Result<()> {
        if let Some(mut old) = self.secrets.remove(key) {
            old.zeroize();
            self.persist()?;
        }
        Ok(())
    }

    /// Elimina todos los secretos cuyo nombre empieza por `prefix`.
    pub fn remove_prefix(&mut self, prefix: &str) -> Result<()> {
        let keys: Vec<String> = self
            .secrets
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        if keys.is_empty() {
            return Ok(());
        }
        for k in keys {
            if let Some(mut old) = self.secrets.remove(&k) {
                old.zeroize();
            }
        }
        self.persist()
    }

    pub fn len(&self) -> usize {
        self.secrets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    /// Cambia la contraseña maestra (re-cifra con sal nueva).
    pub fn change_master(&mut self, new_master: &str) -> Result<()> {
        if new_master.is_empty() {
            bail!("{}", tr("La contraseña maestra no puede estar vacía"));
        }
        let mut salt = [0u8; 16];
        rand::fill(&mut salt);
        let params = (KDF_M_COST, KDF_T_COST, KDF_P_COST);
        self.key = derive_key(new_master, &salt, params)?;
        self.salt = salt;
        self.params = params;
        self.persist()
    }
}

// --- Para la sincronización entre equipos (`crate::sync`) ---------------

/// Costes máximos que se admiten en un baúl que llega de fuera: derivar su
/// clave no debe poder agotar la memoria.
const MAX_FOREIGN_M_COST: u32 = 1024 * 1024;
const MAX_FOREIGN_T_COST: u32 = 16;
const MAX_FOREIGN_P_COST: u32 = 8;

/// Las piezas de un fichero de baúl, comprobadas: sal, nonce, texto cifrado
/// y costes.
type Parts = ([u8; 16], [u8; 24], Vec<u8>, (u32, u32, u32));

fn parts(raw: &[u8]) -> Option<Parts> {
    let file: VaultFile = serde_json::from_slice(raw).ok()?;
    let costs_ok = (8..=MAX_FOREIGN_M_COST).contains(&file.m_cost)
        && (1..=MAX_FOREIGN_T_COST).contains(&file.t_cost)
        && (1..=MAX_FOREIGN_P_COST).contains(&file.p_cost);
    if file.version != 1 || !costs_ok {
        return None;
    }
    let salt: [u8; 16] = B64.decode(&file.salt).ok()?.try_into().ok()?;
    let nonce: [u8; 24] = B64.decode(&file.nonce).ok()?.try_into().ok()?;
    let ciphertext = B64.decode(&file.ciphertext).ok()?;
    (ciphertext.len() >= 16).then_some((
        salt,
        nonce,
        ciphertext,
        (file.m_cost, file.t_cost, file.p_cost),
    ))
}

/// Secretos sacados de un baúl; se borran de la memoria al soltarlos.
pub struct Secrets(BTreeMap<String, String>);

impl std::ops::Deref for Secrets {
    type Target = BTreeMap<String, String>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for Secrets {
    fn drop(&mut self) {
        for value in self.0.values_mut() {
            value.zeroize();
        }
    }
}

/// `true` si `raw` tiene la forma de un fichero de baúl con costes
/// razonables. No dice nada de su contraseña ni de su contenido.
pub fn looks_valid(raw: &[u8]) -> bool {
    parts(raw).is_some()
}

impl Vault {
    /// Todos los secretos, para combinarlos con los de otro baúl.
    pub fn entries(&self) -> &BTreeMap<String, String> {
        &self.secrets
    }

    /// Secretos de otro fichero de baúl (el de otro equipo del usuario, que
    /// llega por la sincronización). Se abre con la clave de este si viene
    /// del mismo baúl o, si no, con `master`.
    pub fn read_other(&self, raw: &[u8], master: Option<&str>) -> Result<Secrets> {
        let (salt, nonce, ciphertext, params) =
            parts(raw).ok_or_else(|| anyhow!("El otro baúl no es válido"))?;
        let key = if salt == self.salt && params == self.params {
            self.key.clone()
        } else if let Some(master) = master.filter(|m| !m.is_empty()) {
            derive_key(master, &salt, params)?
        } else {
            bail!("El otro baúl tiene otra contraseña maestra");
        };
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
            .map_err(|_| anyhow!("clave no válida"))?;
        let plain = Zeroizing::new(
            cipher
                .decrypt(
                    &XNonce::from(nonce),
                    Payload {
                        msg: &ciphertext,
                        aad: AAD,
                    },
                )
                .map_err(|_| anyhow!("La contraseña maestra del otro baúl no es esa"))?,
        );
        Ok(Secrets(
            serde_json::from_slice(&plain).context("el otro baúl está dañado")?,
        ))
    }

    /// Guarda y quita varios secretos de una vez.
    pub fn apply(&mut self, set: &BTreeMap<String, String>, remove: &[String]) -> Result<()> {
        for (key, value) in set {
            if let Some(mut old) = self.secrets.insert(key.clone(), value.clone()) {
                old.zeroize();
            }
        }
        for key in remove {
            if let Some(mut old) = self.secrets.remove(key) {
                old.zeroize();
            }
        }
        self.persist()
    }

    /// Vuelve a leer el fichero, que ha cambiado por fuera (lo ha traído la
    /// sincronización), con la clave que ya se tiene. Falla si ahora es
    /// otro baúl (otra contraseña maestra): hay que volver a abrirlo.
    pub fn reload(&mut self) -> Result<()> {
        let raw = std::fs::read(&self.path)
            .with_context(|| format!("leyendo {}", self.path.display()))?;
        let fresh = self.read_other(&raw, None)?;
        for value in self.secrets.values_mut() {
            value.zeroize();
        }
        self.secrets = fresh.0.clone();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_vaults_are_read_merged_and_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.json");
        let mut mine = Vault::create(&path, "maestra").unwrap();
        mine.set("a/password", "uno").unwrap();
        let copy = std::fs::read(&path).unwrap();
        assert!(looks_valid(&copy));
        assert!(!looks_valid(b"{}") && !looks_valid(b"no es json"));
        // Costes desmedidos: no se intenta siquiera derivar la clave.
        let huge = String::from_utf8(copy.clone())
            .unwrap()
            .replace("\"m_cost\": 65536", "\"m_cost\": 4000000000");
        assert!(!looks_valid(huge.as_bytes()));

        // Una copia del mismo baúl (misma sal) se lee con la clave en uso.
        mine.set("b/password", "dos").unwrap();
        let old = mine.read_other(&copy, None).unwrap();
        assert_eq!(old.len(), 1);
        assert_eq!(old.get("a/password").map(String::as_str), Some("uno"));

        // Otro baúl, con otra contraseña maestra, necesita la suya.
        let other_path = dir.path().join("otro.json");
        let mut other = Vault::create(&other_path, "distinta").unwrap();
        other.set("c/password", "tres").unwrap();
        let other_raw = std::fs::read(&other_path).unwrap();
        // (`Secrets` no se puede imprimir: no hay `unwrap_err`.)
        let e = mine.read_other(&other_raw, None).err().unwrap();
        assert!(
            format!("{e:#}").contains("otra contraseña maestra"),
            "{e:#}"
        );
        assert!(mine.read_other(&other_raw, Some("mala")).is_err());
        let theirs = mine.read_other(&other_raw, Some("distinta")).unwrap();
        assert_eq!(theirs.get("c/password").map(String::as_str), Some("tres"));

        // Varios cambios de una vez, y quedan en disco.
        mine.apply(&theirs, &["a/password".to_string()]).unwrap();
        let reopened = Vault::unlock(&path, "maestra").unwrap();
        assert_eq!(reopened.get("c/password"), Some("tres"));
        assert_eq!(reopened.get("b/password"), Some("dos"));
        assert!(!reopened.contains("a/password"));
        assert_eq!(mine.entries().len(), 2);

        // El fichero cambia por fuera: con la misma clave se relee; si es
        // otro baúl, no.
        std::fs::write(&path, &copy).unwrap();
        mine.reload().unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine.get("a/password"), Some("uno"));
        std::fs::write(&path, &other_raw).unwrap();
        assert!(mine.reload().is_err());
        assert_eq!(mine.get("a/password"), Some("uno"));
    }

    #[test]
    fn roundtrip_and_wrong_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.json");
        {
            let mut v = Vault::create(&path, "maestra").unwrap();
            v.set("a/password", "s3cr3t-ñ").unwrap();
            v.set("a/passphrase", "frase").unwrap();
            v.set("b/password", "otro").unwrap();
        }
        // El secreto no aparece en claro en el fichero.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("s3cr3t"));
        assert!(!raw.contains("a/password"));

        assert!(Vault::unlock(&path, "incorrecta").is_err());
        let mut v = Vault::unlock(&path, "maestra").unwrap();
        assert_eq!(v.get("a/password"), Some("s3cr3t-ñ"));
        assert_eq!(v.len(), 3);

        v.remove_prefix("a/").unwrap();
        let v = Vault::unlock(&path, "maestra").unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v.get("b/password"), Some("otro"));
    }

    #[test]
    fn change_master_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.json");
        let mut v = Vault::create(&path, "vieja").unwrap();
        v.set("k", "v").unwrap();
        v.change_master("nueva").unwrap();
        drop(v);
        assert!(Vault::unlock(&path, "vieja").is_err());
        assert_eq!(Vault::unlock(&path, "nueva").unwrap().get("k"), Some("v"));
    }

    #[test]
    fn tampering_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.json");
        let mut v = Vault::create(&path, "m").unwrap();
        v.set("k", "valor").unwrap();
        drop(v);
        let mut file: VaultFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut ct = B64.decode(&file.ciphertext).unwrap();
        ct[0] ^= 1;
        file.ciphertext = B64.encode(ct);
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert!(Vault::unlock(&path, "m").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.json");
        Vault::create(&path, "m").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
