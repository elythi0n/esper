#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A desktop that changes under a session.
//!
//! # Why these are here rather than on a machine
//!
//! Unplugging a monitor mid-session breaks the announced layout, the selected
//! monitor, the pointer's frame of reference and every buffer size downstream
//! at once — and arranging it on real hardware means somebody standing next to
//! a machine with a cable in their hand. X11's own RandR 1.5 can add a
//! *logical* monitor with no cable, but an `Xvfb` will not, so CI cannot do it
//! either.
//!
//! So the synthetic capturer does it, and everything above capture is the code
//! that will run on the machine with the cable.

use std::time::Duration;

use esper_capture::capturer::MonitorId;
use esper_capture::gate::Policy;
use esper_capture::synthetic::SyntheticCapturer;
use esper_encode::budget::Budget;
use esper_encode::encoder::{EncodeError, EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;
use esper_pipeline::pipeline::Pipeline;
use esper_proto::{MediaMessage, MonitorSelect, Role, StreamConfig};
use esper_scale::Ceiling;
use esper_stream::{ControllerStream, HostStep, HostStream, RecordingSink};

const W: u32 = 320;
const H: u32 = 240;
const MTU: usize = 1200;
const TICK: Duration = Duration::from_millis(80);

fn encoder_for(config: &EncoderConfig) -> Result<Box<dyn VideoEncoder>, EncodeError> {
    Ok(Box::new(OpenH264::new(config)?))
}

/// A host around a capturer somebody else has arranged.
fn host_around(capturer: SyntheticCapturer) -> HostStream {
    let pipeline = Pipeline::new(
        Box::new(capturer),
        &encoder_for,
        Budget::of(100.0),
        Policy {
            max_fps: 30,
            ..Policy::default()
        },
    )
    .expect("a pipeline")
    .with_ceiling(Ceiling::none());
    HostStream::new(pipeline, MTU).expect("a host stream")
}

/// Two monitors of different sizes, so a switch shows up in the frame.
fn two() -> SyntheticCapturer {
    SyntheticCapturer::new(W, H, 1_000_000)
        .expect("a valid size")
        .with_extra_monitor(W / 2, H / 2)
        .expect("a valid second size")
}

/// Run the host until it says something, or give up.
fn until_it_says_something(host: &mut HostStream, sink: &mut RecordingSink) -> Vec<StreamConfig> {
    let mut said = Vec::new();
    for _ in 0..40 {
        match host.step(TICK, sink) {
            Ok(HostStep::Reconfigured { .. }) => said.push(host.config()),
            Ok(_) => {}
            Err(e) => panic!("{e}"),
        }
        if !said.is_empty() {
            return said;
        }
    }
    said
}

/// **The one that breaks a session outright.** A host that keeps announcing a
/// monitor which is no longer there sends a config the controller refuses as
/// malformed, so the stream is never reconfigured and the picture stops.
#[test]
fn a_monitor_that_is_unplugged_does_not_leave_the_host_announcing_it() {
    let mut sink = RecordingSink::new();
    let mut host = host_around(two().with_unplug_at(3, MonitorId(0)));

    // Watching the one that is about to go.
    host.handle(&MediaMessage::Select(MonitorSelect { monitor: 0 }))
        .expect("monitor 0 is there");
    let before = host.config();
    assert_eq!(before.selected, 0);
    assert_eq!(before.monitors.len(), 2);

    let announced = until_it_says_something(&mut host, &mut sink);
    let after = announced.first().cloned().unwrap_or_else(|| host.config());

    assert_eq!(
        after.monitors.len(),
        1,
        "the host still lists a monitor that was unplugged"
    );
    assert!(
        after.monitors.iter().any(|m| m.id == after.selected),
        "the host announced monitor {} against a list of {:?} — a controller \
         refuses that as malformed and never reconfigures",
        after.selected,
        after.monitors.iter().map(|m| m.id).collect::<Vec<_>>()
    );
    assert!(
        after.is_well_formed(),
        "the announced config is not one a controller will accept"
    );

    // And a controller can actually take it, which is the whole point.
    assert!(
        ControllerStream::new(&after).is_ok(),
        "a controller could not open on what the host announced"
    );

    // **A different screen is a different synchronisation source**, exactly as
    // it is when somebody asks for one. The controller resets its reassembly
    // on the announced config either way, so this is not what makes the switch
    // work — it is what makes it legible in a packet capture, and what keeps a
    // fallback and a deliberate switch from looking different on the wire for
    // no reason.
    assert_ne!(
        after.ssrc, before.ssrc,
        "the stream carried on under the old screen's synchronisation source"
    );
    assert_eq!(
        after.selected, 1,
        "the fallback did not land on the monitor that is still there"
    );
}

/// **A layout change that does not change the picture's size.** Rearranging a
/// desktop, or plugging in a second screen, leaves the monitor being watched
/// exactly as it was — so a host that announces only on a size change says
/// nothing, and the controller's list is wrong for the rest of the session.
#[test]
fn a_monitor_appearing_is_announced_even_though_the_picture_did_not_change() {
    let mut sink = RecordingSink::new();
    let mut host = host_around(
        SyntheticCapturer::new(W, H, 1_000_000)
            .expect("a valid size")
            .with_plug_at(3, W, H),
    );
    let before = host.config();
    assert_eq!(before.monitors.len(), 1);
    let (w, h) = (before.width, before.height);

    let announced = until_it_says_something(&mut host, &mut sink);
    assert!(
        !announced.is_empty(),
        "a second monitor appeared and the host said nothing"
    );
    let after = announced.first().expect("something was said");
    assert_eq!(
        after.monitors.len(),
        2,
        "the announcement did not carry the new monitor"
    );
    assert_eq!(
        (after.width, after.height),
        (w, h),
        "the picture changed size, so this is not testing what it means to"
    );
    assert!(after.is_well_formed());
}

/// The same, for a desktop that is rearranged rather than added to.
#[test]
fn a_monitor_moving_is_announced() {
    let mut sink = RecordingSink::new();
    let mut host = host_around(two().with_move_at(3, MonitorId(1), -160, 0));
    let before = host.config();
    let was = before
        .monitors
        .iter()
        .find(|m| m.id == 1)
        .map(|m| m.x)
        .expect("two monitors");

    let announced = until_it_says_something(&mut host, &mut sink);
    assert!(
        !announced.is_empty(),
        "the desktop was rearranged and the host said nothing"
    );
    let after = announced.first().expect("something was said");
    let now = after
        .monitors
        .iter()
        .find(|m| m.id == 1)
        .map(|m| m.x)
        .expect("still two monitors");
    assert_ne!(now, was, "the announcement carried the old position");
    assert_eq!(now, -160, "a monitor to the left has a negative origin");
}

/// A monitor plugged in after one was unplugged must not inherit its number.
///
/// A controller holding the old list would take the new screen for one it
/// already knew, and show it under the old one's name and size.
#[test]
fn a_new_monitor_does_not_reuse_the_number_of_one_that_went() {
    let mut sink = RecordingSink::new();
    let mut host = host_around(
        two()
            .with_unplug_at(3, MonitorId(1))
            .with_plug_at(6, W / 4, H / 4),
    );
    let mut seen: Vec<u32> = host.config().monitors.iter().map(|m| m.id).collect();
    for _ in 0..80 {
        if let Ok(HostStep::Reconfigured { .. }) = host.step(TICK, &mut sink) {
            for m in host.config().monitors {
                if !seen.contains(&m.id) {
                    seen.push(m.id);
                }
            }
        }
    }
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        seen.len(),
        "a number was used for two different monitors: {seen:?}"
    );
    assert!(seen.len() >= 3, "the plug never happened: {seen:?}");
}

