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
    KeyframeReason, KeyframeRequest, MediaMessage, MonitorSelect, Profile, QualityHint, Role,
    StreamConfig,
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

/// What to assume the round trip is until something measures it.
///
/// Conservative: a caller that never says is one whose link is unknown, and
/// on an unknown link the storm is the worse failure.
const UNKNOWN_ROUND_TRIP: std::time::Duration = std::time::Duration::from_millis(200);

/// The floor, so a link measured at nothing still paces.
///
/// Rarely the binding constraint, because the pace also includes the time to
/// make a frame — but a stream announcing a very high framerate is still
/// entitled to a floor.
const SHORTEST_PACE: std::time::Duration = std::time::Duration::from_millis(20);

/// The ceiling, so a bad measurement cannot leave a session black.
const LONGEST_PACE: std::time::Duration = std::time::Duration::from_secs(2);

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
    /// What the link's round trip was measured at.
    /// See [`ControllerStream::set_round_trip`].
    round_trip: std::time::Duration,
    /// The soonest the host could produce the next frame, from the framerate
    /// it announced. Half of the pace, and the half a LAN does not supply.
    frame_interval: std::time::Duration,
    last_ask: Option<std::time::Instant>,
    /// The link ladder: which rung of [`RUNGS`] has been asked for, if any,
    /// and what the window of loss looks like so far.
    rung: Option<usize>,
    window_ends: Option<std::time::Instant>,
    lost_when_window_began: u64,
    clean_windows: u32,
    /// Requests for the caller to send.
    outbound: Vec<MediaMessage>,
    /// What the host has said about the pointer, for the caller to act on.
    cursor: Vec<esper_proto::CursorMessage>,
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
            round_trip: UNKNOWN_ROUND_TRIP,
            frame_interval: frame_interval(config.max_fps),
            last_ask: None,
            rung: None,
            window_ends: None,
            lost_when_window_began: 0,
            clean_windows: 0,
            outbound: Vec::new(),
            cursor: Vec::new(),
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
        self.jitter.due_in(now)
    }

    /// Act on the deadline if it has passed.
    ///
    /// Does nothing if it has not, so a caller may call it whenever it likes.
    pub fn tick(&mut self, now: std::time::Instant, out: &mut Vec<Decoded>) {
        if self.jitter.is_due(now) {
            self.jitter.fired();
            self.flush_at(now, out);
        }
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
        self.stats.keyframes_requested = self.stats.keyframes_requested.saturating_add(1);
        self.outbound
            .push(MediaMessage::Keyframe(KeyframeRequest { reason }));
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
        self.window_ends = Some(now + WINDOW);
        self.lost_when_window_began = self.stats.frames_lost;

        if lost >= LOSSES_TO_STEP_DOWN {
            self.clean_windows = 0;
            self.step(true);
        } else if lost == 0 {
            self.clean_windows = self.clean_windows.saturating_add(1);
            if self.clean_windows >= CLEAN_WINDOWS_TO_STEP_UP {
                self.clean_windows = 0;
                self.step(false);
            }
        }
    }

    /// One rung down, or one up, and tell the host if anything changed.
    fn step(&mut self, down: bool) {
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
            return;
        } else {
            current.saturating_sub(1)
        };
        if Some(next) == self.rung {
            return;
        }
        self.rung = Some(next);
        let Some(edge) = RUNGS.get(next).copied() else {
            return;
        };
        // A hint, not a demand: the host takes the tighter of this and its
        // own limit, so this can only ever ask for less than the machine's
        // owner allowed.
        self.hint(None, Some(edge), Profile::Balanced);
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

    /// A hint for the caller to send, if it wants to.
    ///
    /// A convenience rather than policy: the controller knows what its viewer
    /// is doing and this is the shape of saying so.
    pub fn hint(&mut self, max_fps: Option<u16>, max_long_edge: Option<u32>, profile: Profile) {
        let hint = QualityHint {
            max_fps,
            max_long_edge,
            profile,
        };
        if hint.is_well_formed() {
            self.outbound.push(MediaMessage::Hint(hint));
        }
    }

    /// Ask for a different monitor.
    pub fn select_monitor(&mut self, monitor: u32) {
        self.outbound
            .push(MediaMessage::Select(MonitorSelect { monitor }));
    }

    /// A host sent a new config, so the stream's shape changed.
    pub fn reconfigure(&mut self, config: &StreamConfig) -> Result<(), StreamError> {
        if !config.is_well_formed() {
            return Err(StreamError::Malformed {
                what: "a stream config",
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
        if produced_frame {
            self.jitter.settled();
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
                if keyframe {
                    self.awaiting_keyframe = false;
                } else if self.awaiting_keyframe {
                    // Provably whole, and its references are gone.
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
        match message {
            MediaMessage::Config(config) => self.reconfigure(config),
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
            // Listed rather than caught by a wildcard, so a message added to
            // the protocol later cannot be silently ignored here: the compiler
            // will not accept a non-exhaustive match.
            MediaMessage::Select(_) | MediaMessage::Hint(_) | MediaMessage::Keyframe(_) => Ok(()),
        }
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
            codec: esper_proto::Codec::H264,
            width: 640,
            height: 360,
            max_fps,
            clock_hz: 90_000,
            ssrc: 1,
        }
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
}
