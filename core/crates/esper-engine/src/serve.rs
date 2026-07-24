//! The control channel to the Go control plane.
//!
//! Length-prefixed CBOR over stdin and stdout — the same framing as the wire
//! protocol, so there is one implementation of it to get right rather than two.
//!
//! **Media never crosses this channel.** It carries commands and state at
//! roughly ten messages a second. Everything touching a frame stays inside this
//! process, which is the entire reason the seam is affordable.

use std::io::{BufReader, BufWriter, Read, Write};

use esper_proto::{Capability, Command, Hello, Report, Role, Status, read_frame, write_frame};

/// Run the control channel until the peer closes it.
///
/// The exchange opens with `Hello` in both directions. A version mismatch is
/// reported and the process exits rather than continuing: the two binaries ship
/// together, so a mismatch means a partial install, and every later failure
/// would be a confusing symptom of this one.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());

    handshake(&mut reader, &mut writer)?;

    // The command loop. It ends when the control plane closes the pipe, which
    // is how a clean shutdown reaches us.
    loop {
        match read_frame::<Command, _>(&mut reader) {
            Ok(command) => {
                let report = answer(&command);
                write_frame(&mut writer, &report)?;
                writer.flush()?;
            }
            Err(e) => {
                // A truncated read is the control plane closing the pipe, which
                // is the normal way this process ends.
                if matches!(e, esper_proto::FrameError::Truncated { got: 0, .. }) {
                    return Ok(());
                }
                return Err(Box::new(e));
            }
        }
    }
}

/// Answer one command.
///
/// Every failure is a [`Report::Failed`] rather than an error out of the loop:
/// a control plane that asked something this machine cannot do should hear
/// why, not watch its engine exit.
fn answer(command: &Command) -> Report {
    match command {
        Command::Status => Report::Status(status()),
    }
}

/// What this machine can actually do, established by trying it.
///
/// **Probed, not declared.** An encoder that is compiled in and cannot start,
/// a capture backend that is present and refuses the display — those are what
/// a person runs a diagnostic to find, and a report assembled from build flags
/// would say everything is fine.
fn status() -> Status {
    Status {
        engine: esper_buildinfo::VERSION.to_owned(),
        capture: capture(),
        encoder: encoder(),
        input: input(),
        overlays: crate::settings::overlays()
            .iter()
            .map(|o| o.name.clone())
            .collect(),
        mdns: crate::settings::flag("discovery.mdns", true),
        broker: crate::settings::broker()
            .ok()
            .flatten()
            .map(|via| via.addr.to_string()),
        paired: crate::paths::devices()
            .map(|(store, _)| u32::try_from(store.iter().count()).unwrap_or(u32::MAX))
            .unwrap_or(0),
    }
}

/// Open a capturer and say what it turned out to be.
fn capture() -> Capability {
    match crate::backend::open(crate::backend::Choice::Auto, None, (320, 180)) {
        Ok(selected) => {
            let name = selected.capturer.backend();
            // A synthetic screen is what this machine falls back to when there
            // is no real one. It is a working capturer and it is not a
            // screen, and a diagnostic that called it available would be the
            // reason somebody spent an afternoon wondering why their session
            // showed a test pattern.
            if name == "synthetic" {
                Capability::missing(format!("no real screen: {}", selected.attempts.join("; ")))
            } else {
                Capability::working(name)
            }
        }
        Err(e) => Capability::missing(e.to_string()),
    }
}

/// Build an encoder and encode one frame with it.
fn encoder() -> Capability {
    use esper_encode::encoder::{EncoderConfig, VideoEncoder as _};

    let config = EncoderConfig::for_size(320, 180);
    let mut encoder = match esper_encode::openh264::OpenH264::new(&config) {
        Ok(e) => e,
        Err(e) => return Capability::missing(e.to_string()),
    };
    // A blank frame, which is the cheapest one there is and still exercises
    // everything between here and the bitstream.
    let picture = match esper_color::I420::new(320, 180) {
        Ok(p) => p,
        Err(e) => return Capability::missing(e.to_string()),
    };
    match encoder.encode(&picture, true) {
        Ok(encoded) => Capability::working(format!(
            "openh264, software, {} assembly, {} bytes for one keyframe",
            if esper_encode::has_assembly() {
                "with"
            } else {
                "without"
            },
            encoded.len()
        )),
        Err(e) => Capability::missing(format!("openh264 starts and cannot encode: {e}")),
    }
}

/// Open an input sink and say whether it can actually inject.
fn input() -> Capability {
    let chosen = crate::injector::open();
    if chosen.real {
        Capability::working(chosen.attempts.join("; "))
    } else {
        Capability::missing(chosen.attempts.join("; "))
    }
}

/// Exchange `Hello` and validate the peer's.
fn handshake<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
) -> Result<(), Box<dyn std::error::Error>> {
    let peer: Hello = read_frame(reader)?;

    if !peer.version_matches() {
        return Err(format!(
            "protocol version mismatch: engine speaks {}, control plane speaks {} — \
             the two ship together, so one is from a different install",
            esper_proto::PROTOCOL_VERSION,
            peer.proto
        )
        .into());
    }
    if !peer.name_is_acceptable() {
        return Err("control plane sent an unacceptable name".into());
    }

    // The engine identifies as the host role: it is the side that captures and
    // is controlled. The device key is zero until identity lands in Phase 1.
    let mut ours = Hello::new(Role::Host, [0u8; 32], "esper-engine");
    ours.engine = esper_buildinfo::VERSION.to_owned();
    write_frame(writer, &ours)?;
    writer.flush()?;
    Ok(())
}
