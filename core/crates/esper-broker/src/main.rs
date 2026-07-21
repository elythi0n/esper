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
use esper_rendezvous::{Broker, Limits};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("version" | "--version" | "-v") => {
            print_version();
            ExitCode::SUCCESS
        }
        Some("identity") => match print_identity() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("esper-broker identity: {e}");
                ExitCode::FAILURE
            }
        },
        Some("serve") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match parse(&rest).and_then(|o| serve(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("esper-broker serve: {e}");
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
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut listen: Option<SocketAddr> = None;
    let mut identity: Option<PathBuf> = None;
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
            other => return Err(format!("unexpected argument {other:?}")),
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
    })
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
        let broker = Broker::bind(&identity, o.listen, Limits::default())
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
        println!("  stopped with {} peer(s) waiting", broker.waiting());
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

fn print_help() {
    println!(
        "esper-broker — a stateless introducer you run yourself\n\
         \n\
         Two machines behind ordinary routers cannot dial each other. This\n\
         introduces them and then gets out of the way. It stores nothing.\n\
         \n\
         Commands:\n  \
           serve      introduce peers: --listen <addr|port> [--identity <path>]\n  \
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
    fn a_flag_with_nothing_after_it_is_a_mistake_worth_naming() {
        assert!(parse(&["--listen".to_owned()]).is_err());
        assert!(parse(&["--identity".to_owned()]).is_err());
        assert!(parse(&["--nonsense".to_owned()]).is_err());
    }
}
