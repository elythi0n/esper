//! The receiving half.
//!
//! Reassembles datagrams into frames, decodes them, and says what it needs.
//!
//! # It asks for keyframes; it does not wait for them
//!
//! Loss is reported by the reassembler rather than hidden, and every report
//! turns into a [`KeyframeRequest`](ownview_proto::KeyframeRequest) for the
//! caller to send. The requests are *coalesced*: a burst of loss produces one
//! ask, not one per frame. A controller on a bad link that asked per frame
//! would make the host send nothing but keyframes, which is the largest and
//! most expensive kind of frame — turning a lossy link into an unusable one.
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

use ownview_decode::decoder::VideoDecoder;
use ownview_media::{Depacketizer, Reassembled};
use ownview_proto::{
    KeyframeReason, KeyframeRequest, MediaMessage, MonitorSelect, Profile, QualityHint, Role,
    StreamConfig,
};

use crate::StreamError;

/// What arrived.
#[derive(Debug)]
pub enum Decoded {
    /// A picture, ready to show.
    Picture {
        picture: ownview_color::I420,
        timestamp: u32,
        keyframe: bool,
    },
    /// A frame arrived complete but was not decoded, and why.
    Skipped { timestamp: u32, why: &'static str },
    /// A frame was lost. A keyframe has been asked for.
    Lost { timestamp: u32, why: &'static str },
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
    /// Requests for the caller to send.
    outbound: Vec<MediaMessage>,
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
            outbound: Vec::new(),
            stats: ControllerStats::default(),
        };
        // A fresh receiver has no reference picture, so say so immediately
        // rather than waiting for the host's periodic keyframe.
        me.ask(KeyframeReason::Joined);
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

    /// The reassembler's own totals.
    #[must_use]
    pub fn media_stats(&self) -> ownview_media::depacketize::Stats {
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

    /// Ask the host for a keyframe, unless one is already outstanding.
    ///
    /// # This is the whole coalescing
    ///
    /// A controller that asked per lost frame would make the host send nothing
    /// but keyframes — the largest and most expensive frames there are —
    /// turning a lossy link into an unusable one. So a burst of loss produces
    /// one ask.
    ///
    /// The rule is **one unsent request outstanding**: a request still in the
    /// queue absorbs the rest of the burst, and a new one becomes possible
    /// once the caller has sent it.
    ///
    /// Not "one until a keyframe arrives", which was the first attempt and
    /// deadlocks: on a link bad enough to lose the keyframes as well, a
    /// controller that had already asked would never ask again and the session
    /// would stay black for good.
    ///
    /// So during sustained loss this asks about once per turn, and the host is
    /// the second half of the coalescing — `request_keyframe` is idempotent
    /// until the next frame, so thirty requests inside one frame interval
    /// produce one keyframe.
    fn ask(&mut self, reason: KeyframeReason) {
        let queued = self
            .outbound
            .iter()
            .any(|m| matches!(m, MediaMessage::Keyframe(_)));
        if queued {
            return;
        }
        self.awaiting_keyframe = true;
        self.stats.keyframes_requested = self.stats.keyframes_requested.saturating_add(1);
        self.outbound
            .push(MediaMessage::Keyframe(KeyframeRequest { reason }));
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
        // A different stream, so any ask outstanding was about the old one.
        self.awaiting_keyframe = false;
        self.outbound
            .retain(|m| !matches!(m, MediaMessage::Keyframe(_)));
        self.ask(KeyframeReason::Reconfigured);
        Ok(())
    }

    /// Feed one datagram, appending whatever arrived to `out`.
    pub fn receive(&mut self, datagram: &[u8], out: &mut Vec<Decoded>) -> Result<(), StreamError> {
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

        for frame in frames {
            self.absorb(frame, out);
        }
        Ok(())
    }

    /// Handle one reassembly result.
    ///
    /// **Shared by `receive` and `flush`, and it has to be.** `flush` used to
    /// forward only losses and silently drop a frame it had just released —
    /// so the deadline that exists to make a static screen appear released the
    /// frame and then threw it away. The two paths differ in *when* they run,
    /// never in what they do.
    fn absorb(&mut self, frame: Reassembled, out: &mut Vec<Decoded>) {
        match frame {
            Reassembled::Lost { timestamp, why } => {
                self.stats.frames_lost = self.stats.frames_lost.saturating_add(1);
                // **The flag is not cleared first.** It used to be, which
                // defeated the coalescing on the very next line: every
                // lost frame produced its own request.
                self.ask(KeyframeReason::LostSync);
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
                        // The decoder refused. Its references are gone or the
                        // frame is bad; either way a keyframe is the answer
                        // and the session continues.
                        self.stats.frames_skipped = self.stats.frames_skipped.saturating_add(1);
                        self.ask(KeyframeReason::LostSync);
                        tracing::debug!("the decoder refused a frame: {e}");
                        out.push(Decoded::Skipped {
                            timestamp,
                            why: "the decoder refused it",
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
        if let Some(frame) = self.depacketizer.flush() {
            self.absorb(frame, out);
        }
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
            // Nothing else may travel host to controller, which
            // `may_be_sent_by` has already established.
            _ => Ok(()),
        }
    }
}
