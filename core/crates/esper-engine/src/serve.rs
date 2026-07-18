//! The control channel to the Go control plane.
//!
//! Length-prefixed CBOR over stdin and stdout — the same framing as the wire
//! protocol, so there is one implementation of it to get right rather than two.
//!
//! **Media never crosses this channel.** It carries commands and state at
//! roughly ten messages a second. Everything touching a frame stays inside this
//! process, which is the entire reason the seam is affordable.

use std::io::{BufReader, BufWriter, Read, Write};

use esper_proto::{Hello, Role, read_frame, write_frame};

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

    // The command loop. Today there are no commands beyond the handshake, so it
    // drains until the control plane closes the pipe — which is how a clean
    // shutdown reaches us.
    loop {
        match read_frame::<Hello, _>(&mut reader) {
            Ok(_) => { /* no post-handshake messages defined yet */ }
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
