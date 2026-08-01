//! The parent side of the sandboxed decoder.
//!
//! # Why the decoder is a separate process at all
//!
//! It is the one place a peer's bytes reach a large C parser. Everything else
//! in this engine either produces its own input or parses in safe Rust; this
//! takes an attacker-chosen bitstream and hands it to 155 000 lines of C.
//!
//! So it runs somewhere a memory-safety failure is contained: its own process,
//! with an address-space cap, Landlock, and an eighteen-syscall seccomp
//! filter. What the child can do after the jail goes on is read its stdin,
//! write its stdout, and allocate — and that is checked by
//! [`FromDecoder::Ready`], which the child sends **after** applying the
//! sandbox and which this refuses to proceed without.
//!
//! # What "refuses to proceed" means
//!
//! If the report says the sandbox is not in force, this returns an error and
//! the session does not get a decoder. It does not fall back to decoding in
//! process. A fallback is how a security property becomes a preference — and
//! the one time it would trigger is the one time it matters.
//!
//! # Why it is synchronous
//!
//! A pipe write and a pipe read, in lockstep. The caller is the media loop,
//! which is already on a thread of its own because capture blocks, and a
//! decoder that returned a future would need an executor there for no benefit:
//! there is exactly one packet in flight and nothing else to do while it is.

use std::io::{BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use esper_color::I420;

use crate::decoder::{DecodeError, DecodeStats, DecoderConfig, VideoDecoder};
use crate::protocol::{self, FromDecoder, ToDecoder};
use crate::sandbox::SandboxReport;

/// The environment variable that tells a re-executed engine to be the worker.
///
/// The worker is *this binary*, re-executed. Shipping one binary rather than
/// two means the two halves cannot be at different versions, which for a
/// protocol between them is worth more than the tidiness of a separate file.
pub const WORKER_ENV: &str = "ESPER_DECODE_WORKER";

/// The argument the worker is invoked with.
pub const WORKER_ARG: &str = "decode-worker";

/// Why a sandboxed decoder could not be started or used.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    /// The worker could not be spawned.
    #[error("starting the decoder worker: {0}")]
    Spawn(String),
    /// The worker's pipes were not there.
    #[error("the decoder worker has no {0}")]
    NoPipe(&'static str),
    /// The worker did not report itself ready.
    #[error("the decoder worker did not report itself ready: {0}")]
    NotReady(String),
    /// **The worker started but is not sandboxed.**
    ///
    /// Its own variant because it is the one failure that must never be
    /// worked around. A decoder that is not isolated is the thing this whole
    /// arrangement exists to avoid.
    #[error(
        "the decoder worker started but is not sandboxed ({detail}); refusing \
         to decode a peer's bitstream without isolation"
    )]
    NotSandboxed { detail: String },
    /// The protocol between the halves broke.
    #[error(transparent)]
    Protocol(#[from] protocol::ProtocolError),
}

impl From<WorkerError> for DecodeError {
    fn from(e: WorkerError) -> Self {
        Self::Backend {
            backend: "openh264-sandboxed",
            detail: e.to_string(),
        }
    }
}

/// A decode failure that names this backend.
fn failed(detail: impl Into<String>) -> DecodeError {
    DecodeError::Backend {
        backend: "openh264-sandboxed",
        detail: detail.into(),
    }
}

/// A decoder running in its own sandboxed process.
pub struct SandboxedDecoder {
    child: Child,
    to_child: BufWriter<ChildStdin>,
    from_child: BufReader<ChildStdout>,
    report: SandboxReport,
    stats: DecodeStats,
    /// Set once the stream has failed, so a caller that keeps going is told
    /// the same thing rather than restarting a dead conversation.
    poisoned: Option<String>,
}

