//! The OwnView engine.
//!
//! Owns the realtime path end to end: capture, encode, transport, decode,
//! present, and input injection. It is a separate process from the Go control
//! plane, and media never crosses between them — the socket carries commands
//! and state, roughly ten messages a second, never a frame.
//!
//! At this stage it reports its own identity and exits. Everything else is
//! built on top of that.

mod backend;
mod loopback;
mod serve;

use std::process::ExitCode;

use ownview_buildinfo::{BUILD_TIME, VERSION};

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
                eprintln!("ownview-engine: {e}");
                ExitCode::FAILURE
            }
        },
        Some("loopback") => {
            let rest: Vec<String> = std::env::args().skip(2).collect();
            match loopback::parse(&rest).and_then(|o| loopback::run(&o)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("loopback: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("serve") => match serve::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("ownview-engine: {e}");
                ExitCode::FAILURE
            }
        },
        Some("help" | "--help" | "-h") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("ownview-engine: unknown command {other:?}");
            print_help();
            ExitCode::FAILURE
        }
    }
}

/// Load or create this device's identity and print its fingerprint.
///
/// The fingerprint is what a person reads aloud to confirm two machines are
/// looking at the same device, so it is printed as words rather than hex.
fn print_identity() -> Result<(), Box<dyn std::error::Error>> {
    let path = identity_path()?;
    let id = ownview_crypto::Identity::load_or_create(&path)?;

    println!("device fingerprint");
    println!();
    for line in id.fingerprint().display().lines() {
        println!("    {line}");
    }
    println!();
    println!("  key   {}", id.fingerprint().hex());
    println!("  file  {}", path.display());
    Ok(())
}

/// Where the identity key lives. `$OWNVIEW_HOME` overrides, which is what makes
/// the two-process pairing test possible on one machine.
fn identity_path() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    if let Ok(home) = std::env::var("OWNVIEW_HOME") {
        return Ok(std::path::PathBuf::from(home).join("identity.key"));
    }
    let home = std::env::var("HOME").map_err(|_| "neither OWNVIEW_HOME nor HOME is set")?;
    Ok(std::path::PathBuf::from(home)
        .join(".ownview")
        .join("identity.key"))
}

fn print_version() {
    println!("ownview-engine {VERSION}");
    println!("  built     {BUILD_TIME}");
    println!("  protocol  {}", ownview_proto::PROTOCOL_VERSION);
}

fn print_help() {
    println!(
        "ownview-engine — the OwnView realtime engine\n\
         \n\
         This binary is normally launched and supervised by `ownview`.\n\
         Running it directly is for diagnostics.\n\
         \n\
         Commands:\n  \
           identity   print this device's fingerprint\n  \
           serve      speak the control protocol on stdin/stdout\n  \
           version    print version, build time and protocol version\n  \
           help       this text"
    );
}
