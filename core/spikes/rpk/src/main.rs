//! Can two peers authenticate each other by raw Ed25519 public key over QUIC,
//! with no certificate authority anywhere?
//!
//! This is the shape OwnView needs: pairing stores a peer's public key, and
//! every later connection proves possession of it. No CA, no expiry, no name
//! to validate — the key is the identity.
//!
//! Three things must hold, and the third is the one worth measuring:
//!
//! 1. A paired peer connects.
//! 2. An unpaired peer is refused.
//! 3. A peer presenting a *different* key than the one we paired with is
//!    refused — the "changed key" case, which must fail loudly rather than
//!    prompt.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use quinn::{ClientConfig, Endpoint, ServerConfig};
use rustls::client::AlwaysResolvesClientRawPublicKeys;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, SubjectPublicKeyInfoDer};
use rustls::server::AlwaysResolvesServerRawPublicKeys;

const ALPN: &[u8] = b"ownview/1";

fn keypair() -> SigningKey {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).expect("entropy");
    SigningKey::from_bytes(&seed)
}

/// Build the rustls `CertifiedKey` that presents a bare Ed25519 public key.
fn certified(key: &SigningKey) -> Arc<rustls::sign::CertifiedKey> {
    // Our own hand-built encodings, not a DER library. This spike is now also
    // the test that a TLS provider accepts them.
    let spki = ownview_crypto::der::spki(&key.verifying_key().to_bytes()).to_vec();
    let pkcs8 = ownview_crypto::der::pkcs8(&key.to_bytes()).to_vec();

    let signer = rustls::crypto::aws_lc_rs::default_provider()
        .key_provider
        .load_private_key(PrivateKeyDer::try_from(pkcs8).expect("private key"))
        .expect("load signing key");

    Arc::new(rustls::sign::CertifiedKey::new(
        vec![CertificateDer::from(spki)],
        signer,
    ))
}

/// The SPKI bytes a peer will present for this key — what we compare against.
fn spki_of(key: &SigningKey) -> Vec<u8> {
    ownview_crypto::der::spki(&key.verifying_key().to_bytes()).to_vec()
}

#[tokio::main]
async fn main() {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("provider");

    println!("== raw public key authentication over QUIC ==\n");

    let host = keypair();
    let paired_client = keypair();
    let stranger = keypair();

    for (label, client, host_expects) in [
        ("paired peer", &paired_client, spki_of(&paired_client)),
        ("unpaired peer (stranger's key)", &stranger, spki_of(&paired_client)),
        ("changed key (paired name, new key)", &keypair(), spki_of(&paired_client)),
    ] {
        let outcome = attempt(&host, client, &host_expects, &spki_of(&host)).await;
        println!("  {label:36} -> {outcome}");
    }
}

