//! Inject a key and a pointer move into a wlroots compositor, and say so.
//!
//! This is the end of the loop. Everything else about the Wayland work has been
//! a measurement of what a compositor *offers*; this asks it to do something
//! and leaves evidence that only the compositor could have left.
//!
//! `scripts/check-wlr-input.sh` runs it in a sway session whose config binds
//! the key to a command that writes a file. The file existing afterwards is the
//! proof: this process cannot create it, sway can, and sway only would if the
//! key reached it.
//!
//! Usage: `poke [<hid-usage>]` — default 0x42, which is F9.

use esper_input::{InputSink, Point};
use esper_input_wlr::WlrInput;
use esper_proto::KeyEvent;

fn main() -> std::process::ExitCode {
    let usage = std::env::args()
        .nth(1)
        .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x42);

    let mut input = match WlrInput::open_with_size(1280, 720) {
        Ok(input) => input,
        Err(e) => {
            println!("stopped: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("seat-keymap={}", input.uses_the_seat_keymap());

    // A press and a release. A press alone leaves the compositor believing the
    // key is still down, which is a stuck modifier on a real desktop.
    if let Ok(raw) = std::env::var("ESPER_POKE_RAW") {
        for code in raw.split(',').filter_map(|c| c.trim().parse::<u32>().ok()) {
            for pressed in [true, false] {
                if let Err(e) = input.raw_key(code, pressed) {
                    println!("stopped: raw key {code}: {e}");
                    return std::process::ExitCode::FAILURE;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
    } else {
        for pressed in [true, false] {
            if let Err(e) = input.key(KeyEvent { usage, pressed }) {
                println!("stopped: key: {e}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }

    if let Err(e) = input.pointer(Point { x: 640, y: 360 }) {
        println!("stopped: pointer: {e}");
        return std::process::ExitCode::FAILURE;
    }

    // Anything still held is let go of before leaving, which is what a real
    // session does when it ends and is the one thing worth exercising here
    // even though nothing is held.
    if let Err(e) = input.release_all() {
        println!("stopped: release: {e}");
        return std::process::ExitCode::FAILURE;
    }

    println!("poked");
    // Held open briefly so that something outside can ask the compositor what
    // it sees while the virtual devices still exist — they vanish with the
    // connection, and a device that is gone cannot be inspected.
    let hold: u64 = std::env::var("ESPER_POKE_HOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if hold > 0 {
        std::thread::sleep(std::time::Duration::from_secs(hold));
    }
    std::process::ExitCode::SUCCESS
}
