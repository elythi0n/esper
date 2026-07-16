//! `ownview-engine pair` — the two sides of a pairing.
//!
//! # Why pairing exists at all
//!
//! There are no passwords in this product. A device is either paired — its
//! public key recorded in this machine's device list — or it cannot connect,
//! and the TLS handshake is where that is enforced, so a refused peer never
//! reaches session code.
//!
//! Pairing is the one moment a machine will talk to a device it does not know,
//! and it is deliberately narrow: a short-lived code, a handful of attempts,
//! and a SPAKE2 exchange bound to the connection so a relay cannot sit in the
//! middle of it.
//!
//! # The code is read aloud, and that shapes everything
//!
//! Somebody reads it from one screen and types it into another, so it is
//! short — which means it is guessable, which is why the offer expires and
//! counts attempts. The security does not come from the code's length; it
//! comes from a PAKE that reveals nothing on a wrong guess and from there
//! being only three of them.
//!
//! # Both sides print the fingerprint, and that is not decoration
//!
//! A PAKE proves both ends used the same code. It cannot prove the code went
//! to the person you meant. Reading the fingerprint aloud is what closes that
//! gap, so both sides print it in the same format and a mismatch is something
//! a person can act on.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use ownview_crypto::{Device, Offer, PairingCode};
use ownview_proto::{Hello, Role};
use ownview_transport::{Control, Endpoint, Session, pair, pairing_authority};

use crate::paths;

/// How long to wait for the other side.
///
/// Generous, because a person is reading a code aloud in the middle of it.
const WAIT: Duration = Duration::from_secs(180);

/// What `pair` was asked to do.
#[derive(Debug, Clone)]
pub enum Options {
    /// Offer a code and wait for a device to use it.
    Offer { listen: SocketAddr },
    /// Use a code somebody read out.
    Accept { addr: SocketAddr, code: String },
}

/// Parse the flags.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut listen: Option<SocketAddr> = None;
    let mut addr: Option<SocketAddr> = None;
    let mut code: Option<String> = None;
    let mut i = 0usize;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--listen" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--listen needs an address".to_owned())?;
                listen = Some(crate::net::parse_listen(raw)?);
                i += 2;
            }
            "--code" => {
                code =
                    Some(args.get(i + 1).cloned().ok_or_else(|| {
                        "--code needs the code the other side printed".to_owned()
                    })?);
                i += 2;
            }
            other if !other.starts_with('-') && addr.is_none() => {
                addr = Some(crate::net::parse_addr(other)?);
                i += 1;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    match (listen, addr, code) {
        (Some(listen), None, None) => Ok(Options::Offer { listen }),
        (None, Some(addr), Some(code)) => Ok(Options::Accept { addr, code }),
        (None, Some(_), None) => {
            Err("pairing with an address needs --code, which the other side printed".to_owned())
        }
        _ => Err("either `pair --listen <addr>` to offer a code, or \
                  `pair <addr> --code <code>` to use one"
            .to_owned()),
    }
}

/// Run whichever side was asked for.
pub fn run(options: &Options) -> Result<(), String> {
    crate::net::runtime()?.block_on(async {
        match options {
            Options::Offer { listen } => offer(*listen).await,
            Options::Accept { addr, code } => accept(*addr, code).await,
        }
    })
}

