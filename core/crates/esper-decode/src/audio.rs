//! The sandboxed audio decoder: both halves.
//!
//! # Why sound gets a jail of its own
//!
//! For the same reason the picture has one — a peer's bytes reaching a C
//! library — and for one more: **a malformed packet of sound must not take the
//! picture down.** One worker decoding both would mean an Opus failure that
//! kills the process also ends the video, and a session that goes black
//! because somebody sent bad audio is a worse failure than the one being
//! prevented.
//!
//! It is the same jail, the same handshake and the same framing as the video
//! worker. What differs is one message in each direction and what gets built
//! before the sandbox goes on.
//!
//! # The order, which is the whole security property
//!
//! 1. Build the decoder, which allocates and needs syscalls the sandbox
//!    forbids.
//! 2. **Apply the sandbox.**
//! 3. Announce readiness, carrying the report, so the parent can refuse to
//!    proceed if the jail is not really on.
//! 4. Only now read the first byte a peer chose.

use std::io::{BufReader, BufWriter, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use esper_audio::{AudioError, Decode, Format, Frame};
use esper_proto::AudioConfig;

use crate::protocol::{self, FromDecoder, ToDecoder};
use crate::sandbox::{Sandbox, SandboxReport};
use crate::worker::{WORKER_ENV, WorkerError};

/// The argument the audio worker is invoked with.
///
/// A different argument from the picture's, because they are different
/// programs with different parsers — and an operator reading `ps` should be
/// able to tell which is which.
pub const WORKER_ARG: &str = "decode-audio-worker";

/// Run the audio decoder loop until the parent closes the pipe.
///
/// `input` and `output` rather than stdin and stdout directly, so the loop is
/// testable over a pipe pair without a process.
///
/// # Errors
///
/// If the sandbox cannot be applied, the decoder cannot be built, or the pipe
/// to the parent fails.
pub fn run<R: Read, W: Write>(
    config: &AudioConfig,
    sandbox: &Sandbox,
    input: &mut R,
    output: &mut W,
) -> Result<(), crate::child::ChildError> {
    // 1. Everything that needs a syscall the jail forbids.
    let mut decoder = esper_audio::opus::Decoder::new(config).map_err(|e| {
        crate::decoder::DecodeError::Backend {
            backend: "opus",
            detail: e.to_string(),
        }
    })?;

    // 2. The jail.
    let report = sandbox.apply()?;

    // 3. Say so, carrying what actually took effect, and 4. serve.
    serve(&mut decoder, report, input, output)
}

/// The loop that runs once the jail is on: announce, then answer packets.
///
/// **Split from [`run`] so it can be tested without a sandbox**, and *only*
/// that way round: this takes the report as an argument rather than applying
/// anything, so there is no way to reach the loop with the jail skipped — the
/// only thing that produces a report is [`Sandbox::apply`]. A test builds a
/// decoder itself and calls this; the product goes through `run`, and the
/// ordering that makes the sandbox meaningful lives there.
///
/// # Errors
///
/// If the pipe to the parent fails.
pub fn serve<D: esper_audio::Decode, R: Read, W: Write>(
    decoder: &mut D,
    report: SandboxReport,
    input: &mut R,
    output: &mut W,
) -> Result<(), crate::child::ChildError> {
    protocol::write_reply(output, &FromDecoder::Ready { report })?;
    output
        .flush()
        .map_err(|e| protocol::ProtocolError::Io(e.to_string()))?;

    // And only now, a peer's bytes.
    let mut at = 0u64;
    let step = decoder.format().samples_per_frame() as u64;
    loop {
        let body = match protocol::read_body(input) {
            Ok(b) => b,
            // The parent closed the pipe, which is how a session ends.
            Err(protocol::ProtocolError::Closed) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        match protocol::parse_request(&body)? {
            ToDecoder::Shutdown => return Ok(()),
            ToDecoder::Decode(packet) => {
                // **An empty packet means "this one was lost".** That is
                // libopus's own convention — it asks the decoder to continue
                // the previous frame's spectrum rather than to produce
                // silence — so it needs no message of its own, and a receiver
                // that wanted silence could send a frame of zeros instead.
                let decoded = if packet.is_empty() {
                    decoder.conceal(at)
                } else {
                    decoder.decode(&packet, at)
                };
                let reply = match decoded {
                    Ok(frame) => FromDecoder::Samples {
                        values: frame.samples,
                    },
                    // One packet, not the session: a packet a decoder refuses
                    // is the ordinary outcome on a lossy link, and the next
                    // one usually decodes.
                    Err(e) => FromDecoder::Failed(e.to_string()),
                };
                protocol::write_reply(output, &reply)?;
                output
                    .flush()
                    .map_err(|e| protocol::ProtocolError::Io(e.to_string()))?;
                at = at.saturating_add(step);
            }
        }
    }
}

/// The same, on this process's own stdin and stdout.
///
/// # Errors
///
/// As [`run`].
pub fn run_on_stdio(config: &AudioConfig) -> Result<(), crate::child::ChildError> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    run(config, &Sandbox::default(), &mut input, &mut output)
}

/// A decoder in a jail, from the parent's side.
pub struct SandboxedAudio {
    child: Child,
    to_child: BufWriter<ChildStdin>,
    from_child: BufReader<ChildStdout>,
    report: SandboxReport,
    format: Format,
    frames: u64,
    filled: u64,
    /// Set once the conversation has failed, so a caller that keeps going is
    /// told the same thing rather than restarting a dead one.
    poisoned: Option<String>,
}

impl std::fmt::Debug for SandboxedAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxedAudio")
            .field("pid", &self.child.id())
            .field("sandbox", &self.report.describe())
            .field("frames", &self.frames)
            .finish_non_exhaustive()
    }
}

