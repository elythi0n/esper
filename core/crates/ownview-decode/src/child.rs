//! The sandboxed decoder's own process.
//!
//! Reads bitstream on stdin, writes pictures on stdout, and does nothing else
//! — because after [`crate::sandbox::Sandbox::apply`] there is nothing else it
//! can do.
//!
//! # The order, which is the whole security property
//!
//! 1. Build the decoder. This allocates and initialises openh264, which needs
//!    syscalls the sandbox forbids.
//! 2. **Apply the sandbox.**
//! 3. Announce readiness, carrying the report so the parent can refuse to
//!    proceed if the jail is not really on.
//! 4. Only now read the first byte a peer chose.
//!
//! Getting steps 2 and 4 the wrong way round produces a decoder that works
//! perfectly and is not sandboxed, with nothing to show for it. So the
//! announcement is the barrier: it cannot be sent before step 2 because that
//! is where the report comes from, and the parent will not send bitstream
//! before it arrives.
//!
//! # Why it does not run as a thread
//!
//! A thread would share the address space with the session keys, the paired
//! device store and the input injector. The point of a jail is that a
//! compromise reaches none of those, and a thread reaches all of them.

use std::io::{Read, Write};

use crate::decoder::{DecoderConfig, VideoDecoder};
use crate::openh264::OpenH264Decoder;
use crate::protocol::{self, FromDecoder, ProtocolError, ToDecoder};
use crate::sandbox::Sandbox;

/// Why the child stopped.
#[derive(Debug, thiserror::Error)]
pub enum ChildError {
    /// The sandbox could not be applied, so nothing ran.
    ///
    /// Fatal, deliberately. A decoder that cannot be confined must not decode:
    /// the alternative is a large C parser on a peer's input with nothing
    /// around it.
    #[error("refusing to decode without a sandbox: {0}")]
    NoSandbox(#[from] crate::sandbox::SandboxError),

    /// The decoder could not be built.
    #[error(transparent)]
    Decoder(#[from] crate::decoder::DecodeError),

    /// The pipe to the parent failed.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}

/// Run the decoder loop until the parent closes the pipe.
///
/// `input` and `output` rather than reaching for stdin and stdout directly, so
/// the loop is testable over a pipe pair without a process — and so the probe
/// that proves the sandbox works can use exactly this code rather than a
/// reimplementation of it.
pub fn run<R: Read, W: Write>(
    config: &DecoderConfig,
    sandbox: &Sandbox,
    input: &mut R,
    output: &mut W,
) -> Result<(), ChildError> {
    // 1. Everything that needs a forbidden syscall.
    let mut decoder = OpenH264Decoder::new(config)?;
    warm_the_allocator();

    // 2. The jail.
    let report = sandbox.apply()?;

    // 3. Say so, and hand over what actually took effect.
    protocol::write_reply(output, &FromDecoder::Ready { report })?;

    // 4. Only now, a peer's bytes.
    loop {
        let body = match protocol::read_body(input) {
            Ok(b) => b,
            // The parent closed. An ordinary end, not a failure.
            Err(ProtocolError::Closed) => return Ok(()),
            Err(e) => return Err(e.into()),
        };

        let request = match protocol::parse_request(&body) {
            Ok(r) => r,
            // A malformed request means the parent and child disagree about
            // the wire, which is not something to carry on through.
            Err(e) => {
                let _ = protocol::write_reply(output, &FromDecoder::Failed(e.to_string()));
                return Err(e.into());
            }
        };

        match request {
            ToDecoder::Shutdown => return Ok(()),
            ToDecoder::Decode(packet) => {
                let reply = match decoder.decode(&packet) {
                    Ok(Some(picture)) => {
                        let mut planes = Vec::with_capacity(
                            picture.y().len() + picture.u().len() + picture.v().len(),
                        );
                        planes.extend_from_slice(picture.y());
                        planes.extend_from_slice(picture.u());
                        planes.extend_from_slice(picture.v());
                        FromDecoder::Picture {
                            width: picture.width(),
                            height: picture.height(),
                            planes,
                        }
                    }
                    Ok(None) => FromDecoder::NoPicture,
                    // Reported rather than fatal *here*: the parent decides
                    // what a failed packet means for the session, and it is
                    // the parent that holds the policy. The child simply says
                    // what happened.
                    Err(e) => FromDecoder::Failed(e.to_string()),
                };
                protocol::write_reply(output, &reply)?;
            }
        }
    }
}

/// Make the allocator do its one-time setup before the jail goes on.
///
/// **This exists because of a real and thoroughly non-obvious failure.** The
/// sandboxed child decoded a picture, wrote it back, and then died of `SIGSYS`
/// while *freeing* the buffer. Bisecting the syscall space found the culprit:
/// **`openat`, from inside glibc's allocator.**
///
/// glibc queries the system's transparent-huge-page configuration by opening
/// a file under `/sys`, the first time it takes a particular allocation path.
/// The answer is cached for the life of the process, so the query happens
/// once — and it happened on the first picture-sized allocation, which is
/// after the sandbox.
///
/// `openat` cannot be allowed. It is the syscall that would let a compromised
/// decoder read the user's files, and it is the reason the filter exists.
///
/// So the query is made to happen *before* the jail instead. That is the same
/// principle the whole startup order already follows — setup needs syscalls
/// the sandbox forbids, so setup goes first — applied to a place nobody would
/// think to look.
///
/// The sizes span the allocator's behaviour: below its mmap threshold, above
/// it, and around a 4K picture's worth, so whichever path a real frame takes
/// has already been walked.
fn warm_the_allocator() {
    for size in [
        64 * 1024usize,
        // 320x240 I420, the size that found this.
        115_200,
        // Straddling glibc's default 128 KB mmap threshold.
        128 * 1024,
        256 * 1024,
        // 1080p and 4K I420.
        3_110_400,
        12_441_600,
    ] {
        // `vec!` rather than `with_capacity`, so the pages are touched and
        // not merely reserved.
        let block = vec![0u8; size];
        // Read something back, so the compiler cannot decide the allocation
        // was pointless and remove it.
        std::hint::black_box(block.first());
        drop(block);
    }
}

/// Run against this process's own stdin and stdout.
///
/// What the engine calls when it is invoked as the decoder worker.
pub fn run_on_stdio(config: &DecoderConfig) -> Result<(), ChildError> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    run(config, &Sandbox::default(), &mut input, &mut output)
}
