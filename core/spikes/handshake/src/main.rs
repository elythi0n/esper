//! Handshake spike: does QUIC + TLS 1.3 give us a hybrid post-quantum key
//! agreement, and does it actually engage?
//!
//! Esper requires hybrid post-quantum key agreement from its first release.
//! This measures whether the TLS path delivers it in practice, because a handshake that *configures* X25519MLKEM768 and then
//! silently negotiates classical X25519 is worse than no PQ at all — it buys a
//! claim without the property.
//!
//! Run: `cargo run --release`

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Instant;

use quinn::{ClientConfig, Endpoint, ServerConfig};

const ALPN: &[u8] = b"esper/1";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("install crypto provider");

    println!("== handshake spike ==\n");
    report_kx_groups();

    prove_pq_engages()?;

    for (label, groups) in [
        ("hybrid PQ (X25519MLKEM768)", pq_groups()),
        ("classical only (X25519)", classical_groups()),
    ] {
        println!("\n-- {label} --");
        match run_once(groups).await {
            Ok(m) => m.report(),
            Err(e) => println!("  failed: {e}"),
        }
    }
    downgrade_resistance()?;
    Ok(())
}

/// What happens when one side will only speak PQ and the other will only speak
/// classical?
///
/// This is the property that decides the shipping configuration. If Esper
/// offers a *list* — hybrid first, classical after — then a peer or an active
/// attacker who strips the hybrid gets a classical session and nobody notices.
/// If Esper offers **only** the hybrid, there is nothing to fall back to: the
/// handshake fails. Both ends are ours, so interoperating with a classical-only
/// peer is not a requirement, which makes the strict configuration free.
fn downgrade_resistance() -> Result<(), Box<dyn std::error::Error>> {
    use rustls::{ClientConnection, ServerConnection};

    println!("\n-- downgrade resistance --");

    for (label, client_groups, server_groups) in [
        ("both PQ-only", pq_groups(), pq_groups()),
        ("client PQ-only, server classical-only", pq_groups(), classical_groups()),
        ("client list (PQ first), server classical-only", default_groups(), classical_groups()),
    ] {
        let mut cp = rustls::crypto::aws_lc_rs::default_provider();
        cp.kx_groups = client_groups;
        let mut sp = rustls::crypto::aws_lc_rs::default_provider();
        sp.kx_groups = server_groups;

        let cert = rcgen::generate_simple_self_signed(vec!["esper".into()])?;
        let cert_der = rustls_pki_types::CertificateDer::from(cert.cert.der().to_vec());
        let key_der = rustls_pki_types::PrivateKeyDer::try_from(cert.key_pair.serialize_der())?;

        let server_cfg = rustls::ServerConfig::builder_with_provider(Arc::new(sp))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)?;
        let client_cfg = rustls::ClientConfig::builder_with_provider(Arc::new(cp))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny))
            .with_no_client_auth();

        let name = rustls_pki_types::ServerName::try_from("esper")?.to_owned();
        let mut client = ClientConnection::new(Arc::new(client_cfg), name)?;
        let mut server = ServerConnection::new(Arc::new(server_cfg))?;

        let mut failed: Option<String> = None;
        for _ in 0..16 {
            let mut buf = Vec::new();
            client.write_tls(&mut buf)?;
            if !buf.is_empty() {
                server.read_tls(&mut buf.as_slice())?;
                if let Err(e) = server.process_new_packets() {
                    failed = Some(e.to_string());
                    break;
                }
            }
            let mut buf = Vec::new();
            server.write_tls(&mut buf)?;
            if !buf.is_empty() {
                client.read_tls(&mut buf.as_slice())?;
                if let Err(e) = client.process_new_packets() {
                    failed = Some(e.to_string());
                    break;
                }
            }
            if !client.is_handshaking() && !server.is_handshaking() {
                break;
            }
        }

        match failed {
            Some(e) => println!("  {label:46} -> REFUSED ({e})"),
            None => {
                let g = client
                    .negotiated_key_exchange_group()
                    .map(|g| format!("{:?}", g.name()))
                    .unwrap_or_else(|| "none".into());
                let tag = if g.contains("MLKEM") { "POST-QUANTUM" } else { "CLASSICAL" };
                println!("  {label:46} -> connected on {g} [{tag}]");
            }
        }
    }
    Ok(())
}

