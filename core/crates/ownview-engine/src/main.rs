//! The OwnView engine.
//!
//! Owns the realtime path end to end: capture, encode, transport, decode,
//! present, and input injection. It is a separate process from the Go control
//! plane, and media never crosses between them — the socket carries commands
//! and state, roughly ten messages a second, never a frame.
//!
//! At this stage it reports its own identity and exits. Everything else is
//! built on top of that.

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
           serve      speak the control protocol on stdin/stdout\n  \
           version    print version, build time and protocol version\n  \
           help       this text"
    );
}
