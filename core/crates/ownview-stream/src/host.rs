//! The sending half.
//!
//! Wraps the capture-and-encode pipeline with the packetizer, and answers the
//! three things a controller is allowed to ask for.
//!
//! # Timestamps
//!
//! RTP timestamps are taken from when the frame is *sent*, at 90 kHz from the
//! start of the stream. Not from when it was captured — the pipeline does not
//! carry that out — which adds the pipeline's own latency as a constant offset
//! to every timestamp. That is harmless where it matters: a receiver uses the
//! differences between timestamps to pace playback, and a constant offset
//! cancels. It would matter for measuring glass-to-glass latency, which needs
//! a clock on both machines and is a separate exercise.
//!
//! # A monitor is a stream
//!
//! The SSRC is derived from the monitor's id, so two monitors are two streams
//! with their own sequence spaces — which is what RTP's synchronisation source
//! means and what a receiver needs in order to tell them apart.

use std::time::{Duration, Instant};

use ownview_media::{Packetizer, VIDEO_CLOCK_HZ};
use ownview_pipeline::pipeline::{Pipeline, Step};
use ownview_proto::{
    Codec, MAX_MONITORS, MediaMessage, MonitorDescription, Profile, Role, StreamConfig,
};
use ownview_scale::Ceiling;

use crate::{DatagramSink, StreamError};

/// What one turn of the host produced.
#[derive(Debug, PartialEq, Eq)]
pub enum HostStep {
    /// A frame went out, in this many datagrams.
    Sent {
        datagrams: usize,
        bytes: usize,
        keyframe: bool,
    },
    /// The screen did not change, so nothing was sent.
    ///
    /// The common answer, and the reason a static screen costs almost nothing
    /// all the way to the wire.
    Quiet,
    /// The gate skipped the frame to stay inside the rate ceiling.
    Skipped,
    /// The monitor's geometry changed. The caller must re-announce the config.
    Reconfigured { width: u32, height: u32 },
    /// The capture source ended.
    Finished,
}

/// The sending half of a media session.
pub struct HostStream {
    pipeline: Pipeline,
    packetizer: Packetizer,
    /// Monitor id whose pixels are going out.
    monitor: u32,
    started: Instant,
    /// Datagrams and bytes sent, for the session's stats.
    datagrams: u64,
    bytes: u64,
    /// Reused, so a frame's datagrams are not a fresh allocation each time.
    scratch: Vec<Vec<u8>>,
}

impl std::fmt::Debug for HostStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostStream")
            .field("monitor", &self.monitor)
            .field("ssrc", &self.packetizer.ssrc())
            .field("encoding", &self.pipeline.encode_size())
            .field("datagrams", &self.datagrams)
            .finish_non_exhaustive()
    }
}

/// The SSRC for a monitor.
///
/// Derived rather than random so that a reconnect to the same monitor is the
/// same stream, and switching monitors is visibly a different one. The mix
/// keeps two adjacent monitor ids from producing adjacent SSRCs, which makes a
/// packet capture easier to read.
#[must_use]
pub fn ssrc_for(monitor: u32) -> u32 {
    // A single round of a well-known integer hash. Not a secret — QUIC has
    // already authenticated and encrypted the link — just a spread.
    //
    // The offset is why monitor 0 is not SSRC 0: without it the commonest case
    // in the product hashes to zero and lands on the fallback below, so every
    // single-monitor session announced `ssrc 0x00000001` and the spread this
    // function exists for did nothing for the case that matters most.
    let mut x = monitor.wrapping_add(0x517C_C1B7).wrapping_mul(0x9E37_79B9);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 13;
    // Zero is legal RTP but reads as "unset" to every tool that shows it.
    if x == 0 { 1 } else { x }
}

impl HostStream {
    /// Wrap a pipeline for sending.
    ///
    /// `mtu` is the largest datagram the path will carry. The packetizer
    /// fragments to fit *including* its headers.
    pub fn new(pipeline: Pipeline, mtu: usize) -> Result<Self, StreamError> {
        let monitor = pipeline.selected_monitor().0;
        let packetizer = Packetizer::new(ssrc_for(monitor), mtu)?;
        Ok(Self {
            pipeline,
            packetizer,
            monitor,
            started: Instant::now(),
            datagrams: 0,
            bytes: 0,
            scratch: Vec::new(),
        })
    }

    /// What this host will send, to announce to a controller.
    ///
    /// Built fresh each time rather than cached: the encode size changes when
    /// the CPU budget's ladder moves, and a controller acting on a stale
    /// config would size its window wrongly.
    #[must_use]
    pub fn config(&self) -> StreamConfig {
        let (width, height) = self.pipeline.encode_size();
        let monitors: Vec<MonitorDescription> = self
            .pipeline
            .monitors()
            .iter()
            .take(MAX_MONITORS)
            .map(|m| MonitorDescription {
                id: m.id.0,
                name: m.name.clone(),
                x: m.geometry.x,
                y: m.geometry.y,
                width: m.geometry.width,
                height: m.geometry.height,
                // A float on the wire would round differently in two
                // languages; percent keeps it an integer.
                scale_percent: (m.scale * 100.0).round().clamp(25.0, 800.0) as u16,
                primary: m.primary,
            })
            .collect();
        StreamConfig {
            monitors,
            selected: self.monitor,
            codec: Codec::H264,
            width,
            height,
            max_fps: u16::try_from(self.pipeline.requested_fps()).unwrap_or(u16::MAX),
            clock_hz: VIDEO_CLOCK_HZ,
            ssrc: self.packetizer.ssrc(),
        }
    }