/// Drive a TLS 1.3 handshake in memory, with no QUIC in the way, and read the
/// negotiated key exchange group directly.
///
/// This exists because **quinn 0.11 does not expose the negotiated group** —
/// its `HandshakeData` carries only the ALPN protocol and server name. rustls
/// knows (`CommonState::negotiated_key_exchange_group`), but quinn does not
/// hand it through. So the only way to *confirm* rather than assume that the
/// hybrid engaged is to run the same rustls configuration outside QUIC.
fn prove_pq_engages() -> Result<(), Box<dyn std::error::Error>> {
    use rustls::{ClientConnection, ServerConnection};

    println!("\n-- proving the hybrid actually engages (rustls, no QUIC) --");

    for (label, groups) in [
        ("offered: X25519MLKEM768 only", pq_groups()),
        ("offered: X25519 only", classical_groups()),
        ("offered: provider default order", default_groups()),
    ] {
        let mut provider = rustls::crypto::aws_lc_rs::default_provider();
        provider.kx_groups = groups;
        let provider = Arc::new(provider);

        let cert = rcgen::generate_simple_self_signed(vec!["esper".into()])?;
        let cert_der = rustls_pki_types::CertificateDer::from(cert.cert.der().to_vec());
        let key_der = rustls_pki_types::PrivateKeyDer::try_from(cert.key_pair.serialize_der())?;

        let server_cfg = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)?;
        let client_cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny))
            .with_no_client_auth();

        let name = rustls_pki_types::ServerName::try_from("esper")?.to_owned();
        let mut client = ClientConnection::new(Arc::new(client_cfg), name)?;
        let mut server = ServerConnection::new(Arc::new(server_cfg))?;

        // Pump the two connections against each other until both are ready.
        for _ in 0..16 {
            let mut buf = Vec::new();
            client.write_tls(&mut buf)?;
            if !buf.is_empty() {
                server.read_tls(&mut buf.as_slice())?;
                server.process_new_packets()?;
            }
            let mut buf = Vec::new();
            server.write_tls(&mut buf)?;
            if !buf.is_empty() {
                client.read_tls(&mut buf.as_slice())?;
                client.process_new_packets()?;
            }
            if !client.is_handshaking() && !server.is_handshaking() {
                break;
            }
        }

        let group = client
            .negotiated_key_exchange_group()
            .map(|g| format!("{:?}", g.name()))
            .unwrap_or_else(|| "none".into());
        let pq = group.contains("MLKEM");
        println!(
            "  {label:34} -> {group}  {}",
            if pq { "[POST-QUANTUM]" } else { "[classical]" }
        );
    }
    Ok(())
}

fn default_groups() -> Vec<&'static dyn rustls::crypto::SupportedKxGroup> {
    rustls::crypto::aws_lc_rs::default_provider()
        .kx_groups
        .clone()
}

/// What the provider offers, and in what order. Order is the whole question:
/// the first group the client offers is the one that normally gets used.
fn report_kx_groups() {
    let p = rustls::crypto::aws_lc_rs::default_provider();
    println!("provider key exchange groups, in offer order:");
    for (i, g) in p.kx_groups.iter().enumerate() {
        let name = format!("{:?}", g.name());
        let marker = if name.contains("MLKEM") { "  <- post-quantum" } else { "" };
        println!("  {}. {name}{marker}", i + 1);
    }
    let first_is_pq = p
        .kx_groups
        .first()
        .map(|g| format!("{:?}", g.name()).contains("MLKEM"))
        .unwrap_or(false);
    println!(
        "\n  first offered is post-quantum: {}",
        if first_is_pq { "YES" } else { "NO — classical would win" }
    );
}

