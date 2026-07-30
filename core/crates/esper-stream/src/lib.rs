//! The media path behind a transport.
//!
//! Two halves that never share a process in production: a [`host::HostStream`]
//! that packetizes what it captures, and a [`controller::ControllerStream`]
//! that reassembles and decodes it.
//!
//! # Why this crate does not know about QUIC
//!
//! It takes a [`DatagramSink`] instead. Three reasons, in order of how much
//! they matter:
//!
//! **It can be tested.** A session over a real transport needs two machines,
//! or at least two tasks and a network stack, and it cannot be made to lose
//! the fourth packet of every frame on purpose. An in-memory sink can, which
//! is how the loss and reordering behaviour here is exercised at all.
//!
//! **The transport is not the only sink.** The engine re-offers a session's
//! own media to a webview over a loopback, and later to a browser. Those are
//! sinks too, and a crate that spoke `quinn` would have to grow a second path
//! for each of them.
//!
//! **It keeps the dependency arrow straight.** Media never needs to know how
//! bytes leave the machine, and a crate that knows anyway is one that will
//! eventually make a decision on the transport's behalf.

pub mod controller;
pub mod cursor;
pub mod host;
pub mod jitter;
pub mod resume;

pub use controller::{ControllerStats, ControllerStream, Decoded};
pub use cursor::{CursorTracker, POSITION_INTERVAL};
pub use host::{Handled, HostStep, HostStream, ssrc_for};
pub use jitter::{Jitter, JitterStats};
pub use resume::{Expired, PARK_TIMEOUT, Parked, ResumeRefused};

/// Somewhere unreliable datagrams go.
///
/// Unreliable is the contract, not a limitation: a lost video packet must
/// never delay the next one, and retransmitting a frame already too late to
/// display costs latency for nothing.
pub trait DatagramSink {
    /// Send one datagram. It must not be split.
    fn send(&mut self, payload: &[u8]) -> Result<(), SinkError>;

    /// The largest datagram that will fit without fragmenting.
    ///
    /// `None` means the sink cannot say, and the caller should keep whatever
    /// MTU it was configured with rather than guessing a smaller one.
    fn max_datagram_size(&self) -> Option<usize> {
        None
    }
}

/// Why a datagram could not be sent.
#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    /// The peer is gone.
    #[error("the peer is no longer reachable: {0}")]
    Gone(String),
    /// The datagram is larger than the path will carry.
    ///
    /// Its own variant because it is a bug on this side rather than a network
    /// problem: the packetizer was given the wrong MTU.
    #[error("a {len}-byte datagram exceeds the path's {limit}")]
    TooLarge { len: usize, limit: usize },
    /// Something else.
    #[error("{0}")]
    Other(String),
}

/// Anything that went wrong in a stream.
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    /// The host announced a codec this build cannot decode.
    ///
    /// Its own variant rather than a malformed config: the config is perfectly
    /// well formed, it just describes a stream this end cannot render, and a
    /// person told "malformed" would look for a protocol bug instead of a
    /// build that is missing a decoder.
    #[error("this build cannot decode {}", codec.name())]
    CannotDecode { codec: esper_proto::Codec },
    #[error(transparent)]
    Pipeline(#[from] esper_pipeline::pipeline::PipelineError),
    #[error(transparent)]
    Packetize(#[from] esper_media::PacketizeError),
    #[error(transparent)]
    Decode(#[from] esper_decode::decoder::DecodeError),
    #[error(transparent)]
    Sink(#[from] SinkError),
    /// A peer sent a message it is not allowed to send.
    ///
    /// Direction is part of the protocol: a controller announcing a
    /// `StreamConfig` would be telling the host what its own screen is.
    #[error("a {role:?} may not send {what}")]
    WrongDirection {
        role: esper_proto::Role,
        what: &'static str,
    },
    /// A peer sent a message whose contents are outside the protocol's bounds.
    #[error("{what} is outside the bounds the protocol allows")]
    Malformed { what: &'static str },
    /// A monitor switch mid-session, which no capture backend can do yet.
    /// The switch was asked for and could not be done.
    ///
    /// Carries the backend's own reason, because the two a controller can act
    /// on are opposite: a monitor that does not exist means ask for another
    /// one, and a backend that cannot switch means the monitor it is watching
    /// is the only one it will get on this session.
    #[error("this session is sending monitor {current} and cannot switch to {wanted}: {why}")]
    CannotSwitchMonitor {
        current: u32,
        wanted: u32,
        why: String,
    },
}

/// A sink that keeps everything, for tests and for measuring.
#[derive(Debug, Default)]
pub struct RecordingSink {
    /// Every datagram, in the order it was sent.
    pub sent: Vec<Vec<u8>>,
    /// What to report as the path's limit.
    pub limit: Option<usize>,
}

impl RecordingSink {
    /// A sink with no stated limit.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Total bytes handed over.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.sent.iter().map(Vec::len).sum()
    }
}

impl DatagramSink for RecordingSink {
    fn send(&mut self, payload: &[u8]) -> Result<(), SinkError> {
        if let Some(limit) = self.limit.filter(|&l| payload.len() > l) {
            return Err(SinkError::TooLarge {
                len: payload.len(),
                limit,
            });
        }
        self.sent.push(payload.to_vec());
        Ok(())
    }

    fn max_datagram_size(&self) -> Option<usize> {
        self.limit
    }
}
