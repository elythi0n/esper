//! QUIC endpoints.
//!
//! One endpoint per process, serving both roles: a machine can be a host
//! accepting sessions and a controller dialling out, and there is no reason for
//! those to be separate sockets.
//!
//! # Why QUIC
//!
//! Four things a remote desktop needs, in one place rather than hand-rolled:
//!
//! - **Independent streams** — a lost video packet must never delay a keystroke.
//! - **Unreliable datagrams alongside reliable streams** — retransmitting a
//!   video frame that is already too late to display costs latency for nothing.
//! - **Connection migration** — a laptop moving from wifi to a tether keeps its
//!   session rather than reconnecting.
//! - **Real loss and RTT signal**, which the congestion controller needs and
//!   which TCP does not expose.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use esper_crypto::{Identity, identity::KEY_LEN};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use crate::auth::{PeerAuthority, RawKeyVerifier};
use crate::crypto_suite;

/// ALPN identifier. A peer that does not offer it is not speaking Esper.
pub const ALPN: &[u8] = esper_proto::ALPN;

/// How long a connection may be idle before it is dropped.
///
/// Generous, because an idle session is a legitimate state — a user watching a
/// remote machine do something slow is idle from the transport's point of view.
/// Keepalives run well inside it so a NAT mapping does not lapse first.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// How often to send a keepalive.
///
/// Chosen against NAT rather than against the idle timeout: consumer routers
/// commonly drop a UDP mapping after 30 seconds, and some after 15. Ten seconds
/// keeps a mapping alive through the impatient ones with room for a lost packet.
pub const KEEPALIVE: Duration = Duration::from_secs(10);

/// How long to wait for a handshake before giving up on an address.
///
/// **Not the idle timeout.** A session may sit idle for thirty seconds because
/// somebody is watching a slow build; a *handshake* either completes in a
/// round trip or two or it is not going to. Waiting out the idle timeout for
/// an address with nothing behind it means thirty seconds of silence after a
/// typo, which is how a working product feels broken.
///
/// Ten seconds rather than two: QUIC retransmits its first packet with
/// exponential backoff, and a satellite link with loss deserves a few goes.
/// Three probe timeouts on a slow path is about where other implementations
/// put a handshake deadline.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// What can go wrong setting up or opening a connection.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The TLS configuration was rejected. Almost always a programming error
    /// rather than anything a peer did.
    #[error("TLS configuration: {0}")]
    Tls(String),

    /// The socket could not be bound.
    #[error("binding {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },

    /// The connection attempt failed. Includes being refused by the peer.
    #[error("connecting to {addr}: {source}")]
    Connect {
        addr: SocketAddr,
        #[source]
        source: quinn::ConnectionError,
    },

    /// The connection could not be started at all — a bad address, or no
    /// client configuration.
    #[error("cannot dial {addr}: {source}")]
    Dial {
        addr: SocketAddr,
        #[source]
        source: quinn::ConnectError,
    },

    /// Nothing answered in time.
    ///
    /// Its own variant rather than a `Connect` carrying a timeout, because it
    /// is the one dial failure with an obvious cause worth naming: nothing is
    /// listening there, or nothing can get to it.
    #[error("nothing answered at {addr} within {waited:?} — is the host running there?")]
    NoAnswer { addr: SocketAddr, waited: Duration },
}

/// An Esper QUIC endpoint: dials out, and accepts if configured to.
///
/// A handle rather than a resource: cloning it gives another way to reach the
/// *same* socket, which is what several dials at once need — and the socket is
/// the thing that matters, because a router's mapping belongs to a socket and
/// not to a program.
#[derive(Clone)]
pub struct Endpoint {
    inner: quinn::Endpoint,
    /// Our own public key, so a caller can report which device this is without
    /// holding the identity as well.
    public_key: [u8; KEY_LEN],
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field(
                "device",
                &esper_crypto::Fingerprint::of(&self.public_key).inline(),
            )
            .field("local_addr", &self.inner.local_addr().ok())
            .finish()
    }
}