impl std::fmt::Debug for SandboxedDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxedDecoder")
            .field("pid", &self.child.id())
            .field("sandbox", &self.report.describe())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl SandboxedDecoder {
    /// Spawn a worker and wait for it to report the sandbox is on.
    ///
    /// `worker` is the program to run — normally this engine's own executable.
    pub fn spawn(worker: &std::path::Path, config: &DecoderConfig) -> Result<Self, WorkerError> {
        let mut child = Command::new(worker)
            .arg(WORKER_ARG)
            .env(WORKER_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Inherited on purpose: the child's diagnostics are the operator's
            // only view of a jail that refused something, and swallowing them
            // makes a sandbox failure look like a hang.
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
            stats: DecodeStats::default(),
            poisoned: None,
        };

        // **Nothing is sent before the readiness report arrives.** The child
        // needs syscalls the sandbox forbids in order to set up, so the jail
        // goes on after setup and before the first byte from a peer — and this
        // is where that ordering is enforced from the other side.
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
        let _ = config;
        Ok(me)
    }

    /// Spawn a worker using this process's own executable.
    pub fn spawn_self(config: &DecoderConfig) -> Result<Self, WorkerError> {
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
}

impl Drop for SandboxedDecoder {
    fn drop(&mut self) {
        // Ask first, then insist. A worker told to shut down exits cleanly and
        // its jail's diagnostics land in order; one that is killed outright
        // can leave a confusing signal in the log of an ordinary teardown.
        let _ = protocol::write(&mut self.to_child, &ToDecoder::Shutdown);
        let _ = self.to_child.flush();
        let _ = self.child.wait();
    }
}

impl VideoDecoder for SandboxedDecoder {
    fn backend(&self) -> &'static str {
        "openh264"
    }

    fn is_sandboxed(&self) -> bool {
        true
    }

    fn decode(&mut self, packet: &[u8]) -> Result<Option<I420>, DecodeError> {
        if let Some(why) = self.poisoned.clone() {
            return Err(failed(why));
        }
        self.stats.packets = self.stats.packets.saturating_add(1);
        self.stats.bytes = self.stats.bytes.saturating_add(packet.len() as u64);

        if let Err(e) = self.send(&ToDecoder::Decode(packet.to_vec())) {
            let why = format!("the decoder worker stopped listening: {e}");
            self.poisoned = Some(why.clone());
            return Err(failed(why));
        }
        match self.read_reply() {
            Ok(FromDecoder::Picture {
                width,
                height,
                planes,
            }) => {
                let picture = unpack(width, height, &planes)?;
                self.stats.pictures = self.stats.pictures.saturating_add(1);
                Ok(Some(picture))
            }
            Ok(FromDecoder::NoPicture) => Ok(None),
            Ok(FromDecoder::Failed(why)) => {
                // **One frame, not the session.** Error concealment is off, so
                // a frame whose reference pictures were lost is refused rather
                // than invented — and that refusal is the *ordinary* thing on a
                // lossy link, which is why the receiver asks for a keyframe
                // when it happens.
                //
                // This used to poison the worker, which made the ask
                // meaningless: the first frame with a missing reference ended
                // the video for the rest of the session, keyframes included,
                // without them even being sent to the child. Measured on a link
                // losing five per cent: 39 of 41 keyframes that arrived whole
                // were never looked at, and the session showed 8 pictures where
                // it now shows 70.
                //
                // The child keeps its decoder, which is what makes this sound:
                // openh264 recovers on the next keyframe by itself, and
                // `a_keyframe_rescues_a_decoder_that_lost_sync` is the proof.
                self.stats.undecodable = self.stats.undecodable.saturating_add(1);
                Err(failed(why))
            }
            // **Both of these mean the child is not the child we spawned.**
            // A second readiness, or a frame of sound from a worker asked for
            // pictures, is a worker that has been replaced or subverted —
            // there is no ordinary path that produces either. Poisoned, so the
            // session stops using it rather than carrying on with something
            // that is answering questions it was not asked.
            Ok(FromDecoder::Ready { .. } | FromDecoder::Samples { .. }) => {
                let why =
                    "the decoder worker answered with something it was never asked for".to_owned();
                self.poisoned = Some(why.clone());
                Err(failed(why))
            }
            Err(e) => {
                let why = format!("the decoder worker stopped answering: {e}");
                self.poisoned = Some(why.clone());
                Err(failed(why))
            }
        }
    }

    fn stats(&self) -> DecodeStats {
        self.stats
    }
}

/// Rebuild a picture from the planes the worker sent.
///
/// The sizes are checked against the plane bytes rather than trusted. They
/// come from our own child, but the child is the process holding the C parser
/// and the whole point of the arrangement is that its output is not assumed to
/// be well formed.
fn unpack(width: u32, height: u32, planes: &[u8]) -> Result<I420, DecodeError> {
    let wanted = protocol::plane_bytes(width, height)
        .ok_or_else(|| failed(format!("{width}x{height} has no plane count")))?;
    if planes.len() != wanted {
        return Err(failed(format!(
            "the worker sent {} bytes for a {width}x{height} picture, not {wanted}",
            planes.len()
        )));
    }
    let mut picture = I420::new(width, height)
        .map_err(|e| failed(format!("sizing a {width}x{height} picture: {e}")))?;
    let luma = (width as usize).saturating_mul(height as usize);
    let chroma = luma / 4;
    let y_stride = picture.y_stride();
    let uv_stride = picture.uv_stride();
    let (y, u, v) = picture.planes_mut();

    for row in 0..height as usize {
        let from = row.saturating_mul(width as usize);
        let to = from.saturating_add(width as usize);
        let Some(src) = planes.get(from..to) else {
            return Err(failed("the luma plane is short".to_owned()));
        };
        let at = row.saturating_mul(y_stride);
        if let Some(dst) = y.get_mut(at..at.saturating_add(width as usize)) {
            dst.copy_from_slice(src);
        }
    }
    let half_w = (width as usize).div_ceil(2);
    let half_h = (height as usize).div_ceil(2);
    for (plane, base) in [(u, luma), (v, luma.saturating_add(chroma))] {
        for row in 0..half_h {
            let from = base.saturating_add(row.saturating_mul(half_w));
            let to = from.saturating_add(half_w);
            let Some(src) = planes.get(from..to) else {
                return Err(failed("a chroma plane is short".to_owned()));
            };
            let at = row.saturating_mul(uv_stride);
            if let Some(dst) = plane.get_mut(at..at.saturating_add(half_w)) {
                dst.copy_from_slice(src);
            }
        }
    }
    Ok(picture)
}