impl SandboxedAudio {
    /// Spawn a worker and wait for it to report the sandbox is on.
    ///
    /// `worker` is the program to run — normally this engine's own executable.
    ///
    /// # Errors
    ///
    /// If the worker cannot be started, does not report itself ready, or
    /// reports a sandbox that is not in force.
    pub fn spawn(worker: &std::path::Path, config: &AudioConfig) -> Result<Self, WorkerError> {
        // **The shape goes on the command line**, because the child has to
        // build its decoder before the jail goes on and the jail forbids
        // reading anything else. Three integers, no secrets: a person reading
        // `ps` learns the sample rate.
        let mut child = Command::new(worker)
            .arg(WORKER_ARG)
            .arg(config.sample_rate_hz.to_string())
            .arg(config.channels.to_string())
            .arg(config.frame_ms.to_string())
            .env(WORKER_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Inherited on purpose: the child's diagnostics are the operator's
            // only view of a jail that refused something.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| WorkerError::Spawn(e.to_string()))?;

        let to_child = child.stdin.take().ok_or(WorkerError::NoPipe("stdin"))?;
        let from_child = child.stdout.take().ok_or(WorkerError::NoPipe("stdout"))?;
        let mut me = Self {
            child,
            to_child: BufWriter::new(to_child),
            from_child: BufReader::new(from_child),
            report: SandboxReport::default(),
            format: Format::of(config),
            frames: 0,
            filled: 0,
            poisoned: None,
        };

        // Nothing is sent before the readiness report arrives.
        match me.read_reply() {
            Ok(FromDecoder::Ready { report }) => {
                if !report.is_enforcing() {
                    let detail = report.describe();
                    let _ = me.child.kill();
                    return Err(WorkerError::NotSandboxed { detail });
                }
                me.report = report;
            }
            Ok(other) => {
                let _ = me.child.kill();
                return Err(WorkerError::NotReady(format!("{other:?}")));
            }
            Err(e) => {
                let _ = me.child.kill();
                return Err(WorkerError::NotReady(e.to_string()));
            }
        }
        Ok(me)
    }

    /// Spawn a worker using this process's own executable.
    ///
    /// # Errors
    ///
    /// As [`Self::spawn`].
    pub fn spawn_self(config: &AudioConfig) -> Result<Self, WorkerError> {
        let exe = std::env::current_exe().map_err(|e| WorkerError::Spawn(e.to_string()))?;
        Self::spawn(&exe, config)
    }

    /// What the sandbox actually applied.
    #[must_use]
    pub const fn report(&self) -> &SandboxReport {
        &self.report
    }

    fn read_reply(&mut self) -> Result<FromDecoder, protocol::ProtocolError> {
        let body = protocol::read_body(&mut self.from_child)?;
        protocol::parse_reply(&body)
    }

    fn send(&mut self, message: &ToDecoder) -> Result<(), protocol::ProtocolError> {
        protocol::write(&mut self.to_child, message)?;
        self.to_child
            .flush()
            .map_err(|e| protocol::ProtocolError::Io(e.to_string()))
    }

    /// One round trip: a packet in, a frame out.
    fn round_trip(&mut self, packet: &[u8], at: u64) -> Result<Frame, AudioError> {
        if let Some(why) = self.poisoned.clone() {
            return Err(AudioError::Codec { detail: why });
        }
        if let Err(e) = self.send(&ToDecoder::Decode(packet.to_vec())) {
            let why = format!("the audio worker stopped listening: {e}");
            self.poisoned = Some(why.clone());
            return Err(AudioError::Codec { detail: why });
        }
        match self.read_reply() {
            Ok(FromDecoder::Samples { values }) => match checked(values, &self.format, at) {
                Ok(frame) => {
                    self.frames = self.frames.saturating_add(1);
                    Ok(frame)
                }
                Err(e) => {
                    self.poisoned = Some(e.to_string());
                    Err(e)
                }
            },
            // One packet refused. The worker keeps its decoder, so the next
            // packet is decoded normally.
            Ok(FromDecoder::Failed(why)) => Err(AudioError::Codec { detail: why }),
            Ok(other) => {
                let why = format!("the audio worker answered with {other:?}");
                self.poisoned = Some(why.clone());
                Err(AudioError::Codec { detail: why })
            }
            Err(e) => {
                let why = format!("the audio worker stopped answering: {e}");
                self.poisoned = Some(why.clone());
                Err(AudioError::Codec { detail: why })
            }
        }
    }
}

/// A frame from the worker, checked against the shape that was asked for.
///
/// **Not trusted.** The worker is assumed compromised — that is the whole
/// point of putting it in a jail — and a frame of a length nothing sized a
/// buffer for is exactly what a compromised one would send. A caller that took
/// its word would hand a short frame to a sound card and a long one to a
/// buffer that cannot hold it.
///
/// A free function so the check can be tested without a process: a test that
/// needed a hostile worker to prove this is a test nobody writes.
fn checked(values: Vec<i16>, format: &Format, at: u64) -> Result<Frame, AudioError> {
    let want = format.values_per_frame();
    if values.len() != want {
        return Err(AudioError::WrongSize {
            got: values.len(),
            want,
        });
    }
    Ok(Frame {
        samples: values,
        at,
    })
}

impl Decode for SandboxedAudio {
    fn backend(&self) -> &'static str {
        "opus"
    }

