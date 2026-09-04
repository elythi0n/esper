//! Walk the RemoteDesktop conversation and report where it stops.
//!
//! A container has no person to answer a consent dialog, so this is not
//! expected to reach a granted session — and that is the point. What it
//! establishes is **where** it stops: a portal that refuses `CreateSession` is
//! a different world from one that gets as far as `Start` and waits for
//! somebody. The first would mean the client is wrong; the second means the
//! client is right and the rig has nobody in it.
//!
//! Run inside a session: `try-portal [seconds] [restore-token]`
//!
//! A token is passed to reopen a grant somebody gave once, and the one the
//! portal hands back is printed — which is the whole of what decides whether
//! this machine can honour unattended access.
// Unix only, like the crate it exercises. An example still has to have a
// `main` on every target it is type-checked for, so the gate goes round the
// body rather than round the file.
#[cfg(unix)]
mod unix_only {
    use std::time::Duration;

    use esper_dbus::Connection;
    use esper_input_portal::PortalInput;

    pub fn run() -> std::process::ExitCode {
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

        // A grant somebody gave once, to be reopened without asking again.
        let token = std::env::args().nth(2).filter(|t| !t.is_empty());

        match PortalInput::open_with(bus, Duration::from_secs(seconds), token.as_deref()) {
            Ok(input) => {
                println!("granted");
                match input.restore_token() {
                    Some(token) => println!("restore-token={token}"),
                    // Said rather than left out: the absence is the answer to
                    // "can this machine be reached when nobody is at it".
                    None => println!("restore-token=none"),
                }
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
}

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix_only::run()
}

#[cfg(not(unix))]
fn main() {}