    /// The stream's identity on the wire.
    #[must_use]
    pub const fn ssrc(&self) -> u32 {
        self.packetizer.ssrc()
    }

    /// Datagrams and bytes sent so far.
    #[must_use]
    pub const fn sent(&self) -> (u64, u64) {
        (self.datagrams, self.bytes)
    }

    /// The pipeline, for the stats a session reports.
    #[must_use]
    pub const fn pipeline(&self) -> &Pipeline {
        &self.pipeline
    }

    /// Tell the path's MTU to the packetizer.
    pub fn set_mtu(&mut self, mtu: usize) -> Result<(), StreamError> {
        self.packetizer.set_mtu(mtu)?;
        Ok(())
    }

    /// The sequence number the next datagram will carry.
    ///
    /// Read when parking, so a resumed session continues the numbering.
    #[must_use]
    pub const fn next_sequence(&self) -> u16 {
        self.packetizer.next_sequence()
    }

    /// Take the pipeline back, for parking the session across a reconnect.
    ///
    /// Consumes the stream, because a stream without its pipeline is not one —
    /// and because a caller able to keep both could send on a transport that
    /// has gone.
    #[must_use]
    pub fn into_pipeline(self) -> Pipeline {
        self.pipeline
    }

    /// Resume a parked session onto a new transport.
    ///
    /// `sequence` continues the old stream's numbering, so a capture of the
    /// traffic reads as one stream interrupted rather than two unrelated ones.
    ///
    /// Returns the pipeline on failure, so a refused resume can be parked again
    /// rather than destroying the session it was trying to save.
    // The large `Err` is the whole point: it carries the pipeline back so a
    // refused resume can be parked again rather than destroying the session it
    // was trying to save. Boxing it would add an allocation on the path that
    // exists to avoid losing somebody's session.
    #[allow(clippy::result_large_err)]
    pub fn resume(
        pipeline: Pipeline,
        mtu: usize,
        sequence: u16,
    ) -> Result<Self, (Pipeline, StreamError)> {
        let monitor = pipeline.selected_monitor().0;
        match Packetizer::new(ssrc_for(monitor), mtu) {
            Ok(mut packetizer) => {
                packetizer.set_sequence(sequence);
                Ok(Self {
                    pipeline,
                    packetizer,
                    monitor,
                    started: Instant::now(),
                    datagrams: 0,
                    bytes: 0,
                    scratch: Vec::new(),
                })
            }
            Err(e) => Err((pipeline, e.into())),
        }
    }

    /// Run one turn: capture, encode, packetize, send.
    pub fn step(
        &mut self,
        timeout: Duration,
        sink: &mut impl DatagramSink,
    ) -> Result<HostStep, StreamError> {
        match self.pipeline.step(timeout)? {
            Step::Encoded {
                bitstream,
                keyframe,
                ..
            } => {
                let timestamp = self.rtp_timestamp();
                self.packetizer
                    .packetize(&bitstream, timestamp, &mut self.scratch)?;
                let mut bytes = 0usize;
                for datagram in &self.scratch {
                    sink.send(datagram)?;
                    bytes = bytes.saturating_add(datagram.len());
                }
                let datagrams = self.scratch.len();
                self.datagrams = self.datagrams.saturating_add(datagrams as u64);
                self.bytes = self.bytes.saturating_add(bytes as u64);
                Ok(HostStep::Sent {
                    datagrams,
                    bytes,
                    keyframe,
                })
            }
            Step::Unchanged => Ok(HostStep::Quiet),
            Step::Skipped => Ok(HostStep::Skipped),
            Step::Reconfigured { width, height } => Ok(HostStep::Reconfigured { width, height }),
            Step::Finished => Ok(HostStep::Finished),
        }
    }

    /// The RTP timestamp for a frame sent now.
    fn rtp_timestamp(&self) -> u32 {
        let nanos = self.started.elapsed().as_nanos();
        let ticks = nanos.saturating_mul(u128::from(VIDEO_CLOCK_HZ)) / 1_000_000_000;
        // Wrapping is correct: RTP timestamps are a 32-bit clock and a
        // receiver compares them as signed differences. At 90 kHz that is
        // thirteen hours, which a session can reach.
        (ticks % u128::from(u32::MAX) + 1) as u32
    }