impl Endpoint {
    /// Bind an endpoint that both accepts and dials.
    ///
    /// `accept_from` decides which peers may connect *in*; `dial_to` is applied
    /// when dialling out. Both are consulted during the TLS handshake, so a
    /// refused peer never reaches session code.
    pub fn bind(
        identity: &Identity,
        addr: SocketAddr,
        accept_from: Arc<dyn PeerAuthority>,
        dial_to: Arc<dyn PeerAuthority>,
    ) -> Result<Self, TransportError> {
        let provider = Arc::new(crypto_suite::provider());

        let server = server_config(identity, accept_from, Arc::clone(&provider))?;
        let client = client_config(identity, dial_to, provider)?;

        let mut inner = quinn::Endpoint::server(server, addr)
            .map_err(|source| TransportError::Bind { addr, source })?;
        inner.set_default_client_config(client);

        Ok(Self {
            inner,
            public_key: identity.public_key(),
        })
    }

    /// The address actually bound, which differs from the requested one when
    /// port 0 was asked for.
    pub fn local_addr(&self) -> Result<SocketAddr, std::io::Error> {
        self.inner.local_addr()
    }

    /// This device's public key.
    #[must_use]
    pub const fn public_key(&self) -> &[u8; KEY_LEN] {
        &self.public_key
    }

    /// Wait for an inbound connection whose handshake has completed.
    ///
    /// Returns `None` when the endpoint is closed.
    ///
    /// **A returned connection is authenticated but not yet a session.** The
    /// peer's key was verified during the handshake, but nothing has been
    /// exchanged. See the note in [`crate::auth`].
    pub async fn accept(&self) -> Option<Result<quinn::Connection, quinn::ConnectionError>> {
        let incoming = self.inner.accept().await?;
        Some(incoming.await)
    }

    /// Dial a peer.
    ///
    /// The server name is a fixed placeholder: TLS requires one, and Esper
    /// authenticates by key rather than by name, so there is nothing meaningful
    /// to put there and nothing that inspects it.
    pub async fn connect(&self, addr: SocketAddr) -> Result<quinn::Connection, TransportError> {
        self.connect_within(addr, DIAL_TIMEOUT).await
    }

    /// [`Endpoint::connect`], with the deadline as an argument.
    ///
    /// Separate so a test can prove the deadline without waiting for it, and
    /// so a caller that knows better about its own path — a relay it has
    /// already reached, say — can say so.
    pub async fn connect_within(
        &self,
        addr: SocketAddr,
        wait: Duration,
    ) -> Result<quinn::Connection, TransportError> {
        let attempt = self
            .inner
            .connect(addr, SERVER_NAME)
            .map_err(|source| TransportError::Dial { addr, source })?;

        match tokio::time::timeout(wait, attempt).await {
            Ok(Ok(conn)) => Ok(conn),
            Ok(Err(source)) => Err(TransportError::Connect { addr, source }),
            Err(_) => Err(TransportError::NoAnswer { addr, waited: wait }),
        }
    }

    /// Move to a different local socket, keeping every live connection.
    ///
    /// This is what makes a laptop survive moving from wifi to a tether: QUIC
    /// identifies a connection by its connection ID, not by the four-tuple, so
    /// the peer follows the new address without a reconnection.
    ///
    /// Two things are lost and both are expected: connections to peers not
    /// reachable from the new address, and the ability to accept *new* inbound
    /// connections on the old one. On error the old socket is retained, so a
    /// failed rebind leaves a working endpoint rather than a dead one.
    pub fn rebind(&self, socket: std::net::UdpSocket) -> Result<(), std::io::Error> {
        self.inner.rebind(socket)
    }

    /// Close the endpoint and every connection on it.
    pub fn close(&self, reason: esper_proto::Reason) {
        self.inner
            .close(u32::from(reason.0).into(), reason.to_string().as_bytes());
    }

    /// Wait for every connection to finish closing.
    pub async fn wait_idle(&self) {
        self.inner.wait_idle().await;
    }
}

/// The TLS server name. Authentication is by key; this satisfies a protocol
/// field that has nothing useful to carry.
const SERVER_NAME: &str = "esper";

/// Build the certified key that presents our raw public key.
fn certified_key(
    identity: &Identity,
    provider: &rustls::crypto::CryptoProvider,
) -> Result<Arc<rustls::sign::CertifiedKey>, TransportError> {
    let spki = identity.public_key_spki();
    let mut pkcs8 = identity.private_key_pkcs8();

    let key = PrivateKeyDer::try_from(pkcs8.to_vec())
        .map_err(|e| TransportError::Tls(format!("private key: {e}")))?;
    let signer = provider
        .key_provider
        .load_private_key(key)
        .map_err(|e| TransportError::Tls(format!("loading signing key: {e}")))?;

    // The DER copy has served its purpose; the provider holds its own.
    use zeroize::Zeroize;
    pkcs8.zeroize();

    Ok(Arc::new(rustls::sign::CertifiedKey::new(
        vec![CertificateDer::from(spki.to_vec())],
        signer,
    )))
}

