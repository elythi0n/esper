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

pub mod audio;
pub mod controller;
pub mod cursor;
pub mod host;
pub mod jitter;
pub mod resume;

pub use audio::{AudioStats, ControllerAudio, HostAudio, PAYLOAD_TYPE_OPUS};
pub use controller::Encoded;
pub use controller::{
    Capacity, ControllerStats, ControllerStream, Decoded, LEGIBLE_LONG_EDGE, Measured, RUNGS,
    text_is_legible_at,
};
pub use cursor::{CursorTracker, POSITION_INTERVAL};
pub use host::{Handled, HostStep, HostStream, audio_ssrc, ssrc_for};
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

    /// How many bytes of datagram this sink will take right now.
    ///
    /// **`None` means "as many as you like"**, which is what a test sink and a
    /// loopback are: the default keeps every existing caller behaving exactly
    /// as it did.
    ///
    /// A real transport says a number, and the number matters more than it
    /// looks. Handing over a datagram that does not fit does **not** fail —
    /// quinn discards the oldest *unsent* datagrams to make room and reports
    /// success — so a sender that ignores this loses the leading datagrams of
    /// the frame it is in the middle of, which is the one loss pattern that
    /// poisons the reference chain rather than costing a single frame.
    fn room_for_datagrams(&self) -> Option<usize> {
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
    /// Something about the sound went wrong.
    ///
    /// One variant rather than a family, because every caller does the same
    /// thing with it: say so and carry on without sound. A session with a
    /// picture and no sound is a session; one that ends because a sound card
    /// disappeared is a bug.
    #[error("audio: {0}")]
    Audio(String),

    /// The switch was asked for and could not be done.
    ///
    /// Carries the backend's own reason, because the two a controller can act
    /// on are opposite: a monitor that does not exist means ask for another
    /// one, and a backend that cannot switch means the monitor it is watching
    /// is the only one it will get on this session.
    ///
    /// Sets rather than numbers, because a selection is a set: "screens 1 and
    /// 3" is one request, and a refusal has to say what is still being sent.
    #[error(
        "this session is sending {} and cannot send {}: {why}",
        screens_named(current),
        screens_named(wanted)
    )]
    CannotSwitchMonitor {
        current: Vec<u32>,
        wanted: Vec<u32>,
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

/// Where each screen the host is serving sits in the picture it is sending.
///
/// **The one place a controller works out the arrangement**, and it works it
/// out from the same rule the host laid the picture out with — the desktop's
/// own geometry, moved to the origin — rather than from a description of the
/// layout on the wire. Nothing has to be agreed because nothing is stated: a
/// second copy of the layout is a second chance for the two ends to disagree
/// about it, and a viewer that disagreed would map every click onto the wrong
/// screen.
///
/// **In picture pixels, not desktop pixels.** The picture is not always the
/// size of the screens in it: a ceiling, a bandwidth rung or an odd size
/// rounded to an even one all mean the host encodes something smaller. So each
/// screen's *edges* are scaled from the arrangement's size to the announced
/// size, per axis — edges rather than an origin and a size, because scaling
/// those separately is what leaves a one-pixel seam between two screens where
/// a click lands on neither.
///
/// Screens named in `serving` that are not in `monitors` are skipped, which a
/// well-formed config cannot contain — [`esper_proto::StreamConfig::is_well_formed`]
/// refuses one — so this is the shape of the answer rather than a case.
#[must_use]
pub fn tiles_of(config: &esper_proto::StreamConfig) -> Vec<(u32, esper_capture::geometry::Rect)> {
    let ids: Vec<u32> = config
        .serving
        .iter()
        .copied()
        .filter(|id| config.monitors.iter().any(|m| m.id == *id))
        .collect();
    let screens: Vec<esper_capture::geometry::Rect> = ids
        .iter()
        .filter_map(|id| config.monitors.iter().find(|m| m.id == *id))
        .map(|m| esper_capture::geometry::Rect {
            x: m.x,
            y: m.y,
            width: m.width,
            height: m.height,
        })
        .collect();
    let (bounds, placed) = esper_capture::geometry::tiles(&screens);
    let across = |v: u32| scaled(v, bounds.width, config.width);
    let down = |v: u32| scaled(v, bounds.height, config.height);
    ids.into_iter()
        .zip(placed)
        .map(|(id, r)| {
            // Non-negative by construction: `tiles` moves the arrangement to
            // the origin.
            let (left, top) = (across(r.x.unsigned_abs()), down(r.y.unsigned_abs()));
            let right = across(r.x.unsigned_abs().saturating_add(r.width));
            let bottom = down(r.y.unsigned_abs().saturating_add(r.height));
            (
                id,
                esper_capture::geometry::Rect {
                    x: i32::try_from(left).unwrap_or(0),
                    y: i32::try_from(top).unwrap_or(0),
                    width: right.saturating_sub(left),
                    height: bottom.saturating_sub(top),
                },
            )
        })
        .collect()
}

/// `v` moved from a `from`-sized picture to a `to`-sized one.
fn scaled(v: u32, from: u32, to: u32) -> u32 {
    if from == 0 {
        return 0;
    }
    let scaled = u64::from(v)
        .saturating_mul(u64::from(to))
        .saturating_div(u64::from(from));
    u32::try_from(scaled).unwrap_or(to).min(to)
}

/// Name a set of screens for a person: `monitor 1`, or `monitors 1 and 3`.
///
/// Public because a controller says the same things about the same sets, and
/// two ways of writing "monitors 1 and 3" is one too many.
#[must_use]
pub fn screens_named(ids: &[u32]) -> String {
    match ids {
        [] => "nothing".to_owned(),
        [one] => format!("monitor {one}"),
        [rest @ .., last] => {
            let names: Vec<String> = rest.iter().map(u32::to_string).collect();
            format!("monitors {} and {last}", names.join(", "))
        }
    }
}

#[cfg(test)]
mod naming_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::screens_named as screens;

    /// A refusal is read by a person, so the list has to read like one.
    #[test]
    fn a_set_of_screens_reads_as_a_sentence() {
        assert_eq!(screens(&[]), "nothing");
        assert_eq!(screens(&[1]), "monitor 1");
        assert_eq!(screens(&[1, 3]), "monitors 1 and 3");
        assert_eq!(screens(&[0, 1, 2]), "monitors 0, 1 and 2");
    }
}
