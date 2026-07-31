#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A whole media session, over a link that can be made to misbehave.
//!
//! Two machines are the real test and Phase 4's exit criterion. This is what
//! can be had without them, and it is more than it sounds: the host half and
//! the controller half are the code that will run on those machines, and an
//! in-memory link can drop the fourth packet of every frame on purpose, which
//! a real network cannot be asked to do.

use std::time::Duration;

use esper_capture::gate::Policy;
use esper_capture::synthetic::SyntheticCapturer;
use esper_decode::decoder::DecoderConfig;
use esper_decode::openh264::OpenH264Decoder;
use esper_encode::budget::Budget;
use esper_encode::encoder::{EncodeError, EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;
use esper_pipeline::pipeline::Pipeline;
use esper_proto::{KeyframeReason, MediaMessage, MonitorSelect, Profile, Role};
use esper_scale::Ceiling;
use esper_stream::{
    ControllerStream, DatagramSink, Decoded, Handled, HostStep, HostStream, RecordingSink,
    SinkError,
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

/// A host whose **owner** set a resolution cap, as `--max-resolution` does.
fn host_capped(fps: u32, long_edge: u32) -> HostStream {
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
    .with_ceiling(Ceiling::of(long_edge));
    HostStream::new(pipeline, MTU).expect("a host stream")
}

/// A host with two monitors of **different sizes**, so a switch is visible in
/// the frame rather than taken on trust.
fn host_with_two_monitors(fps: u32) -> HostStream {
    let capturer = SyntheticCapturer::new(W, H, 1_000_000)
        .expect("a valid size")
        .with_extra_monitor(W / 2, H / 2)
        .expect("a valid second size");
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

/// How many of these messages are keyframe requests.
///
/// **Not the length of the queue**, which is what three tests here used to
/// count and call "asks". The outbound queue also carries the ladders' hints
/// and the link reports, and both of those are driven by a *clock* — so on a
/// busy machine the loop in a test runs slowly enough for a hint to join the
/// queue, and a test counting everything reported two keyframe requests where
/// exactly one had been made. It then blamed the coalescing, which was working
/// perfectly.
///
/// That made `a_burst_of_loss_produces_one_keyframe_request` fail about one run
/// in three under load and pass every time on its own.
fn keyframe_asks(messages: &[MediaMessage]) -> usize {
    messages
        .iter()
        .filter(|m| matches!(m, MediaMessage::Keyframe(_)))
        .count()
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
    // **The controller is given the same clock the host is stepped on.**
    // Recovery is paced in wall time — a keyframe takes a round trip to
    // arrive, and asking again before then asks for something already on its
    // way — so a test that stepped the stream by a simulated second while the
    // controller saw a real microsecond would be measuring the pace against
    // the wrong clock.
    let start = std::time::Instant::now();
    for turn in 0..150u32 {
        let now = start + Duration::from_secs(u64::from(turn));
        let _ = h.step(Duration::from_secs(1), &mut link).expect("a step");
        let arrived = std::mem::take(&mut link.sent);
        for datagram in arrived {
            c.receive_at(&datagram, now, &mut out).expect("received");
        }
        for d in out.drain(..) {
            match d {
                Decoded::Picture { .. } => pictures += 1,
                Decoded::Lost { .. } => lost += 1,
                Decoded::Skipped { .. } => {}
            }
        }
        let said = c.take_outbound();
        asks += keyframe_asks(&said) as u32;
        for m in said {
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

/// **A viewer saying what it is looking at has to reach the encoder.**
///
/// The whole chain in one test: a controller states a sharpness, the hint goes
/// on the wire, the host translates the protocol's word into the encoder's,
/// the pipeline takes the coarser of that and its governor's own choice, and
/// the bytes coming out are fewer.
///
/// Every link in it existed in some form before and the chain did not.
/// `Profile` had been on the wire since Phase 4 — validated, plumbed through
/// the host, and recorded in a `tracing::debug!` that nothing read — while the
/// quantiser, which is the middle rung of the degradation order the design
/// asks for, could not be reached from the far end at all.
#[test]
fn a_viewer_that_will_settle_for_less_is_sent_less() {
    let measure = |sharpness: Option<esper_proto::Sharpness>| {
        let mut h = host(30);
        h.handle(&MediaMessage::Hint(esper_proto::QualityHint {
            max_fps: None,
            max_long_edge: None,
            profile: Profile::Balanced,
            sharpness,
        }))
        .expect("accepted");
        let mut sink = RecordingSink::new();
        sink.limit = Some(MTU);
        for _ in 0..20 {
            let _ = h
                .step(Duration::from_millis(30), &mut sink)
                .expect("a step");
        }
        (sink.bytes(), h.asked_fidelity())
    };

    let (sharp_bytes, sharp_asked) = measure(Some(esper_proto::Sharpness::Sharp));
    let (soft_bytes, soft_asked) = measure(Some(esper_proto::Sharpness::Soft));

    // The protocol's word became the encoder's, and `Sharp` is the sharpest
    // rung there is — which is the same as asking for nothing, because a hint
    // may only ever coarsen.
    assert_eq!(sharp_asked, Some(esper_pipeline::Fidelity::High));
    assert_eq!(soft_asked, Some(esper_pipeline::Fidelity::Coarse));

    assert!(
        soft_bytes < sharp_bytes,
        "a viewer that said it would settle for a soft picture was sent \
         {soft_bytes} bytes against {sharp_bytes} for a sharp one: the \
         quantiser rung reached nothing"
    );
    // And by a margin worth having, not a rounding error. Measured on the
    // synthetic pattern at 1280x720 the difference is about a third of the
    // bitrate; a fifth is a floor loose enough for a smaller picture and any
    // machine, and tight enough that a lever which stopped working would fail
    // this.
    let saved = 1.0 - (soft_bytes as f64 / sharp_bytes as f64);
    assert!(
        saved > 0.20,
        "settling for a soft picture saved only {:.0}% of the bytes",
        saved * 100.0
    );
    println!("a soft picture saved {:.0}% of the bytes", saved * 100.0);
}

/// **A link that recovers while a keyframe is outstanding has to come back.**
///
/// Asking happened only when a frame was *lost* or refused. So a session that
/// lost sync and whose link then healed went quiet in exactly the wrong way:
/// whole frames arriving, every one of them discarded for want of a keyframe,
/// and nothing left to trigger another ask. Measured on a link whose queue
/// drained after six seconds — five clean seconds, 74 frames delivered, not
/// one shown, and not one ask made in all that time.
///
/// A frame arriving whole while a keyframe is outstanding is the evidence the
/// last ask did not work: the link is carrying data and the keyframe is still
/// not here.
#[test]
fn whole_frames_arriving_without_a_keyframe_ask_again() {
    let mut h = host(30);
    let mut c = controller(&h);
    let mut clean = RecordingSink::new();
    clean.limit = Some(MTU);

    // Get the stream going, so there is a reference and frames decode.
    let _ = c.take_outbound();
    for _ in 0..4 {
        let _ = h
            .step(Duration::from_millis(30), &mut clean)
            .expect("a step");
    }
    let mut out = Vec::new();
    let start = std::time::Instant::now();
    for datagram in std::mem::take(&mut clean.sent) {
        c.receive_at(&datagram, start, &mut out).expect("received");
    }
    out.clear();
    let _ = c.take_outbound();

    // **Now the controller is waiting for a keyframe and the host does not
    // know it.** A reconfiguration leaves this side waiting — and the ask it
    // makes is deliberately never carried to the host below, which is the
    // ordinary case of a request that did not arrive. Every frame after this
    // arrives whole and is undecodable, which is the state the session got
    // stuck in.
    c.reconfigure(&h.config()).expect("its own config");
    let _ = c.take_outbound();
    let mut asks = 0u32;
    let mut skipped = 0u32;
    let mut now = start;
    for turn in 0..40u32 {
        now = start + Duration::from_millis(u64::from(turn) * 30);
        let _ = h
            .step(Duration::from_millis(30), &mut clean)
            .expect("a step");
        for datagram in std::mem::take(&mut clean.sent) {
            c.receive_at(&datagram, now, &mut out).expect("received");
        }
        skipped += out
            .drain(..)
            .filter(|d| matches!(d, Decoded::Skipped { .. }))
            .count() as u32;
        asks += c
            .take_outbound()
            .iter()
            .filter(|m| matches!(m, MediaMessage::Keyframe(_)))
            .count() as u32;
    }

    assert!(
        skipped >= 4,
        "only {skipped} frames were discarded; this cannot test the hole"
    );
    assert!(
        asks >= 1,
        "{skipped} whole frames were discarded over {:?} and the controller \
         never asked again: a session that lost sync can never get it back",
        now.saturating_duration_since(start)
    );
    // And not one per frame — the pace is what keeps this from being a storm.
    assert!(
        asks <= 8,
        "{asks} asks for {skipped} discarded frames is one per arrival"
    );
}

/// A sink whose path narrows underneath it, and which **swallows** what it
/// cannot carry.
///
/// Swallowing is the point. The real one does the same — a transport that
/// refuses a datagram is not an error a session dies of, so `send` returns
/// `Ok` and increments a counter — and that is exactly what made the bug it
/// guards against invisible for as long as it lasted.
struct NarrowingSink {
    sent: Vec<Vec<u8>>,
    limit: usize,
    swallowed: usize,
}

impl NarrowingSink {
    fn new(limit: usize) -> Self {
        Self {
            sent: Vec::new(),
            limit,
            swallowed: 0,
        }
    }
}

impl DatagramSink for NarrowingSink {
    fn send(&mut self, payload: &[u8]) -> Result<(), SinkError> {
        if payload.len() > self.limit {
            self.swallowed += 1;
            return Ok(());
        }
        self.sent.push(payload.to_vec());
        Ok(())
    }

    fn max_datagram_size(&self) -> Option<usize> {
        Some(self.limit)
    }
}

/// **The packetizer has to follow the path, because the path moves.**
///
/// QUIC probes its MTU upward and drops the estimate back when a probe
/// black-holes, which happens most readily on the links that need everything
/// else here to work. Measured on a rate-limited link with a full queue, one
/// session's path went 1351, 1382, 1414 and then 1162 and stayed there.
///
/// Sized once at the start, the packetizer then produces datagrams the
/// transport refuses as too large — silently, because a refused datagram is
/// not an error a session dies of. Small frames still fit, so the session
/// goes on looking alive while no keyframe ever arrives whole. `set_mtu`
/// existed for this, on both the packetizer and the stream, and neither had a
/// caller.
#[test]
fn the_packetizer_follows_a_path_that_narrows() {
    let mut h = host(30);
    let mut sink = NarrowingSink::new(1400);
    // A few frames at the wide path, so the packetizer is certainly using it.
    for _ in 0..4 {
        let _ = h
            .step(Duration::from_millis(30), &mut sink)
            .expect("a step");
    }
    assert_eq!(sink.swallowed, 0, "nothing should be refused at 1400");
    assert!(
        sink.sent.iter().any(|d| d.len() > 1162),
        "this test needs the wide path to actually be used, \
         or narrowing it proves nothing"
    );

    // The path narrows to QUIC's floor, as a black-hole detection does.
    sink.limit = 1162;
    sink.sent.clear();
    // **And then a keyframe**, which is the frame this matters for: a delta
    // frame fits in one small datagram whatever the path is doing, so a test
    // that sent only delta frames would pass with the packetizer sizing for a
    // path that no longer exists. A keyframe spans several datagrams, and it
    // is the frame a receiver that lost sync is waiting for.
    for _ in 0..12 {
        h.handle(&MediaMessage::Keyframe(esper_proto::KeyframeRequest {
            reason: KeyframeReason::LostSync,
        }))
        .expect("accepted");
        let _ = h
            .step(Duration::from_millis(30), &mut sink)
            .expect("a step");
    }
    assert!(
        sink.sent.len() > 12,
        "the keyframes should have spanned several datagrams each, got {}",
        sink.sent.len()
    );

    assert_eq!(
        sink.swallowed, 0,
        "the transport refused {} datagram(s) after the path narrowed: \
         the packetizer is still sizing for a path that is gone",
        sink.swallowed
    );
    assert!(
        sink.sent.iter().all(|d| d.len() <= 1162),
        "a datagram larger than the path was handed over"
    );

    // And it follows the path back up, or a link that recovers stays
    // needlessly fragmented for the rest of the session.
    sink.limit = 1400;
    sink.sent.clear();
    for _ in 0..12 {
        h.handle(&MediaMessage::Keyframe(esper_proto::KeyframeRequest {
            reason: KeyframeReason::LostSync,
        }))
        .expect("accepted");
        let _ = h
            .step(Duration::from_millis(30), &mut sink)
            .expect("a step");
    }
    assert_eq!(sink.swallowed, 0);
    assert!(
        sink.sent.iter().any(|d| d.len() > 1162),
        "the wider path should be in use again"
    );
}

/// **The link ladder.** A controller that keeps losing frames asks for smaller
/// ones, because frame size is what decides whether loss is survivable — and
/// nothing else in the system can see the link at all. The host's own ladder
/// spends framerate, then quality, then resolution when the *CPU* cannot keep
/// up; every frame it produces leaves the machine, so it cannot know how many
/// arrived.
#[test]
fn sustained_loss_asks_for_smaller_frames() {
    let mut h = host(30);
    let mut c = controller(&h);
    let mut link = LossySink::new(60);
    let _ = c.take_outbound();
    assert_eq!(c.link_ceiling(), None, "a fresh stream asks for nothing");

    let start = std::time::Instant::now();
    let mut out = Vec::new();
    let mut hints = Vec::new();
    // Four seconds of a link losing most of it, a window a second.
    for turn in 0..160u32 {
        let now = start + Duration::from_millis(u64::from(turn) * 25);
        let _ = h
            .step(Duration::from_millis(25), &mut link)
            .expect("a step");
        for datagram in std::mem::take(&mut link.sent) {
            c.receive_at(&datagram, now, &mut out).expect("received");
        }
        out.clear();
        for m in c.take_outbound() {
            if let MediaMessage::Hint(hint) = &m {
                hints.push(hint.max_long_edge);
            }
            h.handle(&m).expect("accepted");
        }
    }

    println!("asked for {hints:?}");
    let asked = c.link_ceiling().expect("the ladder should have moved");
    assert!(
        !hints.is_empty(),
        "four seconds of heavy loss produced no hint at all"
    );
    // The first step starts from the size the host is actually sending — this
    // synthetic screen is 320 wide — rather than from the top of the list, so
    // a small session does not spend three windows asking for sizes larger
    // than it has. There is nowhere below 320, so one hint is the whole
    // ladder here; the rung count is what the recovery test exercises.
    assert!(
        asked <= 320,
        "the ladder asked for {asked} after four seconds of heavy loss"
    );
    // And the host is now capped at what was asked for, which is the point:
    // smaller frames span fewer datagrams and survive loss.
    assert_eq!(h.pipeline().ceiling().long_edge(), Some(asked));
}

/// And it gives the picture back when the link recovers.
///
/// Down fast, up slow — but never stuck: a session that survived a bad minute
/// at 320 pixels and stayed there for the rest of the afternoon would be a
/// worse product than one that never stepped down.
#[test]
fn a_link_that_recovers_gets_its_resolution_back() {
    let mut h = host(30);
    let mut c = controller(&h);
    let mut link = LossySink::new(60);
    let _ = c.take_outbound();

    let start = std::time::Instant::now();
    let mut out = Vec::new();
    let mut turn = 0u32;
    // Two seconds of loss to move the ladder down.
    while turn < 80 {
        let now = start + Duration::from_millis(u64::from(turn) * 25);
        let _ = h
            .step(Duration::from_millis(25), &mut link)
            .expect("a step");
        for datagram in std::mem::take(&mut link.sent) {
            c.receive_at(&datagram, now, &mut out).expect("received");
        }
        out.clear();
        for m in c.take_outbound() {
            h.handle(&m).expect("accepted");
        }
        turn += 1;
    }
    let stepped_down = c.link_ceiling().expect("the ladder should have moved down");

    // Then a clean link for long enough to earn it back.
    let mut clean = LossySink::new(0);
    while turn < 560 {
        let now = start + Duration::from_millis(u64::from(turn) * 25);
        let _ = h
            .step(Duration::from_millis(25), &mut clean)
            .expect("a step");
        for datagram in std::mem::take(&mut clean.sent) {
            c.receive_at(&datagram, now, &mut out).expect("received");
        }
        out.clear();
        for m in c.take_outbound() {
            h.handle(&m).expect("accepted");
        }
        turn += 1;
    }

    let recovered = c.link_ceiling().expect("still on the ladder");
    println!("stepped down to {stepped_down}, recovered to {recovered}");
    assert!(
        recovered > stepped_down,
        "a link that recovered stayed at {recovered}"
    );
}

/// **A controller may never exceed what this machine's owner allowed, and may
/// always relax its own request back to it.**
///
/// Two values and a `min`, rather than one value that only ever tightens. The
/// difference matters because a controller has a reason to lower the ceiling
/// and then raise it: a link losing frames wants smaller frames, and a link
/// that has recovered wants the picture back. With one value, "the tightest
/// thing ever asked for" is permanent, and a viewer that survived a bad minute
/// spends the rest of the session at 320 pixels.
///
/// What is preserved is the property the one-value version was protecting: a
/// viewer cannot make somebody else's laptop work harder than its owner said.
#[test]
fn a_hint_is_bounded_by_the_owner_and_not_by_its_own_past() {
    let mut h = host(60);
    assert_eq!(h.pipeline().requested_fps(), 60);

    // Asking for less is honoured.
    h.handle(&MediaMessage::Hint(esper_proto::QualityHint {
        max_fps: Some(15),
        max_long_edge: Some(640),
        profile: Profile::Text,
        sharpness: None,
    }))
    .expect("accepted");
    assert_eq!(h.pipeline().requested_fps(), 15);
    assert_eq!(h.pipeline().ceiling().long_edge(), Some(640));

    // **Asking for more than the owner allowed is not.**
    h.handle(&MediaMessage::Hint(esper_proto::QualityHint {
        max_fps: Some(240),
        max_long_edge: Some(4096),
        profile: Profile::Motion,
        sharpness: None,
    }))
    .expect("accepted");
    assert_eq!(
        h.pipeline().requested_fps(),
        60,
        "a hint raised the frame rate above what this machine's owner allowed"
    );
    // This pipeline's owner set **no** resolution cap, so the controller's own
    // number is the only limit and asking for a larger one is honoured. That
    // is the relaxation the two-value model exists for: what it may not do is
    // exceed a cap the owner set, which the next test is about.
    assert_eq!(
        h.pipeline().ceiling().long_edge(),
        Some(4096),
        "a controller could not relax a cap it had set itself"
    );

    // And relaxing to nothing at all leaves the owner's own values in force.
    h.handle(&MediaMessage::Hint(esper_proto::QualityHint {
        max_fps: None,
        max_long_edge: None,
        profile: Profile::Balanced,
        sharpness: None,
    }))
    .expect("accepted");
    assert_eq!(h.pipeline().requested_fps(), 60);
    assert_eq!(h.pipeline().ceiling().long_edge(), None);
}

/// The owner's cap holds against a controller that asks for more, at every
/// point — including after the controller has already asked for less.
#[test]
fn an_owners_resolution_cap_is_never_exceeded() {
    let mut h = host_capped(30, 960);
    assert_eq!(h.pipeline().ceiling().long_edge(), Some(960));

    for asked in [640u32, 4096, 1920, 320] {
        h.handle(&MediaMessage::Hint(esper_proto::QualityHint {
            max_fps: None,
            max_long_edge: Some(asked),
            profile: Profile::Balanced,
            sharpness: None,
        }))
        .expect("accepted");
        let effective = h.pipeline().ceiling().long_edge().expect("a cap");
        assert!(
            effective <= 960,
            "asking for {asked} produced a ceiling of {effective}, above the owner's 960"
        );
        assert_eq!(
            effective,
            asked.min(960),
            "asking for {asked} should give the tighter of it and the owner's 960"
        );
    }
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
            esper_stream::StreamError::WrongDirection {
                role: Role::Controller,
                ..
            }
        ),
        "got {err:?}"
    );
}

/// **A switch changes the stream's shape, and the config is the only thing
/// that says so.** A controller that was not told would size its buffers from
/// the old announcement and hold a decoder whose reference pictures are all of
/// another screen.
#[test]
fn switching_monitor_re_announces_a_new_size_and_a_new_stream() {
    let mut h = host_with_two_monitors(30);
    let before = h.config();
    assert!(before.monitors.len() >= 2, "the fixture needs two monitors");
    let other = before
        .monitors
        .iter()
        .map(|m| m.id)
        .find(|id| *id != before.selected)
        .expect("another monitor");

    let Handled::Announce(after) = h
        .handle(&MediaMessage::Select(MonitorSelect { monitor: other }))
        .expect("the switch")
    else {
        panic!("a switch must answer with the config the controller has to be told");
    };

    assert_eq!(after.selected, other);
    assert_ne!(
        (after.width, after.height),
        (before.width, before.height),
        "the fixture's monitors differ in size, so the announced size must too"
    );
    assert_ne!(
        after.ssrc, before.ssrc,
        "a different screen is a different synchronisation source"
    );
    assert!(
        after.is_well_formed(),
        "and it must still be a legal config"
    );

    // What comes out is the new monitor's size, not merely what was announced.
    let mut sink = RecordingSink::default();
    let mut frames = 0;
    for _ in 0..16 {
        if let HostStep::Sent { .. } = h
            .step(Duration::from_millis(50), &mut sink)
            .expect("a step")
        {
            frames += 1;
            if frames == 2 {
                break;
            }
        }
    }
    assert!(frames > 0, "nothing was sent after the switch");

    let mut c = ControllerStream::new(&after)
        .expect("the host's own config")
        .with_decoder(Box::new(
            OpenH264Decoder::new(&DecoderConfig::default()).expect("a decoder"),
        ));
    let mut out = Vec::new();
    for datagram in &sink.sent {
        c.receive(datagram, &mut out).expect("a datagram");
    }
    // A frame is released when the next one begins, or when the stream is
    // flushed. The last frame here has no successor.
    c.flush(&mut out);
    let picture = out
        .iter()
        .find_map(|d| match d {
            Decoded::Picture { picture, .. } => Some(picture),
            _ => None,
        })
        .expect("a picture of the new monitor");
    assert_eq!(
        (picture.width(), picture.height()),
        (after.width, after.height),
        "the pixels are of the newly selected monitor"
    );
}

/// Selecting the monitor already being sent costs nothing.
///
/// It is what a controller does when somebody clicks the screen they are
/// already watching, and the wrong answer is expensive: a keyframe, a whole
/// frame's conversion, and a visible hitch for no change at all.
#[test]
fn selecting_the_current_monitor_changes_nothing() {
    let mut h = host_with_two_monitors(30);
    let before = h.config();
    let announced = h
        .handle(&MediaMessage::Select(MonitorSelect {
            monitor: before.selected,
        }))
        .expect("selecting the current monitor is fine");
    assert!(
        matches!(announced, Handled::Nothing),
        "there is nothing to announce, so nothing must be announced: {announced:?}"
    );
    assert_eq!(
        h.config().ssrc,
        before.ssrc,
        "the stream is the same stream"
    );
}

/// **A refusal is still an answer.** A monitor that does not exist is refused
/// with the reason *and* with the config the host is keeping — because a
/// controller told nothing cannot tell "no" from a host that has stopped
/// responding, and would sit showing a still picture waiting for a switch that
/// is never coming.
#[test]
fn switching_to_a_monitor_that_is_not_there_keeps_the_one_that_is() {
    let mut h = host_with_two_monitors(30);
    let before = h.config();
    let answer = h
        .handle(&MediaMessage::Select(MonitorSelect { monitor: 99 }))
        .expect("a refusal is not a session failure");
    let Handled::Refused { config, why } = answer else {
        panic!("the controller has to be told, and told why: {answer:?}");
    };
    let text = why.to_string();
    assert!(
        text.contains("cannot switch") && text.contains("99"),
        "the refusal must name what was asked for: {text}"
    );
    assert_eq!(
        config.selected, before.selected,
        "and the answer is the monitor it is keeping"
    );
    let after = h.config();
    assert_eq!(
        (after.selected, after.ssrc, after.width, after.height),
        (before.selected, before.ssrc, before.width, before.height),
        "a refused switch must leave the session exactly as it was"
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
    too_big.width = esper_proto::MAX_PICTURE_EDGE + 1;
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
    let queued = c.take_outbound();
    let asks = keyframe_asks(&queued);
    println!(
        "{} dropped, {lost} frames lost, {asks} keyframe request(s) out of {} \
         message(s) queued",
        link.dropped,
        queued.len()
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

/// **The request storm, and the rule that stops it.**
///
/// The queue rule alone is only as good as how often the caller drains it. A
/// caller that drains every turn — which is what a real one does — turns every
/// lost frame into another request while the first keyframe is still in
/// flight, and each of those produces a keyframe, which is large, which is
/// more likely to lose a datagram.
///
/// What it bounds is the ask driven by *refusal*: a frame that arrives whole
/// and cannot be decoded asks for a keyframe too, so a session in trouble
/// would otherwise ask once per arriving frame. The numbers first recorded
/// here — three pictures in ten seconds — came from runs against a decoder
/// worker that gave up permanently on its first undecodable frame, and were a
/// coin flip rather than a measurement of this rule.
#[test]
fn sustained_loss_asks_no_faster_than_the_pace() {
    let mut h = host(30);
    let mut c = controller(&h);
    c.set_round_trip(Duration::from_millis(200));
    // Losing most of them, so the link loses more frames per second than the
    // pace allows requests. A gentler link cannot test a rate limit: it never
    // reaches the limit.
    let mut link = LossySink::new(70);
    let _ = c.take_outbound();

    let start = std::time::Instant::now();
    let mut asks = 0u32;
    let mut lost = 0u32;
    let mut out = Vec::new();
    // Forty turns of twenty-five milliseconds: one second of link at thirty
    // frames a second, drained every turn like a real caller. Fine-grained on
    // purpose — the storm this prevents is one request per lost frame, so the
    // test has to lose more frames per second than the pace allows requests.
    for turn in 0..40u32 {
        let now = start + Duration::from_millis(u64::from(turn) * 25);
        let _ = h
            .step(Duration::from_millis(25), &mut link)
            .expect("a step");
        for datagram in std::mem::take(&mut link.sent) {
            c.receive_at(&datagram, now, &mut out).expect("received");
        }
        lost += out
            .drain(..)
            .filter(|d| matches!(d, Decoded::Lost { .. }))
            .count() as u32;
        asks += keyframe_asks(&c.take_outbound()) as u32;
    }

    println!(
        "{} dropped, {lost} lost, {asks} asks in one second",
        link.dropped
    );
    assert!(
        lost >= 8,
        "only {lost} frames were lost in a second; this cannot test pacing"
    );
    // One second at one per 200 ms: five, and a sixth would need the clock to
    // have moved further than this loop moves it.
    assert!(
        asks <= 6,
        "{lost} lost frames produced {asks} requests in one second, at a pace of 200 ms"
    );
    // And it does not stop asking: a link that keeps losing must keep
    // recovering, or a session that lost one keyframe stays black for good.
    assert!(asks >= 2, "sustained loss produced only {asks} request(s)");
}

/// Joining is not loss recovery, and is never paced.
///
/// There is nothing to show until the first keyframe arrives, so making
/// somebody wait a round trip for the *request* would be adding the one delay
/// the pace exists to remove.
#[test]
fn joining_asks_immediately() {
    let h = host(30);
    let mut c = controller(&h);
    let asked = c
        .take_outbound()
        .iter()
        .any(|m| matches!(m, MediaMessage::Keyframe(_)));
    assert!(asked, "a fresh stream did not ask for a keyframe");
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
        MediaMessage::Hint(esper_proto::QualityHint {
            max_fps: Some(30),
            max_long_edge: None,
            profile: Profile::Balanced,
            sharpness: None,
        }),
        MediaMessage::Keyframe(esper_proto::KeyframeRequest {
            reason: KeyframeReason::LostSync,
        }),
        // **The one where the direction is the whole point.** A controller
        // that could state its own permissions could grant itself any of
        // them, so this must be refused rather than merely ignored.
        MediaMessage::Permissions(esper_proto::PermissionChange {
            granted: vec![
                esper_proto::Permission::View,
                esper_proto::Permission::Control,
            ],
        }),
        // And the other one: a controller that could hold the picture could
        // stop a host showing what is happening on its own screen.
        MediaMessage::Paused(esper_proto::Paused { paused: true }),
    ];
    for message in messages {
        let allowed = message.may_be_sent_by(Role::Controller);
        let refused_for_direction = matches!(
            h.handle(&message),
            Err(esper_stream::StreamError::WrongDirection { .. })
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

/// **A static screen must still show something.**
///
/// Reassembly is deliberately correct rather than eager: a frame whose start
/// is not yet known waits for the *next* frame to bound it. On a static screen
/// there is no next frame — the host has nothing to send and correctly sends
/// nothing — so without a deadline the first frame waits forever and the
/// viewer shows nothing at all.
///
/// Pointed at a real desktop for the first time, that is exactly what
/// happened: 382 datagrams arrived, none were lost, none were skipped, and no
/// picture was ever produced. `flush` existed for this and nothing called it.
///
/// So this pins both halves of the contract: without a flush the frame is
/// held, and with one it arrives.
#[test]
fn one_frame_followed_by_silence_needs_a_flush_to_be_shown() {
    let mut h = host(30);
    let mut c = controller(&h);
    let mut sink = RecordingSink::new();
    for m in c.take_outbound() {
        h.handle(&m).expect("accepted");
    }

    // Exactly one frame, then nothing more — a screen that changed once.
    let mut sent = 0;
    while sent == 0 {
        if let HostStep::Sent { .. } = h.step(Duration::from_secs(1), &mut sink).expect("a step") {
            sent += 1;
        }
    }
    let mut out = Vec::new();
    for datagram in sink.sent.drain(..) {
        c.receive(&datagram, &mut out).expect("received");
    }

    // Held, because nothing has said the frame is over.
    assert!(
        !out.iter().any(|d| matches!(d, Decoded::Picture { .. })),
        "a frame was emitted before anything bounded it: {out:?}"
    );

    // The deadline is what releases it.
    c.flush(&mut out);
    assert!(
        out.iter().any(|d| matches!(d, Decoded::Picture { .. })),
        "a flush did not release the frame, so a static screen would never \
         appear: {out:?}"
    );
    println!("{} result(s) after the flush", out.len());
}

/// **The deadline lives in one place now, and it works through the stream.**
///
/// The static-screen bug was fixed by making a caller apply a hundred-
/// millisecond wait. That is the wrong home for the number: a quarter of a
/// second of stutter at 60 fps, and one copy per caller. `ControllerStream`
/// owns it, derives it from the stream's own framerate, and tells a caller how
/// long to wait — so a caller cannot get it wrong by not knowing.
///
/// Timed with an explicit clock rather than by sleeping, because a test that
/// waits real milliseconds to check a timeout is slow and flaky for nothing.
#[test]
fn the_stream_says_how_long_to_wait_and_releases_the_frame_when_it_expires() {
    use std::time::Instant;

    let mut h = host(30);
    let mut c = controller(&h);
    let mut sink = RecordingSink::new();
    for m in c.take_outbound() {
        h.handle(&m).expect("accepted");
    }

    // No frame waiting — but a keyframe was asked for on joining and has not
    // arrived, and nothing incoming will ever wake this side to ask again. So
    // the deadline is that retry, and a caller that blocked for as long as it
    // liked would sleep through it.
    let t0 = Instant::now();
    let waiting = c.due_in(t0).expect("the keyframe retry is a deadline too");
    assert!(
        waiting > Duration::ZERO && waiting <= Duration::from_millis(500),
        "with no frame to release the only deadline is the retry, got {waiting:?}"
    );

    // One frame, then silence.
    while !matches!(
        h.step(Duration::from_secs(1), &mut sink).expect("a step"),
        HostStep::Sent { .. }
    ) {}
    let mut out = Vec::new();
    for datagram in sink.sent.drain(..) {
        c.receive_at(&datagram, t0, &mut out).expect("received");
    }

    // Now something is waiting, and the stream says for how long.
    let left = c.due_in(t0).expect("a frame is waiting");
    assert!(
        left > Duration::ZERO && left <= esper_stream::jitter::MAX_WAIT,
        "the deadline was {left:?}"
    );
    // Two frame intervals at 30 fps, which is what the derivation says.
    assert_eq!(left.as_millis(), 66, "the deadline is not the derived one");

    // Before it expires, nothing is released.
    c.tick(t0 + Duration::from_millis(50), &mut out);
    assert!(
        !out.iter().any(|d| matches!(d, Decoded::Picture { .. })),
        "the frame was released before its deadline: {out:?}"
    );

    // After it expires, it is.
    c.tick(t0 + Duration::from_millis(70), &mut out);
    assert!(
        out.iter().any(|d| matches!(d, Decoded::Picture { .. })),
        "the deadline passed and the frame was not released: {out:?}"
    );
    assert_eq!(c.jitter().deadlines, 1);
    // And nothing is waiting any more.
    assert_eq!(c.due_in(t0 + Duration::from_millis(70)), None);
}

/// A frame that completes on its own must not leave a deadline armed — the
/// next thing the caller does would then be to flush nothing, repeatedly.
#[test]
fn a_frame_that_completes_normally_arms_no_deadline() {
    use std::time::Instant;

    let mut h = host(30);
    let mut c = controller(&h);
    let mut sink = RecordingSink::new();
    for m in c.take_outbound() {
        h.handle(&m).expect("accepted");
    }

    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut pictures = 0;
    // Enough frames that later ones bound earlier ones and complete normally.
    for i in 0..12u32 {
        let _ = h.step(Duration::from_secs(1), &mut sink).expect("a step");
        for datagram in sink.sent.drain(..) {
            c.receive_at(
                &datagram,
                t0 + Duration::from_millis(u64::from(i) * 33),
                &mut out,
            )
            .expect("received");
        }
        pictures += out
            .drain(..)
            .filter(|d| matches!(d, Decoded::Picture { .. }))
            .count();
    }
    assert!(pictures > 3, "only {pictures} pictures completed normally");
    assert_eq!(
        c.jitter().deadlines,
        0,
        "a deadline fired on a stream that never needed one"
    );
}

// ── Reconnect ───────────────────────────────────────────────────────────────

/// **A media session is resumed, not replayed — and that is the design.**
///
/// Terminal mode keeps a backlog, because a shell goes on producing output
/// while nobody is connected and output nobody saw is output lost. Video is the
/// opposite: a frame from ten seconds ago is a picture of a screen that no
/// longer exists. So there is no backlog, and what a reconnect gets is a
/// keyframe.
#[test]
fn a_resumed_session_keeps_the_pipeline_and_sends_a_keyframe() {
    use esper_stream::Parked;
    use std::time::Instant;

    const OWNER: [u8; 32] = [7u8; 32];
    let mut h = host(30);
    let before = h.config();
    let ssrc = h.ssrc();

    // Run a little, so the pipeline has state worth keeping.
    let mut sink = RecordingSink::new();
    for _ in 0..8 {
        let _ = h.step(Duration::from_secs(1), &mut sink).expect("a step");
    }
    let sequence = h.next_sequence();
    assert!(
        sequence > 0,
        "nothing was sent, so there is nothing to resume"
    );

    // The transport goes away.
    let parked = Parked::park(h, OWNER, Instant::now());
    assert_eq!(parked.encode_size(), (before.width, before.height));

    // And comes back.
    let mut resumed = parked
        .resume(&OWNER, 1200)
        .map_err(|(_, e)| e)
        .expect("the owner may resume");

    // **Nothing was re-negotiated.** Same monitor, same size, same stream.
    let after = resumed.config();
    assert_eq!(after.selected, before.selected, "the monitor changed");
    assert_eq!((after.width, after.height), (before.width, before.height));
    assert_eq!(resumed.ssrc(), ssrc, "the stream's identity changed");
    assert_eq!(
        resumed.next_sequence(),
        sequence,
        "the numbering restarted, so a capture reads as two streams"
    );

    // And the first frame out is one a fresh decoder can start from, because
    // the controller on the other end is new.
    let mut sink = RecordingSink::new();
    let mut first_keyframe = None;
    for _ in 0..20 {
        if let HostStep::Sent { keyframe, .. } = resumed
            .step(Duration::from_secs(1), &mut sink)
            .expect("a step")
        {
            first_keyframe = Some(keyframe);
            break;
        }
    }
    assert_eq!(
        first_keyframe,
        Some(true),
        "the first frame after a resume was not a keyframe, so the reconnected \
         controller has nothing it can decode"
    );
}

/// **A parked session belongs to the device that had it.**
///
/// Without this check a controller could inherit another controller's session
/// by reconnecting first — a session it was never granted, on a screen it may
/// not be allowed to see.
#[test]
fn another_device_cannot_resume_someone_elses_session() {
    use esper_stream::{Parked, ResumeRefused};
    use std::time::Instant;

    const OWNER: [u8; 32] = [1u8; 32];
    const STRANGER: [u8; 32] = [2u8; 32];
    let parked = Parked::park(host(30), OWNER, Instant::now());
    assert!(parked.may_resume(&OWNER));
    assert!(!parked.may_resume(&STRANGER));

    let (parked, refused) = parked
        .resume(&STRANGER, 1200)
        .map(|_| panic!("a stranger resumed somebody else's session"))
        .unwrap_err();
    assert_eq!(refused, ResumeRefused::NotYours);
    assert_eq!(refused.reason(), esper_proto::Reason::PERMISSION_DENIED);

    // And it is still there for its owner, because a refused resume must not
    // destroy the session it was refusing to hand over.
    assert!(
        parked.resume(&OWNER, 1200).is_ok(),
        "a refused resume destroyed the session"
    );
}

/// **Parking is bounded.** A parked session holds a capture pipeline, an
/// encoder, and on a host the screen being captured — so a controller that is
/// never coming back must stop costing anything.
#[test]
fn a_parked_session_expires() {
    use esper_stream::{Expired, PARK_TIMEOUT, Parked};
    use std::time::Instant;

    let t0 = Instant::now();
    let parked = Parked::park(host(30), [0u8; 32], t0);
    assert!(!parked.is_expired(t0));
    assert!(!parked.is_expired(t0 + PARK_TIMEOUT - Duration::from_millis(1)));
    assert!(parked.is_expired(t0 + PARK_TIMEOUT));
    assert_eq!(
        parked.parked_for(t0 + Duration::from_secs(3)),
        Duration::from_secs(3)
    );

    // Every way it can end carries a reason the peer can act on, and none of
    // them is a failure of either side's making.
    for e in [Expired::TimedOut, Expired::Superseded, Expired::HostGone] {
        assert!(
            !e.reason().is_failure(),
            "{e:?} closes with a failure code, so a laptop that closed its lid \
             looks like a peer that misbehaved"
        );
        assert!(e.reason().describe().is_some());
        assert!(!e.describe().is_empty());
    }
}

/// A resume onto a path that cannot carry video is refused, and the session
/// survives the refusal.
#[test]
fn a_resume_onto_an_unusable_path_keeps_the_session() {
    use esper_stream::{Parked, ResumeRefused};
    use std::time::Instant;

    const OWNER: [u8; 32] = [3u8; 32];
    let parked = Parked::park(host(30), OWNER, Instant::now());
    let (parked, refused) = parked
        .resume(&OWNER, 40)
        .map(|_| panic!("a 40-byte path was accepted"))
        .unwrap_err();
    assert!(matches!(refused, ResumeRefused::Stream(_)), "{refused:?}");
    assert_eq!(refused.reason(), esper_proto::Reason::MIGRATION_FAILED);
    assert!(
        parked.resume(&OWNER, 1200).is_ok(),
        "a refused resume destroyed the session"
    );
}

/// **What the far end measures has to reach the end that can act on it.**
///
/// The controller measures the link from how a frame's datagrams arrive; the
/// host's transport is the only place that can pace without putting a second
/// serialiser in front of the wire. The message between them is the whole
/// mechanism, and a host that accepted it and forgot it would look exactly
/// like a link nobody could measure.
#[test]
fn a_host_remembers_what_the_controller_measured_and_forgets_it_when_it_is_old() {
    let mut host = host(30);
    let fresh = Duration::from_secs(5);
    assert_eq!(
        host.reported_capacity(fresh),
        None,
        "a host that was told nothing claims to know something"
    );

    host.handle(&MediaMessage::Report(esper_proto::LinkReport {
        capacity_kbit: Some(1234),
    }))
    .expect("a report is a message a controller may send");
    assert_eq!(host.reported_capacity(fresh), Some(1234));

    // **A stale estimate is worse than none.** A link that got faster would be
    // held at the old rate for the rest of the session with nothing saying why,
    // so anything older than the caller's own patience is not an answer.
    assert_eq!(
        host.reported_capacity(Duration::ZERO),
        None,
        "a reading from the past was given as a reading from now"
    );

    // Withdrawn, because the far end can stop being able to measure.
    host.handle(&MediaMessage::Report(esper_proto::LinkReport {
        capacity_kbit: None,
    }))
    .expect("a withdrawal is a report");
    assert_eq!(host.reported_capacity(fresh), None);
}

/// A report is something a controller sends, and a host sending one would be
/// telling the controller what the controller measured.
#[test]
fn a_report_only_travels_one_way() {
    let report = MediaMessage::Report(esper_proto::LinkReport {
        capacity_kbit: Some(800),
    });
    assert!(report.may_be_sent_by(Role::Controller));
    assert!(!report.may_be_sent_by(Role::Host));

    let mut viewer = controller(&host(30));
    assert!(
        matches!(
            viewer.handle(&report),
            Err(esper_stream::StreamError::WrongDirection { .. })
        ),
        "a controller accepted a report from a host"
    );
}

/// A number outside anything plausible is refused before it can throttle a
/// session to nothing.
#[test]
fn a_report_outside_its_bounds_is_refused() {
    let mut host = host(30);
    for bad in [0u32, esper_proto::MOST_REPORTED_KBIT + 1, u32::MAX] {
        let message = MediaMessage::Report(esper_proto::LinkReport {
            capacity_kbit: Some(bad),
        });
        assert!(
            !message.is_well_formed(),
            "{bad} was accepted as a capacity"
        );
        assert!(
            matches!(
                host.handle(&message),
                Err(esper_stream::StreamError::Malformed { .. })
            ),
            "{bad} reached the host"
        );
    }
    assert_eq!(host.reported_capacity(Duration::from_secs(5)), None);
}
