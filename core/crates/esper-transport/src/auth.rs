//! Who is allowed to connect, and how that is proven.
//!
//! Esper authenticates peers by **raw public key** (RFC 7250). There is no
//! certificate authority, no chain to build, no expiry to check and no name to
//! match. Pairing stored a key; every later connection proves possession of it.
//!
//! An allowlist is the only mode. A key that is not authorized is refused
//! before any session state is allocated, and there is no anonymous or
//! trust-on-first-use path.
//!
//! # A completed handshake is not an authenticated peer
//!
//! TLS 1.3 finishes the *client* handshake before the server has verified the
//! client's certificate. A client whose key the server rejects still sees
//! `connect()` succeed; the server's alert arrives afterwards.
//!
//! So a connected transport proves only that the client accepted the server.
//! Authentication is established when the peer has done something that
//! required accepting us — which is why the session opens with `Hello` in both
//! directions and nothing may happen until it has been **received**.

use std::fmt;
use std::sync::Arc;

use esper_crypto::{der, identity::KEY_LEN};
use rustls_pki_types::{CertificateDer, ServerName, SubjectPublicKeyInfoDer, UnixTime};

/// Decides whether a peer key may open a connection.
///
/// The transport does not own the device store: it asks. That keeps the store's
/// locking, persistence and policy out of the handshake path, and lets pairing
/// supply a different, deliberately narrower authority for first contact.
pub trait PeerAuthority: fmt::Debug + Send + Sync + 'static {
    /// Whether this key is permitted to connect at all.
    ///
    /// Called during the TLS handshake, so it must not block and must not
    /// perform I/O. Any answer other than "yes, this exact key" is a refusal.
    fn is_authorized(&self, public_key: &[u8; KEY_LEN]) -> bool;

    /// A short description for logs. Must not include key material.
    fn describe(&self) -> &'static str;
}

/// Authorizes exactly one key.
///
/// Used by a controller dialling a specific host: the peer either is the device
/// we paired with or the connection has no business succeeding.
#[derive(Clone)]
pub struct OnlyPeer {
    key: [u8; KEY_LEN],
}

impl OnlyPeer {
    /// Authorize only this key.
    #[must_use]
    pub const fn new(key: [u8; KEY_LEN]) -> Self {
        Self { key }
    }
}

impl fmt::Debug for OnlyPeer {
    /// Renders the fingerprint, never the key. A `Debug` that prints key
    /// material is how one ends up in a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OnlyPeer")
            .field("peer", &esper_crypto::Fingerprint::of(&self.key).inline())
            .finish()
    }
}

impl PeerAuthority for OnlyPeer {
    fn is_authorized(&self, public_key: &[u8; KEY_LEN]) -> bool {
        // Constant-time comparison. The timing of a key comparison is not a
        // realistic oracle here — the key is public — but using the constant
        // time primitive by default means no judgement call is required at each
        // site about whether this one is the exception.
        use subtle::ConstantTimeEq;
        self.key.ct_eq(public_key).into()
    }

    fn describe(&self) -> &'static str {
        "a single paired peer"
    }
}

/// Authorizes any key in a set — a host accepting connections from its paired
/// devices.
#[derive(Clone, Default)]
pub struct AnyOf {
    keys: Vec<[u8; KEY_LEN]>,
}

impl AnyOf {
    /// Authorize the given keys. An empty set authorizes nothing, which is the
    /// correct behaviour for a host with no paired devices.
    #[must_use]
    pub fn new(keys: Vec<[u8; KEY_LEN]>) -> Self {
        Self { keys }
    }

    /// How many keys are authorized.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether nothing is authorized.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

impl fmt::Debug for AnyOf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnyOf")
            .field("peers", &self.keys.len())
            .finish()
    }
}

impl PeerAuthority for AnyOf {
    fn is_authorized(&self, public_key: &[u8; KEY_LEN]) -> bool {
        use subtle::ConstantTimeEq;
        // Fold rather than short-circuit: iterate the whole set so the time
        // taken does not reveal the position of a match.
        self.keys
            .iter()
            .fold(subtle::Choice::from(0u8), |acc, k| {
                acc | k.ct_eq(public_key)
            })
            .into()
    }

