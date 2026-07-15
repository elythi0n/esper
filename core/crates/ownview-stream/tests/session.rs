#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A whole media session, over a link that can be made to misbehave.
//!
//! Two machines are the real test and Phase 4's exit criterion. This is what
//! can be had without them, and it is more than it sounds: the host half and
//! the controller half are the code that will run on those machines, and an
//! in-memory link can drop the fourth packet of every frame on purpose, which
//! a real network cannot be asked to do.

use std::time::Duration;

use ownview_capture::gate::Policy;
use ownview_capture::synthetic::SyntheticCapturer;
use ownview_decode::decoder::DecoderConfig;
use ownview_decode::openh264::OpenH264Decoder;
use ownview_encode::budget::Budget;
use ownview_encode::encoder::{EncodeError, EncoderConfig, VideoEncoder};
use ownview_encode::openh264::OpenH264;
use ownview_pipeline::pipeline::Pipeline;
use ownview_proto::{KeyframeReason, MediaMessage, MonitorSelect, Profile, Role};
use ownview_scale::Ceiling;
use ownview_stream::{
    ControllerStream, DatagramSink, Decoded, HostStep, HostStream, RecordingSink, SinkError,
};

const W: u32 = 320;
const H: u32 = 240;
const MTU: usize = 1200;

fn encoder_for(config: &EncoderConfig) -> Result<Box<dyn VideoEncoder>, EncodeError> {
    Ok(Box::new(OpenH264::new(config)?))
}

fn host(fps: u32) -> HostStream {
    let capturer = SyntheticCapturer::new(W, H, 1_000_000).expect("a valid size");
    let pipeline = Pipeline::new(
        Box::new(capturer),
        &encoder_for,
        Budget::of(100.0),
        Policy {
            max_fps: fps,
            ..Policy::default()
        },
    )
    .expect("a pipeline")
    .with_ceiling(Ceiling::none());
    HostStream::new(pipeline, MTU).expect("a host stream")
}

fn controller(host: &HostStream) -> ControllerStream {
    ControllerStream::new(&host.config())
        .expect("the host's own config must be acceptable")
        .with_decoder(Box::new(
            OpenH264Decoder::new(&DecoderConfig::default()).expect("a decoder"),
        ))
}

/// A link that drops a chosen fraction of datagrams, deterministically.
struct LossySink {
    sent: Vec<Vec<u8>>,
    dropped: usize,
    percent: u64,
    state: u64,
}

impl LossySink {
    fn new(percent: u64) -> Self {
        Self {
            sent: Vec::new(),
            dropped: 0,
            percent,
            state: 0x2545_F491_4F6C_DD1D,
        }
    }
}

impl DatagramSink for LossySink {
    fn send(&mut self, payload: &[u8]) -> Result<(), SinkError> {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1);
        if (self.state >> 33) % 100 < self.percent {
            self.dropped += 1;
            // A dropped datagram is not a send failure. The sender is not
            // told, which is the whole character of an unreliable link.
            return Ok(());
        }
        self.sent.push(payload.to_vec());
        Ok(())
    }

    fn max_datagram_size(&self) -> Option<usize> {
        Some(MTU)
    }
}

