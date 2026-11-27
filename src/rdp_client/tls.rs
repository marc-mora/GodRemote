//! TLS de la conexión RDP.
//!
//! En RDP lo habitual son certificados autofirmados, así que no se validan
//! contra ninguna autoridad: quien llama decide después si confía, por la
//! huella del certificado. Lo que sí se comprueba aquí es que el servidor
//! tiene la clave privada de ese certificado.

use std::net::IpAddr;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use tokio::io::AsyncWriteExt as _;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls;
use tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use tokio_rustls::rustls::pki_types;

use crate::ssh::NetStream;

/// Acepta cualquier certificado (la confianza se decide después, por huella)
/// pero SÍ comprueba la firma del intercambio TLS: sin eso un intermediario
/// podría presentar el certificado legítimo sin tener su clave privada y la
/// comprobación de la huella no valdría nada.
#[derive(Debug)]
struct PinnedLater {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for PinnedLater {
    fn verify_server_cert(
        &self,
        _end_entity: &pki_types::CertificateDer<'_>,
        _intermediates: &[pki_types::CertificateDer<'_>],
        _server_name: &pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: pki_types::UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub(super) async fn upgrade(
    stream: NetStream,
    host: String,
    peer_ip: IpAddr,
) -> Result<TlsStream<NetStream>> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .context("No se pudo preparar TLS")?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedLater { provider }))
        .with_no_client_auth();
    // CredSSP no admite la reanudación de sesiones TLS.
    config.resumption = rustls::client::Resumption::disabled();

    // El nombre solo se usa para SNI; si no es válido se usa la IP.
    let name = pki_types::ServerName::try_from(host)
        .unwrap_or_else(|_| pki_types::ServerName::IpAddress(peer_ip.into()));
    let mut tls = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, stream)
        .await
        .context("Fallo en la negociación TLS")?;
    tls.flush().await.context("Fallo en la negociación TLS")?;
    Ok(tls)
}

/// Clave pública del certificado (el contenido del `subjectPublicKey`), a la
/// que CredSSP liga la autenticación.
pub(super) fn server_public_key(certificate: &[u8]) -> Result<Vec<u8>> {
    use x509_cert::der::Decode as _;

    // Perfil «Raw»: los certificados autofirmados de RDP no siempre cumplen
    // la RFC 5280 al pie de la letra (números de serie, etc.).
    let cert = x509_cert::certificate::CertificateInner::<x509_cert::certificate::Raw>::from_der(
        certificate,
    )
    .map_err(|e| anyhow!("No se pudo interpretar el certificado del servidor: {e}"))?;
    cert.tbs_certificate()
        .subject_public_key_info()
        .subject_public_key
        .as_bytes()
        .map(<[u8]>::to_vec)
        .ok_or_else(|| anyhow!("El certificado del servidor tiene una clave pública no válida"))
}