async fn attempt(
    host: &SigningKey,
    client: &SigningKey,
    host_expects_client_spki: &[u8],
    client_expects_host_spki: &[u8],
) -> String {
    match try_connect(host, client, host_expects_client_spki, client_expects_host_spki).await {
        Ok(()) => "CONNECTED".to_owned(),
        Err(e) => format!("REFUSED ({})", first_line(&e.to_string())),
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or(s).chars().take(64).collect()
}

async fn try_connect(
    host: &SigningKey,
    client: &SigningKey,
    host_expects: &[u8],
    client_expects: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut provider = rustls::crypto::aws_lc_rs::default_provider();
    provider.kx_groups = vec![rustls::crypto::aws_lc_rs::kx_group::X25519MLKEM768];
    let provider = Arc::new(provider);

    // Server: present a raw public key, and require the client to present one
    // we recognise.
    let mut server_tls = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(ExpectKey {
            expected: host_expects.to_vec(),
        }))
        .with_cert_resolver(Arc::new(AlwaysResolvesServerRawPublicKeys::new(certified(
            host,
        ))));
    server_tls.alpn_protocols = vec![ALPN.to_vec()];

    // Client: same, in the other direction.
    let mut client_tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(ExpectKey {
            expected: client_expects.to_vec(),
        }))
        .with_client_cert_resolver(Arc::new(AlwaysResolvesClientRawPublicKeys::new(certified(
            client,
        ))));
    client_tls.alpn_protocols = vec![ALPN.to_vec()];

    let server_cfg = ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_tls)?,
    ));
    let client_cfg = ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(
        client_tls,
    )?));

    let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    let addr: SocketAddr = sock.local_addr()?;
    let server = Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(server_cfg),
        sock,
        Arc::new(quinn::TokioRuntime),
    )?;
    // The server echoes on a bidirectional stream. That matters: see below.
    tokio::spawn(async move {
        if let Some(inc) = server.accept().await {
            if let Ok(c) = inc.await {
                if let Ok((mut send, mut recv)) = c.accept_bi().await {
                    let _ = recv.read_to_end(64).await;
                    let _ = send.write_all(b"ok").await;
                    let _ = send.finish();
                }
                c.closed().await;
            }
        }
    });

    let mut ep = Endpoint::client((Ipv4Addr::LOCALHOST, 0).into())?;
    ep.set_default_client_config(client_cfg);
    let conn = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        ep.connect(addr, "ownview")?,
    )
    .await??;
    // **A completed `connect()` does not mean the peer accepted us.**
    //
    // In TLS 1.3 the client's handshake finishes before the server has
    // processed the client's certificate — the client sends its Certificate
    // and Finished, then considers itself connected. A server that rejects the
    // client key sends its alert afterwards.
    //
    // So authentication is only proven once the server has *done something*
    // that required accepting us. A round trip is the cheapest such proof, and
    // this is why OwnView's session establishment must not treat a connected
    // QUIC handle as an authenticated peer.
    let (mut send, mut recv) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        conn.open_bi(),
    )
    .await??;
    send.write_all(b"hello").await?;
    send.finish()?;
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        recv.read_to_end(64),
    )
    .await??;
    if reply != b"ok" {
        return Err("peer did not complete the round trip".into());
    }

    conn.close(0u32.into(), b"ok");
    ep.wait_idle().await;
    Ok(())
}

/// Accepts exactly one raw public key: the one we paired with.
///
/// This is the whole authorization model. There is no CA, no chain, no name to
/// validate, and no "unknown but plausible" case — the presented key either is
/// the key we stored or it is not.
#[derive(Debug)]
struct ExpectKey {
    expected: Vec<u8>,
}

impl ExpectKey {
    fn check(&self, presented: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        if std::env::var("RPK_DEBUG").is_ok() {
            eprintln!(
                "    presented {} bytes: {}",
                presented.as_ref().len(),
                hex(presented.as_ref())
            );
            eprintln!(
                "    expected  {} bytes: {}",
                self.expected.len(),
                hex(&self.expected)
            );
        }
        if presented.as_ref() == self.expected.as_slice() {
            Ok(())
        } else {
            Err(rustls::Error::General(
                "peer key is not the one paired with this device".into(),
            ))
        }
    }
}

impl rustls::client::danger::ServerCertVerifier for ExpectKey {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &rustls_pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        self.check(end_entity)
            .map(|()| rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not supported".into()))
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        // The raw-key variant. The ordinary `verify_tls13_signature` parses an
        // X.509 certificate to extract the public key; with RFC 7250 there is
        // no certificate, only the SubjectPublicKeyInfo itself.
        rustls::crypto::verify_tls13_signature_with_raw_key(
            message,
            &SubjectPublicKeyInfoDer::from(cert.as_ref()),
            dss,
            &rustls::crypto::aws_lc_rs::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

impl rustls::server::danger::ClientCertVerifier for ExpectKey {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: rustls_pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        self.check(end_entity)
            .map(|()| rustls::server::danger::ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not supported".into()))
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        // The raw-key variant. The ordinary `verify_tls13_signature` parses an
        // X.509 certificate to extract the public key; with RFC 7250 there is
        // no certificate, only the SubjectPublicKeyInfo itself.
        rustls::crypto::verify_tls13_signature_with_raw_key(
            message,
            &SubjectPublicKeyInfoDer::from(cert.as_ref()),
            dss,
            &rustls::crypto::aws_lc_rs::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