/// **A session, end to end.** The host's screen arrives as pictures on the
/// controller.
#[test]
fn a_clean_link_delivers_the_hosts_screen() {
    let mut h = host(30);
    let mut c = controller(&h);
    let mut sink = RecordingSink::new();
    sink.limit = Some(MTU);

    // The controller asks for a keyframe the moment it starts, because it has
    // no reference picture.
    let opening = c.take_outbound();
    assert!(
        matches!(
            opening.first(),
            Some(MediaMessage::Keyframe(r)) if r.reason == KeyframeReason::Joined
        ),
        "a fresh controller must ask for a keyframe: {opening:?}"
    );
    for m in &opening {
        h.handle(m).expect("the host accepts it");
    }

    let mut pictures = 0u32;
    let mut sent_frames = 0u32;
    let mut out = Vec::new();
    for _ in 0..40 {
        match h.step(Duration::from_secs(1), &mut sink).expect("a step") {
            HostStep::Sent { datagrams, .. } => {
                sent_frames += 1;
                assert!(datagrams > 0);
            }
            HostStep::Quiet | HostStep::Skipped => {}
            other => panic!("unexpected {other:?}"),
        }
        for datagram in sink.sent.drain(..) {
            c.receive(&datagram, &mut out).expect("received");
        }
        for d in out.drain(..) {
            match d {
                Decoded::Picture { picture, .. } => {
                    assert_eq!((picture.width(), picture.height()), (W, H));
                    pictures += 1;
                }
                Decoded::Skipped { why, .. } => {
                    // Only while the opening keyframe is outstanding.
                    assert!(
                        why.contains("keyframe") || why.contains("decoder"),
                        "unexpected skip: {why}"
                    );
                }
                Decoded::Lost { why, .. } => panic!("a frame was lost on a clean link: {why}"),
            }
        }
        for m in c.take_outbound() {
            h.handle(&m).expect("the host accepts it");
        }
    }
    println!(
        "{sent_frames} frames sent, {pictures} pictures decoded, {:?}",
        c.stats()
    );
    assert!(sent_frames > 5, "only {sent_frames} frames were sent");
    assert!(
        pictures + 1 >= sent_frames,
        "{pictures} pictures from {sent_frames} frames on a clean link"
    );
    assert_eq!(c.stats().frames_lost, 0);
    assert!(!c.awaiting_keyframe(), "the session never got in sync");
}

/// **Under loss, the session recovers.** Every abandoned frame becomes one
/// keyframe request, the host obliges, and the picture comes back.
#[test]
fn a_lossy_link_recovers_and_keeps_delivering() {
    let mut h = host(30);
    let mut c = controller(&h);
    let mut link = LossySink::new(5);
    for m in c.take_outbound() {
        h.handle(&m).expect("accepted");
    }

    let mut pictures = 0u32;
    let mut lost = 0u32;
    let mut asks = 0u32;
    let mut out = Vec::new();
    for _ in 0..150 {
        let _ = h.step(Duration::from_secs(1), &mut link).expect("a step");
        let arrived = std::mem::take(&mut link.sent);
        for datagram in arrived {
            c.receive(&datagram, &mut out).expect("received");
        }
        for d in out.drain(..) {
            match d {
                Decoded::Picture { .. } => pictures += 1,
                Decoded::Lost { .. } => lost += 1,
                Decoded::Skipped { .. } => {}
            }
        }
        for m in c.take_outbound() {
            asks += 1;
            h.handle(&m).expect("accepted");
        }
    }
    println!(
        "{} dropped, {lost} lost, {asks} asks, {pictures} pictures, {:?}",
        link.dropped,
        c.stats()
    );
    assert!(link.dropped > 0, "the link dropped nothing");
    assert!(lost > 0, "loss was induced but no frame was reported lost");
    assert!(
        pictures > 30,
        "only {pictures} pictures survived 5% loss; the session is not recovering"
    );
    // **Coalesced.** A controller that asked per lost frame would make the
    // host send nothing but keyframes, which are the largest frames there
    // are — turning a lossy link into an unusable one.
    assert!(
        asks <= lost + 2,
        "{asks} keyframe requests for {lost} lost frames: the asking is not coalesced"
    );
}

/// A controller that asks for less must get less. The reverse must not work.
#[test]
fn a_quality_hint_can_only_lower_what_the_host_was_going_to_do() {
    let mut h = host(60);
    assert_eq!(h.pipeline().requested_fps(), 60);

    // Asking for less is honoured.
    h.handle(&MediaMessage::Hint(ownview_proto::QualityHint {
        max_fps: Some(15),
        max_long_edge: None,
        profile: Profile::Text,
    }))
    .expect("accepted");
    assert_eq!(h.pipeline().requested_fps(), 15);

    // **Asking for more is not.** A viewer must not be able to make somebody
    // else's laptop work harder by opening a window.
    h.handle(&MediaMessage::Hint(ownview_proto::QualityHint {
        max_fps: Some(240),
        max_long_edge: None,
        profile: Profile::Motion,
    }))
    .expect("accepted");
    assert_eq!(
        h.pipeline().requested_fps(),
        15,
        "a hint raised the frame rate the host had settled on"
    );

    // The same for resolution.
    h.handle(&MediaMessage::Hint(ownview_proto::QualityHint {
        max_fps: None,
        max_long_edge: Some(640),
        profile: Profile::Balanced,
    }))
    .expect("accepted");
    assert_eq!(h.pipeline().ceiling().long_edge(), Some(640));
    h.handle(&MediaMessage::Hint(ownview_proto::QualityHint {
        max_fps: None,
        max_long_edge: Some(4096),
        profile: Profile::Balanced,
    }))
    .expect("accepted");
    assert_eq!(
        h.pipeline().ceiling().long_edge(),
        Some(640),
        "a hint raised the resolution ceiling the host had settled on"
    );
}

