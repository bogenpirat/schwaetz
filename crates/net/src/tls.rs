//! TLS configuration: Windows certificate store verification and client certificates.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_platform_verifier::BuilderVerifierExt;
use std::sync::Arc;

/// A PEM-encoded client certificate chain and private key (CertFP / SASL EXTERNAL).
#[derive(Clone, PartialEq, Eq)]
pub struct ClientCert {
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
}

impl std::fmt::Debug for ClientCert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClientCert { .. }")
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

pub(crate) fn client_config(accept_invalid: bool, cert: Option<&ClientCert>) -> Result<Arc<ClientConfig>, String> {
    let provider = provider();
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?;
    let builder = if accept_invalid {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny(provider.signature_verification_algorithms)))
    } else {
        builder.with_platform_verifier().map_err(|e| e.to_string())?
    };
    let config = match cert {
        Some(c) => {
            let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&c.cert_pem)
                .collect::<Result<_, _>>()
                .map_err(|e| format!("client certificate: {e}"))?;
            // Keys are commonly stored in the same PEM file as the certificate.
            let key = PrivateKeyDer::from_pem_slice(&c.key_pem)
                .or_else(|_| PrivateKeyDer::from_pem_slice(&c.cert_pem))
                .map_err(|e| format!("client key: {e}"))?;
            builder.with_client_auth_cert(chain, key).map_err(|e| e.to_string())?
        }
        None => builder.with_no_client_auth(),
    };
    Ok(Arc::new(config))
}

/// Verifier that accepts any certificate (still checks handshake signatures). Opt-in only.
#[derive(Debug)]
struct AcceptAny(WebPkiSupportedAlgorithms);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}
