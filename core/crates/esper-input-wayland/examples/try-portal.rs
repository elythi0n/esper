//! Walk the RemoteDesktop conversation and report where it stops.
//!
//! A container has no person to answer a consent dialog, so this is not
//! expected to reach a granted session — and that is the point. What it
//! establishes is **where** it stops: a portal that refuses `CreateSession` is
//! a different world from one that gets as far as `Start` and waits for
//! somebody. The first would mean the client is wrong; the second means the
//! client is right and the rig has nobody in it.
//!
//! Run inside a session: `try-portal`

use std::time::Duration;

use esper_dbus::Connection;
use esper_input_wayland::PortalInput;

fn main() -> std::process::ExitCode {
    // How long to wait at the consent prompt. A person gets two minutes by
    // default; a check that knows there is nobody there passes a second, so
    // that "nobody answered" arrives promptly rather than being waited out.
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);

    let bus = match Connection::session() {
        Ok(bus) => bus,
        Err(e) => {
            println!("stopped: no session bus: {e}");
            return std::process::ExitCode::SUCCESS;
        }
    };

    match PortalInput::open_on(bus, Duration::from_secs(seconds)) {
        Ok(input) => {
            println!("granted");
            println!(
                "absolute-pointer={}",
                if input.can_point_absolutely() {
                    "yes"
                } else {
                    "no"
                }
            );
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            // Printed on stdout rather than stderr: this is the measurement,
            // not a complaint, and the check that runs it reads it.
            println!("stopped: {e}");
            std::process::ExitCode::SUCCESS
        }
    }
}
