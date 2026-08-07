//! The receiving half.
//!
//! Reassembles datagrams into frames, decodes them, and says what it needs.
//!
//! # It asks for keyframes; it does not wait for them
//!
//! Loss is reported by the reassembler rather than hidden, and every report
//! turns into a [`KeyframeRequest`] for the caller to send. The requests are
//! *coalesced*: a burst of loss produces one ask, not one per frame. A
//! controller on a bad link that asked per frame would make the host send
//! nothing but keyframes, which is the largest and most expensive kind of
//! frame — turning a lossy link into an unusable one.
//!
//! # Frames it can prove complete but cannot decode
//!
//! The reassembler hands back frames it can prove whole. Whether they are
//! *decodable* is a different question: a delta frame references pictures
//! before it, and after a loss one of those is gone.
//!
//! So this skips them, on purpose, while a keyframe is outstanding. The
//! decoder is a large C parser configured to refuse rather than conceal, and
//! feeding it frames already known to be undecodable is strictly more
//! attacker-shaped input for no benefit.
//!
//! **All of which assumes a keyframe can put the decoder back in step.** It
//! could not, for a while: the decoder's own worker treated the first frame it
//! could not decode as the end of the stream and refused every frame after it,
//! so the ask this module makes was answered and then discarded a layer down.
//! Worth knowing about from here, because nothing in this file can detect it —
//! a session simply stops showing anything and goes on asking politely.

use esper_decode::decoder::VideoDecoder;
use esper_media::{Depacketizer, Reassembled};
use esper_proto::{
    KeyframeReason, KeyframeRequest, LinkReport, MediaMessage, MonitorSelect, Profile, QualityHint,
    Role, Sharpness, StreamConfig,
};

use crate::StreamError;
use crate::jitter::{Jitter, JitterStats};

/// What arrived.
#[derive(Debug)]
pub enum Decoded {
    /// A picture, ready to show.
    Picture {
        picture: esper_color::I420,
        timestamp: u32,
        keyframe: bool,
    },
    /// A frame arrived complete but was not decoded, and why.
    Skipped { timestamp: u32, why: &'static str },
    /// A frame was lost. A keyframe has been asked for.
    Lost { timestamp: u32, why: &'static str },
}
/// The resolutions a controller will ask for when the link is losing frames.
///
/// **Frame size is what decides whether loss is survivable.** A frame is given
/// up on when any of its datagrams is missing, so a frame spanning three of
/// them survives 5% loss 86% of the time and one spanning forty survives it
/// 13% of the time. That arithmetic is the whole argument, and it is why the
/// same five per cent that is a working session at 640x360 showed one picture
/// at 1280x720 in the smoke test's own environment.
///
/// Coarse rungs, because each step has to be worth the reconfiguration it
/// costs the encoder, and because a ladder with many rungs spends its time
/// between them.
const RUNGS: &[u32] = &[1920, 1280, 960, 640, 480, 320];

/// The smallest frame whose arrival says anything about the link.
///
/// Below this the spread is dominated by whether two datagrams happened to be
/// handed over in the same breath rather than by the wire between them.
const SMALLEST_BURST_WORTH_TIMING: u64 = 4_000;

/// A link's capacity, and how it was established.
///
/// # Why the provenance travels with the number
///
/// There are two measurements here and they are not equally trustworthy. One
/// times a frame's own datagrams arriving together, which is the only thing
/// that measures a *link* rather than the demand on it. The other counts bytes
/// while frames are failing — it cannot tell a frame from a fragment, and it
/// is a lower bound rather than a reading.
///
/// The session report used to print both under the same sentence, "from how
/// fast a frame's datagrams arrived together". That sentence is what somebody
/// reads to decide whether to believe the number, and for the second
/// measurement it was false. Measured: an 800 kbit/s link with a buffer too
/// short for one frame reported 398 kbit/s — a true lower bound, presented as
/// a measurement of the link.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Capacity {
    /// The rate, in kbit/s.
    pub kbit: f64,
    /// What it was established from.
    pub how: Measured,
}

impl Capacity {
    /// Timed from a frame's datagrams arriving together.
    #[must_use]
    pub fn timed(kbit: f64) -> Self {
        Self {
            kbit,
            how: Measured::ByABurst,
        }
    }

    /// A lower bound, from bytes counted while frames were failing.
    #[must_use]
    pub fn at_least(kbit: f64) -> Self {
        Self {
            kbit,
            how: Measured::AtLeast,
        }
    }
}

/// How a capacity figure was arrived at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measured {
    /// Timed from a frame's own datagrams arriving together: the accurate one,
    /// and the only measurement of the link itself.
    ByABurst,
    /// Bytes counted while frames were failing — the one regime nothing can
    /// time. A lower bound on what the link carries, which is what a pacer
    /// wants and is not a reading of the link.
    AtLeast,
}

/// How many burst measurements to keep.
///
/// Sixteen, which at a keyframe every few seconds is most of a minute of
/// evidence, and rolls over fast enough that a link which genuinely changed
/// is not remembered as what it was.
const BURSTS_REMEMBERED: usize = 16;

/// How many are needed before the middle of them means anything.
///
/// Four. Below that the median is one of two or three readings and inherits
/// their noise, and a wrong capacity is worse than none — nothing has to act
/// on this until it is worth acting on.
const ENOUGH_BURSTS_TO_JUDGE: usize = 4;

/// How many losing windows at one size mean that size does not work here.
///
/// **Two.** One is a bad moment; two at the same size, with a step down and a
/// climb back in between, is the link saying the same thing twice.
const FAILURES_THAT_CLOSE_A_RUNG: u8 = 2;

/// How long the arrival-rate estimator averages over.
///
/// **A second, matching the loss window**, so the two signals a starved
/// session produces are about the same stretch of time.
const ARRIVAL_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);

/// How long with no frame completing before what *arrived* is the only
/// evidence about the link.
///
/// **Two seconds is several frames at any rate this matters at.** Below that a
/// session that simply has nothing to send — an idle screen sends almost
/// nothing — would be read as a starved one, and the host would be paced down
/// to the trickle it was already sending. That is the failure mode this
/// estimator has to avoid, and the reason it is gated on frames *failing*
/// rather than on capacity being unknown.
const STARVED_AFTER: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a window of loss is judged over.
const WINDOW: std::time::Duration = std::time::Duration::from_secs(1);

/// Frames lost in a window that mean the link cannot carry this frame size.
///
/// Two rather than one: a single lost frame is a packet, and a ladder that
/// moved on one would spend a clean session stepping down and up.
const LOSSES_TO_STEP_DOWN: u64 = 2;

/// Clean windows before asking for more again.
///
/// **Down fast, up slow.** Stepping down costs a resolution somebody may not
/// notice; stepping up too eagerly costs the collapse this ladder exists to
/// prevent, and then costs it again a second later.
const CLEAN_WINDOWS_TO_STEP_UP: u32 = 5;

/// The framerates a controller will ask for when the link is queueing.
///
/// **A different lever for a different problem.** Loss is per packet, so
/// against loss the thing to change is how many packets a frame spans — frame
/// size. A queue filling is about bytes per second, and against that the
/// cheapest thing to give up is how often a frame is sent: a screen at ten
/// frames a second is still a screen somebody can read, where the same screen
/// at half the pixels is not.
///
/// Which is also the order `capture-and-codec.md` asks for — framerate first,
/// then resolution — for the reason it gives: text staying readable is worth
/// more than motion staying smooth.
const FPS_RUNGS: &[u16] = &[60, 30, 20, 15, 10, 5];

/// The sharpnesses a controller will settle for when the link is short of
/// bandwidth.
///
/// **The middle rung of the degradation order**, and for a long time the one
/// nothing could reach: a controller could ask for fewer frames and smaller
/// ones and had no way to ask for cheaper ones, so a session short of
/// bandwidth skipped from framerate straight to resolution. Resolution is the
/// most damaging lever there is for the thing this product is mostly used
/// for, which is reading somebody's screen — a glyph at half the pixels is
/// not a softer glyph, it is a different one.
/// Queueing delay at which the link is judged to be filling.
///
/// **Above the baseline, not absolute.** A satellite link with a 600 ms round
/// trip and no queue at all is a fine link, and an absolute trigger would
/// spend its framerate for nothing. What matters is the round trip *growing*,
/// which is a queue with this session's own bytes in it.
///
/// A tenth of a second, because that is roughly where an interactive session
/// stops feeling like one — and because a queue that deep is already holding
/// three frames nobody will ever see.
const BLOAT: std::time::Duration = std::time::Duration::from_millis(100);

/// And the queueing delay at which it is judged to have drained.
///
/// Lower than [`BLOAT`] on purpose. Stepping back up at the same threshold
/// that stepped down is how a controller oscillates: it would raise the
/// framerate, refill the queue, lower it, and spend the session doing that.
const CALM: std::time::Duration = std::time::Duration::from_millis(30);

/// How many windows of round trips the baseline is the smallest of.
///
/// A sliding minimum rather than the session's: ten seconds is long enough
/// that a queue cannot hide inside it, and short enough that a session which
/// *began* with somebody else's download in the queue learns the truth once
/// it drains.
const BASELINE_WINDOWS: usize = 10;

/// What to assume the round trip is until something measures it.
///
/// Conservative: a caller that never says is one whose link is unknown, and
/// on an unknown link the storm is the worse failure.
const UNKNOWN_ROUND_TRIP: std::time::Duration = std::time::Duration::from_millis(200);

/// What this build can decode.
///
/// Stated rather than assumed, so the check below is already right on the day
/// a second codec arrives — and so the list a controller *offers* and the list
/// it will *accept* are the same list rather than two that can drift.
pub const DECODES: &[esper_proto::Codec] = &[esper_proto::Codec::H264];

/// The floor, so a link measured at nothing still paces.
///
/// Rarely the binding constraint, because the pace also includes the time to
/// make a frame — but a stream announcing a very high framerate is still
/// entitled to a floor.
const SHORTEST_PACE: std::time::Duration = std::time::Duration::from_millis(20);

/// The ceiling, so a bad measurement cannot leave a session black.
const LONGEST_PACE: std::time::Duration = std::time::Duration::from_secs(2);

/// The tighter of two limits, when either may be absent.
fn tighter<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

/// How long between frames at an announced framerate.
///
/// A ceiling on the rate is a floor on the interval, which is the direction
/// that matters here: the question is the *soonest* a keyframe could arrive.
/// A framerate of zero is not well-formed, but arithmetic that divides by it
/// would be, so it falls back to the slowest interval this cares about.
fn frame_interval(max_fps: u16) -> std::time::Duration {
    if max_fps == 0 {
        return LONGEST_PACE;
    }
    std::time::Duration::from_secs(1) / u32::from(max_fps)
}

/// Running totals, for the stats a session reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControllerStats {
    pub datagrams: u64,
    pub bytes: u64,
    pub pictures: u64,
    pub frames_lost: u64,
    pub frames_skipped: u64,
    pub keyframes_requested: u64,
    /// How many times the host has narrowed what this session may do.
    pub narrowings: u64,
    /// Of those, the ones asked for because nothing was arriving.
    ///
    /// Counted separately because it is the diagnostic for the state that used
    /// to be unrecoverable: a keyframe outstanding on a link that has gone
    /// quiet. One or two is a lost keyframe recovered. A number that climbs
    /// without a picture ever appearing is a host that is not answering.
    pub keyframes_asked_into_silence: u64,
    /// Datagrams refused before reassembly: wrong stream, wrong type,
    /// malformed.
    pub rejected: u64,
}

/// The receiving half of a media session.
/// The most pointer messages held for a caller that has not drained them.
///
/// A pointer produces at most a hundred and twenty-five positions a second and
/// a caller drains them every time round its loop, so this is reached only by
/// a caller that has stopped looking — and then the *oldest* position is the
/// one worth dropping.
const MAX_PENDING_CURSOR: usize = 256;

/// How often the link's measured capacity is reported to the host.
///
/// The estimate is a median over a window of frames, so it drifts a little
/// every frame; half a second is often enough for a host to follow a link that
/// changed and rare enough that the reporting is not itself traffic.
const REPORT_EVERY: std::time::Duration = std::time::Duration::from_millis(500);

/// How long to wait, with a keyframe outstanding and nothing arriving, before
/// asking again.
///
/// **Silence is evidence too**, and where a still screen is concerned it is
/// the only evidence there is. Every other ask in this file is triggered by
/// something *arriving* — a frame lost, or a whole frame turning up while a
/// keyframe is still outstanding — which is sound while frames are flowing and
/// useless the moment they stop. A host whose screen is not changing sends
/// nothing at all, so there is no arrival left to notice the gap with, and a
/// controller that had already asked would wait for ever.
///
/// Half a second is two numbers at once: about the longest a person will hold
/// a stale picture without deciding the session is broken, and slow enough
/// that a genuinely dead link costs two tiny messages a second rather than
/// thirty.
const ASK_AGAIN_INTO_SILENCE: std::time::Duration = std::time::Duration::from_millis(500);

/// How far the measurement has to move, in per cent, before it is worth saying.
///
/// Ten. The estimate is accurate to 8–15%, so anything tighter would be
/// reporting the noise.
const REPORT_WHEN_OFF_BY: u64 = 10;

/// One encoded frame, kept for a recording.
///
/// **The bytes that arrived, not the picture that came out of them.** A
/// recording made from decoded pictures would have to encode them again —
/// slower, larger and worse-looking than the stream it already has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    pub bitstream: Vec<u8>,
    pub keyframe: bool,
}