/// Offer a code and wait for one device to use it.
async fn offer(listen: SocketAddr) -> Result<(), String> {
    crate::net::install_crypto();
    let identity = paths::identity()?;
    let mut offer = Offer::new();

    // **The pairing authority accepts any key, and only for this.** It is the
    // one place in the product that does, which is why it is a separate
    // endpoint bound for the pairing and dropped afterwards rather than a mode
    // the session endpoint can be put into. A session endpoint that could be
    // switched into accepting anyone is a session endpoint that will be.
    let endpoint = Endpoint::bind(&identity, listen, pairing_authority(), pairing_authority())
        .map_err(|e| format!("binding {listen}: {e}"))?;
    let bound = endpoint
        .local_addr()
        .map_err(|e| format!("reading the bound address: {e}"))?;

    println!("🔗 pairing");
    println!();
    println!("  code       {}", offer.code().display());
    println!("  listening  {bound}");
    println!(
        "  expires    in {} seconds",
        offer.remaining(Instant::now()).map_or(0, |d| d.as_secs())
    );
    println!();
    println!("  On the other machine:");
    println!(
        "      ownview-engine pair <this-machine>:{} --code {}",
        bound.port(),
        offer.code().display()
    );
    println!();
    println!("  This device's fingerprint — read it aloud and check it matches:");
    for line in identity.fingerprint().display().lines() {
        println!("      {line}");
    }
    println!();

    let conn = tokio::time::timeout(WAIT, endpoint.accept())
        .await
        .map_err(|_| format!("nobody connected within {} seconds", WAIT.as_secs()))?
        .ok_or("the endpoint closed")?
        .map_err(|e| format!("accepting: {e}"))?;

    let mut session = Session::open(
        conn.clone(),
        Hello::new(
            Role::Host,
            identity.public_key(),
            &crate::net::this_device(),
        ),
    )
    .await
    .map_err(|(e, _)| format!("opening the session: {e}"))?;
    let mut control: Control = session
        .take_control()
        .ok_or("the session has no control stream")?;

    let peer_key = pair::host(&conn, &mut control, &identity, &mut offer, Instant::now())
        .await
        .map_err(|e| format!("pairing failed: {e}"))?;

    let name = session.peer_hello().name.clone();
    let device = record(peer_key, &name)?;
    report(&device);

    // **The host writes last, so the host must not close first.**
    //
    // The exchange ends with a confirmation the controller is still waiting to
    // read. Closing here cuts it off mid-read, and the controller reports a
    // lost connection on a pairing that in fact succeeded — both sides did the
    // right thing and one of them says it failed. `pair::host` says so in its
    // own documentation, and this code did it anyway on the first attempt.
    //
    // So the controller drives the teardown and this waits for it.
    tokio::time::timeout(Duration::from_secs(5), conn.closed())
        .await
        .ok();
    tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle())
        .await
        .ok();
    Ok(())
}

/// Use a code somebody read out.
async fn accept(addr: SocketAddr, code: &str) -> Result<(), String> {
    crate::net::install_crypto();
    let identity = paths::identity()?;
    let code = PairingCode::parse(code).map_err(|e| format!("that code cannot be used: {e}"))?;

    let endpoint = Endpoint::bind(
        &identity,
        crate::net::any_port(addr),
        pairing_authority(),
        pairing_authority(),
    )
    .map_err(|e| format!("binding a local port: {e}"))?;

    println!("🔗 pairing with {addr}");
    println!();
    let conn = tokio::time::timeout(WAIT, endpoint.connect(addr))
        .await
        .map_err(|_| format!("{addr} did not answer within {} seconds", WAIT.as_secs()))?
        .map_err(|e| format!("connecting to {addr}: {e}"))?;

    let mut session = Session::open(
        conn.clone(),
        Hello::new(
            Role::Controller,
            identity.public_key(),
            &crate::net::this_device(),
        ),
    )
    .await
    .map_err(|(e, _)| format!("opening the session: {e}"))?;
    let mut control: Control = session
        .take_control()
        .ok_or("the session has no control stream")?;

    let peer_key = pair::controller(&conn, &mut control, &identity, &code)
        .await
        .map_err(|e| format!("pairing failed: {e}"))?;

    let name = session.peer_hello().name.clone();
    let device = record(peer_key, &name)?;
    report(&device);

    // The controller read last, so it closes — which is what releases the
    // host from the wait above.
    conn.close(0u32.into(), b"paired");
    tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle())
        .await
        .ok();
    Ok(())
}

/// Record a newly paired device.
fn record(public_key: [u8; 32], name: &str) -> Result<Device, String> {
    let (mut store, dropped) = paths::devices()?;
    for note in &dropped {
        eprintln!("  · the device list needed correcting: {note}");
    }
    // A peer-supplied name, so it is shown and never interpreted. The
    // fingerprint is the identity; the name is a label for a person.
    let shown = if name.trim().is_empty() {
        "an unnamed device".to_owned()
    } else {
        name.to_owned()
    };
    let device = Device::new(public_key, shown, paths::now_secs());
    store
        .insert(device.clone())
        .map_err(|e| format!("recording the device: {e}"))?;
    paths::save_devices(&store)?;
    Ok(device)
}

/// Tell the person what happened, and what to check.
fn report(device: &Device) {
    println!("  ✓ paired with {}", device.name);
    println!();
    println!("  Their fingerprint — it must match what the other screen shows:");
    for line in device.fingerprint().display().lines() {
        println!("      {line}");
    }
    println!();
    println!("  granted   {}", crate::net::permissions(device));
    println!();
    println!(
        "  A pairing proves both sides used the same code. It cannot prove the \
         code reached the person you meant, which is what the fingerprint is for."
    );
}