/// Direction is part of the protocol, and a peer sending the wrong way is
/// refused by name rather than ignored.
#[test]
fn a_host_refuses_a_message_only_it_may_send() {
    let mut h = host(30);
    let config = h.config();
    let err = h
        .handle(&MediaMessage::Config(config))
        .expect_err("a controller may not announce a config");
    assert!(
        matches!(
            err,
            ownview_stream::StreamError::WrongDirection {
                role: Role::Controller,
                ..
            }
        ),
        "got {err:?}"
    );
}

/// Refused with the reason, not ignored. A controller told nothing would show
/// a still picture and conclude the host had hung.
#[test]
fn switching_monitor_mid_session_is_refused_with_a_reason() {
    let mut h = host(30);
    let current = h.config().selected;
    // Selecting the monitor already being sent is a no-op, not an error.
    h.handle(&MediaMessage::Select(MonitorSelect { monitor: current }))
        .expect("selecting the current monitor is fine");

    let err = h
        .handle(&MediaMessage::Select(MonitorSelect {
            monitor: current.wrapping_add(1),
        }))
        .expect_err("no backend can switch monitor yet");
    let text = err.to_string();
    assert!(
        text.contains("cannot switch") && text.contains("reconnect"),
        "the refusal must say why and what would work: {text}"
    );
}

/// A controller must not accept a config that is outside the protocol's
/// bounds: it is the first thing a peer says about the shape of what follows,
/// and every buffer on this side is sized from it.
#[test]
fn a_controller_refuses_a_malformed_config() {
    let h = host(30);
    let mut config = h.config();
    config.selected = 9999;
    assert!(
        ControllerStream::new(&config).is_err(),
        "a config selecting a monitor it does not list was accepted"
    );

    let mut too_big = h.config();
    too_big.width = ownview_proto::MAX_PICTURE_EDGE + 1;
    assert!(ControllerStream::new(&too_big).is_err());
}

/// A datagram from another stream must not end the session. A peer able to do
/// that by sending one bad packet is a peer with a denial of service.
#[test]
fn a_stray_datagram_is_counted_and_ignored() {
    let h = host(30);
    let mut c = controller(&h);
    let mut out = Vec::new();
    for junk in [vec![0u8; 4], vec![0x80, 96, 0, 0], vec![0xFF; 64]] {
        c.receive(&junk, &mut out)
            .expect("a bad datagram is not fatal");
    }
    assert!(out.is_empty());
    assert_eq!(c.stats().rejected, 3, "{:?}", c.stats());
    assert_eq!(c.stats().pictures, 0);
}

/// Every datagram the host produces has to fit the path, or the sink refuses
/// it — which is how a packetizer given the wrong MTU is caught here rather
/// than on a network with an unusual one.
#[test]
fn every_datagram_fits_the_path() {
    let mut h = host(30);
    let mut sink = RecordingSink::new();
    sink.limit = Some(MTU);
    for _ in 0..30 {
        h.step(Duration::from_secs(1), &mut sink)
            .expect("no datagram exceeded the path");
    }
    assert!(!sink.sent.is_empty());
    for d in &sink.sent {
        assert!(d.len() <= MTU, "a {}-byte datagram", d.len());
    }
    println!("{} datagrams, {} bytes", sink.sent.len(), sink.bytes());
}

