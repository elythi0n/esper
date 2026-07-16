//! Surviving a transport loss.
//!
//! # A media session is resumed, not replayed
//!
//! Terminal mode keeps a **backlog**: the shell goes on producing output while
//! nobody is connected, and output nobody saw is output lost, so it is held and
//! replayed on reconnect.
//!
//! **Video is the opposite, and the asymmetry is the whole design.** A frame
//! from ten seconds ago is not something anybody wants to see; showing it would
//! be showing a screen that no longer exists. So there is no backlog here, and
//! not for want of one — holding frames across a reconnect would cost memory to
//! deliver a stale picture, and then the picture would still need replacing.
//!
//! What a reconnect wants is a **keyframe**, immediately. One frame, current.
//!
//! # What is worth keeping, then
//!
//! Everything that took a round trip to agree and nothing that took a
//! microsecond to produce:
//!
//! - **The pipeline.** Capture is open, the encoder is configured, the CPU
//!   budget's governor has learned what this machine costs. Rebuilding it
//!   throws away that learning and re-opens a capture backend, which on some
//!   platforms means asking a person for permission again.
//! - **The monitor, and the quality the controller asked for.** Both were
//!   negotiated. A reconnect that reverted them would look like the session
//!   silently changing its mind.
//! - **The stream's identity.** Same monitor, same SSRC, so a capture of the
//!   traffic reads as one stream interrupted rather than two unrelated ones.
//!
//! # Why parking is bounded
//!
//! A parked session holds a capture pipeline and an encoder — and on a host,
//! it holds *the screen being captured*. A session parked indefinitely is a
//! machine quietly doing work for a controller that is never coming back, so
//! it expires, and it expires on a timer rather than on a guess about whether
//! the peer will return.

use std::time::{Duration, Instant};

use ownview_pipeline::pipeline::Pipeline;
use ownview_proto::Reason;

use crate::HostStream;

/// How long a media session waits to be resumed.
///
/// Fifteen seconds. Long enough to cover a laptop changing networks or a lid
/// closing briefly; short enough that a controller which is not coming back
/// stops costing the host anything soon. Terminal mode parks for far longer,
/// because a shell holds *state a person built* and a screen holds none.
pub const PARK_TIMEOUT: Duration = Duration::from_secs(15);

/// Why a parked session was let go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expired {
    /// Nobody came back in time.
    TimedOut,
    /// A different device asked for the same monitor.
    Superseded,
    /// The host is shutting down.
    HostGone,
}

impl Expired {
    /// The code to close with, so the reason travels.
    #[must_use]
    pub const fn reason(self) -> Reason {
        match self {
            Self::TimedOut => Reason::PEER_GONE,
            Self::Superseded => Reason::SUPERSEDED,
            Self::HostGone => Reason::HOST_SHUTTING_DOWN,
        }
    }

    /// Why, for the log.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::TimedOut => "nobody resumed it in time",
            Self::Superseded => "another device took the same monitor",
            Self::HostGone => "the host is shutting down",
        }
    }
}

/// A media session whose transport has gone, waiting to be resumed.
///
/// Holds the pipeline and nothing else that matters — emphatically **no
/// frames**. See the module documentation.
pub struct Parked {
    pipeline: Pipeline,
    /// Which device may resume it. A parked session is *that device's*, and
    /// letting any paired device take it would let one controller inherit
    /// another's session by reconnecting first.
    owner: [u8; 32],
    parked_at: Instant,
    /// Sequence numbering, carried over so a capture of the traffic reads as
    /// one stream interrupted rather than two unrelated ones.
    next_sequence: u16,
}

impl std::fmt::Debug for Parked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Parked")
            .field("encoding", &self.pipeline.encode_size())
            .field("parked_for", &self.parked_at.elapsed())
            .finish_non_exhaustive()
    }
}

impl Parked {
    /// Park a sending session whose transport has gone.
    #[must_use]
    pub fn park(stream: HostStream, owner: [u8; 32], now: Instant) -> Self {
        let next_sequence = stream.next_sequence();
        Self {
            pipeline: stream.into_pipeline(),
            owner,
            parked_at: now,
            next_sequence,
        }
    }

    /// How long it has been waiting.
    #[must_use]
    pub fn parked_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.parked_at)
    }

    /// Whether it has waited too long.
    #[must_use]
    pub fn is_expired(&self, now: Instant) -> bool {
        self.parked_for(now) >= PARK_TIMEOUT
    }

    /// Which device may resume it.
    #[must_use]
    pub const fn owner(&self) -> &[u8; 32] {
        &self.owner
    }

    /// Whether this device may resume it.
    #[must_use]
    pub fn may_resume(&self, device: &[u8; 32]) -> bool {
        // Constant time is not the point here — the key is not a secret, it
        // was proven by the TLS handshake — but *whose* session this is very
        // much is.
        self.owner == *device
    }

    /// What is being encoded, so a caller can report it while parked.
    #[must_use]
    pub fn encode_size(&self) -> (u32, u32) {
        self.pipeline.encode_size()
    }

    /// Resume onto a new transport.
    ///
    /// **A keyframe is requested before the first frame goes out**, because
    /// the controller on the other end is new: its decoder has no reference
    /// picture, and a delta frame would be undecodable. That is the whole of
    /// what "resumed" means for video — no replay, one current picture.
    // The large `Err` is the whole point: it carries the pipeline back so a
    // refused resume can be parked again rather than destroying the session it
    // was trying to save. Boxing it would add an allocation on the path that
    // exists to avoid losing somebody's session.
    #[allow(clippy::result_large_err)]
    pub fn resume(
        self,
        device: &[u8; 32],
        mtu: usize,
    ) -> Result<HostStream, (Self, ResumeRefused)> {
        if !self.may_resume(device) {
            return Err((self, ResumeRefused::NotYours));
        }
        let Self {
            mut pipeline,
            owner,
            parked_at,
            next_sequence,
        } = self;
        pipeline.request_keyframe();
        match HostStream::resume(pipeline, mtu, next_sequence) {
            Ok(stream) => Ok(stream),
            Err((pipeline, e)) => Err((
                Self {
                    pipeline,
                    owner,
                    parked_at,
                    next_sequence,
                },
                ResumeRefused::Stream(e.to_string()),
            )),
        }
    }

    /// Give it up, returning the pipeline so a caller can close it cleanly.
    #[must_use]
    pub fn release(self) -> Pipeline {
        self.pipeline
    }
}

/// Why a resume was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeRefused {
    /// A different device asked for it.
    ///
    /// Its own case because it is the one that matters: without this check a
    /// controller could inherit another controller's session by reconnecting
    /// first, which is a session it was never granted.
    NotYours,
    /// The new transport cannot carry it.
    Stream(String),
}

impl ResumeRefused {
    /// The code to close with.
    #[must_use]
    pub const fn reason(&self) -> Reason {
        match self {
            Self::NotYours => Reason::PERMISSION_DENIED,
            Self::Stream(_) => Reason::MIGRATION_FAILED,
        }
    }
}

impl std::fmt::Display for ResumeRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Deliberately not "not paired" or "unknown device": the device is
            // paired and known, the session simply belongs to another one.
            Self::NotYours => f.write_str("it belongs to a different device"),
            Self::Stream(e) => write!(f, "the new path will not carry it: {e}"),
        }
    }
}
