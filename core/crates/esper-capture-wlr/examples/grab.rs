//! Capture one frame and write it where something else can be asked the same.
//!
//! Written as a **PPM**, which is a three-line header and then bytes, because
//! `grim` — the wlroots screenshot tool, which is not this code — writes the
//! same format. Two files that should be identical is a comparison a shell can
//! make, and it is the same shape as checking the mDNS encoder against
//! dnspython rather than against itself.
//!
//! Usage: `grab <path.ppm>`

// Unix only, like the crate it exercises. An example still has to have a
// `main` on every target it is type-checked for, so the gate goes round the
// body rather than round the file.
#[cfg(unix)]
mod unix_only {
    use std::io::Write as _;
    use std::time::Duration;

    use esper_capture::{Capture, Capturer, PixelFormat};
    use esper_capture_wlr::WlrCapturer;

    pub fn run() -> std::process::ExitCode {
        // The last argument, because the flags before it have values of their
        // own. "The first thing that is not a flag" picked `--hold`'s number
        // and wrote a frame to a file called `6`, and the failure read as a
        // missing file rather than as a misread command line.
        let Some(path) = std::env::args()
            .next_back()
            .filter(|a| !a.starts_with("--"))
        else {
            eprintln!("usage: grab [--with-indicator] [--hold <seconds>] <path.ppm>");
            return std::process::ExitCode::FAILURE;
        };

        // Something on screen with a colour in it, when asked.
        //
        // A uniform screen makes a channel swap invisible: red and blue are
        // the same when everything is grey. The session indicator paints an
        // amber frame — more red than blue — so a comparison against another
        // tool can tell the difference. Held for the life of this process,
        // because the frame goes away with the connection.
        let _indicator = if std::env::args().any(|a| a == "--with-indicator") {
            match esper_presence_wlr::WlrPresence::open() {
                Ok(mut presence) => {
                    let watching = esper_presence::Watching {
                        peer: "the check".to_owned(),
                        controlling: false,
                        recording: false,
                        paused: false,
                    };
                    if let Err(e) = esper_presence::Presence::show(&mut presence, &watching) {
                        println!("stopped: showing the indicator: {e}");
                        return std::process::ExitCode::FAILURE;
                    }
                    Some(presence)
                }
                Err(e) => {
                    println!("stopped: opening the indicator: {e}");
                    return std::process::ExitCode::FAILURE;
                }
            }
        } else {
            None
        };

        let mut capturer = match WlrCapturer::open() {
            Ok(c) => c,
            Err(e) => {
                println!("stopped: {e}");
                return std::process::ExitCode::FAILURE;
            }
        };

        let frame = match capturer.next_frame(Duration::from_secs(5)) {
            Ok(Capture::Frame(frame)) => frame,
            Ok(other) => {
                println!("stopped: the compositor answered {other:?} rather than a frame");
                return std::process::ExitCode::FAILURE;
            }
            Err(e) => {
                println!("stopped: {e}");
                return std::process::ExitCode::FAILURE;
            }
        };

        let (width, height) = (frame.width(), frame.height());
        println!("size={width}x{height}");
        println!("format={:?}", frame.format());
        println!("backend={}", capturer.backend());

        // PPM is RGB, and the compositor's buffer is not — so the channels are
        // put in the order the file format names rather than the order they
        // arrived in. Getting this wrong swaps red and blue, which looks like
        // a colour-management problem and is not one.
        let mut out = Vec::with_capacity((width as usize) * (height as usize) * 3 + 32);
        out.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
        for y in 0..height {
            for x in 0..width {
                let Some(quad) = frame.pixel(x, y) else {
                    continue;
                };
                let (r, g, b) = match frame.format() {
                    PixelFormat::Bgra8 | PixelFormat::Bgrx8 => (quad[2], quad[1], quad[0]),
                    PixelFormat::Rgba8 | PixelFormat::Rgbx8 => (quad[0], quad[1], quad[2]),
                };
                out.extend_from_slice(&[r, g, b]);
            }
        }

        match std::fs::File::create(&path).and_then(|mut f| f.write_all(&out)) {
            Ok(()) => {
                println!("wrote={path}");
                // Held open so something else can photograph the *same*
                // screen. The indicator exists only while this connection
                // does, so a second tool run after this exits would be
                // comparing a different screen and calling the difference a
                // fault.
                if let Some(seconds) = std::env::args()
                    .skip_while(|a| a != "--hold")
                    .nth(1)
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    std::thread::sleep(std::time::Duration::from_secs(seconds));
                }
                std::process::ExitCode::SUCCESS
            }
            Err(e) => {
                println!("stopped: writing {path}: {e}");
                std::process::ExitCode::FAILURE
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