/// The config a host announces has to describe what it actually sends.
#[test]
fn the_announced_config_matches_what_arrives() {
    let mut h = host(30);
    let config = h.config();
    assert!(config.is_well_formed());
    assert_eq!(config.ssrc, h.ssrc());
    assert_eq!(config.clock_hz, 90_000);
    assert_eq!((config.width, config.height), h.pipeline().encode_size());
    assert!(
        config.monitors.iter().any(|m| m.id == config.selected),
        "the config must name a monitor it lists"
    );

    // And a controller built from it expects the same size the pictures are.
    let mut c = controller(&h);
    let mut sink = RecordingSink::new();
    let mut out = Vec::new();
    for _ in 0..20 {
        let _ = h.step(Duration::from_secs(1), &mut sink).expect("a step");
        for d in sink.sent.drain(..) {
            c.receive(&d, &mut out).expect("received");
        }
    }
    let mut seen = 0;
    for d in out {
        if let Decoded::Picture { picture, .. } = d {
            assert_eq!(
                (picture.width(), picture.height()),
                c.expected_size(),
                "a picture arrived at a size the config did not announce"
            );
            seen += 1;
        }
    }
    assert!(seen > 0, "no picture arrived");
}

/// **The coalescing, tested properly.** A controller that asked for a keyframe
/// per lost frame would make the host send nothing but keyframes — the largest
/// and most expensive frames there are — turning a lossy link into an unusable
/// one.
///
/// The earlier test could not see this: it drained the outbound queue every
/// turn, so two losses never accumulated between drains and one ask per loss
/// looked identical to one ask per burst. Here the queue is left alone across
/// a burst, which is what a real caller sending on a timer does.
#[test]
fn a_burst_of_loss_produces_one_keyframe_request() {
    let mut h = host(30);
    let mut c = controller(&h);
    // Drop a lot, so several frames in a row are abandoned.
    let mut link = LossySink::new(40);
    let _ = c.take_outbound();

    let mut lost = 0u32;
    let mut out = Vec::new();
    for _ in 0..60 {
        let _ = h.step(Duration::from_secs(1), &mut link).expect("a step");
        for datagram in std::mem::take(&mut link.sent) {
            c.receive(&datagram, &mut out).expect("received");
        }
        lost += out
            .drain(..)
            .filter(|d| matches!(d, Decoded::Lost { .. }))
            .count() as u32;
        // Deliberately *not* draining the outbound queue.
    }
    let asks = c.take_outbound().len();
    println!(
        "{} dropped, {lost} frames lost, {asks} keyframe request(s)",
        link.dropped
    );
    assert!(
        lost >= 2,
        "only {lost} frames were lost; the loss rate is too low to test this"
    );
    assert_eq!(
        asks, 1,
        "{lost} lost frames produced {asks} keyframe requests; a burst must coalesce to one"
    );
}

/// The direction rule is stated twice — once in the protocol as
/// `may_be_sent_by`, once in what the host's `handle` actually accepts — and
/// the two must agree. If either changes alone, this fails rather than a peer
/// discovering the difference.
#[test]
fn the_direction_rule_and_what_the_host_accepts_agree() {
    let mut h = host(30);
    let messages = [
        MediaMessage::Config(h.config()),
        MediaMessage::Select(MonitorSelect {
            monitor: h.config().selected,
        }),
        MediaMessage::Hint(ownview_proto::QualityHint {
            max_fps: Some(30),
            max_long_edge: None,
            profile: Profile::Balanced,
        }),
        MediaMessage::Keyframe(ownview_proto::KeyframeRequest {
            reason: KeyframeReason::LostSync,
        }),
    ];
    for message in messages {
        let allowed = message.may_be_sent_by(Role::Controller);
        let refused_for_direction = matches!(
            h.handle(&message),
            Err(ownview_stream::StreamError::WrongDirection { .. })
        );
        assert_eq!(
            allowed,
            !refused_for_direction,
            "the protocol says a controller {} send {message:?}, and the host {}",
            if allowed { "may" } else { "may not" },
            if refused_for_direction {
                "refuses it"
            } else {
                "accepts it"
            }
        );
    }
}