fn pq_groups() -> Vec<&'static dyn rustls::crypto::SupportedKxGroup> {
    vec![rustls::crypto::aws_lc_rs::kx_group::X25519MLKEM768]
}

fn classical_groups() -> Vec<&'static dyn rustls::crypto::SupportedKxGroup> {
    vec![rustls::crypto::aws_lc_rs::kx_group::X25519]
}

struct Measurement {
    handshake: std::time::Duration,
    client_to_server: u64,
    server_to_client: u64,
    alpn: String,
}

impl Measurement {
    fn report(&self) {
        println!("  handshake        {:?}", self.handshake);
        println!("  client -> server {} bytes", self.client_to_server);
        println!("  server -> client {} bytes", self.server_to_client);
        println!("  alpn             {}", self.alpn);
    }
}

async fn run_once(
    groups: Vec<&'static dyn rustls::crypto::SupportedKxGroup>,
) -> Result<Measurement, Box<dyn std::error::Error>> {
    let mut provider = rustls::crypto::aws_lc_rs::default_provider();
    provider.kx_groups = groups;
    let provider = Arc::new(provider);

    // A self-signed certificate stands in for raw public keys here. RPK is the
    // shipping intent; what this spike measures is the key agreement, which is
    // independent of how identity is carried.
    let cert = rcgen::generate_simple_self_signed(vec!["esper".into()])?;
    let cert_der = rustls_pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls_pki_types::PrivateKeyDer::try_from(cert.key_pair.serialize_der())?;

    let mut server_crypto =
        rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_single_cert(vec![cert_der.clone()], key_der)?;
    server_crypto.alpn_protocols = vec![ALPN.to_vec()];

    let mut client_crypto = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny))
        .with_no_client_auth();
    client_crypto.alpn_protocols = vec![ALPN.to_vec()];

    let server_cfg = ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?,
    ));
    let client_cfg = ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)?,
    ));

    let server_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    let server_addr: SocketAddr = server_sock.local_addr()?;
    let server = Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(server_cfg),
        server_sock,
        Arc::new(quinn::TokioRuntime),
    )?;

    let accept = tokio::spawn(async move {
        let incoming = server.accept().await.expect("incoming");
        let conn = incoming.await.expect("accepted");
        conn.closed().await;
    });

    let mut client = Endpoint::client((Ipv4Addr::LOCALHOST, 0).into())?;
    client.set_default_client_config(client_cfg);

    let started = Instant::now();
    let conn = client.connect(server_addr, "esper")?.await?;
    let handshake = started.elapsed();

    let stats = conn.stats();
    // quinn exposes only ALPN and server name here — not the key exchange
    // group. Confirming PQ is done at the rustls layer, above.
    let alpn = conn
        .handshake_data()
        .and_then(|d| {
            d.downcast::<quinn::crypto::rustls::HandshakeData>()
                .ok()
                .map(|h| {
                    h.protocol
                        .as_deref()
                        .map(|p| String::from_utf8_lossy(p).into_owned())
                        .unwrap_or_else(|| "none".into())
                })
        })
        .unwrap_or_else(|| "unknown".into());

    let measurement = Measurement {
        handshake,
        client_to_server: stats.udp_tx.bytes,
        server_to_client: stats.udp_rx.bytes,
        alpn,
    };

    conn.close(0u32.into(), b"done");
    client.wait_idle().await;
    let _ = accept.await;
    Ok(measurement)
}

/// Accepts any certificate. This is a spike measuring key agreement; identity
/// verification is what raw public keys replace and is not what is being timed.
#[derive(Debug)]
struct AcceptAny;

impl rustls::client::danger::ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _: &rustls_pki_types::CertificateDer<'_>,
        _: &[rustls_pki_types::CertificateDer<'_>],
        _: &rustls_pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls_pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls_pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