    fn describe(&self) -> &'static str {
        "any paired peer"
    }
}

/// Authorizes nothing.
///
/// The correct authority for a host with sessions disabled, and the safe default
/// if a caller forgets to supply one.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPeers;

impl PeerAuthority for NoPeers {
    fn is_authorized(&self, _public_key: &[u8; KEY_LEN]) -> bool {
        false
    }

    fn describe(&self) -> &'static str {
        "no peers (sessions disabled)"
    }
}

/// The rustls verifier that turns a [`PeerAuthority`] into TLS behaviour.
///
/// Implements both the client-side and server-side verifier traits, because
/// Esper authenticates mutually and the check is identical in both
/// directions: the presented SPKI must decode to a key the authority permits.
pub(crate) struct RawKeyVerifier {
    authority: Arc<dyn PeerAuthority>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl fmt::Debug for RawKeyVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawKeyVerifier")
            .field("authority", &self.authority)
            .finish()
    }
}

impl RawKeyVerifier {
    pub(crate) fn new(
        authority: Arc<dyn PeerAuthority>,
        provider: Arc<rustls::crypto::CryptoProvider>,
    ) -> Self {
        Self {
            authority,
            provider,
        }
    }

    /// Decode the presented SPKI and ask the authority about it.
    ///
    /// The decode is a length check and a constant comparison against a fixed
    /// prefix — see `esper_crypto::der`. A peer cannot steer it, and anything
    /// that is not exactly an Ed25519 SPKI is refused before the authority is
    /// consulted.
    fn check(&self, presented: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        let Some(key) = der::public_key_from_spki(presented.as_ref()) else {
            return Err(rustls::Error::General(
                "peer did not present an Ed25519 raw public key".into(),
            ));
        };
        if self.authority.is_authorized(&key) {
            Ok(())
        } else {
            // The message is deliberately the same whether the key is unknown
            // or belongs to a revoked device: the peer learns only that it was
            // refused, and the host's own log carries the detail.
            Err(rustls::Error::General(
                "peer key is not paired with this device".into(),
            ))
        }
    }

    fn verify_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        // The *raw key* variant. The ordinary `verify_tls13_signature` parses an
        // X.509 certificate to find the public key; under RFC 7250 there is no
        // certificate, only the SubjectPublicKeyInfo. Passing an SPKI to the
        // ordinary function fails with a certificate error several steps later,
        // which sends you looking in the wrong place entirely.
        rustls::crypto::verify_tls13_signature_with_raw_key(
            message,
            &SubjectPublicKeyInfoDer::from(cert.as_ref()),
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl rustls::client::danger::ServerCertVerifier for RawKeyVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        // There is no chain in RFC 7250. A peer sending intermediates is either
        // confused or probing; either way it is not speaking our protocol.
        if !intermediates.is_empty() {
            return Err(rustls::Error::General(
                "raw public key authentication takes no certificate chain".into(),
            ));
        }
        self.check(end_entity)
            .map(|()| rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        // TLS 1.3 only. Refusing here rather than relying on version
        // negotiation means a downgrade cannot succeed even if a future config
        // change accidentally re-enabled 1.2.
        Err(rustls::Error::General("TLS 1.2 is not supported".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.verify_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.schemes()
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

impl rustls::server::danger::ClientCertVerifier for RawKeyVerifier {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        // No certificate authorities exist, so there are none to hint at.
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        if !intermediates.is_empty() {
            return Err(rustls::Error::General(
                "raw public key authentication takes no certificate chain".into(),
            ));
        }
        self.check(end_entity)
            .map(|()| rustls::server::danger::ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not supported".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.verify_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.schemes()
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }

    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        // Mutual authentication, always. A client that declines to present a
        // key is refused, not admitted as anonymous.
        true
    }
}
