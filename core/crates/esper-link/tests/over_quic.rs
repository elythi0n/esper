#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A media session over a real QUIC connection.
//!
//! The in-memory tests in `esper-stream` prove the halves. This proves the
//! thing they cannot: that the halves work over an authenticated, encrypted
//! transport with real unreliable datagrams and a real path MTU.
//!
//! Two endpoints in one process, on loopback. Not two machines — that is the
//! phase's exit criterion and needs two machines — but everything between the
//! capture loop and the socket is the code that will run on them.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Once};
use std::time::Duration;

use esper_capture::gate::Policy;
use esper_capture::synthetic::SyntheticCapturer;
use esper_crypto::Identity;
use esper_decode::decoder::DecoderConfig;
use esper_decode::openh264::OpenH264Decoder;
use esper_encode::budget::Budget;
use esper_encode::encoder::{EncodeError, EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;
use esper_link::{ControllerLink, HostLink, LinkError, parts};
use esper_pipeline::pipeline::Pipeline;
use esper_proto::{Hello, KeyframeReason, MediaMessage, Profile, Role};
use esper_scale::Ceiling;
use esper_stream::Decoded;
use esper_transport::{AnyOf, Endpoint, NoPeers, OnlyPeer, Session};

const W: u32 = 320;
const H: u32 = 240;
const TIMEOUT: Duration = Duration::from_secs(20);

/// rustls needs a process-wide provider, installed once.
fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

fn encoder_for(config: &EncoderConfig) -> Result<Box<dyn VideoEncoder>, EncodeError> {
    Ok(Box::new(OpenH264::new(config)?))
}

fn pipeline(fps: u32) -> Pipeline {
    let capturer = SyntheticCapturer::new(W, H, 1_000_000).expect("a valid size");
    Pipeline::new(
        Box::new(capturer),
        &encoder_for,
        Budget::of(100.0),
        Policy {
            max_fps: fps,
            ..Policy::default()
        },
    )
    .expect("a pipeline")
    .with_ceiling(Ceiling::none())
}

/// A paired host and controller, each refusing everyone else.
struct Pair {
    host: Endpoint,
    controller: Endpoint,
    host_addr: SocketAddr,
    host_key: [u8; 32],
    controller_key: [u8; 32],
}

impl Pair {
    fn build() -> Self {
        init();
        let host_id = Identity::generate().expect("host identity");
        let controller_id = Identity::generate().expect("controller identity");
        let loopback: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();

        let host = Endpoint::bind(
            &host_id,
            loopback,
            Arc::new(AnyOf::new(vec![controller_id.public_key()])),
            Arc::new(NoPeers),
        )
        .expect("host endpoint");
        let host_addr = host.local_addr().expect("host addr");

        let controller = Endpoint::bind(
            &controller_id,
            loopback,
            Arc::new(NoPeers),
            Arc::new(OnlyPeer::new(host_id.public_key())),
        )
        .expect("controller endpoint");

        Self {
            host,
            controller,
            host_addr,
            host_key: host_id.public_key(),
            controller_key: controller_id.public_key(),
        }
    }

    /// Both sides of one authenticated session.
    async fn connect(&self) -> (Session, Session) {
        let accepting = self.host.accept();
        let dialling = self.controller.connect(self.host_addr);
        let (accepted, dialled) = tokio::join!(accepting, dialling);
        let accepted = accepted.expect("a connection").expect("accepted");
        let dialled = dialled.expect("dialled");

        let host_hello = Hello::new(Role::Host, self.host_key, "test-host");
        let ctrl_hello = Hello::new(Role::Controller, self.controller_key, "test-controller");
        let (h, c) = tokio::join!(
            Session::open(accepted, host_hello),
            Session::open(dialled, ctrl_hello)
        );
        (
            h.map_err(|(e, _)| e).expect("host session"),
            c.map_err(|(e, _)| e).expect("controller session"),
        )
    }
}

/// **A session, over QUIC.** The host's screen arrives as decoded pictures on
/// the controller, through a real handshake and real datagrams.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_screen_arrives_over_a_real_connection() {
    let pair = Pair::build();
    let (mut host_session, mut ctrl_session) = pair.connect().await;

    let (host_media, host_control) = parts(&mut host_session).expect("host parts");
    let (ctrl_media, ctrl_control) = parts(&mut ctrl_session).expect("controller parts");

    let mut host = HostLink::new(
        &host_media,
        host_control,
        pipeline(30),
        esper_proto::Seen::Shown,
    )
    .expect("a host link");
    let announced = host.announce().await.expect("announced");
    assert!(announced.is_well_formed());
    assert_eq!((announced.width, announced.height), (W, H));

    // The controller reads the config off the control stream, exactly as it
    // will on another machine.
    let mut controller = ControllerLink::open(ctrl_media, ctrl_control)
        .await
        .expect("the config arrived")
        .with_decoder(Box::new(
            OpenH264Decoder::new(&DecoderConfig::default()).expect("a decoder"),
        ));
    assert_eq!(controller.config().ssrc, announced.ssrc);
    assert_eq!(
        controller.config().width,
        W,
        "the controller must size itself from what the host announced"
    );

    // The host pumps on a thread of its own, because capture blocks.
    let (mut pump, mut control) = host.split();
    let sending = std::thread::spawn(move || {
        let mut sent = 0u32;
        for _ in 0..60 {
            match pump.pump(Duration::from_millis(200)) {
                Ok(esper_stream::HostStep::Sent { .. }) => sent += 1,
                Ok(_) => {}
                Err(e) => return Err(format!("{e}")),
            }
            if !pump.is_open() {
                break;
            }
        }
        Ok((sent, pump.sent()))
    });

    // The async side serves the control stream: the controller's keyframe
    // requests have to be acted on or the session never starts.
    let control_task = tokio::spawn(async move {
        let mut handled = 0u32;
        while let Ok(message) = control.recv().await {
            // The host cannot act on it here — the pump owns the pipeline —
            // so this proves the exchange works and counts what arrived.
            // Acting on it is what `HostPump::handle` is for, and a real
            // engine passes the message across on a channel.
            if matches!(message, MediaMessage::Keyframe(_)) {
                handled += 1;
            }
            if handled >= 1 {
                break;
            }
        }
        handled
    });

    let mut pictures = 0u32;
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while pictures < 5 && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), controller.receive(&mut out)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => panic!("receiving failed: {e}"),
            Err(_) => break,
        }
        for d in out.drain(..) {
            match d {
                Decoded::Picture { picture, .. } => {
                    assert_eq!((picture.width(), picture.height()), (W, H));
                    pictures += 1;
                }
                Decoded::Skipped { .. } => {}
                Decoded::Lost { why, .. } => {
                    // Loopback loses nothing, so this would be a real defect.
                    panic!("a frame was lost on loopback: {why}");
                }
            }
        }
    }

    let asks = tokio::time::timeout(Duration::from_secs(5), control_task)
        .await
        .map(|r| r.unwrap_or(0))
        .unwrap_or(0);
    let sent = sending.join().expect("the sending thread").expect("pumped");
    println!(
        "{} frames sent as {} datagrams ({} bytes), {pictures} pictures decoded, \
         {asks} keyframe request(s), {:?}",
        sent.0,
        sent.1.datagrams,
        sent.1.bytes,
        controller.stats()
    );

    assert!(sent.0 > 0, "the host sent nothing");
    assert!(
        pictures >= 5,
        "only {pictures} pictures arrived over QUIC; {:?}",
        controller.stats()
    );
    assert_eq!(
        sent.1.refused, 0,
        "the transport refused datagrams on loopback"
    );
    assert!(
        asks >= 1,
        "the controller's keyframe request never reached the host"
    );
}

