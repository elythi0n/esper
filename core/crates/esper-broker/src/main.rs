//! The Esper broker.
//!
//! A stateless introducer. It holds no keys but its own, stores nothing across
//! a restart, and cannot read what it introduces: every session is end-to-end
//! encrypted between device keys, and a broker that substituted an address of
//! its own would find the QUIC handshake refusing it.
//!
//! **There is no default broker address anywhere in Esper.** If a user has not
//! configured one, the broker tiers simply do not exist for them. This binary
//! is the entire server-side story, and it is run by the user, not by us.
//!
//! What it inevitably learns is worth stating where the operator will read it:
//! **which device keys asked to meet which, from which addresses, and when.**
//! That is unavoidable for any introducer, and is exactly the argument for the
//! broker being yours.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Once;

use esper_buildinfo::{BUILD_TIME, VERSION};
use esper_crypto::Identity;
use esper_rendezvous::{Broker, Limits, RelayLimits};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {
            print_version();
            ExitCode::SUCCESS
        }
        Some("version" | "--version" | "-v") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            if rest.iter().any(|a| a == "--help" || a == "-h") {
                println!("{}", version_usage());
                return ExitCode::SUCCESS;
            }
            let mut as_json = false;
            for arg in &rest {
                match arg.as_str() {
                    "--json" => as_json = true,
                    unknown => {
                        eprintln!(
                            "esper-broker version: unknown option {unknown:?}\n\n{}",
                            version_usage()
                        );
                        return ExitCode::FAILURE;
                    }
                }
            }
            if as_json {
                print_version_as_json();
            } else {
                print_version();
            }
            ExitCode::SUCCESS
        }
        Some("identity") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            // `identity` creates the key if there is not one yet, so a person
            // asking what the command does was getting a key file written into
            // their home as the answer. Asking is asking.
            if rest.iter().any(|a| a == "--help" || a == "-h") {
                println!("{}", identity_usage());
                return ExitCode::SUCCESS;
            }
            let mut as_json = false;
            for arg in &rest {
                match arg.as_str() {
                    "--json" => as_json = true,
                    unknown => {
                        eprintln!(
                            "esper-broker identity: unknown option {unknown:?}\n\n{}",
                            identity_usage()
                        );
                        return ExitCode::FAILURE;
                    }
                }
            }
            let printed = if as_json {
                print_identity_as_json()
            } else {
                print_identity()
            };
            match printed {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("esper-broker identity: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("serve") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            // **Asking about a command is asking, not attempting.** This
            // reached the parser, which refused `--help` as an option it does
            // not have — the same answer the other two binaries used to give,
            // and the same reason it is wrong: a person who asks gets an answer
            // on stdout and a nought, not a refusal on stderr.
            if rest.iter().any(|a| a == "--help" || a == "-h") {
                println!("{}", serve_usage());
                return ExitCode::SUCCESS;
            }
            match parse(&rest).and_then(|o| serve(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("esper-broker serve: {e}\n\n{}", serve_usage());
                    ExitCode::FAILURE
                }
            }
        }
        Some("help" | "--help" | "-h") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("esper-broker: unknown command {other:?}");
            print_help();
            ExitCode::FAILURE
        }
    }
}

/// What `serve` was asked to do.
#[derive(Debug)]
struct Options {
    listen: SocketAddr,
    identity: PathBuf,
    /// What it will forward for, if anything.
    ///
    /// **Off unless asked for.** Introducing two peers costs a few packets;
    /// forwarding for them costs the operator's bandwidth for as long as the
    /// session lasts, and nobody should discover they are paying for that
    /// because a default said so.
    relay: Option<RelayLimits>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut listen: Option<SocketAddr> = None;
    let mut identity: Option<PathBuf> = None;
    let mut relay = false;
    let mut bitrate: Option<u64> = None;
    let mut pairs: Option<usize> = None;
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--listen" => {
                let raw = rest
                    .next()
                    .ok_or_else(|| "--listen needs an address".to_owned())?;
                listen = Some(address(raw)?);
            }
            "--identity" => {
                let raw = rest
                    .next()
                    .ok_or_else(|| "--identity needs a path".to_owned())?;
                identity = Some(PathBuf::from(raw));
            }
            "--relay" => relay = true,
            "--relay-bitrate" => {
                let raw = rest.next().ok_or_else(|| {
                    "--relay-bitrate needs a number of bits per second".to_owned()
                })?;
                bitrate = Some(bits_per_second(raw)?);
            }
            "--relay-pairs" => {
                let raw = rest
                    .next()
                    .ok_or_else(|| "--relay-pairs needs a number".to_owned())?;
                pairs = Some(
                    raw.parse()
                        .map_err(|_| format!("{raw:?} is not a number of pairs"))?,
                );
            }
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(Options {
        // No default port hidden in a constant somewhere: an operator saying
        // where to listen is the whole configuration of this program.
        listen: listen.ok_or_else(|| {
            "serve needs an address to listen on: esper-broker serve --listen 0.0.0.0:7443"
                .to_owned()
        })?,
        identity: match identity {
            Some(path) => path,
            None => default_identity()?,
        },
        relay: match (relay, bitrate, pairs) {
            (false, None, None) => None,
            (false, _, _) => {
                return Err(
                    "--relay-bitrate and --relay-pairs are settings for --relay, \
                     which was not given"
                        .to_owned(),
                );
            }
            (true, bitrate, pairs) => Some(RelayLimits {
                ceiling: bitrate.unwrap_or(RelayLimits::default().ceiling),
                pairs: pairs.unwrap_or(RelayLimits::default().pairs),
                ..RelayLimits::default()
            }),
        },
    })
}

