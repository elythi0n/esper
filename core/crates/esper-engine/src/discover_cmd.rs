//! `discover` — who else is on this segment.
//!
//! # What the list means
//!
//! An advertisement is a hint about where to look, and nothing in it decides
//! who may connect. So the list is in two parts, and the split is the whole
//! point of the command:
//!
//! - a device whose fingerprint matches one this machine has **paired** is
//!   offered as connectable, with the address to connect to;
//! - anything else is a **pairing candidate**, which means somebody would have
//!   to read a code off its screen.
//!
//! A peer that puts a paired device's fingerprint in its own advertisement
//! gets listed as connectable and then refused by the TLS handshake a moment
//! later, having gained nothing but a place in a list.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use esper_crypto::Fingerprint;
use esper_discovery::{Advertisement, Browser, Found};

use crate::{net, paths};

/// How long to listen before printing.
const LISTEN: Duration = Duration::from_secs(3);

/// Options for `discover`.
#[derive(Debug, Clone, Copy)]
pub struct DiscoverOptions {
    pub seconds: u64,
    /// One line per device, tab separated, for a program rather than a person.
    ///
    /// # Why not JSON
    ///
    /// Nothing in this workspace parses or writes JSON, and adding a crate to
    /// print four fields would be a dependency bought for a convenience.
    /// Tab-separated is *unambiguous here* rather than merely shorter: a
    /// fingerprint is hex, an address is an address, and neither can contain
    /// a tab or a newline — so a reader splits on the first three tabs and
    /// whatever remains is the name, however a peer chose to spell it. A JSON
    /// writer of our own would need string escaping, which is the part that
    /// gets one case wrong.
    pub porcelain: bool,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        Self {
            seconds: 3,
            porcelain: false,
        }
    }
}

/// Parse `discover` flags.
pub fn parse(args: &[String]) -> Result<DiscoverOptions, String> {
    let mut o = DiscoverOptions::default();
    let mut i = 0usize;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--seconds" => {
                o.seconds = args
                    .get(i + 1)
                    .ok_or_else(|| "--seconds needs a number".to_owned())?
                    .parse()
                    .map_err(|_| "--seconds wants a number".to_owned())?;
                i += 2;
            }
            "--porcelain" => {
                o.porcelain = true;
                i += 1;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(o)
}

/// Look for other Espers and say which are already paired.
pub fn run(o: &DiscoverOptions) -> Result<(), String> {
    let identity = paths::identity()?;
    let (store, _dropped) = paths::devices()?;
    let mine = Fingerprint::of(&identity.public_key()).hex();

    let runtime = net::runtime()?;
    runtime.block_on(async {
        let mut browser = Browser::start(&crate::settings::mdns_interfaces())
            .await
            .map_err(|e| format!("looking for devices: {e}"))?;

        if !o.porcelain {
            println!("🔎 looking for Esper devices on this network");
            println!();
        }

        let how_long = if o.seconds == 0 {
            LISTEN
        } else {
            Duration::from_secs(o.seconds)
        };
        // Asked once on start; asked again part way through, because a single
        // query lost to a busy segment would otherwise mean an empty list.
        let half = how_long / 2;
        browser
            .listen(half)
            .await
            .map_err(|e| format!("listening: {e}"))?;
        let _ = browser.ask().await;
        browser
            .listen(how_long.saturating_sub(half))
            .await
            .map_err(|e| format!("listening: {e}"))?;

        let found = browser.found();
        let paired: Vec<Found> = found
            .iter()
            .filter(|f| {
                f.fingerprint.as_ref().is_some_and(|fp| {
                    store
                        .iter()
                        .any(|d| Fingerprint::of(&d.public_key).hex() == *fp)
                })
            })
            .cloned()
            .collect();
        let candidates: Vec<Found> = found
            .iter()
            // This machine's own advertisement is not a device to connect to.
            .filter(|f| f.fingerprint.as_deref() != Some(mine.as_str()))
            .filter(|f| !paired.iter().any(|p| p.is_same(f)))
            .cloned()
            .collect();

        if o.porcelain {
            // One line per device, and **the fingerprint is on every line**.
            // The human listing prints a name and an address for a paired
            // device, which is all a person needs and not enough for a
            // program: two machines may share a name, and only the
            // fingerprint says which device this is.
            for f in &paired {
                let fp = f.fingerprint.as_deref().unwrap_or("");
                let name = store
                    .iter()
                    .find(|d| Some(Fingerprint::of(&d.public_key).hex()) == f.fingerprint)
                    .map(|d| d.name.clone())
                    .unwrap_or_else(|| f.device.clone());
                println!("paired\t{fp}\t{}\t{name}", f.address);
            }
            for f in &candidates {
                let fp = f.fingerprint.as_deref().unwrap_or("");
                println!("candidate\t{fp}\t{}\t{}", f.address, f.device);
            }
            return Ok(());
        }

        if found.is_empty() {
            println!("  nothing found");
            println!();
            println!(
                "  Discovery needs the other machine to be running `{} host`",
                crate::net::me()
            );
            println!("  on the same network. A device reachable by address works without it.");
        }

        if !paired.is_empty() {
            println!("  paired — ready to connect");
            for f in &paired {
                let name = store
                    .iter()
                    .find(|d| Some(Fingerprint::of(&d.public_key).hex()) == f.fingerprint)
                    .map(|d| d.name.clone())
                    .unwrap_or_else(|| f.device.clone());
                println!("      {name}  {}", f.address);
                println!(
                    "          connect with: {} connect {}",
                    crate::net::me(),
                    f.address
                );
            }
            println!();
        }

        if !candidates.is_empty() {
            println!("  not paired — a code has to be read off its screen");
            for f in &candidates {
                let fp = f.fingerprint.as_deref().unwrap_or("no fingerprint");
                println!("      {}  {}  {fp}", f.device, f.address);
            }
            println!();
            println!(
                "  To pair, on the other machine:  {} pair --listen 7645",
                crate::net::me()
            );
            println!(
                "  then here:                      {} pair <its address> --code <code>",
                crate::net::me()
            );
            println!();
        }

        let dropped = browser.dropped();
        if dropped > 0 {
            // Said rather than hidden: packets this could not read are either
            // another implementation doing something unexpected or somebody
            // probing, and both are worth knowing about.
            println!("  ({dropped} packet(s) on the network could not be read)");
        }
        Ok(())
    })
}

