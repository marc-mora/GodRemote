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
        .map_err(|e| anyhow!("parámetros KDF no válidos: {e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(master.as_bytes(), salt, key.as_mut())
        .map_err(|e| anyhow!("no se pudo derivar la clave: {e}"))?;
    Ok(key)
}

impl Vault {
    pub fn exists(path: &Path) -> bool {
        path.exists()
    }

    /// Crea un baúl nuevo (vacío) protegido por `master`.
    pub fn create(path: &Path, master: &str) -> Result<Self> {
        if master.is_empty() {
            bail!("La contraseña maestra no puede estar vacía");
        }
        if path.exists() {
            bail!("Ya existe un baúl en {}", path.display());
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
        let raw = std::fs::read(path).with_context(|| format!("leyendo {}", path.display()))?;
        let file: VaultFile = serde_json::from_slice(&raw).context("baúl dañado")?;
        if file.version != 1 {
            bail!("Versión de baúl no soportada: {}", file.version);
        }
        let salt: [u8; 16] = B64
            .decode(&file.salt)?
            .try_into()
            .map_err(|_| anyhow!("baúl dañado (salt)"))?;
        let nonce: [u8; 24] = B64
            .decode(&file.nonce)?
            .try_into()
            .map_err(|_| anyhow!("baúl dañado (nonce)"))?;
        let ciphertext = B64.decode(&file.ciphertext)?;
        let params = (file.m_cost, file.t_cost, file.p_cost);
        let key = derive_key(master, &salt, params)?;
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
                .map_err(|_| anyhow!("Contraseña maestra incorrecta"))?,
        );
        let secrets: BTreeMap<String, String> =
            serde_json::from_slice(&plain).context("baúl dañado (contenido)")?;
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
            .map_err(|_| anyhow!("clave no válida"))?;
        let ciphertext = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &plain,
                    aad: AAD,
                },
            )
            .map_err(|_| anyhow!("fallo al cifrar el baúl"))?;
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
            bail!("La contraseña maestra no puede estar vacía");
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

#[cfg(test)]
mod tests {
    use super::*;

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