/// Read a bitrate the way an operator writes one.
///
/// Bits per second, because that is what a line is sold in — and converted to
/// bytes here, because that is what a socket counts. `2M` and `2000000` are
/// the same thing; getting that wrong by a factor of eight is the classic way
/// to set a ceiling nobody meant.
fn bits_per_second(raw: &str) -> Result<u64, String> {
    let text = raw.trim().to_ascii_lowercase();
    let (digits, scale) = match text.strip_suffix('m') {
        Some(d) => (d, 1_000_000u64),
        None => match text.strip_suffix('k') {
            Some(d) => (d, 1_000u64),
            None => (text.as_str(), 1u64),
        },
    };
    let bits: u64 = digits
        .parse()
        .map_err(|_| format!("{raw:?} is not a bitrate — try 2M, 500k or a number of bits"))?;
    let bits = bits
        .checked_mul(scale)
        .ok_or_else(|| format!("{raw:?} is more bits than there are"))?;
    Ok(bits / 8)
}

/// Read an address, accepting a bare port the way a server operator writes it.
fn address(raw: &str) -> Result<SocketAddr, String> {
    if let Ok(addr) = raw.parse::<SocketAddr>() {
        return Ok(addr);
    }
    // `:7443` and `7443` both mean "every address, this port", which is what
    // a server is usually asked to do.
    let port = raw.trim_start_matches(':');
    let port: u16 = port
        .parse()
        .map_err(|_| format!("{raw:?} is not an address or a port"))?;
    Ok(SocketAddr::from(([0, 0, 0, 0], port)))
}

/// Where the broker's own key lives.
fn default_identity() -> Result<PathBuf, String> {
    if let Ok(dir) = std::env::var("ESPER_HOME") {
        return Ok(PathBuf::from(dir).join("broker.key"));
    }
    let home = std::env::var("HOME")
        .map_err(|_| "neither ESPER_HOME nor HOME is set, so pass --identity <path>".to_owned())?;
    Ok(PathBuf::from(home).join(".esper").join("broker.key"))
}

/// Load or create the broker's key.
fn identity_at(path: &std::path::Path) -> Result<Identity, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    Identity::load_or_create(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn serve(o: &Options) -> Result<(), String> {
    install_crypto();
    let identity = identity_at(&o.identity)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("starting a runtime: {e}"))?;

    runtime.block_on(async {
        let broker = Broker::bind(
            &identity,
            o.listen,
            Limits {
                relay: o.relay,
                ..Limits::default()
            },
        )
        .map_err(|e| format!("binding {}: {e}", o.listen))?;
        let bound = broker
            .local_addr()
            .map_err(|e| format!("reading the bound address: {e}"))?;

        println!("🔗 esper broker");
        println!();
        println!("  listening  {bound}");
        println!("  identity   {}", o.identity.display());
        println!("  key        {}", hex(&identity.public_key()));
        println!();
        println!("  Peers pin that key. Give it to them with the address:");
        println!(
            "      --via <this-machine>:{} --broker-key {}",
            bound.port(),
            hex(&identity.public_key())
        );
        println!();
        println!("  The same key, as words, for reading aloud:");
        for line in identity.fingerprint().display().lines() {
            println!("      {line}");
        }
        println!();
        match o.relay {
            Some(limits) => {
                println!(
                    "  relaying   up to {} pair(s) at {} Mbit/s each way",
                    limits.pairs,
                    limits.ceiling * 8 / 1_000_000
                );
                println!("             This spends your bandwidth. It is off unless asked for.");
            }
            None => println!("  relaying   off — this broker introduces and does not forward"),
        }
        println!();
        println!("  This broker keeps nothing on disk and nothing across a restart.");
        println!("  It learns which device keys asked to meet which, from which");
        println!("  addresses, and when. It cannot read what it introduces.");
        println!();

        // A stop signal, so a `SIGTERM` from a service manager is a clean
        // shutdown rather than a process vanishing mid-introduction. A broker
        // is the one thing here that runs as a service, so this is the one
        // place it matters most.
        let (stop, stopping) = tokio::sync::watch::channel(false);
        let signals = tokio::spawn(async move {
            wait_for_stop().await;
            println!("  · stopping");
            let _ = stop.send(true);
        });

        broker.run(stopping).await;
        signals.abort();
        println!(
            "  stopped with {} peer(s) waiting and {} pair(s) being forwarded for",
            broker.waiting(),
            broker.relaying()
        );
        Ok(())
    })
}

