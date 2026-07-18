//! The Esper broker.
//!
//! A stateless introducer, and a blind relay when hole-punching fails. It holds
//! no keys, stores nothing across a restart, and cannot read what it forwards.
//!
//! **There is no default broker address anywhere in Esper.** If a user has not
//! configured one, the broker tiers simply do not exist for them. This binary is
//! the entire server-side story, and it is run by the user, not by us.
//!
//! At this stage it reports its own identity and exits.

use std::process::ExitCode;

use esper_buildinfo::{BUILD_TIME, VERSION};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("version" | "--version" | "-v") => {
            print_version();
            ExitCode::SUCCESS
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

fn print_version() {
    println!("esper-broker {VERSION}");
    println!("  built     {BUILD_TIME}");
    println!("  protocol  {}", esper_proto::PROTOCOL_VERSION);
}

fn print_help() {
    println!(
        "esper-broker — a stateless introducer and blind relay\n\
         \n\
         Run this yourself to give NAT'd peers a way to find each other.\n\
         It never sees session content.\n\
         \n\
         Commands:\n  \
           version    print version, build time and protocol version\n  \
           help       this text"
    );
}