pub struct ControllerStream {
    depacketizer: Depacketizer,
    decoder: Option<Box<dyn VideoDecoder>>,
    /// The size the host said it would send, so a picture of another size is
    /// noticed rather than silently rendered.
    expected: (u32, u32),
    /// A keyframe has been asked for and has not arrived.
    ///
    /// Coalesces the asking: a burst of loss produces one request.
    awaiting_keyframe: bool,
    /// Whether the host last said the picture was held, if it has said.
    ///
    /// `None` once a caller has taken it. Like the narrowing, held until
    /// taken: a viewer that missed a pause would show a frozen picture with
    /// nothing to explain it, which is what a dead link looks like.
    paused: Option<bool>,
    /// The sound the host has announced, if it has.
    ///
    /// `None` once a caller has taken it, and taken rather than reported for
    /// the same reason as the two above: opening a speaker is the caller's
    /// business, and a viewer that missed the announcement would receive
    /// packets for a stream it never built a decoder for.
    audio: Option<esper_proto::AudioConfig>,
    /// The permission set the host last said this session holds.
    ///
    /// `None` once a caller has taken it. See
    /// [`ControllerStream::take_permissions`].
    granted_now: Option<Vec<esper_proto::Permission>>,
    /// How many times each resolution rung has lost frames.
    ///
    /// **The ladder's memory, and what stops it oscillating.** Down fast and
    /// up slow is not enough on a link whose buffer cannot hold one frame: the
    /// size sequence there reads `480 → 320 → 480 → 320`, and every climb
    /// floods the link again, so the queue never drains and no keyframe ever
    /// arrives whole. Measured: **no picture at all in fifteen seconds**.
    ///
    /// A rung that has cost frames twice is not asked for again. That cannot
    /// affect a session which is not losing frames — a fast link never touches
    /// this — and it is what lets a thin link settle instead of thrashing.
    failures: [u8; RUNGS.len()],
    /// The largest size the ladder may still ask for, as an index into
    /// [`RUNGS`]. Grows only downwards in size, never back up.
    floor: usize,
    /// Which rung was in force when the window being judged began.
    ///
    /// **Not the one in force now.** The first version blamed the current
    /// rung, and after a collapse that is the *bottom* one — which then took
    /// the blame for losses still arriving from the flood that caused the
    /// collapse. Two such windows and the bottom rung was marked as failing,
    /// the floor pinned there, and the session could never climb again:
    /// measured, three runs in three, a session that lost frames for one
    /// second stayed at 320x180 for the remaining forty.
    rung_when_window_began: Option<usize>,
    /// The rung the session was at before its first collapse, so the climb has
    /// something to aim at rather than walking.
    before_the_collapse: Option<usize>,
    /// The rung the last upward probe asked for, if it has not yet proved
    /// itself.
    ///
    /// **A probe that fails is closed at once**, rather than after the second
    /// failure an ordinary rung gets. A probe only happens after five clean
    /// windows, so one that immediately loses frames is the link answering
    /// the question the probe asked — and each further attempt costs another
    /// flood. Measured: probing without this took a link with 100 ms of
    /// buffer from 155 pictures to 84, because it kept climbing back into a
    /// size that could not work.
    probing: Option<usize>,
    /// How much of the quality lever the link ladder has spent.
    ///
    /// **The middle rung of the degradation order**, which the link ladder
    /// could reach and never used: the order this product states is framerate,
    /// then quality, then resolution, and the link's loss arm went straight to
    /// resolution — the most damaging lever — on every kind of loss.
    ///
    /// `None` until loss has been seen. See [`Self::spend_quality`].
    softened: Option<Sharpness>,
    /// Bytes that arrived in the current averaging window, and when it began.
    ///
    /// **The estimator that cannot be starved.** Everything else this side
    /// knows about the link is measured from frames that *completed*, and a
    /// link whose buffer cannot hold one frame completes none — so the
    /// capacity stays unknown, the host is never told to pace, it keeps
    /// bursting whole frames at a queue that cannot hold them, and nothing
    /// completes. The measurement requires the thing that is failing.
    ///
    /// What arrived is not that: the bytes are on this side whether or not
    /// they add up to a picture.
    arrived_bytes: u64,
    arrived_since: Option<std::time::Instant>,
    /// The last rate a full window measured, in kbit/s.
    arrival_rate: Option<f64>,
    /// The last arrival rate measured while the link was demonstrably full.
    ///
    /// **Remembered rather than recomputed.** The first version asked whether
    /// the link was full *at the moment somebody read the estimate*, and the
    /// moment that matters most is the end of a session — where the last
    /// window is as likely as not to have been a clean one, so the estimate
    /// declined to answer and the session reported no capacity at all. It
    /// failed the check that asserts an 800 kbit/s link reads as about 800,
    /// one run in three, which is what a condition evaluated at the wrong
    /// instant looks like.
    full_link_rate: Option<f64>,
    /// Frames lost in the window the ladder last judged.
    ///
    /// Kept so the capacity estimate can tell a link that is *full* from one
    /// that simply has little to carry — see [`Self::starved`].
    lost_last_window: u64,
    /// When a frame last completed, for telling a starved session from a quiet
    /// one.
    last_whole_frame: Option<std::time::Instant>,
    /// Encoded frames held for a recording, when one is being made.
    ///
    /// **Off unless asked for**, so a session that is not being recorded
    /// carries no cost at all: the bytes are moved into this only when
    /// [`ControllerStream::keep_encoded`] has been called.
    kept: Vec<Encoded>,
    keeping: bool,
    /// What the outstanding ask gave as its reason.
    ///
    /// Kept so that asking again into silence repeats the original reason
    /// rather than inventing one. The host logs it, and a session that joined
    /// and never got its first picture is a different fault from one that lost
    /// sync — reporting the second would send whoever reads the log looking
    /// for a loss that never happened.
    asked_for: KeyframeReason,
    /// What the link's round trip was measured at.
    /// See [`ControllerStream::set_round_trip`].
    round_trip: std::time::Duration,
    /// The soonest the host could produce the next frame, from the framerate
    /// it announced. Half of the pace, and the half a LAN does not supply.
    frame_interval: std::time::Duration,
    /// The framerate the host announced, which is where the bloat ladder
    /// starts from.
    ///
    /// **Stale once this side has asked for less, on purpose.** A host that
    /// lowered its framerate on request does not re-announce: an announcement
    /// is a reconfiguration, and a reconfiguration restarts reassembly and
    /// costs a keyframe — which on the link that provoked the ask is the most
    /// expensive thing there is. So this stays at what the stream opened with,
    /// and it is only ever a starting point: once [`Self::fps_rung`] is set
    /// the ladder steps from that.
    announced_fps: u16,
    last_ask: Option<std::time::Instant>,
    /// The link ladder: which rung of [`RUNGS`] has been asked for, if any,
    /// and what the window of loss looks like so far.
    rung: Option<usize>,
    window_ends: Option<std::time::Instant>,
    lost_when_window_began: u64,
    /// Bytes of the frame being assembled, excluding its first datagram, and
    /// the fastest burst seen in this window. See [`ControllerStream::note_burst`].
    burst_bytes: u64,
    /// The fastest burst in each of the last few windows, so one quiet window
    /// does not lose what the link is known to carry.
    burst: [Option<f64>; BURSTS_REMEMBERED],
    burst_at: usize,
    clean_windows: u32,
    /// The bloat ladder: which rung of [`FPS_RUNGS`] has been asked for, the
    /// smallest round trip seen in each of the last [`BASELINE_WINDOWS`]
    /// windows, and how many windows the queue has been drained for.
    fps_rung: Option<usize>,
    rtt_this_window: Option<std::time::Duration>,
    baseline: [Option<std::time::Duration>; BASELINE_WINDOWS],
    baseline_at: usize,
    calm_windows: u32,
    /// What a *caller* asked for, kept apart from what the ladders ask for.
    ///
    /// The host replaces both halves of a hint each time it gets one, so every
    /// hint has to carry everything this side currently wants. Keeping the
    /// caller's wishes separately is what lets the two ladders and the viewer
    /// each ask for something without wiping the others out — the same
    /// two-value shape the host uses for its owner's caps, one level up.
    wanted_fps: Option<u16>,
    wanted_edge: Option<u32>,
    wanted_profile: Profile,
    wanted_sharpness: Option<Sharpness>,
    /// Requests for the caller to send.
    outbound: Vec<MediaMessage>,
    /// What the host has said about the pointer, for the caller to act on.
    cursor: Vec<esper_proto::CursorMessage>,
    /// What the host was last told the link carries, in kbit/s.
    reported: Option<u32>,
    /// When it was told, so the cadence is a cadence rather than a burst.
    reported_when: Option<std::time::Instant>,
    /// When to stop waiting for a frame.
    ///
    /// Owned here rather than left to the caller, because a caller that has to
    /// invent the number invents the wrong one — and did: the first version of
    /// the command-line viewer hardcoded a hundred milliseconds, which is a
    /// quarter of a second of stutter at 60 fps and one copy per caller.
    jitter: Jitter,
    stats: ControllerStats,
}