/// Install the crypto provider this process will use.
fn install_crypto() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

/// Print the broker's own fingerprint, which is what peers pin.
fn print_identity() -> Result<(), String> {
    let path = default_identity()?;
    let identity = identity_at(&path)?;
    println!("broker identity");
    println!();
    for line in identity.fingerprint().display().lines() {
        println!("    {line}");
    }
    println!();
    // **The whole key, because that is what a peer pins.** The fingerprint
    // above is for a person to read aloud; pinning against sixty-four bits of
    // a hash would be a security decision made on a birthday bound.
    println!("  key   {}", hex(&identity.public_key()));
    println!("  file  {}", path.display());
    Ok(())
}

/// The same broker, as one object.
///
/// The field names `esper devices --json` and `esper identity --json` use for
/// the same facts. One vocabulary across the three binaries: `public_key` is
/// always the whole key, and `fingerprint` is always the short form derived
/// from it.
fn print_identity_as_json() -> Result<(), String> {
    let path = default_identity()?;
    let identity = identity_at(&path)?;
    let out = serde_json::json!({
        "public_key": hex(&identity.public_key()),
        "fingerprint": {
            "hex": identity.fingerprint().hex(),
            "words": identity.fingerprint().words(),
        },
        "file": path.display().to_string(),
    });
    println!("{out}");
    Ok(())
}

