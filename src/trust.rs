//! Confianza en certificados TLS «a la primera» (TOFU), para servidores que
//! usan certificados autofirmados (lo habitual en RDP).
//!
//! La primera vez se pregunta al usuario y se recuerda la huella; si más
//! adelante el certificado cambia, la conexión se rechaza.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::trf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustStatus {
    /// La huella coincide con la recordada.
    Trusted,
    /// No hay ninguna huella recordada para este servidor.
    Unknown,
    /// La huella recordada es otra: posible suplantación.
    Changed { expected: String },
}

fn key(host: &str, port: u16) -> String {
    format!("{}:{port}", host.trim().to_lowercase())
}

fn load(path: &Path) -> Result<BTreeMap<String, String>> {
    match std::fs::read(path) {
        Ok(data) => serde_json::from_slice(&data)
            .with_context(|| trf!("fichero de confianza dañado: {path}", path = path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e).with_context(|| trf!("leyendo {path}", path = path.display())),
    }
}

/// Compara la huella de un servidor con la recordada.
pub fn check(path: &Path, host: &str, port: u16, fingerprint: &str) -> Result<TrustStatus> {
    Ok(match load(path)?.get(&key(host, port)) {
        None => TrustStatus::Unknown,
        Some(known) if known == fingerprint => TrustStatus::Trusted,
        Some(known) => TrustStatus::Changed {
            expected: known.clone(),
        },
    })
}

/// Recuerda la huella de un servidor.
pub fn remember(path: &Path, host: &str, port: u16, fingerprint: &str) -> Result<()> {
    let mut map = load(path)?;
    map.insert(key(host, port), fingerprint.to_string());
    crate::paths::write_private(path, &serde_json::to_vec_pretty(&map)?)?;
    Ok(())
}

/// Huella `SHA256:…` (hexadecimal) de un certificado en DER.
pub fn fingerprint(der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(der);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("SHA256:{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tofu_flow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tls.json");
        let fp = fingerprint(b"certificado");
        assert!(fp.starts_with("SHA256:") && fp.len() == 7 + 64);
        assert_eq!(
            check(&path, "Srv", 3389, &fp).unwrap(),
            TrustStatus::Unknown
        );
        remember(&path, "Srv", 3389, &fp).unwrap();
        // El nombre no distingue mayúsculas; el puerto sí cuenta.
        assert_eq!(
            check(&path, "srv", 3389, &fp).unwrap(),
            TrustStatus::Trusted
        );
        assert_eq!(
            check(&path, "srv", 3390, &fp).unwrap(),
            TrustStatus::Unknown
        );
        let other = fingerprint(b"otro");
        assert_eq!(
            check(&path, "srv", 3389, &other).unwrap(),
            TrustStatus::Changed { expected: fp }
        );
    }
}