fn server_config(
    identity: &Identity,
    authority: Arc<dyn PeerAuthority>,
    provider: Arc<rustls::crypto::CryptoProvider>,
) -> Result<quinn::ServerConfig, TransportError> {
    let certified = certified_key(identity, &provider)?;
    let verifier = Arc::new(RawKeyVerifier::new(authority, Arc::clone(&provider)));

    let mut tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(crypto_suite::TLS_VERSIONS)
        .map_err(|e| TransportError::Tls(e.to_string()))?
        .with_client_cert_verifier(verifier)
        .with_cert_resolver(Arc::new(
            rustls::server::AlwaysResolvesServerRawPublicKeys::new(certified),
        ));
    tls.alpn_protocols = vec![ALPN.to_vec()];

    let quic = QuicServerConfig::try_from(tls).map_err(|e| TransportError::Tls(e.to_string()))?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(quic));
    cfg.transport_config(Arc::new(transport_config()));

    // Enabled by default in quinn, and pinned here anyway.
    //
    // Migration is the reason a laptop moving from wifi to a tether keeps its
    // session instead of reconnecting — one of the four properties QUIC was
    // chosen for. A change to quinn's defaults must not be able to turn it off
    // silently, the same reasoning that names `prefer-post-quantum` explicitly.
    cfg.migration(true);

    Ok(cfg)
}

fn client_config(
    identity: &Identity,
    authority: Arc<dyn PeerAuthority>,
    provider: Arc<rustls::crypto::CryptoProvider>,
) -> Result<quinn::ClientConfig, TransportError> {
    let certified = certified_key(identity, &provider)?;
    let verifier = Arc::new(RawKeyVerifier::new(authority, Arc::clone(&provider)));

    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(crypto_suite::TLS_VERSIONS)
        .map_err(|e| TransportError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_cert_resolver(Arc::new(
            rustls::client::AlwaysResolvesClientRawPublicKeys::new(certified),
        ));
    tls.alpn_protocols = vec![ALPN.to_vec()];

    let quic = QuicClientConfig::try_from(tls).map_err(|e| TransportError::Tls(e.to_string()))?;
    let mut cfg = quinn::ClientConfig::new(Arc::new(quic));
    cfg.transport_config(Arc::new(transport_config()));
    Ok(cfg)
}

/// QUIC transport parameters.
///
/// Every value here is a resource bound as much as a tuning knob: an
/// unauthenticated peer can open streams and send datagrams, so the limits are
/// what stop a connection from costing us more than it costs them.
fn transport_config() -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();

    t.max_idle_timeout(Some(IDLE_TIMEOUT.try_into().unwrap_or_else(|_| {
        quinn::IdleTimeout::from(quinn::VarInt::from_u32(30_000))
    })));
    t.keep_alive_interval(Some(KEEPALIVE));

    // Stream limits. Media travels on datagrams; the stream users are the
    // control streams, bulk transfers, and — the reason this number is not 16
    // any more — **one bidirectional stream per forwarded TCP connection**.
    //
    // Raising it does not multiply memory: the connection-wide receive window
    // bounds what all streams together can buffer, so what more streams cost
    // is bookkeeping. The bound that matters for forwarding is the host's own
    // `forward.max_connections`, which is checked before anything is dialled;
    // this one sits above it with headroom, so a session that has hit its
    // forward cap gets a refusal it can read rather than a stream that will
    // not open.
    t.max_concurrent_bidi_streams(quinn::VarInt::from_u32(256));
    t.max_concurrent_uni_streams(quinn::VarInt::from_u32(64));

    // Datagrams carry media. The receive buffer is sized to hold roughly a
    // frame's worth of packets so a burst is absorbed rather than dropped, but
    // not so large that a hostile peer can pin memory.
    t.datagram_receive_buffer_size(Some(1024 * 1024));
    t.datagram_send_buffer_size(1024 * 1024);

    t
}