/// Wait for the operator to ask this process to stop.
///
/// `SIGTERM` as well as an interrupt, because a broker is run by a service
/// manager and that is how a service manager asks. Off Unix there is no
/// `SIGTERM` to wait for and the console interrupt is the whole story.
async fn wait_for_stop() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  · no SIGTERM handler ({e}); stopping will be abrupt");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// A key as the hex a peer pastes.
fn hex(key: &[u8; 32]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

fn print_version() {
    println!("esper-broker {VERSION}");
    println!("  built     {BUILD_TIME}");
    println!("  protocol  {}", esper_proto::PROTOCOL_VERSION);
    println!("  rendezvous {}", esper_rendezvous::RENDEZVOUS_VERSION);
}

/// The same facts, in the shape the other two binaries answer with.
///
/// **One object with the same keys**, so somebody asking a deployment what it
/// is running gets three answers they can compare rather than three formats.
/// The broker speaks a rendezvous version as well as a protocol one, and both
/// are here: a peer and a broker that disagree about either cannot be
/// introduced.
fn print_version_as_json() {
    let out = serde_json::json!({
        "version": VERSION,
        "buildTime": BUILD_TIME,
        "protocol": esper_proto::PROTOCOL_VERSION,
        "rendezvous": esper_rendezvous::RENDEZVOUS_VERSION,
    });
    println!("{out}");
}

/// What `serve` takes.
///
/// Its own text rather than the general help, for the same reason every verb in
/// the engine has one: the list of commands answers "what is there", and
/// somebody who typed `serve` and got an option wrong is asking "what does
/// *this* take". Printed on request and after a refusal, from one copy, so the
/// two cannot disagree.
fn serve_usage() -> String {
    "usage: esper-broker serve --listen <addr|port> [options]\n\
     \n\
     Introduce two machines that cannot dial each other, and get out of the\n\
     way. Nothing is stored: no peer list, no session, no log of who met whom.\n\
     \n\
     \x20 --listen <addr|port>   where to answer. A bare port means every\n\
     \x20                        address on this machine\n\
     \x20 --identity <path>      the key peers pin, created on first use.\n\
     \x20                        Default: beside this machine's Esper state\n\
     \x20 --relay                carry traffic for peers that cannot reach\n\
     \x20                        each other at all, which costs this machine\n\
     \x20                        bandwidth\n\
     \x20 --relay-bitrate 2M     the most one relayed pair may use\n\
     \x20 --relay-pairs 64       how many pairs may be relayed at once\n\
     \n\
     for example: esper-broker serve --listen :7443"
        .to_owned()
}

fn identity_usage() -> String {
    "usage: esper-broker identity [--json]\n\
     \n\
     Print this broker's key, which peers pin with --broker-key, and the same\n\
     key as words for reading aloud. Creates the key if there is not one yet.\n\
     \n\
     The key lives beside this machine's Esper state. `serve --identity <path>`\n\
     is what puts it somewhere else, and this prints whichever one that command\n\
     would use.\n\
     \n\
     options:\n  \
     \x20 --json   one object, for a script rather than a person"
        .to_owned()
}

fn version_usage() -> String {
    "usage: esper-broker version [--json]\n\
     \n\
     Print the version, the build time and the protocol versions this broker\n\
     speaks. Two peers and a broker have to agree on the protocol; the numbers\n\
     here are what this one will accept.\n\
     \n\
     options:\n  \
     \x20 --json   one object, for a script rather than a person"
        .to_owned()
}

fn print_help() {
    println!(
        "esper-broker — a stateless introducer you run yourself\n\
         \n\
         Two machines behind ordinary routers cannot dial each other. This\n\
         introduces them and then gets out of the way. It stores nothing.\n\
         \n\
         Commands:\n  \
           serve      introduce peers: --listen <addr|port> [--identity <path>]\n  \
                      [--relay] [--relay-bitrate 2M] [--relay-pairs 64]\n  \
           identity   print this broker's fingerprint, which peers pin\n  \
           version    print version, build time and protocol versions\n  \
           help       this text"
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn an_address_or_a_bare_port_are_both_accepted() {
        // An operator writes `:7443` or `7443`; both mean every address.
        assert_eq!(
            address("0.0.0.0:7443").unwrap(),
            SocketAddr::from(([0, 0, 0, 0], 7443))
        );
        assert_eq!(
            address(":7443").unwrap(),
            SocketAddr::from(([0, 0, 0, 0], 7443))
        );
        assert_eq!(
            address("7443").unwrap(),
            SocketAddr::from(([0, 0, 0, 0], 7443))
        );
        assert_eq!(
            address("127.0.0.1:7443").unwrap(),
            SocketAddr::from(([127, 0, 0, 1], 7443))
        );
        assert!(address("not-an-address").is_err());
        assert!(address("").is_err());
    }

    #[test]
    fn serve_refuses_to_guess_an_address() {
        // **No default port.** An operator saying where to listen is the whole
        // configuration of this program, and a broker that picked a port would
        // be a broker somebody did not know they were running.
        let e = parse(&[]).unwrap_err();
        assert!(e.contains("--listen"), "{e}");
    }

    #[test]
    fn relaying_is_off_unless_it_is_asked_for() {
        // Forwarding spends the operator's bandwidth for as long as a session
        // lasts. Nobody should discover they are paying for that because a
        // default said so.
        let o = parse(&["--listen".to_owned(), "127.0.0.1:1".to_owned()]).unwrap();
        assert!(o.relay.is_none());

        let o = parse(&[
            "--listen".to_owned(),
            "127.0.0.1:1".to_owned(),
            "--relay".to_owned(),
        ])
        .unwrap();
        assert!(o.relay.is_some());

        // A ceiling with no relay is a mistake worth naming rather than
        // silently ignoring: somebody who wrote it meant to forward.
        let e = parse(&[
            "--listen".to_owned(),
            "127.0.0.1:1".to_owned(),
            "--relay-bitrate".to_owned(),
            "2M".to_owned(),
        ])
        .unwrap_err();
        assert!(e.contains("--relay"), "{e}");
    }

    #[test]
    fn a_bitrate_is_read_in_the_units_a_line_is_sold_in() {
        // Bits per second in, bytes per second out. Getting that wrong by a
        // factor of eight is the classic way to set a ceiling nobody meant.
        assert_eq!(bits_per_second("8").unwrap(), 1);
        assert_eq!(bits_per_second("2M").unwrap(), 250_000);
        assert_eq!(bits_per_second("2m").unwrap(), 250_000);
        assert_eq!(bits_per_second("500k").unwrap(), 62_500);
        assert_eq!(bits_per_second(" 1000 ").unwrap(), 125);
        assert!(bits_per_second("fast").is_err());
        assert!(bits_per_second("").is_err());
        // And a number that would overflow is refused rather than wrapped.
        assert!(bits_per_second("99999999999999999999M").is_err());
    }

    #[test]
    fn a_flag_with_nothing_after_it_is_a_mistake_worth_naming() {
        assert!(parse(&["--listen".to_owned()]).is_err());
        assert!(parse(&["--identity".to_owned()]).is_err());
        assert!(parse(&["--relay-bitrate".to_owned()]).is_err());
        assert!(parse(&["--relay-pairs".to_owned()]).is_err());
        assert!(parse(&["--nonsense".to_owned()]).is_err());
    }
}