    fn is_sandboxed(&self) -> bool {
        true
    }

    fn format(&self) -> Format {
        self.format
    }

    fn decode(&mut self, packet: &[u8], at: u64) -> Result<Frame, AudioError> {
        if packet.is_empty() {
            // An empty packet is the conceal request, and a caller that wants
            // concealment has `conceal` for it. Refused here so the two cannot
            // be confused by accident.
            return Err(AudioError::Codec {
                detail: "an empty packet: use `conceal` to say a packet was lost".to_owned(),
            });
        }
        self.round_trip(packet, at)
    }

    fn conceal(&mut self, at: u64) -> Result<Frame, AudioError> {
        let frame = self.round_trip(&[], at)?;
        self.filled = self.filled.saturating_add(1);
        Ok(frame)
    }

    fn decoded(&self) -> (u64, u64) {
        (self.frames, self.filled)
    }
}

impl Drop for SandboxedAudio {
    fn drop(&mut self) {
        // Ask first, then let it go: a worker told to shut down exits cleanly
        // and its diagnostics land in order.
        let _ = protocol::write(&mut self.to_child, &ToDecoder::Shutdown);
        let _ = self.to_child.flush();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use esper_audio::{Captured, SENT, Source, SyntheticTone};

    /// A packet of real Opus, for feeding a decoder.
    fn packets(n: usize) -> Vec<Vec<u8>> {
        let mut source = SyntheticTone::new().unpaced();
        let mut encoder = esper_audio::opus::Encoder::new(SENT).expect("an encoder");
        let mut out = Vec::new();
        for _ in 0..n {
            let Captured::Frame(f) = source
                .next_frame(std::time::Duration::from_secs(1))
                .expect("a frame")
            else {
                panic!("the generator stopped");
            };
            out.push(encoder.encode(&f).expect("encoded").to_vec());
        }
        out
    }

    /// **A frame the worker's length does not match is refused.**
    ///
    /// The worker is in a jail because it is assumed compromised, and this is
    /// the check on what it says. Tested directly rather than through a
    /// process, because a hostile worker is not a thing a test can conveniently
    /// spawn — and a guard that can only be exercised by one is a guard nobody
    /// checks.
    #[test]
    fn a_frame_of_the_wrong_length_from_the_worker_is_refused() {
        let format = SENT;
        assert!(matches!(
            checked(vec![0i16; 960], &format, 0),
            Err(AudioError::WrongSize {
                got: 960,
                want: 1920
            })
        ));
        assert!(matches!(
            checked(vec![0i16; 4000], &format, 0),
            Err(AudioError::WrongSize { got: 4000, .. })
        ));
        let frame = checked(vec![7i16; 1920], &format, 480).expect("the right length");
        assert_eq!(frame.at, 480);
        assert!(frame.fits(&format));
    }

    /// **The loop works over a pipe pair, with no sandbox and no process.**
    ///
    /// The same code the child runs, so what is checked here is the protocol
    /// and the decoding rather than a reimplementation of them. The sandbox is
    /// checked by the worker tests, which need a process to apply it to.
    #[test]
    fn the_child_loop_answers_packets_with_frames() {
        let config = SENT.as_config(1);
        let mut input = Vec::new();
        for p in packets(5) {
            protocol::write(&mut input, &ToDecoder::Decode(p)).expect("written");
        }
        protocol::write(&mut input, &ToDecoder::Shutdown).expect("written");
        let mut output = Vec::new();

        let mut decoder = esper_audio::opus::Decoder::new(&config).expect("a decoder");
        serve(
            &mut decoder,
            SandboxReport::default(),
            &mut input.as_slice(),
            &mut output,
        )
        .expect("the loop ran");

        // Ready, then one reply per packet.
        let mut replies = Vec::new();
        let mut cursor = output.as_slice();
        while let Ok(body) = protocol::read_body(&mut cursor) {
            replies.push(protocol::parse_reply(&body).expect("a reply"));
        }
        assert!(matches!(replies.first(), Some(FromDecoder::Ready { .. })));
        assert_eq!(replies.len(), 6, "readiness and five frames: {replies:?}");
        for reply in replies.iter().skip(1) {
            match reply {
                FromDecoder::Samples { values } => {
                    assert_eq!(values.len(), SENT.values_per_frame());
                }
                other => panic!("expected samples, got {other:?}"),
            }
        }
    }

    /// **An empty packet is a lost packet**, and the frame that comes back
    /// continues the sound rather than being silence.
    #[test]
    fn an_empty_packet_asks_for_concealment() {
        let config = SENT.as_config(1);
        let mut input = Vec::new();
        // Ten real packets so the decoder has something to continue from.
        for p in packets(10) {
            protocol::write(&mut input, &ToDecoder::Decode(p)).expect("written");
        }
        protocol::write(&mut input, &ToDecoder::Decode(Vec::new())).expect("written");
        protocol::write(&mut input, &ToDecoder::Shutdown).expect("written");
        let mut output = Vec::new();
        let mut decoder = esper_audio::opus::Decoder::new(&config).expect("a decoder");
        serve(
            &mut decoder,
            SandboxReport::default(),
            &mut input.as_slice(),
            &mut output,
        )
        .expect("ran");

        let mut replies = Vec::new();
        let mut cursor = output.as_slice();
        while let Ok(body) = protocol::read_body(&mut cursor) {
            replies.push(protocol::parse_reply(&body).expect("a reply"));
        }
        let Some(FromDecoder::Samples { values }) = replies.last() else {
            panic!("the concealed frame is missing: {replies:?}");
        };
        assert_eq!(values.len(), SENT.values_per_frame());
        let peak = values.iter().map(|v| v.unsigned_abs()).max().unwrap_or(0);
        assert!(
            peak > 1_000,
            "concealment produced a peak of {peak}, which is a click"
        );
    }

    /// A packet that is not Opus comes back as a refusal, and the loop carries
    /// on — because one bad packet is not a session.
    #[test]
    fn a_bad_packet_is_refused_and_the_loop_continues() {
        let config = SENT.as_config(1);
        let mut input = Vec::new();
        protocol::write(&mut input, &ToDecoder::Decode(vec![0xFF; 40])).expect("written");
        for p in packets(1) {
            protocol::write(&mut input, &ToDecoder::Decode(p)).expect("written");
        }
        protocol::write(&mut input, &ToDecoder::Shutdown).expect("written");
        let mut output = Vec::new();
        let mut decoder = esper_audio::opus::Decoder::new(&config).expect("a decoder");
        serve(
            &mut decoder,
            SandboxReport::default(),
            &mut input.as_slice(),
            &mut output,
        )
        .expect("ran");

        let mut replies = Vec::new();
        let mut cursor = output.as_slice();
        while let Ok(body) = protocol::read_body(&mut cursor) {
            replies.push(protocol::parse_reply(&body).expect("a reply"));
        }
        assert_eq!(replies.len(), 3, "{replies:?}");
        assert!(
            matches!(replies.get(2), Some(FromDecoder::Samples { .. })),
            "the packet after a bad one must still decode: {replies:?}"
        );
    }
}