impl std::fmt::Debug for ControllerStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControllerStream")
            .field("ssrc", &self.depacketizer.ssrc())
            .field("expected", &self.expected)
            .field("awaiting_keyframe", &self.awaiting_keyframe)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl ControllerStream {
    /// Start receiving the stream a host announced.
    ///
    /// The config is checked rather than trusted: it is the first thing a peer
    /// says about the shape of what follows, and everything downstream sizes
    /// buffers from it.
    pub fn new(config: &StreamConfig) -> Result<Self, StreamError> {
        if !config.is_well_formed() {
            return Err(StreamError::Malformed {
                what: "a stream config",
            });
        }
        let mut me = Self {
            depacketizer: Depacketizer::new(config.ssrc),
            decoder: None,
            expected: (config.width, config.height),
            awaiting_keyframe: true,
            failures: [0; RUNGS.len()],
            floor: 0,
            rung_when_window_began: None,
            before_the_collapse: None,
            probing: None,
            softened: None,
            arrived_bytes: 0,
            arrived_since: None,
            arrival_rate: None,
            full_link_rate: None,
            lost_last_window: 0,
            last_whole_frame: None,
            kept: Vec::new(),
            keeping: false,
            asked_for: KeyframeReason::Joined,
            granted_now: None,
            paused: None,
            audio: None,
            round_trip: UNKNOWN_ROUND_TRIP,
            frame_interval: frame_interval(config.max_fps),
            announced_fps: config.max_fps,
            last_ask: None,
            rung: None,
            window_ends: None,
            lost_when_window_began: 0,
            burst_bytes: 0,
            burst: [None; BURSTS_REMEMBERED],
            burst_at: 0,
            clean_windows: 0,
            fps_rung: None,
            rtt_this_window: None,
            baseline: [None; BASELINE_WINDOWS],
            baseline_at: 0,
            calm_windows: 0,
            wanted_fps: None,
            wanted_edge: None,
            wanted_profile: Profile::Balanced,
            wanted_sharpness: None,
            outbound: Vec::new(),
            cursor: Vec::new(),
            reported: None,
            reported_when: None,
            jitter: Jitter::new(config.max_fps, config.clock_hz),
            stats: ControllerStats::default(),
        };
        // A fresh receiver has no reference picture, so say so immediately
        // rather than waiting for the host's periodic keyframe.
        // Joining is not loss recovery: there is nothing to show until a
        // keyframe arrives, so this one is never paced.
        me.ask_now(KeyframeReason::Joined);
        Ok(me)
    }

    /// Attach a decoder.
    ///
    /// Separate from construction so a caller can choose: a controller
    /// decoding a peer's bitstream must use the sandboxed decoder, and a
    /// loopback checking its own output may not need one at all. Leaving that
    /// to whoever wires it up is deliberate — the alternative is this crate
    /// choosing, and choosing wrongly for the case that matters.
    #[must_use]
    pub fn with_decoder(mut self, decoder: Box<dyn VideoDecoder>) -> Self {
        self.decoder = Some(decoder);
        self
    }

    /// Running totals.
    #[must_use]
    pub const fn stats(&self) -> ControllerStats {
        self.stats
    }

    /// How long to wait before deciding the frame in hand is complete.
    ///
    /// `None` when nothing is waiting, so a caller may block for as long as it
    /// likes. `Some(d)` is how long is left; `Some(ZERO)` means now.
    ///
    /// A caller polls with this as its timeout and calls [`Self::tick`] when it
    /// expires. That is the whole protocol between them, and it exists so the
    /// deadline is one reasoned number rather than one per caller.
    #[must_use]
    pub fn due_in(&self, now: std::time::Instant) -> Option<std::time::Duration> {
        // **Both deadlines, not just the buffer's.** A keyframe outstanding
        // with nothing arriving is the one state where this side has work to
        // do and no incoming packet will ever wake it, so a caller told only
        // about the jitter buffer would sleep through the retry.
        match (self.jitter.due_in(now), self.silence_ask_due_in(now)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (only, None) | (None, only) => only,
        }
    }

    /// How long until an unanswered keyframe request is worth repeating.
    ///
    /// `None` when none is outstanding. Never `Some(ZERO)` for longer than one
    /// tick, because the interval is at least the ask pace — a deadline this
    /// side reports as due but [`Self::ask`] then declines to act on would
    /// spin a caller that honours it.
    fn silence_ask_due_in(&self, now: std::time::Instant) -> Option<std::time::Duration> {
        if !self.awaiting_keyframe {
            return None;
        }
        let interval = ASK_AGAIN_INTO_SILENCE.max(self.pace());
        let Some(last) = self.last_ask else {
            return Some(std::time::Duration::ZERO);
        };
        Some(interval.saturating_sub(now.saturating_duration_since(last)))
    }

    /// Ask again for a keyframe that nothing is going to prompt.
    fn ask_into_silence(&mut self, now: std::time::Instant) {
        if self
            .silence_ask_due_in(now)
            .is_none_or(|left| !left.is_zero())
        {
            return;
        }
        self.stats.keyframes_asked_into_silence =
            self.stats.keyframes_asked_into_silence.saturating_add(1);
        self.ask(self.asked_for, now);
    }

    /// Act on the deadline if it has passed.
    ///
    /// Does nothing if it has not, so a caller may call it whenever it likes.
    pub fn tick(&mut self, now: std::time::Instant, out: &mut Vec<Decoded>) {
        if self.jitter.is_due(now) {
            self.jitter.fired(now);
            self.flush_at(now, out);
        }
        // **And the ladders' window is a clock, not an arrival.** It used to
        // advance only when a datagram came in or a deadline fired, so a link
        // that went quiet froze both ladders wherever they happened to be —
        // and a queue can be full while nothing is arriving at all, which is
        // the state the bloat ladder exists for.
        self.step_ladder(now);
        self.ask_into_silence(now);
        self.say_what_the_link_carries(now);
    }

    /// Tell the host what this end has measured the link at.
    ///
    /// **The sending end cannot measure this**, so it has to be told. See
    /// [`esper_proto::LinkReport`], and [`ControllerStream::link_capacity`] for
    /// how a frame's arrival spread becomes a rate.
    ///
    /// Sent on a cadence rather than on every change: the estimate is a median
    /// over a window of frames, so it moves a little every frame and a message
    /// per movement would be a message per frame for a number that barely
    /// changed. A report that is *withdrawn* — the estimate going away — is
    /// sent immediately, because a host pacing on a number nothing stands
    /// behind is the one state worth leaving quickly.
    fn say_what_the_link_carries(&mut self, now: std::time::Instant) {
        let measured = self.link_capacity().and_then(|carried| {
            let rounded = carried.kbit.round();
            (rounded > 0.0).then_some(rounded as u32)
        });
        let withdrawn = measured.is_none() && self.reported.is_some();
        let due = self
            .reported_when
            .is_none_or(|then| now.saturating_duration_since(then) >= REPORT_EVERY);
        if !withdrawn && !due {
            return;
        }
        // **Only when it has actually moved**, past the noise a median over a
        // sliding window always has. A host given the same number again pays
        // for a message and changes nothing.
        let moved = match (measured, self.reported) {
            (Some(now_kbit), Some(was)) => {
                let bigger = now_kbit.max(was);
                u64::from(now_kbit.abs_diff(was)) * 100 > u64::from(bigger) * REPORT_WHEN_OFF_BY
            }
            (a, b) => a != b,
        };
        if !moved {
            return;
        }
        let report = LinkReport {
            capacity_kbit: measured,
        };
        if !report.is_well_formed() {
            return;
        }
        self.reported = measured;
        self.reported_when = Some(now);
        // One outstanding is enough: the host keeps the latest, and a queue of
        // superseded measurements is a queue of stale numbers.
        self.outbound
            .retain(|m| !matches!(m, MediaMessage::Report(_)));
        self.outbound.push(MediaMessage::Report(report));
    }

    /// Keep the encoded frames, for a recording.
    ///
    /// Off by default: a session nobody is recording pays nothing for this.
    pub const fn keep_encoded(&mut self, keeping: bool) {
        self.keeping = keeping;
    }

    /// Take the encoded frames kept since the last call.
    ///
    /// **Drained rather than read**, so a caller that stops draining cannot
    /// grow this without bound: the frames are gone from here once they are
    /// somebody else's problem, and a recording that stopped being written is
    /// one that stops holding memory.
    pub fn take_encoded(&mut self) -> Vec<Encoded> {
        std::mem::take(&mut self.kept)
    }
    /// What the arrivals have looked like.
    #[must_use]
    pub fn jitter(&self) -> JitterStats {
        self.jitter.stats()
    }

    /// Interarrival jitter in milliseconds.
    #[must_use]
    pub fn jitter_ms(&self) -> f64 {
        self.jitter.jitter_ms()
    }

    /// The reassembler's own totals.
    #[must_use]
    pub fn media_stats(&self) -> esper_media::depacketize::Stats {
        self.depacketizer.stats()
    }

    /// Whether a keyframe is outstanding.
    #[must_use]
    pub const fn awaiting_keyframe(&self) -> bool {
        self.awaiting_keyframe
    }

    /// The size the host said it would send.
    #[must_use]
    pub const fn expected_size(&self) -> (u32, u32) {
        self.expected
    }

    /// Messages the caller should send to the host, and clears them.
    ///
    /// Drained rather than sent from here, because this crate does not know
    /// how bytes leave the machine — and because a caller may want to batch
    /// them with whatever else it is about to send.
    pub fn take_outbound(&mut self) -> Vec<MediaMessage> {
        std::mem::take(&mut self.outbound)
    }

    /// Ask the host for a keyframe, unless one is already outstanding or one
    /// was asked for too recently.
    ///
    /// # This is the whole recovery policy, and it has three rules
    ///
    /// A controller that asked per lost frame would make the host send nothing
    /// but keyframes — the largest and most expensive frames there are —
    /// turning a lossy link into an unusable one.
    ///
    /// **One unsent request outstanding.** A request still in the queue
    /// absorbs the rest of a burst, and a new one becomes possible once the
    /// caller has sent it.
    ///
    /// **And not more than one per [`ControllerStream::pace`].**
    /// The queue rule alone is only as good as how often the caller drains it:
    /// on a link with a long round trip it drains immediately, so every lost
    /// frame becomes another request while the first keyframe is still in
    /// flight — and each of those produces a keyframe, which is large, which
    /// is more likely to lose a datagram, which produces another request.
    ///
    /// The first measurements of this claimed the pace was what stood between
    /// a session and a collapse. They were taken against a decoder worker that
    /// gave up permanently on the first frame it could not decode, so they
    /// were a coin flip rather than a reading, and the collapse was mostly
    /// that. What the pace demonstrably does is bound the refusal-driven ask:
    /// a frame that arrives whole and cannot be decoded asks too, so without a
    /// pace a session in trouble asks once per arriving frame.
    ///
    /// **Never "one until a keyframe arrives".** That was the first attempt
    /// and it deadlocks: on a link bad enough to lose the keyframes as well, a
    /// controller that had already asked would never ask again and the session
    /// would stay black for good. The pace expires, so another ask always
    /// becomes possible.
    /// Ask now, whatever the pace says.
    ///
    /// For the two reasons that are not loss recovery — joining a stream and a
    /// reconfiguration — where there is nothing to show until a keyframe
    /// arrives and pacing would only make the wait longer.
    fn ask_now(&mut self, reason: KeyframeReason) {
        self.last_ask = None;
        self.ask(reason, std::time::Instant::now());
    }

    fn ask(&mut self, reason: KeyframeReason, now: std::time::Instant) {
        let queued = self
            .outbound
            .iter()
            .any(|m| matches!(m, MediaMessage::Keyframe(_)));
        if queued {
            return;
        }
        if let Some(last) = self.last_ask
            && now.saturating_duration_since(last) < self.pace()
        {
            return;
        }
        self.last_ask = Some(now);
        self.awaiting_keyframe = true;
        self.asked_for = reason;
        self.stats.keyframes_requested = self.stats.keyframes_requested.saturating_add(1);
        self.outbound
            .push(MediaMessage::Keyframe(KeyframeRequest { reason }));
    }

    /// What a frame's arrival said about the link's capacity.
    ///
    /// # Why a burst is the only honest measurement of a link
    ///
    /// Everything else a receiver can count measures **demand**. Bytes per
    /// second is what the sender chose to send; frames per second likewise.
    /// On a link with room those numbers say nothing about the link at all,
    /// which is the trap five separate attempts at pacing fell into — pacing
    /// to any of them is a rate limit in disguise, and a rate limit on a link
    /// with room costs exactly the frames it saves on one without.
    ///
    /// A frame is different, because a sender hands its datagrams over all at
    /// once. Whatever spread they arrive with is the link serialising them,
    /// so `bytes / spread` is the rate the link *can* do rather than the rate
    /// it was asked for. On a fast link a frame lands in a fraction of a
    /// millisecond and this reads in the hundreds of megabits; on a link
    /// carrying 800 kbit/s a five-kilobyte keyframe occupies fifty
    /// milliseconds and it reads 800.
    ///
    /// The largest reading wins, because a burst can be *slowed* by things
    /// that are not the link — this side being descheduled, a frame arriving
    /// while another is decoding — and nothing can make one arrive faster
    /// than the link carried it.
    /// Count a datagram, and close the averaging window when it is full.
    ///
    /// **Bytes on the wire, not bytes in a picture.** This is deliberately the
    /// crudest measurement in the file: it cannot tell a frame from a
    /// fragment, and that is the point — it keeps working in the one situation
    /// where every other measurement here has nothing to work with.
    fn note_arrival(&mut self, bytes: u64, now: std::time::Instant) {
        let since = *self.arrived_since.get_or_insert(now);
        self.arrived_bytes = self.arrived_bytes.saturating_add(bytes);
        let elapsed = now.saturating_duration_since(since);
        if elapsed >= ARRIVAL_WINDOW {
            let seconds = elapsed.as_secs_f64();
            if seconds > 0.0 {
                self.arrival_rate = Some(self.arrived_bytes as f64 * 8.0 / 1000.0 / seconds);
            }
            self.arrived_bytes = 0;
            self.arrived_since = Some(now);
        }
    }

    /// Whether frames are arriving in pieces rather than not at all.
    ///
    /// The distinction the fallback rests on: a session with **nothing to
    /// send** and a session whose frames **cannot fit** look the same to a
    /// capacity estimator that only counts bytes. This one asks whether bytes
    /// are arriving *and* no frame has completed for a while, which only the
    /// second can be.
    fn starved(&self, now: std::time::Instant) -> bool {
        if self.arrival_rate.unwrap_or(0.0) <= 0.0 {
            return false;
        }
        // **Frames being lost is the same evidence.** The guard on this
        // estimate is that the link must be *full* — otherwise what arrived is
        // demand, not capacity, and reporting it would pace the host down to
        // whatever it happened to send. Nothing completing proves the link is
        // full; so does losing frames, and the second is what a session looks
        // like once it has collapsed to small frames.
        //
        // Which it now does: the ladder's first losing window drops to the
        // smallest rung, and there every frame is under the four kilobytes a
        // burst has to be to be worth timing — so the accurate estimator has
        // nothing to work with and the link went unmeasured. Caught by the
        // check that asserts an 800 kbit/s link reads as about 800.
        if self.lost_last_window >= LOSSES_TO_STEP_DOWN {
            return true;
        }
        match self.last_whole_frame {
            Some(when) => now.saturating_duration_since(when) >= STARVED_AFTER,
            // Nothing has ever completed. The session has been running for at
            // least one averaging window by the time there is a rate at all.
            None => true,
        }
    }

    fn note_burst(&mut self, bytes: u64, spread: std::time::Duration) {
        // **Only frames big enough to have occupied the link.**
        //
        // A small frame measures nothing: two datagrams that happen to be
        // handed over together arrive together, and `bytes / spread` for them
        // reads in the gigabits. Measured on a link carrying 800 kbit/s, the
        // frames over four kilobytes read 763 to 886 — within a few per cent
        // of the truth every time — while the whole population had a maximum
        // of 5.7 Gbit/s.
        //
        // Four kilobytes is about three datagrams, and on any link slow
        // enough for this to matter it is milliseconds of wire.
        if bytes < SMALLEST_BURST_WORTH_TIMING || spread.is_zero() {
            return;
        }
        let kbit = bytes as f64 * 8.0 / 1000.0 / spread.as_secs_f64();
        if let Some(slot) = self.burst.get_mut(self.burst_at) {
            *slot = Some(kbit);
            self.burst_at = self.burst_at.saturating_add(1) % BURSTS_REMEMBERED;
        }
    }

    /// How fast this link carried a burst, in kbit/s, over the recent past.
    ///
    /// `None` before any frame has arrived in more than one piece: a session
    /// whose frames all fit in a single datagram has never asked the link for
    /// anything at once, so nothing has measured it.
    /// **The middle reading, not the best one.** Both tails lie: a frame can
    /// be *slowed* by this side being descheduled, and it can appear
    /// impossibly fast when its datagrams were handed over together. Measured
    /// on a link carrying 800 kbit/s, the readings from frames worth timing
    /// ran 763 to 886 while the maximum over all of them was 5.7 Gbit/s.
    #[must_use]
    pub fn link_capacity(&self) -> Option<Capacity> {
        self.capacity_at(std::time::Instant::now())
    }

    /// The same, at a stated instant, so a test can say when it is asking.
    #[must_use]
    pub fn capacity_at(&self, now: std::time::Instant) -> Option<Capacity> {
        let mut seen: Vec<f64> = self.burst.iter().flatten().copied().collect();
        if seen.len() >= ENOUGH_BURSTS_TO_JUDGE {
            seen.sort_by(f64::total_cmp);
            return seen.get(seen.len() / 2).copied().map(Capacity::timed);
        }
        // **The fallback, and only while frames are failing.**
        //
        // The measurement above needs frames that *completed*, and the one
        // link regime that matters most completes none: a buffer too short to
        // hold one frame drops the tail of every burst, so nothing is ever
        // timed, the host is never told to pace, and it goes on bursting. The
        // measurement requires the thing that is failing.
        //
        // What arrived is a lower bound on what the link carries, and a lower
        // bound is exactly what a pacer wants. It is **not** used when frames
        // are completing: an idle screen sends almost nothing, and reporting
        // "the link carries what I received" for a session with nothing to
        // send would pace the host down to the trickle it was already
        // sending — the opposite mistake, and a worse one.
        if self.starved(now) {
            return self.arrival_rate.map(Capacity::at_least);
        }
        // And otherwise what was measured while the link *was* full, which is
        // the answer a session reports once it has finished struggling.
        self.full_link_rate.map(Capacity::at_least)
    }

    /// Move the link ladder if a window has closed.
    ///
    /// **The quality ladder the host runs answers a different question.** It
    /// spends framerate, then quality, then resolution when the *CPU* cannot
    /// keep up, and it cannot see the link at all: every frame it produces
    /// leaves the machine. Only the controller knows how many arrived.
    ///
    /// So this is the other half, and it moves the only lever that changes
    /// whether loss is survivable — frame size. Down after a window that lost
    /// frames, up after several that did not, and never above what the host
    /// announced it was sending.
    /// Come back up, halving the distance to where the session fell from.
    ///
    /// # Why not one rung at a time
    ///
    /// Because the fall is not one rung. The first losing window collapses
    /// straight to the smallest size, and walking back from there takes a rung
    /// every five clean windows — twenty seconds to undo one bad second, on a
    /// link that was fine again after the first of them. Measured before this:
    /// a session that lost frames for one second never regained its full size
    /// in the forty that followed.
    ///
    /// So the climb aims at the rung the session fell from and covers half the
    /// remaining distance each time: from the bottom of five rungs that is
    /// two steps rather than four, and each step is still gated on the same
    /// clean windows, so a link that cannot carry the larger size says so
    /// before the next one.
    ///
    /// A rung that has already failed twice is never asked for again — see
    /// [`Self::note_failure`] — which is what stops this becoming the
    /// oscillation the collapse exists to prevent.
    fn probe_upward(&mut self) -> bool {
        let Some(current) = self.rung else {
            return false;
        };
        let aim = self.before_the_collapse.unwrap_or(0).max(self.floor);
        if current <= aim {
            return false;
        }
        // Halfway, rounded towards the aim so the last step always arrives.
        let next = current - (current - aim).div_ceil(2);
        if next == current {
            return false;
        }
        self.rung = Some(next);
        self.probing = Some(next);
        self.say_what_we_want();
        true
    }

    /// Remember that the size in force lost frames, and stop climbing back to
    /// it once it has done so twice.
    ///
    /// Twice rather than once: a single window of loss can be one bad moment
    /// on an otherwise fine link, and a ladder that gave up a rung for that
    /// would spend a good session creeping downwards.
    fn note_failure(&mut self, judged: Option<usize>) {
        let at = judged.unwrap_or(0);
        // **Never close the smallest rung.** There is nothing below it, so a
        // floor there means the session can never come back up — which is
        // exactly what happened, and what this argument now prevents.
        if at >= RUNGS.len().saturating_sub(1) {
            return;
        }
        // A rung the session was probing is closed on its first failure: see
        // `probing`.
        let closes_now = self.probing == Some(at);
        if let Some(count) = self.failures.get_mut(at) {
            *count = count.saturating_add(1);
            if (closes_now || *count >= FAILURES_THAT_CLOSE_A_RUNG) && self.floor < at {
                self.floor = at.saturating_add(1).min(RUNGS.len().saturating_sub(1));
            }
        }
    }

    /// Whether the round trip says the link's queue is *not* filling up.
    ///
    /// `false` when there is no baseline yet: without one there is nothing to
    /// compare against, and guessing "flat" would spend the quality lever on
    /// an overload, which is measured to be the wrong answer.
    ///
    /// The threshold is [`BLOAT`], the same queueing delay the bloat ladder
    /// steps on — used here for the opposite purpose, to establish that the
    /// queue is *not* what went wrong, which is what makes the loss mean
    /// something else.
    fn queue_is_flat(&self) -> bool {
        let Some(floor) = self.baseline_round_trip() else {
            return false;
        };
        self.round_trip.saturating_sub(floor) < BLOAT
    }

    /// Ask for a coarser picture, one rung, saying whether there was one left.
    ///
    /// Coarser means **fewer bytes in every frame**, which is what a link that
    /// cannot carry one frame needs. It is capped at `Soft`: below that is not
    /// a rung this protocol has, and a request may only ever coarsen what the
    /// host chose — see [`Self::want_sharpness`].
    fn spend_quality(&mut self) -> bool {
        let next = match self.softened {
            None => Sharpness::Soft,
            // Already as soft as the protocol can ask for.
            Some(Sharpness::Soft) => return false,
            Some(_) => Sharpness::Soft,
        };
        self.softened = Some(next);
        self.wanted_sharpness = Some(next);
        self.say_what_we_want();
        true
    }

    /// Give back a rung of quality, saying whether there was one to give.
    fn return_quality(&mut self) -> bool {
        if self.softened.take().is_none() {
            return false;
        }
        // Back to no request at all rather than to `Balanced`: a hint may only
        // coarsen, so asking for balanced and asking for nothing are the same
        // ask, and the second says what is meant.
        self.wanted_sharpness = None;
        self.say_what_we_want();
        true
    }

    fn step_ladder(&mut self, now: std::time::Instant) {
        let Some(ends) = self.window_ends else {
            self.window_ends = Some(now + WINDOW);
            self.lost_when_window_began = self.stats.frames_lost;
            return;
        };
        if now < ends {
            return;
        }
        let lost = self
            .stats
            .frames_lost
            .saturating_sub(self.lost_when_window_began);
        let judged = self.rung_when_window_began;
        self.lost_last_window = lost;
        // **Remembered here, while the evidence is present** — and only when
        // the loss means what this is about to read into it.
        //
        // A window that lost frames *with the round trip growing* is a window
        // in which the link was full, so what arrived during it is what the
        // link carries. A window that lost frames with the round trip flat is
        // a buffer too short to hold one frame, and there the rate was never
        // the problem: what arrived is what the host happened to be offering
        // after the collapse, which on a link with plenty of rate is a small
        // fraction of it.
        //
        // Recording it anyway was measured: on an 800 kbit/s link with 40
        // packets of queue, the session reported a capacity of 398 kbit/s
        // against the 337 it was carrying — a number the host then paces to,
        // which holds the session down at the size that produced it. The two
        // failures are already told apart a few lines below, for the same
        // reason and with the same test.
        if lost >= LOSSES_TO_STEP_DOWN
            && !self.queue_is_flat()
            && let Some(rate) = self.arrival_rate
            && rate > 0.0
        {
            self.full_link_rate = Some(rate);
        }
        self.window_ends = Some(now + WINDOW);
        self.lost_when_window_began = self.stats.frames_lost;
        self.rung_when_window_began = self.rung;

        if lost >= LOSSES_TO_STEP_DOWN {
            self.clean_windows = 0;
            self.note_failure(judged);
            // **Which lever, decided by whether the queue is growing.**
            //
            // Two link failures arrive here looking the same in the loss
            // count, and they want opposite things:
            //
            // - A **queue filling up** is an overload. Frames have to get
            //   smaller quickly, and the measurement below is unambiguous
            //   about that: making resolution wait cost a session everything
            //   it had (26-36 pictures to none) under a five-times overload,
            //   because one rung a window is not a policy for that size.
            // - A **buffer too short to hold one frame** is not an overload at
            //   all. The round trip never moves, resolution cannot fix it —
            //   the session says so in as many words at the end — and what
            //   *does* fix it is fewer bytes in the frame. Measured: at
            //   320x180 the coarsest quantiser takes a keyframe from 5 449
            //   bytes to 3 291 and a delta from 264 to 127.
            //
            // The round trip against its own floor is what tells them apart,
            // and this side has both numbers. So: a flat round trip spends
            // **quality** first, which is the order this product states; a
            // growing one keeps spending resolution, which is what the
            // measurement says that case needs.
            if self.queue_is_flat() && self.spend_quality() {
                self.step_bloat_ladder();
                return;
            }
            // **The first descent is a collapse, not a step.**
            //
            // Walking down one rung a window is a policy for a link that is
            // slightly too slow. It is not one for a link that cannot carry
            // the size a session *opens* at: measured on a link with 100 ms of
            // buffer at 200 kbit/s the walk took four seconds, and by the time
            // it reached the bottom the queue was so far behind that no
            // keyframe ever arrived whole — **no picture at all in fifteen
            // seconds**, while the same link pinned at the bottom rung
            // delivered 362.
            //
            // So the first losing window goes straight to the smallest rung,
            // and the ordinary "up slow" climb brings it back as the link
            // proves itself. A session that never loses a frame never comes
            // here at all, which is what makes this cost nothing on a good
            // link — unlike capping what a session *opens* at, which was tried
            // and charged every session on every link three seconds of soft
            // picture for the benefit of the few that need it.
            if self.rung.is_none() {
                self.before_the_collapse = Some(0);
                self.rung = Some(RUNGS.len().saturating_sub(1));
                self.say_what_we_want();
                self.step_bloat_ladder();
                return;
            }
            // **And it takes frame size even when the queue is what filled,
            // which is not what the degradation order asks for — see the note
            // on [`order_for`].**
            //
            // Tail drop from an overloaded link arrives here looking exactly
            // like the random loss of a bad wireless link, and answering it
            // with the resolution lever spends the most damaging one on a
            // problem that fewer or cheaper frames would have fixed.
            //
            // Suppressing it here was tried and **measured a clear
            // regression**: leaving congestive loss to the ladder below
            // serialised the two responses, so resolution only began to fall
            // in the sixth second while the link was five times overloaded,
            // and a session that had shown 26-36 pictures showed none. One
            // rung per window is not a policy for an overload of that size.
            //
            // So this stays until the response can be *sized* to the
            // overload rather than walked one rung at a time. See the tracker
            // for what that needs.
            self.step(true);
        } else if lost == 0 {
            // The probe has proved itself: a clean window at the size it asked
            // for is the answer it was looking for.
            self.probing = None;
            self.clean_windows = self.clean_windows.saturating_add(1);
            if self.clean_windows >= CLEAN_WINDOWS_TO_STEP_UP {
                self.clean_windows = 0;
                // **Resolution comes back first, then sharpness.** The
                // opposite order of spending them, and deliberately: a bigger
                // soft picture is worth more to somebody reading a screen than
                // a small sharp one, and it is also the cheaper of the two to
                // give back — the quantiser's saving is proportional to every
                // frame, so returning it last keeps the link's headroom for
                // longest.
                if !self.probe_upward() {
                    self.return_quality();
                }
            }
        }

        self.step_bloat_ladder();
    }

    /// Move the bloat ladder, and roll the baseline on.
    ///
    /// **The signal loss cannot give.** A queue filling costs latency long
    /// before it costs a packet: a home uplink with a second of buffer hands
    /// back every byte it is given and simply hands them back a second late.
    /// Measured on a link whose ceiling was below what the host was offering,
    /// with a second of queue: the round trip reached 736 ms, and the session
    /// showed **no pictures at all** in ten seconds. Nothing was lost until
    /// the queue overflowed, by which point every frame in it was stale.
    ///
    /// So this watches the round trip against its own baseline, and spends
    /// framerate — the lever that reduces bytes per second without taking the
    /// picture apart.
    fn step_bloat_ladder(&mut self) {
        let seen = self.rtt_this_window.take();
        let before = self.baseline_round_trip();

        if let Some(least) = seen {
            if let Some(slot) = self.baseline.get_mut(self.baseline_at) {
                *slot = Some(least);
            }
            self.baseline_at = self.baseline_at.saturating_add(1) % BASELINE_WINDOWS;
        }

        // A baseline needs a window that closed before this one. Judging the
        // first window against itself would compare the round trip with its
        // own minimum and never see anything.
        let Some(baseline) = before else {
            return;
        };
        let queued = self.round_trip.saturating_sub(baseline);
        if queued >= BLOAT {
            self.calm_windows = 0;
            self.step_fps(true);
        } else if queued <= CALM {
            self.calm_windows = self.calm_windows.saturating_add(1);
            if self.calm_windows >= CLEAN_WINDOWS_TO_STEP_UP {
                self.calm_windows = 0;
                self.step_fps(false);
            }
        }
    }

    /// One framerate rung down, or one up.
    ///
    /// Starts from what the host announced rather than the top of the list,
    /// for the same reason the size ladder does: a stream already at fifteen
    /// frames a second should not spend three windows asking for thirty.
    fn step_fps(&mut self, down: bool) -> bool {
        let current = self.fps_rung.unwrap_or_else(|| {
            let announced = self.announced_fps;
            FPS_RUNGS
                .iter()
                .position(|r| *r <= announced)
                .unwrap_or(FPS_RUNGS.len().saturating_sub(1))
        });
        let next = if down {
            current
                .saturating_add(1)
                .min(FPS_RUNGS.len().saturating_sub(1))
        } else if self.fps_rung.is_none() {
            // Nothing has been asked for, so there is nothing to relax.
            return false;
        } else {
            current.saturating_sub(1)
        };
        if Some(next) == self.fps_rung {
            return false;
        }
        self.fps_rung = Some(next);
        self.say_what_we_want();
        true
    }

    /// Which framerate the bloat ladder is asking for, if any.
    ///
    /// For saying why a session looks less smooth than it started, which is
    /// otherwise indistinguishable from the host being slow.
    #[must_use]
    pub fn link_fps(&self) -> Option<u16> {
        self.fps_rung.and_then(|r| FPS_RUNGS.get(r).copied())
    }

    /// One rung down, or one up, and tell the host if anything changed.
    fn step(&mut self, down: bool) -> bool {
        // The first step down starts from the size the host is actually
        // sending rather than from the top of the list, so a session already
        // at 640 does not spend three windows asking for sizes larger than it
        // has.
        let current = self.rung.unwrap_or_else(|| {
            let widest = self.expected.0.max(self.expected.1);
            RUNGS
                .iter()
                .position(|r| *r <= widest)
                .unwrap_or(RUNGS.len().saturating_sub(1))
        });
        let next = if down {
            current.saturating_add(1).min(RUNGS.len().saturating_sub(1))
        } else if self.rung.is_none() {
            // Nothing has been asked for, so there is nothing to relax.
            return false;
        } else {
            // **Never back above a rung that has already failed twice.** See
            // `failures`: without this the ladder climbs into the size that
            // just cost it every frame, floods the link and comes back down,
            // for as long as the session lasts.
            current.saturating_sub(1).max(self.floor)
        };
        if Some(next) == self.rung {
            return false;
        }
        self.rung = Some(next);
        if RUNGS.get(next).is_none() {
            return false;
        }
        self.say_what_we_want();
        true
    }

    /// Which resolution the link ladder is asking for, if any.
    ///
    /// For a caller that wants to say so on a screen: a session that has
    /// stepped down should be able to explain why it looks softer.
    #[must_use]
    pub fn link_ceiling(&self) -> Option<u32> {
        self.rung.and_then(|r| RUNGS.get(r).copied())
    }

    /// How often, at most, to ask for a keyframe while a link is losing
    /// frames.
    ///
    /// **A round trip is the right unit**, because that is how long it takes
    /// for an ask to become a picture: the request goes out, the host encodes
    /// a keyframe, and it comes back. Asking again before then is asking for
    /// something already on its way.
    ///
    /// The caller sets it from the link's measured round trip. Bounded at both
    /// ends: too small is the storm this exists to prevent, and too large is a
    /// session that stays black long after the link recovered.
    pub fn set_round_trip(&mut self, rtt: std::time::Duration) {
        self.round_trip = rtt;
        // The smallest seen in this window, which is what the baseline is made
        // of. A minimum rather than a mean, because a mean of a queueing link
        // *is* the queue — the number wanted here is what the path costs when
        // nothing is waiting on it.
        self.rtt_this_window = Some(match self.rtt_this_window {
            Some(least) => least.min(rtt),
            None => rtt,
        });
    }

    /// The round trip this path costs when nothing is queued on it.
    ///
    /// `None` until a window has closed, because one sample is not a baseline
    /// and acting on it would spend framerate on a single slow ack.
    #[must_use]
    pub fn baseline_round_trip(&self) -> Option<std::time::Duration> {
        self.baseline.iter().flatten().copied().min()
    }

    /// How long to wait before asking again.
    ///
    /// **A round trip is half of it, and on a LAN it is the smaller half.**
    /// The first version of this used the round trip alone, which reads
    /// correctly and is wrong wherever the link is fast: on loopback the round
    /// trip is a third of a millisecond, so the pace fell to its 20 ms floor
    /// and a stream at fifteen frames a second could be asked for a keyframe
    /// three times over before it had the chance to make one. Not the storm
    /// the pace exists to prevent — the asks were still bounded by how often
    /// frames were lost — but a pace that does not pace is not one.
    ///
    /// The host answers a request by making the *next* frame a keyframe, so
    /// the wait is the request's flight, plus however long until that frame,
    /// plus the flight back. Round trip plus one frame interval, which is
    /// slightly optimistic — it leaves out the encode — and being optimistic
    /// here costs one extra request rather than a black session.
    ///
    /// **It changed no measured session, and that is worth stating.** An ask
    /// is made when a frame is lost or refused, so the ask rate cannot exceed
    /// the rate frames arrive at — a pace shorter than a frame interval is
    /// indistinguishable from one equal to it, and six-second sessions at 5%
    /// loss ask for the same four to nine keyframes either way. What this
    /// fixes is a pace that permitted asks for something that could not have
    /// arrived yet, and it derives the number from the framerate the host
    /// announced instead of leaning on [`SHORTEST_PACE`], which was an
    /// arbitrary stand-in for exactly this quantity.
    fn pace(&self) -> std::time::Duration {
        self.round_trip
            .saturating_add(self.frame_interval)
            .clamp(SHORTEST_PACE, LONGEST_PACE)
    }

    /// What the *viewer* would like, which only the viewer knows.
    ///
    /// A window half the size of the screen does not need the whole screen's
    /// pixels, and somebody reading a document wants sharpness over motion.
    /// Recorded rather than sent directly, because the host replaces both
    /// halves of a hint every time it receives one — so a hint that carried
    /// only this would wipe out whatever the ladders are asking for, and one
    /// carrying only the ladders' asks would wipe out this.
    pub fn hint(&mut self, max_fps: Option<u16>, max_long_edge: Option<u32>, profile: Profile) {
        self.wanted_fps = max_fps;
        self.wanted_edge = max_long_edge;
        self.wanted_profile = profile;
        self.say_what_we_want();
    }

    /// And how sharp a picture the viewer wants, if it has a view.
    ///
    /// Separate from [`Self::hint`] because it is the one field a viewer
    /// almost never sets: a person does not choose a quantiser, they choose
    /// what they are doing, and the profile says that. This exists for the
    /// case where somebody has decided anyway.
    pub fn want_sharpness(&mut self, sharpness: Option<Sharpness>) {
        self.wanted_sharpness = sharpness;
        self.say_what_we_want();
    }

    /// Send the tighter of everything this side wants.
    ///
    /// **One hint carries every reason.** Three things ask for less — the
    /// viewer, the loss ladder and the bloat ladder — and the host keeps only
    /// the last hint it was sent. So each of them changes what this side
    /// wants, and this says the whole of it.
    ///
    /// A hint, not a demand: the host takes the tighter of this and its own
    /// owner's limits, so nothing here can ask for more than that machine
    /// allowed.
    fn say_what_we_want(&mut self) {
        let hint = QualityHint {
            max_fps: tighter(self.wanted_fps, self.link_fps()),
            max_long_edge: tighter(self.wanted_edge, self.link_ceiling()),
            profile: self.wanted_profile,
            // **The coarser of the two, not the tighter.** Every other field
            // here is a number where less is a smaller ask; this one is an
            // enum where the *later* variant is. Read the wrong way round it
            // would turn a viewer's willingness to accept a soft picture into
            // a demand for a sharp one.
            sharpness: self.wanted_sharpness,
        };
        if !hint.is_well_formed() {
            return;
        }
        // One outstanding is enough: the host acts on the latest, and a queue
        // of superseded hints is a queue of wrong numbers.
        self.outbound
            .retain(|m| !matches!(m, MediaMessage::Hint(_)));
        self.outbound.push(MediaMessage::Hint(hint));
    }

    /// Ask for a different monitor.
    pub fn select_monitor(&mut self, monitor: u32) {
        self.select_monitors(vec![monitor]);
    }

    /// Ask for these screens, as one picture.
    ///
    /// **The previous ask is replaced rather than queued behind.** A host acts
    /// on every selection it receives, and each one costs a reshape and a
    /// keyframe — so two asks a person made in half a second, before either
    /// had been flushed, would switch twice and charge the session for a
    /// picture nobody ever saw. Only the latest can be what they meant.
    ///
    /// A set that could not be sent — empty, repeated, longer than a desktop
    /// can be — is dropped here, the same way a malformed hint is: the host
    /// would refuse it, and the round trip teaches nobody anything.
    pub fn select_monitors(&mut self, monitors: Vec<u32>) {
        let select = MonitorSelect { monitors };
        if !select.is_well_formed() {
            return;
        }
        self.outbound
            .retain(|m| !matches!(m, MediaMessage::Select(_)));
        self.outbound.push(MediaMessage::Select(select));
    }

    /// A host sent a new config, so the stream's shape changed.
    pub fn reconfigure(&mut self, config: &StreamConfig) -> Result<(), StreamError> {
        if !config.is_well_formed() {
            return Err(StreamError::Malformed {
                what: "a stream config",
            });
        }
        // **And a codec this end can decode, checked on every announcement
        // rather than only on the first.**
        //
        // The host refuses a session whose controller cannot decode what it
        // will send, so by the time a config arrives the codec is agreed. This
        // is the re-check: a config is re-announced whenever the stream
        // changes shape, and a stream that came back naming a different codec
        // would otherwise be reassembled and handed to a decoder that cannot
        // read it — a session that is up and showing nothing, which is the
        // failure this product has had twice and would not notice a third
        // time.
        if !DECODES.contains(&config.codec) {
            return Err(StreamError::CannotDecode {
                codec: config.codec,
            });
        }
        // A new SSRC is a new stream, so reassembly starts over rather than
        // trying to interpret the old sequence space.
        self.depacketizer = Depacketizer::new(config.ssrc);
        self.expected = (config.width, config.height);
        // The framerate is part of how long a keyframe takes to arrive, and a
        // reconfiguration is where it changes: the host's own ladder spends
        // framerate first.
        self.frame_interval = frame_interval(config.max_fps);
        self.announced_fps = config.max_fps;
        // A different stream, so any ask outstanding was about the old one.
        self.awaiting_keyframe = false;
        self.outbound
            .retain(|m| !matches!(m, MediaMessage::Keyframe(_)));
        // Nor is a reconfiguration: the stream has changed shape and the
        // pictures in hand are of the old one.
        self.ask_now(KeyframeReason::Reconfigured);
        Ok(())
    }

    /// Feed one datagram, appending whatever arrived to `out`.
    pub fn receive(&mut self, datagram: &[u8], out: &mut Vec<Decoded>) -> Result<(), StreamError> {
        self.receive_at(datagram, std::time::Instant::now(), out)
    }

    /// Feed one datagram, saying when it arrived.
    ///
    /// The time is a parameter so the deadline can be tested without sleeping.
    /// A test that waits real milliseconds to check a timeout is slow and
    /// flaky in exchange for nothing.
    pub fn receive_at(
        &mut self,
        datagram: &[u8],
        now: std::time::Instant,
        out: &mut Vec<Decoded>,
    ) -> Result<(), StreamError> {
        self.stats.datagrams = self.stats.datagrams.saturating_add(1);
        self.stats.bytes = self.stats.bytes.saturating_add(datagram.len() as u64);
        self.note_arrival(datagram.len() as u64, now);

        // Whether a frame was already being assembled *before* this datagram,
        // which is what makes this one "not the first of its frame".
        let opened_before = self.depacketizer.is_assembling();

        let mut frames = Vec::new();
        if let Err(e) = self.depacketizer.push(datagram, &mut frames) {
            // A datagram this stream does not accept is not a session
            // failure: it is one packet, and the session carries on. Counted
            // rather than propagated, because a peer able to end a session by
            // sending one bad datagram is a peer with a denial of service.
            self.stats.rejected = self.stats.rejected.saturating_add(1);
            tracing::debug!("a datagram was refused: {e}");
            return Ok(());
        }

        // The timestamp for the jitter estimator, read back out of the header
        // rather than tracked separately: it is the only thing that says when
        // the *sender* thought this datagram belonged.
        let timestamp = esper_media::RtpHeader::parse(datagram)
            .map(|(h, _)| h.timestamp)
            .unwrap_or(0);
        let produced_frame = frames
            .iter()
            .any(|f| matches!(f, Reassembled::Frame { .. }));
        for frame in frames {
            self.absorb(frame, now, out);
        }
        // A frame that completed on its own needs no deadline; one still being
        // assembled does.
        // **Bytes that arrived after this frame opened, and only this
        // frame's.**
        //
        // Not the frame's whole size: the first datagram's own time on the
        // wire falls outside the spread between first and last, so dividing
        // the whole frame by that spread overstates the link by n/(n-1) —
        // double, for a frame of two datagrams.
        //
        // And reset on *every* ending, not only a completion. Kept only
        // across completions it accumulated through abandoned frames as well,
        // so on a link losing half of them it measured two frames' bytes
        // against one frame's spread and read an 800 kbit/s link at 1500.
        //
        // The datagram in hand belongs to the frame that just ended unless it
        // is the one that *started the next* — which is what ended it. That
        // one is the next frame's first, and first datagrams are never
        // counted.
        let starts_the_next = self.depacketizer.is_assembling();
        if produced_frame {
            self.last_whole_frame = Some(now);
            if opened_before && !starts_the_next {
                self.burst_bytes = self.burst_bytes.saturating_add(datagram.len() as u64);
            }
            let carried = std::mem::take(&mut self.burst_bytes);
            if let Some(spread) = self.jitter.settled(now) {
                self.note_burst(carried, spread);
            }
        } else {
            if !opened_before {
                // A frame ended without producing one — abandoned. Whatever
                // was counted belonged to it and is gone with it.
                self.burst_bytes = 0;
            } else {
                self.burst_bytes = self.burst_bytes.saturating_add(datagram.len() as u64);
            }
        }
        self.jitter
            .arrived(now, timestamp, self.depacketizer.is_assembling());
        self.step_ladder(now);
        Ok(())
    }

    /// Handle one reassembly result.
    ///
    /// **Shared by `receive` and `flush`, and it has to be.** `flush` used to
    /// forward only losses and silently drop a frame it had just released —
    /// so the deadline that exists to make a static screen appear released the
    /// frame and then threw it away. The two paths differ in *when* they run,
    /// never in what they do.
    fn absorb(&mut self, frame: Reassembled, now: std::time::Instant, out: &mut Vec<Decoded>) {
        match frame {
            Reassembled::Lost { timestamp, why } => {
                self.stats.frames_lost = self.stats.frames_lost.saturating_add(1);
                // **The flag is not cleared first.** It used to be, which
                // defeated the coalescing on the very next line: every
                // lost frame produced its own request.
                self.ask(KeyframeReason::LostSync, now);
                out.push(Decoded::Lost {
                    timestamp,
                    why: why.reason(),
                });
            }
            Reassembled::Frame {
                bitstream,
                timestamp,
                keyframe,
            } => {
                // **Kept before anything can decide not to decode it.** A
                // recording is of what the host *sent*, not of what this
                // machine managed to show: a frame skipped because the CPU was
                // busy still belongs in the file, and a recording with holes
                // where the viewer stuttered would be a worse record of the
                // session than the session was.
                if self.keeping {
                    self.kept.push(Encoded {
                        bitstream: bitstream.clone(),
                        keyframe,
                    });
                }
                if keyframe {
                    self.awaiting_keyframe = false;
                } else if self.awaiting_keyframe {
                    // Provably whole, and its references are gone.
                    //
                    // **And ask again, because this frame is the evidence the
                    // last ask did not work.** A frame arriving whole while a
                    // keyframe is outstanding says the link is carrying data
                    // and the keyframe is still not here — the request was
                    // lost, or its answer was, or the host never made one.
                    //
                    // Without this the session has a hole it never comes out
                    // of. Asking happened only when a frame was *lost* or
                    // refused, so a link that recovered while a keyframe was
                    // outstanding went quiet in exactly the wrong way: whole
                    // frames arriving, every one of them discarded, and
                    // nothing left to trigger another ask. Measured on a link
                    // whose queue drained after six seconds: five clean
                    // seconds, 74 frames delivered, not one shown, and no ask
                    // made in all that time.
                    //
                    // The pace is what makes this safe — one per round trip
                    // plus a frame, not one per arrival.
                    self.ask(KeyframeReason::LostSync, now);
                    self.stats.frames_skipped = self.stats.frames_skipped.saturating_add(1);
                    out.push(Decoded::Skipped {
                        timestamp,
                        why: "a keyframe is outstanding, so this frame's \
                              reference pictures are missing",
                    });
                    return;
                }
                let Some(decoder) = self.decoder.as_mut() else {
                    out.push(Decoded::Skipped {
                        timestamp,
                        why: "no decoder is attached",
                    });
                    return;
                };
                match decoder.decode(&bitstream) {
                    Ok(Some(picture)) => {
                        if (picture.width(), picture.height()) != self.expected {
                            // The host changed size without saying so. Worth
                            // noticing rather than rendering: every buffer on
                            // this side was sized from the config.
                            self.expected = (picture.width(), picture.height());
                            tracing::warn!(
                                "the stream changed to {}x{} without a new config",
                                picture.width(),
                                picture.height()
                            );
                        }
                        self.stats.pictures = self.stats.pictures.saturating_add(1);
                        out.push(Decoded::Picture {
                            picture,
                            timestamp,
                            keyframe,
                        });
                    }
                    Ok(None) => {}
                    Err(e) => {
                        // The decoder could not use it. Its references are
                        // gone or the frame is damaged; either way a keyframe
                        // is the answer and the session continues.
                        self.stats.frames_skipped = self.stats.frames_skipped.saturating_add(1);
                        self.ask(KeyframeReason::LostSync, now);
                        tracing::debug!("the decoder refused a frame: {e}");
                        out.push(Decoded::Skipped {
                            timestamp,
                            // Said the way it actually happens: after loss
                            // this is the ordinary case, and "the decoder
                            // refused it" reads like a fault in the decoder.
                            why: "the decoder could not use it: its reference \
                                  pictures were lost, or the frame was damaged",
                        });
                    }
                }
            }
        }
    }

    /// Decide whatever frame is part-assembled, without waiting for the next.
    ///
    /// # Why a caller has to do this
    ///
    /// Reassembly is correct rather than eager: a frame whose start is not yet
    /// known waits for the next frame to bound it. On a **static screen** there
    /// is no next frame — the host has nothing to send and correctly sends
    /// nothing — so without a deadline the first frame waits forever and the
    /// viewer shows nothing at all.
    ///
    /// How long to wait is a latency decision and belongs to the caller, which
    /// is why it is not a timer in here. What is in here is the guarantee that
    /// calling it produces whatever there was to produce.
    pub fn flush(&mut self, out: &mut Vec<Decoded>) {
        self.flush_at(std::time::Instant::now(), out);
    }

    /// [`Self::flush`], with the clock as an argument.
    ///
    /// Separate so the deadline path and the recovery pace read the same time
    /// as the rest of the turn, rather than one a microsecond later.
    pub fn flush_at(&mut self, now: std::time::Instant, out: &mut Vec<Decoded>) {
        if let Some(frame) = self.depacketizer.flush() {
            self.absorb(frame, now, out);
        }
        // A link that has gone quiet is exactly when a window closes with
        // losses in it, so the ladder is asked here as well as on arrival.
        self.step_ladder(now);
    }

    /// A host message a controller is allowed to receive.
    pub fn handle(&mut self, message: &MediaMessage) -> Result<(), StreamError> {
        if !message.may_be_sent_by(Role::Host) {
            return Err(StreamError::WrongDirection {
                role: Role::Host,
                what: "that message",
            });
        }
        // **Checked at the boundary, for every kind, before anything is kept.**
        // Each arm below used to be trusted to check its own, which held for as
        // long as every arm remembered to — and the pointer arm did not, so a
        // host could fill a controller's queue with positions off every screen
        // it had.
        if !message.is_well_formed() {
            return Err(StreamError::Malformed {
                what: message.name(),
            });
        }
        match message {
            MediaMessage::Config(config) => self.reconfigure(config),
            // **Refused, because this direction is meaningless.** A recording
            // is made by the controller and announced to the host; a host
            // sending one has either been modified or is confused, and
            // accepting it would mean a viewer could be told it is recording
            // when it is not.
            MediaMessage::Recording(_) => Err(StreamError::Malformed {
                what: "a recording notice from a host",
            }),
            // **Kept for the caller rather than acted on here.** What to do
            // with a pointer is a viewer's decision — wear it, draw it, ignore
            // it — and this layer has no window to put it in.
            MediaMessage::Cursor(cursor) => {
                self.cursor.push(cursor.clone());
                // Bounded, because a peer that sends pointer messages faster
                // than a caller drains them must not be able to grow this
                // without limit. The *oldest* position goes: a stale one is
                // worth less than the one after it.
                while self.cursor.len() > MAX_PENDING_CURSOR {
                    self.cursor.remove(0);
                }
                Ok(())
            }
            // **Kept for the caller, like the pointer, and for the same
            // reason.** Applying a narrowing means closing an input source and
            // telling a person, and this layer has neither. What it can do is
            // make sure the message cannot be missed: it is held until taken.
            //
            // Nothing on *this* stream needs suppressing. A hint, a keyframe
            // request, a monitor selection and a link report all need only
            // `View`, and a session that has lost `View` is ending rather than
            // narrowing.
            // **Kept for the caller, like the pointer and the narrowing.** What
            // to do about a held picture is a viewer's decision — say so in a
            // title bar, dim what is on show, leave it alone — and this layer
            // has no window to do any of it in.
            MediaMessage::Paused(state) => {
                self.paused = Some(state.paused);
                Ok(())
            }
            // **Kept for the caller for the same reason.** What to do about
            // sound starting is a viewer's decision — open a device, say so,
            // ignore it because nobody asked for sound — and this layer has no
            // speaker to send it to. A session's sound is its own stream with
            // its own decoder, and `ControllerAudio` is where it is received.
            MediaMessage::Audio(config) => {
                self.audio = Some(*config);
                Ok(())
            }
            MediaMessage::Permissions(change) => {
                // The last word wins. Two narrowings in a row are not a delta
                // to accumulate -- each carries the complete set, which is
                // what stops the two ends drifting -- so an older one that a
                // caller has not read yet is stale rather than lost.
                self.granted_now = Some(change.granted.clone());
                self.stats.narrowings = self.stats.narrowings.saturating_add(1);
                Ok(())
            }
            // Listed rather than caught by a wildcard, so a message added to
            // the protocol later cannot be silently ignored here. Each of
            // these is something this end *sends*; the direction check above
            // has already refused one arriving.
            MediaMessage::Select(_)
            | MediaMessage::Hint(_)
            | MediaMessage::Keyframe(_)
            | MediaMessage::Report(_) => Ok(()),
        }
    }

    /// What the host has said this session may now do, if it has said so.
    ///
    /// Taken rather than read, so a caller that acts on it cannot act twice —
    /// and held until taken, because a narrowing that a busy caller missed
    /// would be a permission the host has withdrawn and the controller is
    /// still offering.
    #[must_use]
    pub fn take_permissions(&mut self) -> Option<Vec<esper_proto::Permission>> {
        self.granted_now.take()
    }

    /// The sound the host has announced, once, if it has.
    ///
    /// **Taken rather than read**, so a caller cannot build two decoders for
    /// one announcement — and so a caller that is not interested in sound
    /// simply never asks.
    #[must_use]
    pub fn take_audio(&mut self) -> Option<esper_proto::AudioConfig> {
        self.audio.take()
    }

    /// Whether the host has said the picture is held, if it has said so.
    ///
    /// Taken rather than read, so a caller that acts on it cannot act twice.
    #[must_use]
    pub fn take_paused(&mut self) -> Option<bool> {
        self.paused.take()
    }

    /// Everything the host has said about the pointer since this was last
    /// called.
    pub fn take_cursor(&mut self) -> Vec<esper_proto::CursorMessage> {
        std::mem::take(&mut self.cursor)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// **The smallest rung is never closed**, or a session can never come back.
    ///
    /// The bug this pins: a failure was recorded against the rung *in force*,
    /// and after a collapse that is the bottom one — which then took the blame
    /// for losses still arriving from the flood that caused the collapse. Two
    /// such windows pinned the floor at the bottom, and a session that lost
    /// frames for one second stayed at 320x180 for the forty that followed.
    /// Measured, three runs in three.
    #[test]
    fn the_smallest_rung_cannot_be_closed() {
        let mut stream = ControllerStream::new(&announced(30)).expect("a config");
        let bottom = RUNGS.len().saturating_sub(1);
        stream.rung = Some(bottom);
        for _ in 0..5 {
            stream.note_failure(Some(bottom));
        }
        assert_eq!(
            stream.floor, 0,
            "closing the smallest rung leaves a session no way back up"
        );
    }

    /// **Coming back halves the distance rather than walking it.**
    ///
    /// The fall is not one rung — the first losing window goes straight to the
    /// bottom — so a climb of one rung per five clean windows takes twenty
    /// seconds to undo one bad second. Measured before this: a session never
    /// regained its full size at all in the forty seconds after the glitch.
    #[test]
    fn the_climb_halves_the_distance_it_fell() {
        let mut stream = ControllerStream::new(&announced(30)).expect("a config");
        let bottom = RUNGS.len().saturating_sub(1);
        stream.rung = Some(bottom);
        stream.before_the_collapse = Some(0);

        let mut steps = 0;
        while stream.probe_upward() {
            steps += 1;
            assert!(steps <= RUNGS.len(), "the climb has to terminate");
        }
        assert_eq!(stream.rung, Some(0), "and it arrives at the top");
        assert!(
            steps < bottom,
            "halving should take fewer steps than walking: {steps} for {bottom} rungs"
        );
    }

    /// **A probe that loses frames is closed at once.**
    ///
    /// A probe only happens after five clean windows, so one that immediately
    /// loses frames is the link answering the question it asked — and every
    /// further attempt costs another flood. Measured: probing without this took
    /// a link with 100 ms of buffer from 155 pictures to 84.
    #[test]
    fn a_probe_that_fails_is_not_tried_again() {
        let mut stream = ControllerStream::new(&announced(30)).expect("a config");
        let bottom = RUNGS.len().saturating_sub(1);
        stream.rung = Some(bottom);
        stream.before_the_collapse = Some(0);

        assert!(stream.probe_upward(), "there is somewhere to probe");
        let probed = stream.rung.expect("a rung");
        stream.note_failure(Some(probed));

        assert!(
            stream.floor > probed,
            "a failed probe has to close the size it asked for: floor {} against {probed}",
            stream.floor
        );
        // And the ladder will not ask for it again.
        stream.rung = Some(bottom);
        while stream.probe_upward() {}
        assert!(
            stream.rung.is_some_and(|r| r > probed),
            "the ladder came back to a size that had already failed"
        );
    }

    /// **The first losing window collapses the size; later ones step.**
    ///
    /// Measured on a link with 100 ms of buffer at 200 kbit/s: walking down a
    /// rung a window took four seconds to reach the bottom, and by then no
    /// keyframe could arrive whole — **no picture at all in fifteen seconds**,
    /// where the same link pinned at the bottom rung delivered 362. A session
    /// that loses no frames never reaches this code, which is what makes it
    /// free on a good link.
    #[test]
    fn the_first_losing_window_goes_straight_to_the_bottom() {
        let mut stream = ControllerStream::new(&announced(30)).expect("a config");
        // A round trip with a queue in it, so the quality lever is not the one
        // spent — this test is about the resolution collapse.
        stream.set_round_trip(Duration::from_millis(40));
        stream.step_bloat_ladder();
        stream.set_round_trip(Duration::from_millis(400));

        let now = std::time::Instant::now();
        stream.step_ladder(now);
        stream.stats.frames_lost = LOSSES_TO_STEP_DOWN;
        stream.step_ladder(now + WINDOW + Duration::from_millis(1));

        assert_eq!(
            stream.link_ceiling(),
            RUNGS.last().copied(),
            "the first descent has to be a collapse, not a step"
        );

        // And the way back is the ordinary climb, one rung at a time.
        assert!(stream.step(false));
        assert_ne!(
            stream.link_ceiling(),
            RUNGS.last().copied(),
            "coming back up is still one rung at a time"
        );
    }

    /// **A capacity estimate that does not need the thing that is failing.**
    ///
    /// The burst estimator times frames that *completed*, and the one link
    /// regime that matters most completes none — so the capacity stayed
    /// unknown, the host was never told to pace, and it went on bursting. This
    /// is the fallback, and the important half of it is when it declines to
    /// answer: a session with nothing to send must not be read as a starved
    /// one, or the host is paced down to the trickle it was already sending.
    #[test]
    fn what_arrived_is_a_capacity_only_while_frames_are_failing() {
        let mut stream = ControllerStream::new(&announced(30)).expect("a config");
        let start = std::time::Instant::now();

        // Datagrams arriving, no frame ever completing: after a window there
        // is a rate, and it is the capacity.
        for i in 0..40u32 {
            stream.note_arrival(1_200, start + Duration::from_millis(u64::from(i) * 50));
        }
        let later = start + Duration::from_secs(3);
        assert!(
            stream.capacity_at(later).is_some(),
            "a starved session has to produce a number, or nothing paces the host"
        );

        // The same bytes, with frames completing: not starved, so the crude
        // estimate is withheld and the accurate one is allowed to be absent.
        stream.last_whole_frame = Some(later);
        assert_eq!(
            stream.capacity_at(later),
            None,
            "a session whose frames arrive must not be paced from what it happened to receive"
        );
    }

    /// **A short buffer is not a full link, and what arrived does not measure
    /// one.**
    ///
    /// A window that loses frames with the round trip *growing* is a link that
    /// is full, and there what arrived is what it carries. A window that loses
    /// frames with the round trip *flat* is a buffer too short to hold one
    /// frame: the rate was never the problem, and what arrived is only what
    /// the host happened to be offering after the collapse.
    ///
    /// This is the figure a session reports **once it has finished
    /// struggling** — the one that outlives the trouble and goes on pacing the
    /// host. Measured before this, on an 800 kbit/s link with 40 packets of
    /// queue: 398 kbit/s against the 337 the session was carrying.
    #[test]
    fn a_short_buffer_is_not_remembered_as_a_full_link() {
        /// One losing window, then a clean one, on a link whose round trip
        /// behaves as `queueing` says — and then what it remembers.
        fn after_struggling(queueing: bool) -> Option<Capacity> {
            let t0 = std::time::Instant::now();
            let mut c = ControllerStream::new(&announced(30)).expect("a config");

            // A baseline to compare against, then the round trip this case is
            // about.
            c.set_round_trip(Duration::from_millis(20));
            c.step_bloat_ladder();
            c.set_round_trip(Duration::from_millis(if queueing { 400 } else { 20 }));

            // Bytes arriving for more than one arrival window, so there is a
            // rate to be tempted by.
            for i in 0..40u32 {
                c.note_arrival(1_200, t0 + Duration::from_millis(u64::from(i) * 30));
            }

            c.step_ladder(t0);
            c.stats.frames_lost = LOSSES_TO_STEP_DOWN;
            c.step_ladder(t0 + Duration::from_millis(1_200));
            // And the struggle is over: a window with nothing lost, and a
            // frame that completed, so the starved fallback is not the thing
            // answering.
            c.step_ladder(t0 + Duration::from_millis(2_400));
            c.last_whole_frame = Some(t0 + Duration::from_millis(2_400));
            c.capacity_at(t0 + Duration::from_millis(2_401))
        }

        assert_eq!(
            after_struggling(false),
            None,
            "what arrived while a short buffer dropped frames is not a capacity"
        );
        let full = after_struggling(true)
            .expect("a link that was genuinely full has to leave a number behind");
        assert_eq!(
            full.how,
            Measured::AtLeast,
            "counted bytes are a lower bound, and the report has to say so"
        );
        assert!(full.kbit > 0.0, "a remembered rate of nothing is not one");
    }

    /// **The degradation order, which the link ladder did not follow.**
    ///
    /// This product states it: framerate, then quality, then resolution. The
    /// link's loss arm went straight to resolution — the most damaging lever —
    /// on every kind of loss. Measured on a link whose buffer is shorter than
    /// one frame, spending quality first took a session from **8 pictures to
    /// 162**.
    #[test]
    fn a_flat_round_trip_spends_quality_before_resolution() {
        let mut stream = ControllerStream::new(&announced(30)).expect("a config");
        // A baseline is made of the smallest round trip in a window that has
        // *closed*, so the window is closed the way a session closes it.
        stream.set_round_trip(Duration::from_millis(38));
        stream.step_bloat_ladder();
        stream.set_round_trip(Duration::from_millis(40));
        assert!(
            stream.queue_is_flat(),
            "40 ms against a floor of 38 is not a queue"
        );

        assert!(stream.spend_quality(), "there is a rung to spend");
        assert_eq!(stream.wanted_sharpness, Some(Sharpness::Soft));
        assert_eq!(stream.link_ceiling(), None, "and resolution is untouched");

        assert!(!stream.spend_quality(), "and only one rung exists to spend");
    }

    /// **A growing queue keeps spending resolution**, because that was
    /// measured to be what an overload needs: making resolution wait cost a
    /// session everything it had (26-36 pictures to none) under a five-times
    /// overload.
    #[test]
    fn a_growing_queue_is_not_a_reason_to_spend_quality() {
        let mut stream = ControllerStream::new(&announced(30)).expect("a config");
        stream.set_round_trip(Duration::from_millis(40));
        stream.step_bloat_ladder();
        stream.set_round_trip(Duration::from_millis(400));
        assert!(
            !stream.queue_is_flat(),
            "a round trip ten times its floor is a queue, not a short buffer"
        );
    }

    /// Coming back, resolution is returned before sharpness — the opposite
    /// order of spending them, and deliberately: a bigger soft picture is
    /// worth more than a small sharp one to somebody reading a screen.
    #[test]
    fn quality_is_the_last_thing_given_back() {
        let mut stream = ControllerStream::new(&announced(30)).expect("a config");
        stream.set_round_trip(Duration::from_millis(38));
        stream.step_bloat_ladder();
        stream.set_round_trip(Duration::from_millis(40));
        assert!(stream.spend_quality());
        assert!(stream.step(true), "and then a resolution rung");

        // Resolution comes back first, and for as long as there is any of it
        // to come back — the sharpness is untouched throughout.
        let mut returned = 0;
        while stream.step(false) {
            returned += 1;
            assert_eq!(
                stream.wanted_sharpness,
                Some(Sharpness::Soft),
                "sharpness must not be given back while resolution is still short"
            );
        }
        assert!(returned > 0, "there was a resolution rung to give back");

        // And then quality, once there is no resolution left to give.
        assert!(stream.return_quality());
        assert_eq!(stream.wanted_sharpness, None, "back to asking for nothing");
        assert!(!stream.return_quality(), "and there is nothing left twice");
    }

    /// **The pace has to be derived from the framerate, not from a floor.**
    ///
    /// This is a test of the arithmetic rather than of a session, deliberately:
    /// the ask rate is bounded by how often frames arrive, so no session can
    /// distinguish a pace of twenty milliseconds from one of sixty-six. What
    /// can be distinguished is whether the number comes from the stream or from
    /// a constant that was standing in for it.
    #[test]
    fn a_pace_on_an_instant_link_is_still_a_frame_long() {
        // Exact division, so a fifteenth of a second is 66.666… ms rather
        // than 66: the pace is a duration, not a millisecond count.
        assert_eq!(frame_interval(15), Duration::from_secs(1) / 15);
        assert_eq!(frame_interval(30), Duration::from_secs(1) / 30);
        assert_eq!(frame_interval(60), Duration::from_secs(1) / 60);
        assert!(frame_interval(15) > Duration::from_millis(66));
        assert!(frame_interval(15) < Duration::from_millis(67));
        // Not well formed, and arithmetic that divided by it would not be
        // either. The slowest interval this cares about, rather than a panic.
        assert_eq!(frame_interval(0), LONGEST_PACE);
    }

    /// A stream to receive, announced the way a host announces one.
    fn announced(max_fps: u16) -> esper_proto::StreamConfig {
        esper_proto::StreamConfig {
            monitors: vec![esper_proto::MonitorDescription {
                id: 0,
                name: "test-0".to_owned(),
                x: 0,
                y: 0,
                width: 640,
                height: 360,
                scale_percent: 100,
                primary: true,
            }],
            selected: 0,
            serving: vec![0],
            codec: esper_proto::Codec::H264,
            width: 640,
            height: 360,
            max_fps,
            clock_hz: 90_000,
            ssrc: 1,
            seen: esper_proto::Seen::Shown,
        }
    }

    /// How many keyframe requests are sitting in the outbound queue.
    fn asks(c: &ControllerStream) -> usize {
        c.outbound
            .iter()
            .filter(|m| matches!(m, MediaMessage::Keyframe(_)))
            .count()
    }

    /// **A keyframe that never arrives, on a link where nothing else does
    /// either, has to be asked for again.**
    ///
    /// Every other ask is triggered by an arrival, which is exactly the thing
    /// missing here: the host's screen is still, so it sends nothing, so
    /// nothing wakes this side to notice the keyframe never came. Before the
    /// retry the session stayed stale for as long as the screen stayed still,
    /// and the only thing that recovered it was the host's periodic keyframe
    /// arriving by luck — which is not a recovery path, it is a coincidence
    /// that happened to be scheduled.
    #[test]
    fn a_keyframe_that_never_comes_is_asked_for_again_out_of_silence() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        let start = std::time::Instant::now();
        // The join ask, taken by a caller and lost on the way.
        assert_eq!(asks(&c), 1, "joining asks once");
        let _ = c.take_outbound();
        let mut out = Vec::new();

        // Ticking straight away must not ask again: the first one may still
        // be in flight, and a retry per tick is a flood, not a recovery.
        c.tick(start, &mut out);
        assert_eq!(asks(&c), 0, "not before the interval is up");

        c.tick(start + ASK_AGAIN_INTO_SILENCE, &mut out);
        assert_eq!(
            asks(&c),
            1,
            "and then it asks again, with nothing to prompt it"
        );
        assert_eq!(
            c.stats().keyframes_asked_into_silence,
            1,
            "counted separately, because it diagnoses a state that used to be \
             unrecoverable"
        );
        let _ = c.take_outbound();

        // Repeating the *reason that is outstanding*, not the one this side
        // joined with. A session that lost sync is a different fault from one
        // that never got its first picture, and the host logs which.
        let lost = start + ASK_AGAIN_INTO_SILENCE * 2;
        c.ask(KeyframeReason::LostSync, lost);
        let _ = c.take_outbound();
        c.tick(lost + ASK_AGAIN_INTO_SILENCE, &mut out);
        let asked = c
            .take_outbound()
            .into_iter()
            .find_map(|m| match m {
                MediaMessage::Keyframe(k) => Some(k.reason),
                _ => None,
            })
            .expect("a keyframe request");
        assert_eq!(asked, KeyframeReason::LostSync);
    }

    /// And it stops the moment a keyframe arrives, rather than asking for ever.
    #[test]
    fn the_retry_stops_when_the_picture_does_arrive() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        let start = std::time::Instant::now();
        let _ = c.take_outbound();

        c.awaiting_keyframe = false;
        let mut out = Vec::new();
        c.tick(start + ASK_AGAIN_INTO_SILENCE * 4, &mut out);
        assert_eq!(asks(&c), 0, "nothing is outstanding, so nothing is asked");
        assert_eq!(c.due_in(start), None, "and there is no deadline to honour");
    }

    /// **The deadline this side reports must be one it will act on.**
    ///
    /// A caller that honours `due_in` sleeps for what it is told and calls
    /// `tick`. If the reported deadline were shorter than the pace `ask`
    /// enforces, every wake-up would decline to ask and immediately report
    /// zero again — a spin at whatever rate the caller can loop.
    #[test]
    fn the_retry_deadline_is_never_shorter_than_the_ask_pace() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        // A slow link, so the pace is far longer than the silence interval.
        c.set_round_trip(Duration::from_secs(5));
        assert!(c.pace() > ASK_AGAIN_INTO_SILENCE, "arrange the case");
        let start = std::time::Instant::now();
        let _ = c.take_outbound();
        let mut out = Vec::new();

        c.tick(start + ASK_AGAIN_INTO_SILENCE, &mut out);
        assert_eq!(asks(&c), 0, "the pace has not expired");
        let left = c
            .due_in(start + ASK_AGAIN_INTO_SILENCE)
            .expect("still outstanding");
        assert!(
            left > Duration::ZERO,
            "reported as due while `ask` would decline: that is a spin"
        );
    }

    /// **A narrowing is handed over once, and cannot be missed.**
    ///
    /// Held until taken rather than reported on arrival, because the caller
    /// that has to act on it — closing an input source, telling a person — is
    /// not the one reading the stream, and a permission the host has withdrawn
    /// while the controller goes on offering it is the one outcome worth
    /// designing against.
    #[test]
    fn a_narrowing_is_kept_for_the_caller_until_taken() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        assert_eq!(c.take_permissions(), None, "nothing has been said yet");

        let only_view = esper_proto::PermissionChange {
            granted: vec![esper_proto::Permission::View],
        };
        c.handle(&MediaMessage::Permissions(only_view))
            .expect("a host may say this");
        assert_eq!(
            c.take_permissions(),
            Some(vec![esper_proto::Permission::View])
        );
        assert_eq!(c.take_permissions(), None, "and not a second time");
        assert_eq!(c.stats().narrowings, 1);
    }

    /// **A pause is handed over once, and both edges arrive.**
    ///
    /// Same shape as a narrowing and for the same reason: the caller that has
    /// to say something to a person is not the one reading the stream. The
    /// release matters as much as the hold — a viewer left saying "held" after
    /// the frames came back is worse than one that never said it.
    #[test]
    fn a_pause_and_its_release_are_each_kept_until_taken() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        assert_eq!(c.take_paused(), None, "nothing has been said yet");

        c.handle(&MediaMessage::Paused(esper_proto::Paused { paused: true }))
            .expect("a host may say this");
        assert_eq!(c.take_paused(), Some(true));
        assert_eq!(c.take_paused(), None, "and not a second time");

        c.handle(&MediaMessage::Paused(esper_proto::Paused { paused: false }))
            .expect("a host may say this");
        assert_eq!(c.take_paused(), Some(false), "the release arrives too");
    }

    /// **The last word wins, because each message is the complete set.**
    ///
    /// Two narrowings before a caller looks are not a delta to accumulate. The
    /// protocol sends the whole set precisely so the two ends cannot drift, so
    /// an older one still waiting is stale rather than lost — and the newer is
    /// always the narrower.
    #[test]
    fn two_narrowings_before_a_caller_looks_leave_the_later_one() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        for granted in [
            vec![
                esper_proto::Permission::View,
                esper_proto::Permission::Control,
            ],
            vec![esper_proto::Permission::View],
        ] {
            c.handle(&MediaMessage::Permissions(esper_proto::PermissionChange {
                granted,
            }))
            .expect("a host may say this");
        }
        assert_eq!(
            c.take_permissions(),
            Some(vec![esper_proto::Permission::View]),
            "the narrower of the two, not the first one seen"
        );
    }

    /// **A config is checked against what this build decodes, on every
    /// announcement rather than only the first.**
    ///
    /// The host refuses a session whose controller cannot decode what it will
    /// send, so this is the re-check rather than the check — worth having
    /// because a config is re-announced whenever the stream changes shape, and
    /// a stream that came back naming something else would be handed to a
    /// decoder that cannot read it: a session that is up and showing nothing.
    ///
    /// **What this asserts is the accepting half**, because `Codec` has one
    /// variant today and a codec this build cannot decode is therefore not
    /// something a test can construct. Naming it after the refusal would be a
    /// test that passes without exercising anything. The refusing half is
    /// proven by sabotage — emptying [`DECODES`] fails this — and becomes
    /// directly testable on the day a second codec exists.
    #[test]
    fn a_config_is_checked_against_what_this_build_decodes() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        let config = announced(30);
        assert!(DECODES.contains(&config.codec), "arrange the case");
        assert!(
            c.reconfigure(&config).is_ok(),
            "a codec this build decodes must be accepted"
        );
    }

    /// And the two lists are the same list.
    ///
    /// A controller that offered a codec it would then refuse would negotiate
    /// itself into a session it cannot render. One constant, used by both.
    #[test]
    fn what_this_build_offers_is_what_it_accepts() {
        assert!(!DECODES.is_empty(), "a controller that decodes nothing");
        for c in DECODES {
            assert!(
                c.is_named_in(&[c.name().to_owned()]),
                "{c:?} does not answer to its own name"
            );
        }
    }

    /// **A burst measures the link; everything else measures the demand.**
    ///
    /// The distinction that five separate attempts at pacing foundered on.
    /// Bytes per second is what the sender chose to send, and on a link with
    /// room it says nothing whatever about the link — so a session that had
    /// settled at 320x180 and five frames a second reported 68 kbit/s on a
    /// link carrying 800. A frame is different: its datagrams are handed over
    /// at once, so the spread they arrive with is the wire serialising them.
    #[test]
    fn a_bursts_arrival_measures_the_wire() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        // A hundred kilobits a second: 10 000 bytes takes 800 ms.
        for _ in 0..ENOUGH_BURSTS_TO_JUDGE {
            c.note_burst(10_000, Duration::from_millis(800));
        }
        let measured = c
            .link_capacity()
            .expect("four readings is enough to judge")
            .kbit;
        assert!(
            (measured - 100.0).abs() < 1.0,
            "10 000 bytes over 800 ms is 100 kbit/s, not {measured:.0}"
        );

        // And the same bytes arriving in a tenth of the time is a link ten
        // times as fast — not a session sending ten times as much.
        let mut fast = ControllerStream::new(&announced(30)).expect("a well-formed config");
        for _ in 0..ENOUGH_BURSTS_TO_JUDGE {
            fast.note_burst(10_000, Duration::from_millis(80));
        }
        let measured = fast.link_capacity().expect("readings").kbit;
        assert!((measured - 1000.0).abs() < 10.0, "{measured:.0}");
    }

    /// **A frame too small to occupy the link measures nothing.**
    ///
    /// Two datagrams handed over in the same breath arrive in the same
    /// breath, and dividing by that spread reads in the gigabits. Measured on
    /// a real 800 kbit/s link, the frames over the threshold read 763 to 886
    /// while the maximum over the whole population was 5.7 Gbit/s.
    #[test]
    fn a_frame_too_small_to_time_is_not_timed() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        // A literal rather than the constant less one: derived from the
        // threshold, this test would follow the threshold anywhere, and a
        // threshold moved to nothing is exactly what it exists to catch.
        const A_DELTA_FRAME: u64 = 900;
        // Constant, and that is the point: it costs nothing and it fails the
        // moment somebody moves the threshold under what a delta frame
        // weighs, which is the mistake this whole test exists for.
        #[allow(clippy::assertions_on_constants)]
        {
            assert!(
                A_DELTA_FRAME < SMALLEST_BURST_WORTH_TIMING,
                "the threshold has moved below what a delta frame weighs, so \
                 small frames are being timed and they read in the gigabits"
            );
        }
        for _ in 0..(ENOUGH_BURSTS_TO_JUDGE * 4) {
            // A plausible delta frame, arriving absurdly fast.
            c.note_burst(A_DELTA_FRAME, Duration::from_micros(10));
        }
        assert_eq!(
            c.link_capacity(),
            None,
            "small frames were timed, and they read in the gigabits"
        );
        // A frame with no spread at all is one datagram, and one datagram
        // never waited for anything.
        c.note_burst(100_000, Duration::ZERO);
        assert_eq!(c.link_capacity(), None);
    }

    /// **The middle reading, because both tails lie.**
    ///
    /// A burst can be slowed by this side being descheduled and can look
    /// impossibly fast when its datagrams were handed over together. A
    /// maximum takes the second kind every time.
    #[test]
    fn the_middle_reading_survives_both_tails() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        // Eight honest readings around a megabit, one absurdly fast and one
        // absurdly slow — the shape a real link produces.
        for _ in 0..4 {
            c.note_burst(10_000, Duration::from_millis(80));
        }
        c.note_burst(10_000, Duration::from_micros(20));
        for _ in 0..4 {
            c.note_burst(10_000, Duration::from_millis(80));
        }
        c.note_burst(10_000, Duration::from_secs(4));
        let measured = c.link_capacity().expect("readings").kbit;
        assert!(
            (measured - 1000.0).abs() < 10.0,
            "one absurd reading either way moved the estimate to {measured:.0}"
        );
    }

    /// And nothing is claimed before there is enough to claim it from.
    #[test]
    fn too_few_readings_is_no_answer_rather_than_a_bad_one() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        for taken in 1..ENOUGH_BURSTS_TO_JUDGE {
            c.note_burst(10_000, Duration::from_millis(80));
            assert_eq!(c.link_capacity(), None, "answered after {taken} reading(s)");
        }
        c.note_burst(10_000, Duration::from_millis(80));
        assert!(
            c.link_capacity().is_some(),
            "still silent with enough to judge"
        );
    }

    /// **A queue filling is the signal loss cannot give.**
    ///
    /// A bufferbloated uplink hands back every byte it is given and hands it
    /// back late: nothing is lost until the buffer overflows, by which point
    /// every frame in it is stale. Measured on a link whose ceiling was below
    /// what the host offered, with a second of queue, the round trip reached
    /// 736 ms and the session showed no pictures at all in ten seconds.
    ///
    /// So the round trip is watched against its own baseline. Against the
    /// baseline and not an absolute figure: a satellite link answering in
    /// 600 ms with no queue at all is a perfectly good link, and an absolute
    /// trigger would spend its framerate for nothing.
    #[test]
    fn a_filling_queue_spends_framerate() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        let start = std::time::Instant::now();
        let mut out = Vec::new();

        // A quiet second, so there is a baseline to judge against.
        c.set_round_trip(Duration::from_millis(20));
        c.tick(start, &mut out);
        c.tick(start + WINDOW + Duration::from_millis(1), &mut out);
        assert_eq!(
            c.baseline_round_trip(),
            Some(Duration::from_millis(20)),
            "a window closed, so the path's cost should be known"
        );
        assert_eq!(c.link_fps(), None, "nothing has gone wrong yet");

        // Then the queue fills: the same path, answering far more slowly.
        let mut now = start + WINDOW;
        for _ in 0..3 {
            c.set_round_trip(Duration::from_millis(700));
            now += WINDOW + Duration::from_millis(1);
            c.tick(now, &mut out);
        }

        let asked = c.link_fps().expect("the bloat ladder should have moved");
        assert!(
            asked < 30,
            "a queue seven hundred milliseconds deep left the framerate at {asked}"
        );
        // And it says so on the wire, as a hint the host can act on.
        assert!(
            c.take_outbound().iter().any(|m| matches!(
                m,
                MediaMessage::Hint(h) if h.max_fps == Some(asked)
            )),
            "the ladder moved and told nobody"
        );
    }

    /// And gives it back when the queue drains — at a tighter threshold than
    /// it stepped down on, or it would raise the framerate, refill the queue,
    /// and spend the session doing that.
    #[test]
    fn a_drained_queue_gets_the_framerate_back() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        let start = std::time::Instant::now();
        let mut out = Vec::new();
        let mut now = start;
        let mut close = |c: &mut ControllerStream, rtt: Duration, now: &mut std::time::Instant| {
            c.set_round_trip(rtt);
            *now += WINDOW + Duration::from_millis(1);
            c.tick(*now, &mut out);
        };

        close(&mut c, Duration::from_millis(20), &mut now);
        for _ in 0..3 {
            close(&mut c, Duration::from_millis(700), &mut now);
        }
        let stepped_down = c.link_fps().expect("the ladder should have moved down");

        // The queue drains, and stays drained for long enough to earn it.
        for _ in 0..(CLEAN_WINDOWS_TO_STEP_UP + 1) {
            close(&mut c, Duration::from_millis(25), &mut now);
        }
        let recovered = c.link_fps().expect("still on the ladder");
        assert!(
            recovered > stepped_down,
            "a queue that drained left the framerate at {recovered}"
        );
    }

    /// **And a queue that is merely uncomfortable does not get it back.**
    ///
    /// Stepping up at the same threshold that stepped down is how a
    /// controller oscillates: it raises the framerate, refills the queue,
    /// lowers it again, and spends the session doing that. So the step down
    /// is at [`BLOAT`] and the step up at [`CALM`], and a link sitting
    /// between them stays where it is.
    #[test]
    fn a_queue_between_the_thresholds_stays_where_it_is() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        let mut now = std::time::Instant::now();
        let mut out = Vec::new();
        let mut close = |c: &mut ControllerStream, rtt: Duration, now: &mut std::time::Instant| {
            c.set_round_trip(rtt);
            *now += WINDOW + Duration::from_millis(1);
            c.tick(*now, &mut out);
        };

        close(&mut c, Duration::from_millis(20), &mut now);
        for _ in 0..3 {
            close(&mut c, Duration::from_millis(700), &mut now);
        }
        let stepped_down = c.link_fps().expect("the ladder should have moved down");

        // Half a queue: past CALM, short of BLOAT. Neither better nor worse.
        //
        // Held for longer than the baseline's own window and it would stop
        // being a queue at all — a standing sixty milliseconds that never
        // drains is indistinguishable from sixty milliseconds of distance,
        // and after ten seconds the sliding minimum rightly calls it that.
        // What this is about is the window before then.
        for _ in 0..(CLEAN_WINDOWS_TO_STEP_UP + 1) {
            close(&mut c, Duration::from_millis(20 + 60), &mut now);
        }
        assert_eq!(
            c.link_fps(),
            Some(stepped_down),
            "a queue between the thresholds moved the ladder, which is how it oscillates"
        );
    }

    /// **A satellite link is not a full queue.** Six hundred milliseconds of
    /// distance, steady, must cost nothing: the signal is the round trip
    /// *growing*, which is this session's own bytes waiting.
    #[test]
    fn a_long_path_with_no_queue_costs_nothing() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        let mut now = std::time::Instant::now();
        let mut out = Vec::new();
        for _ in 0..8 {
            c.set_round_trip(Duration::from_millis(600));
            now += WINDOW + Duration::from_millis(1);
            c.tick(now, &mut out);
        }
        assert_eq!(
            c.link_fps(),
            None,
            "a steady six-hundred-millisecond path is a distance, not a queue"
        );
    }

    /// **Every hint carries every reason.** The host keeps only the last hint
    /// it was sent and replaces both halves of it, so a hint carrying one
    /// ladder's ask wipes out the other's — and a ladder's hint used to
    /// overwrite the viewer's own profile with `Balanced`.
    #[test]
    fn one_hint_carries_the_viewer_and_both_ladders() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");
        let mut out = Vec::new();
        let mut now = std::time::Instant::now();

        // The viewer is reading text in a small window.
        c.hint(None, Some(960), Profile::Text);

        // The link starts losing frames, so the size ladder moves.
        c.set_round_trip(Duration::from_millis(20));
        c.tick(now, &mut out);
        now += WINDOW + Duration::from_millis(1);
        c.tick(now, &mut out);
        c.step(true);

        // And the queue fills, so the bloat ladder moves too.
        for _ in 0..3 {
            c.set_round_trip(Duration::from_millis(700));
            now += WINDOW + Duration::from_millis(1);
            c.tick(now, &mut out);
        }

        let last = c
            .take_outbound()
            .into_iter()
            .filter_map(|m| match m {
                MediaMessage::Hint(h) => Some(h),
                _ => None,
            })
            .next_back()
            .expect("a hint");
        assert_eq!(
            last.profile,
            Profile::Text,
            "a ladder overwrote what the viewer said it was doing"
        );
        assert_eq!(
            last.max_fps,
            c.link_fps(),
            "the bloat ladder's ask is missing from the hint"
        );
        assert_eq!(
            last.max_long_edge,
            Some(c.link_ceiling().expect("a rung").min(960)),
            "the size ladder's ask, or the viewer's, is missing from the hint"
        );
    }

    /// A round trip long enough to matter is added to it, not replaced by it.
    #[test]
    fn a_slow_link_waits_for_the_round_trip_and_the_frame() {
        let mut c = ControllerStream::new(&announced(15)).expect("a well-formed config");

        // Nothing measured yet: conservative, plus the frame.
        assert_eq!(c.pace(), UNKNOWN_ROUND_TRIP + frame_interval(15));

        // An instant link is still a frame away from a keyframe.
        c.set_round_trip(Duration::ZERO);
        assert_eq!(c.pace(), frame_interval(15));
        assert!(
            c.pace() > SHORTEST_PACE,
            "the floor must not be what decides this"
        );

        // A transatlantic one waits for both.
        c.set_round_trip(Duration::from_millis(200));
        assert_eq!(c.pace(), Duration::from_millis(200) + frame_interval(15));

        // **And the framerate follows a reconfiguration.** The host's own
        // ladder spends framerate first, so this is where it changes — and a
        // pace still counting the old rate would be waiting for a frame that
        // is no longer the next one.
        c.reconfigure(&announced(5)).expect("a well-formed config");
        assert_eq!(c.pace(), Duration::from_millis(200) + frame_interval(5));

        // And a link measured absurdly slowly does not leave a session black
        // for ever.
        c.set_round_trip(Duration::from_secs(30));
        assert_eq!(c.pace(), LONGEST_PACE);
    }

    /// **Every message is checked before anything is kept.** The pointer arm
    /// was trusted to check its own bounds and did not, so a host could hand a
    /// controller a cursor whose pixels do not match the size it declares —
    /// which is the shape of every read past the end of a buffer there has
    /// ever been.
    #[test]
    fn a_malformed_message_is_refused_before_it_is_kept() {
        let mut c = ControllerStream::new(&announced(30)).expect("a well-formed config");

        let bad_cursor = MediaMessage::Cursor(esper_proto::CursorMessage::Shape(
            esper_proto::CursorShape {
                serial: 1,
                image: esper_proto::CursorImage::Bitmap {
                    width: 32,
                    height: 32,
                    hotspot_x: 0,
                    hotspot_y: 0,
                    // Eight pixels' worth, for an image that claims 1024.
                    rgba: vec![0u8; 32],
                },
            },
        ));
        assert!(!bad_cursor.is_well_formed(), "the test's input is not bad");
        assert!(matches!(
            c.handle(&bad_cursor),
            Err(StreamError::Malformed { .. })
        ));
        assert!(c.take_cursor().is_empty(), "it was kept anyway");
    }
}