/// The first thing a controller must see is the config. A peer that sends
/// something else is not one that speaks this protocol, and guessing the size
/// and codec of the pixels that follow is worse than refusing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_controller_refuses_a_link_that_does_not_announce_itself() {
    let pair = Pair::build();
    let (mut host_session, mut ctrl_session) = pair.connect().await;
    let (_, mut host_control) = parts(&mut host_session).expect("host parts");
    let (ctrl_media, ctrl_control) = parts(&mut ctrl_session).expect("controller parts");

    // A keyframe request where a config belongs.
    host_control
        .send(&MediaMessage::Keyframe(esper_proto::KeyframeRequest {
            reason: KeyframeReason::Joined,
        }))
        .await
        .expect("sent");

    let err = tokio::time::timeout(TIMEOUT, ControllerLink::open(ctrl_media, ctrl_control))
        .await
        .expect("did not hang")
        .expect_err("a keyframe request is not a configuration");
    assert!(
        matches!(err, LinkError::NoConfig),
        "expected NoConfig, got {err:?}"
    );
}

/// The MTU comes from the transport, not from a guess, and it is checked. A
/// path that cannot carry a usable datagram is worth saying so about — the
/// alternative is a session that comes up and then refuses every frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_path_mtu_comes_from_the_transport() {
    let pair = Pair::build();
    let (mut host_session, _ctrl) = pair.connect().await;
    let (media, control) = parts(&mut host_session).expect("host parts");
    let from_transport = media
        .max_datagram_size()
        .expect("loopback carries datagrams");
    assert!(
        from_transport >= esper_link::MIN_PATH,
        "loopback reported a {from_transport}-byte path"
    );

    let host = HostLink::new(&media, control, pipeline(30), esper_proto::Seen::Shown)
        .expect("a host link");
    // Every datagram the packetizer produces has to fit what the transport
    // said, which is what the sink's zero refusals in the session test show.
    assert!(host.config().is_well_formed());
    println!("the transport reports a {from_transport}-byte path");
}

