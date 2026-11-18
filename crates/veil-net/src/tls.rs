//! TLS 1.3 with raw Ed25519 public keys (RFC 7250).
//!
//! Each side presents its device key as a 44-byte SubjectPublicKeyInfo instead of a
//! certificate. Parsing is an exact match on that fixed encoding, so there is no X.509 parser
//! to attack. Handshake signatures are checked with the key that was presented, and the
//! dialler additionally requires that key to be the one it expected.

use std::sync::Arc;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::sign::CertifiedKey;
use rustls::{
    CertificateError, DigitallySignedStruct, DistinguishedName, Error, SignatureScheme,
};
use veil_security::{Identity, PublicId};
use zeroize::Zeroizing;

use crate::NetError;

pub const ALPN: &[u8] = b"veil/1";

const SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];
const PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// Encodes a device key as an Ed25519 SubjectPublicKeyInfo.
pub fn spki_der(id: &PublicId) -> Vec<u8> {
    let mut der = Vec::with_capacity(44);
    der.extend_from_slice(&SPKI_PREFIX);
    der.extend_from_slice(&id.0);
    der
}

/// Reads a device key from an Ed25519 SubjectPublicKeyInfo. Anything else is refused.
pub fn parse_spki(der: &[u8]) -> Option<PublicId> {
    let key = der.strip_prefix(&SPKI_PREFIX)?;
    let bytes: [u8; 32] = key.try_into().ok()?;
    Some(PublicId(bytes))
}

/// The device key as a rustls signing identity.
pub fn certified_key(identity: &Identity) -> Result<Arc<CertifiedKey>, NetError> {
    let secret = identity.secret_bytes();
    let mut pkcs8 = Zeroizing::new(Vec::with_capacity(48));
    pkcs8.extend_from_slice(&PKCS8_PREFIX);
    pkcs8.extend_from_slice(secret.as_ref());
    let key = rustls::crypto::ring::sign::any_eddsa_type(&PrivatePkcs8KeyDer::from(&pkcs8[..]))
        .map_err(|e| NetError::Config(e.to_string()))?;
    let spki = CertificateDer::from(spki_der(&identity.public()));
    Ok(Arc::new(CertifiedKey::new(vec![spki], key)))
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn verify_signature(
    message: &[u8],
    presented: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, Error> {
    if dss.scheme != SignatureScheme::ED25519 {
        return Err(Error::InvalidCertificate(CertificateError::BadSignature));
    }
    let id = parse_spki(presented).ok_or(Error::InvalidCertificate(CertificateError::BadEncoding))?;
    let signature: [u8; 64] = dss
        .signature()
        .try_into()
        .map_err(|_| Error::InvalidCertificate(CertificateError::BadSignature))?;
    id.verify(message, &signature)
        .map_err(|_| Error::InvalidCertificate(CertificateError::BadSignature))?;
    Ok(HandshakeSignatureValid::assertion())
}

/// Dialler side: accepts only the expected device key.
#[derive(Debug)]
struct PinnedPeer {
    expected: PublicId,
}

impl ServerCertVerifier for PinnedPeer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        if !intermediates.is_empty() {
            return Err(Error::InvalidCertificate(CertificateError::BadEncoding));
        }
        match parse_spki(end_entity) {
            Some(id) if id == self.expected => Ok(ServerCertVerified::assertion()),
            Some(_) => Err(Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            )),
            None => Err(Error::InvalidCertificate(CertificateError::BadEncoding)),
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        // TLS 1.2 is never negotiated: only TLS 1.3 is enabled.
        Err(Error::InvalidCertificate(CertificateError::BadSignature))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        if parse_spki(cert) != Some(self.expected) {
            return Err(Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ));
        }
        verify_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

/// Dialler side, for a fetch whose answer proves itself: accepts any key its holder can prove.
/// The link is still encrypted, but says nothing about who is at the other end.
#[derive(Debug)]
struct AnyServerKey;

impl ServerCertVerifier for AnyServerKey {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        if !intermediates.is_empty() || parse_spki(end_entity).is_none() {
            return Err(Error::InvalidCertificate(CertificateError::BadEncoding));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Err(Error::InvalidCertificate(CertificateError::BadSignature))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

/// Listener side: requires a valid device key and proof of holding it. Admission is decided
/// later, by the room.
#[derive(Debug)]
struct AnyDeviceKey;

impl ClientCertVerifier for AnyDeviceKey {
    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        if !intermediates.is_empty() || parse_spki(end_entity).is_none() {
            return Err(Error::InvalidCertificate(CertificateError::BadEncoding));
        }
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Err(Error::InvalidCertificate(CertificateError::BadSignature))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

pub(crate) fn server_config(key: Arc<CertifiedKey>) -> Result<QuicServerConfig, NetError> {
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| NetError::Config(e.to_string()))?
        .with_client_cert_verifier(Arc::new(AnyDeviceKey))
        .with_cert_resolver(Arc::new(
            rustls::server::AlwaysResolvesServerRawPublicKeys::new(key),
        ));
    config.alpn_protocols = vec![ALPN.to_vec()];
    QuicServerConfig::try_from(config).map_err(|e| NetError::Config(e.to_string()))
}

pub(crate) fn client_config(
    key: Arc<CertifiedKey>,
    expected: PublicId,
) -> Result<QuicClientConfig, NetError> {
    client_with(key, Arc::new(PinnedPeer { expected }))
}

/// For [`crate::Node::connect_unverified`] only.
pub(crate) fn unverified_client_config(
    key: Arc<CertifiedKey>,
) -> Result<QuicClientConfig, NetError> {
    client_with(key, Arc::new(AnyServerKey))
}

fn client_with(
    key: Arc<CertifiedKey>,
    verifier: Arc<dyn ServerCertVerifier>,
) -> Result<QuicClientConfig, NetError> {
    let mut config = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| NetError::Config(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_cert_resolver(Arc::new(
            rustls::client::AlwaysResolvesClientRawPublicKeys::new(key),
        ));
    config.alpn_protocols = vec![ALPN.to_vec()];
    QuicClientConfig::try_from(config).map_err(|e| NetError::Config(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spki_round_trip() {
        let id = PublicId([5; 32]);
        assert_eq!(parse_spki(&spki_der(&id)), Some(id));
    }

    #[test]
    fn spki_with_extra_or_missing_bytes_is_refused() {
        let mut der = spki_der(&PublicId([5; 32]));
        der.push(0);
        assert_eq!(parse_spki(&der), None);
        assert_eq!(parse_spki(&der[..40]), None);
        assert_eq!(parse_spki(b"not a key"), None);
    }

    #[test]
    fn spki_of_another_algorithm_is_refused() {
        let mut der = spki_der(&PublicId([5; 32]));
        der[8] = 0x71; // Ed448 object identifier
        assert_eq!(parse_spki(&der), None);
    }

    #[test]
    fn device_key_loads_into_tls() {
        assert!(certified_key(&Identity::generate()).is_ok());
    }
}