/// What this device should advertise while it is hosting.
///
/// `bound` is where the endpoint actually is, and it decides the address: a
/// host bound to one address must not advertise another.
pub fn advertisement(bound: SocketAddr) -> Result<Advertisement, String> {
    let identity = paths::identity()?;
    let device = net::this_device();
    let address = match bound.ip() {
        // Bound to everything, so the address is whichever of this machine's
        // reaches the network.
        IpAddr::V4(v4) if v4.is_unspecified() => esper_discovery::socket::local_address(),
        // Bound to one address, which is the only one it can be reached at.
        IpAddr::V4(v4) => Some(v4),
        // An IPv6 endpoint has no IPv4 address to offer. The record is left
        // out rather than invented; a browser uses the packet's source anyway.
        IpAddr::V6(_) => None,
    };
    Ok(Advertisement {
        fingerprint: Fingerprint::of(&identity.public_key()).hex(),
        port: bound.port(),
        protocol: esper_proto::PROTOCOL_VERSION,
        address,
        host: device.clone(),
        device,
    })
}

/// Why this host should not be advertised, if it should not be.
///
/// **A host listening on loopback is not on the network**, and advertising it
/// would put an address in front of somebody that nothing is listening on:
/// the browser learns the address from the packet's source, which is this
/// machine's LAN address, and the port from the record — so the two together
/// name an endpoint that refuses every connection. Discovered exactly that
/// way, from a list offering `192.168.1.112:17720` for a host bound to
/// `127.0.0.1:17720`.
#[must_use]
pub fn why_not_advertise(bound: SocketAddr) -> Option<String> {
    if bound.ip().is_loopback() {
        return Some(format!("listening on {bound}, which is this machine only"));
    }
    None
}

/// An advertisement running in the background, and the way to take it back.
#[derive(Debug)]
pub struct Announcing {
    instance: String,
    /// Where it is being announced, for the line the host prints. A machine
    /// with two networks and a machine with a named interface both want to
    /// know which one this went out on, and "on this network" answers neither.
    wires: String,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Announcing {
    /// The instance name on the wire, for the log.
    #[must_use]
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// Which interfaces it went out on, for the log.
    #[must_use]
    pub fn wires(&self) -> &str {
        &self.wires
    }

    /// Say goodbye and stop.
    ///
    /// Awaited rather than dropped, for the reason a session's teardown is
    /// awaited: the goodbye is a *message*, and a process that exits before it
    /// goes out has not sent it. The wait is bounded because one lost packet
    /// on shutdown is not worth hanging for — the record's own lifetime covers
    /// that case.
    pub async fn stop(self) {
        let _ = self.stop.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), self.task).await;
    }
}

/// Start advertising this device on the LAN.
pub async fn advertise(bound: SocketAddr) -> Result<Announcing, String> {
    if let Some(why) = why_not_advertise(bound) {
        return Err(why);
    }
    let advertisement = advertisement(bound)?;
    let instance = advertisement
        .instance()
        .map(|n| n.to_string())
        .map_err(|e| format!("naming this device: {e}"))?;
    let on = crate::settings::mdns_interfaces();
    let mut responder = esper_discovery::Responder::start(advertisement, &on)
        .await
        .map_err(|e| e.to_string())?;
    let wires = responder.describe_interfaces();

    let (stop, mut stopping) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stopping => break,
                served = responder.serve_one() => {
                    if let Err(e) = served {
                        // The socket has gone, which on a machine whose
                        // network went away is expected. The host carries on
                        // serving whoever is already connected.
                        tracing::warn!("no longer announcing on this network: {e}");
                        return;
                    }
                }
            }
        }
        if let Err(e) = responder.goodbye().await {
            tracing::warn!("could not say goodbye on this network: {e}");
        }
    });
    Ok(Announcing {
        instance,
        wires,
        stop,
        task,
    })
}