/// **Nothing goes out for a screen nobody asked for.** Damage is desktop-wide
/// on X11, so activity on another monitor wakes the capturer — and a host that
/// then sent a frame would charge a session for a screen it is not watching.
#[test]
fn a_monitor_nobody_is_watching_costs_the_session_nothing() {
    let mut sink = RecordingSink::new();
    let mut host = host_around(two());
    // The second monitor is the small one; watch the big one and let it settle.
    for _ in 0..10 {
        let _ = host.step(TICK, &mut sink);
    }
    let quiet = sink.sent.len();

    // Now switch to the other and back, which is the only thing a test at this
    // level can do to the *other* monitor — and check the switch itself is what
    // costs, not the mere existence of a second screen.
    host.handle(&MediaMessage::Select(MonitorSelect { monitor: 1 }))
        .expect("monitor 1 is there");
    let after_switch = sink.sent.len();
    assert_eq!(
        after_switch, quiet,
        "selecting a monitor sent pixels before anything was captured"
    );
    assert_eq!(host.config().selected, 1);
    assert_eq!(
        (host.config().width, host.config().height),
        (W / 2, H / 2),
        "the switch did not change the picture"
    );
}

/// A controller must be able to open on whatever the host announces, at any
/// point in a layout's life. This is the property every test above rests on.
#[test]
fn every_layout_the_host_announces_is_one_a_controller_accepts() {
    let mut sink = RecordingSink::new();
    let mut host = host_around(
        two()
            .with_plug_at(2, W / 4, H / 4)
            .with_unplug_at(5, MonitorId(0))
            .with_move_at(8, MonitorId(1), 900, 40)
            .with_unplug_at(11, MonitorId(1)),
    );
    let mut checked = 0;
    for _ in 0..120 {
        if let Ok(HostStep::Reconfigured { .. }) = host.step(TICK, &mut sink) {
            let config = host.config();
            assert!(
                config.is_well_formed(),
                "the host announced {config:?}, which a controller refuses"
            );
            assert!(
                ControllerStream::new(&config).is_ok(),
                "a controller could not open on {config:?}"
            );
            checked += 1;
        }
        // Whatever happens, the host must not be sending for a role it is not.
        assert!(MediaMessage::Config(host.config()).may_be_sent_by(Role::Host));
    }
    assert!(
        checked >= 3,
        "only {checked} layout changes were seen; the fixture is not exercising this"
    );
}