    /// Act on something the controller sent.
    ///
    /// Refuses by name rather than ignoring. A peer sending a message it may
    /// not send is misbehaving, and a host that silently discarded it would
    /// leave the controller waiting for an effect that is never coming.
    ///
    /// # Where the direction is checked, and why it is only checked once
    ///
    /// In the match below, not in a guard above it. A separate
    /// `may_be_sent_by` check here would be *redundant* — the match is
    /// exhaustive, so every host-only message already has to be refused
    /// explicitly — and redundant is worse than it sounds: removing the guard
    /// broke no test, because the match was doing the work.
    ///
    /// The match is also the safer of the two places. A message added to
    /// `MediaMessage` later cannot be forgotten there, because the compiler
    /// will not accept a non-exhaustive match; a guard listing roles by hand
    /// would go stale silently. `the_direction_rule_and_what_the_host_accepts_
    /// agree` holds the two statements of the rule to each other.
    pub fn handle(&mut self, message: &MediaMessage) -> Result<(), StreamError> {
        if !message.is_well_formed() {
            return Err(StreamError::Malformed {
                what: name_of(message),
            });
        }
        match message {
            MediaMessage::Keyframe(_) => {
                self.pipeline.request_keyframe();
                Ok(())
            }
            MediaMessage::Hint(hint) => {
                self.apply(hint.max_fps, hint.max_long_edge, hint.profile);
                Ok(())
            }
            MediaMessage::Select(select) => {
                if select.monitor == self.monitor {
                    return Ok(());
                }
                // Refused with the reason rather than ignored. No capture
                // backend can change monitor without a reconnect yet, and a
                // controller told nothing would show a still picture and
                // assume the host had hung.
                Err(StreamError::CannotSwitchMonitor {
                    current: self.monitor,
                    wanted: select.monitor,
                })
            }
            // Host to controller only: a controller announcing a config
            // would be telling the host what its own screen is.
            MediaMessage::Config(_) => Err(StreamError::WrongDirection {
                role: Role::Controller,
                what: name_of(message),
            }),
        }
    }

    /// Apply a controller's preferences, as far as the host is willing.
    ///
    /// **Every one of these is a ceiling on what the host would have done, not
    /// a floor under it.** A controller asking for 60 fps on a machine that
    /// can afford 15 gets 15; the request lowers a limit and never raises one.
    /// A protocol where the viewer could demand more would let a viewer make
    /// somebody else's laptop unusable by opening a window.
    fn apply(&mut self, max_fps: Option<u16>, max_long_edge: Option<u32>, profile: Profile) {
        if let Some(fps) = max_fps {
            let asked = u32::from(fps);
            self.pipeline
                .set_requested_fps(asked.min(self.pipeline.requested_fps()));
        }
        if let Some(edge) = max_long_edge {
            // The tighter of what the controller asked for and what the host
            // already decided.
            let current = self.pipeline.ceiling().long_edge().unwrap_or(u32::MAX);
            self.pipeline.set_ceiling(Ceiling::of(edge.min(current)));
        }
        // The profile is a hint about content, and what to do with it is the
        // encoder's business. Recorded so the governor can read it; acting on
        // it is Phase 12's rate control.
        tracing::debug!("controller profile hint: {profile:?}");
    }
}

/// A name for an error message.
const fn name_of(message: &MediaMessage) -> &'static str {
    match message {
        MediaMessage::Config(_) => "a stream config",
        MediaMessage::Select(_) => "a monitor selection",
        MediaMessage::Hint(_) => "a quality hint",
        MediaMessage::Keyframe(_) => "a keyframe request",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The commonest case must not be the degenerate one.** Monitor 0 used
    /// to hash to zero and fall through to the fallback, so every
    /// single-monitor session announced `ssrc 0x00000001` — the one case the
    /// spread exists for, and the one case it did nothing for.
    #[test]
    fn monitor_zero_gets_a_real_ssrc() {
        let first = ssrc_for(0);
        assert_ne!(first, 0, "zero reads as unset to every tool that shows it");
        assert_ne!(
            first, 1,
            "monitor 0 fell through to the fallback, so the hash did nothing"
        );
    }

    #[test]
    fn adjacent_monitors_do_not_get_adjacent_ssrcs() {
        // A packet capture with two streams in it should be readable, which it
        // is not if the two identities differ by one.
        for id in 0..8u32 {
            let a = ssrc_for(id);
            let b = ssrc_for(id + 1);
            assert!(
                a.abs_diff(b) > 1000,
                "monitors {id} and {} got {a:#010x} and {b:#010x}",
                id + 1
            );
        }
    }

    #[test]
    fn the_same_monitor_always_gets_the_same_ssrc() {
        // A reconnect to the same monitor is the same stream, which is what
        // lets a receiver carry on rather than resynchronising.
        for id in [0u32, 1, 7, 1000, u32::MAX] {
            assert_eq!(ssrc_for(id), ssrc_for(id));
        }
    }

    #[test]
    fn every_monitor_gets_a_usable_ssrc() {
        for id in 0..2000u32 {
            assert_ne!(ssrc_for(id), 0, "monitor {id} got zero");
        }
    }
}