/// A hint travels the other way and lowers what the host was going to do.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hint_reaches_the_host_over_the_control_stream() {
    let pair = Pair::build();
    let (mut host_session, mut ctrl_session) = pair.connect().await;
    let (host_media, host_control) = parts(&mut host_session).expect("host parts");
    let (ctrl_media, ctrl_control) = parts(&mut ctrl_session).expect("controller parts");

    let mut host = HostLink::new(
        &host_media,
        host_control,
        pipeline(60),
        esper_proto::Seen::Shown,
    )
    .expect("a host link");
    host.announce().await.expect("announced");
    assert_eq!(host.stream().pipeline().requested_fps(), 60);

    let mut controller = ControllerLink::open(ctrl_media, ctrl_control)
        .await
        .expect("the config arrived");
    controller
        .hint(Some(15), None, Profile::Text)
        .await
        .expect("hinted");

    // The host reads what arrives and acts on each. The controller's opening
    // keyframe request comes first, because a fresh controller asks for one
    // the moment it knows what the stream is — so this reads until the hint
    // rather than assuming it is first.
    let mut saw_hint = false;
    for _ in 0..4 {
        let message = tokio::time::timeout(TIMEOUT, host.recv())
            .await
            .expect("did not hang")
            .expect("a message");
        host.handle(&message).expect("accepted");
        if matches!(message, MediaMessage::Hint(_)) {
            saw_hint = true;
            break;
        }
    }
    assert!(saw_hint, "the hint never reached the host");
    assert_eq!(
        host.stream().pipeline().requested_fps(),
        15,
        "the hint did not reach the pipeline"
    );
}

/// **The opening ask goes out immediately, before any pixel arrives.**
///
/// A controller has no reference picture when it starts, so until a keyframe
/// arrives every frame it receives is one it must throw away. Leaving the
/// request queued until something else flushed it cost a round trip of black
/// at the start of every session — and it was invisible, because the ordinary
/// receive path flushes too, so the request went out on the *first datagram*
/// instead of straight away.
///
/// So this asserts the ordering: the host has the request in hand before it
/// has pumped a single frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_controllers_opening_request_arrives_before_any_frame() {
    let pair = Pair::build();
    let (mut host_session, mut ctrl_session) = pair.connect().await;
    let (host_media, host_control) = parts(&mut host_session).expect("host parts");
    let (ctrl_media, ctrl_control) = parts(&mut ctrl_session).expect("controller parts");

    let mut host = HostLink::new(
        &host_media,
        host_control,
        pipeline(30),
        esper_proto::Seen::Shown,
    )
    .expect("a host link");
    host.announce().await.expect("announced");

    // Opening the controller is the only thing that happens. No frame is
    // pumped, and nothing calls `receive`.
    let controller = ControllerLink::open(ctrl_media, ctrl_control)
        .await
        .expect("the config arrived");
    assert!(
        controller.awaiting_keyframe(),
        "a fresh controller must be waiting for a keyframe"
    );

    let message = tokio::time::timeout(Duration::from_secs(5), host.recv())
        .await
        .expect("the opening request never arrived")
        .expect("a message");
    assert!(
        matches!(
            message,
            MediaMessage::Keyframe(r) if r.reason == KeyframeReason::Joined
        ),
        "expected the opening keyframe request, got {message:?}"
    );
    // And the host acts on it, so the very first frame it sends is one the
    // controller can start from.
    host.handle(&message).expect("accepted");
}
