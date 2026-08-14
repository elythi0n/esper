#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! **A red-team pass against a real host, from a controller that misbehaves.**
//!
//! Phase 15's exit criterion is that every presence guarantee survives a
//! deliberate attempt to defeat it from a modified controller. Every other
//! test in this tree drives the product the way it is meant to be driven; this
//! one speaks the protocol by hand and sends what no shipped controller ever
//! sends.
//!
//! # Why against the binary
//!
//! The refusals themselves have unit tests where the rule lives, and those
//! prove the *function* refuses. What they cannot prove is that the function
//! is on the path: a message could be acted on somewhere upstream, by a loop
//! that reads it before anybody checks who may send it. So these start the
//! real `esper-engine host`, connect to it over a real authenticated
//! connection, and attack it.
//!
//! # What is asserted, and what is not
//!
//! Asserted: that the host **refuses** and that the guarantee still holds
//! afterwards — the second half being the point. A message that is refused and
//! took effect anyway is not a refusal, so where a guarantee is observable
//! from the far end, the attack ends by checking it.
//!
//! Not asserted: anything about a modified *host*. A host owns the screen it
//! is serving, and a person who runs a modified one has already decided
//! everything this product could otherwise promise them. That limit is written
//! up where the recording notice is.
//!
//! Headless on purpose: `DISPLAY` is removed from the host's environment, so
//! there is no indicator and no privacy screen in the way. The guarantees
//! under attack here are about permissions and streams, and the screen ones
//! are measured by the suite that has a screen.

use std::time::Duration;

use esper_proto::{MediaMessage, Permission, StreamPurpose};

mod common;
use common::{Host, Peer, Silent, facing};

/// **It cannot grant itself anything.**
///
/// `PermissionChange` is the host's statement of what a session may do. A
/// controller that could send one would be a controller with every permission
/// it cared to name — so the direction is refused at the boundary, and this
/// checks the *consequence* as well as the refusal: an input stream is still
/// refused afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_controller_cannot_grant_itself_permissions() {
    let (_home, host, host_key, red_home) = facing("granting", &[Permission::View]);
    let mut red = Peer::open(&red_home, &host, host_key, &[Permission::View]).await;
    assert_eq!(red.granted, vec![Permission::View]);

    red.sends(&MediaMessage::Permissions(esper_proto::PermissionChange {
        granted: Permission::ALL.to_vec(),
    }))
    .await;
    assert!(
        host.waits_to_say("refused a message from the controller"),
        "the host acted on a permission set the controller sent: {:?}",
        host.everything()
    );

    // And the guarantee, which is the half a refusal alone does not prove.
    let _stream = red.opens(StreamPurpose::Input).await;
    assert!(
        host.waits_to_say("refused a input stream"),
        "a session that granted itself control got a keyboard: {:?}",
        host.everything()
    );
}

/// **It cannot answer its own question.**
///
/// Asking is allowed and grants nothing; the answer comes from a person at the
/// machine. A controller that could send the answer would be back where the
/// message above leaves it, with one extra step.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_controller_cannot_answer_its_own_question() {
    let (_home, host, host_key, red_home) = facing("answering", &[Permission::View]);
    let mut red = Peer::open(&red_home, &host, host_key, &[Permission::View]).await;

    red.sends(&MediaMessage::Answer(
        esper_proto::PermissionAnswer::Allowed,
    ))
    .await;
    assert!(
        host.waits_to_say("refused a message from the controller"),
        "the host took an answer from the far end: {:?}",
        host.everything()
    );
    let _stream = red.opens(StreamPurpose::Input).await;
    assert!(
        host.waits_to_say("refused a input stream"),
        "answering its own question got the controller a keyboard: {:?}",
        host.everything()
    );
}

/// **A host with nobody to ask allows nothing**, and says which of the two
/// things happened: nobody there is not somebody saying no.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_question_on_a_host_with_nobody_at_it_is_refused_as_such() {
    let (_home, host, host_key, red_home) = facing("nobody", &[Permission::View]);
    let mut red = Peer::open(&red_home, &host, host_key, &[Permission::View]).await;

    // **The clipboard rather than control.** A headless host has nothing to
    // inject with either, and it says *that* first — a question whose yes
    // could not be honoured is refused before anybody is asked, which is a
    // different guarantee and has its own test. The clipboard needs no
    // keyboard, so this reaches the rule about who is there to answer.
    red.sends(&MediaMessage::Ask(esper_proto::PermissionAsk {
        wanted: Permission::ClipboardOut,
    }))
    .await;
    let answer = red.next_answer(Duration::from_secs(10)).await;
    match answer {
        Some(esper_proto::PermissionAnswer::Refused { reason, detail }) => {
            assert_eq!(
                reason,
                esper_proto::Reason::NOBODY_TO_ASK,
                "a headless host has nobody to ask, and that is its own answer: {detail}"
            );
            assert!(
                detail.contains("grant"),
                "the refusal should name what to do about it: {detail}"
            );
        }
        other => panic!("expected a refusal naming nobody to ask, got {other:?}"),
    }
}

/// **It cannot ask for what would outlive the session.** `unattended` lets the
/// next connection in unasked and `reboot` takes the machine away; neither is
/// a thing to hand over in a dialog about a screen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_controller_cannot_ask_for_what_outlives_the_session() {
    let (_home, host, host_key, red_home) = facing("outliving", &[Permission::View]);
    let mut red = Peer::open(&red_home, &host, host_key, &[Permission::View]).await;

    for wanted in [
        Permission::Unattended,
        Permission::Reboot,
        Permission::Logoff,
        Permission::Terminal,
    ] {
        red.sends(&MediaMessage::Ask(esper_proto::PermissionAsk { wanted }))
            .await;
        match red.next_answer(Duration::from_secs(10)).await {
            Some(esper_proto::PermissionAnswer::Refused { reason, detail }) => {
                assert_eq!(
                    reason,
                    esper_proto::Reason::PERMISSION_DENIED,
                    "{} should be refused before anybody is asked: {detail}",
                    wanted.name()
                );
            }
            other => panic!("expected {} to be refused, got {other:?}", wanted.name()),
        }
    }
}

/// **It cannot ask without end.** Every question would draw a prompt in front
/// of whoever is at the machine, and the person answering has no way to say
/// "and stop asking". Counted here on a host with nobody at it, which is the
/// case where the questions cost the far end nothing at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_controller_cannot_ask_without_end() {
    let (_home, host, host_key, red_home) = facing("knocking", &[Permission::View]);
    let mut red = Peer::open(&red_home, &host, host_key, &[Permission::View]).await;

    let mut reasons = Vec::new();
    for _ in 0..16 {
        red.sends(&MediaMessage::Ask(esper_proto::PermissionAsk {
            wanted: Permission::Control,
        }))
        .await;
        match red.next_answer(Duration::from_secs(10)).await {
            Some(esper_proto::PermissionAnswer::Refused { reason, .. }) => reasons.push(reason),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
    assert_eq!(
        reasons.last(),
        Some(&esper_proto::Reason::ASKED_ENOUGH),
        "a session asked sixteen times and was still being answered: {reasons:?}"
    );
    // **And the last few cost nothing.** Before the bound existed, every one
    // of these read the device list off disk and opened an injector to find
    // out whether a yes could be honoured — which on Linux creates a `uinput`
    // device. The refusal above is the one that happens before either.
    assert!(
        reasons
            .iter()
            .filter(|r| **r == esper_proto::Reason::ASKED_ENOUGH)
            .count()
            >= 2,
        "the bound should hold once it is met, not answer one and then relent: {reasons:?}"
    );
}

/// **A stream is not permission to do the thing it names.**
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stream_is_not_permission_to_do_what_it_names() {
    let (_home, host, host_key, red_home) = facing("streams", &[Permission::View]);
    let red = Peer::open(&red_home, &host, host_key, &[Permission::View]).await;

    let _input = red.opens(StreamPurpose::Input).await;
    assert!(
        host.waits_to_say("refused a input stream"),
        "an input stream was served to a session without control: {:?}",
        host.everything()
    );
    let _clipboard = red.opens(StreamPurpose::Clipboard).await;
    assert!(
        host.waits_to_say("refused a clipboard stream"),
        "a clipboard stream was served to a session without one: {:?}",
        host.everything()
    );
}

/// **It cannot decide for the person at the machine.** Holding the picture is
/// the host's own: a controller that could hold it would be deciding what
/// somebody at that desk may see of their own screen being watched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_controller_cannot_hold_the_hosts_picture() {
    let (_home, host, host_key, red_home) = facing("pausing", &[Permission::View]);
    let mut red = Peer::open(&red_home, &host, host_key, &[Permission::View]).await;

    red.sends(&MediaMessage::Paused(esper_proto::Paused { paused: true }))
        .await;
    assert!(
        host.waits_to_say("refused a message from the controller"),
        "a controller held the host's own picture: {:?}",
        host.everything()
    );
}

/// **It cannot describe the host's own sound.** There is no microphone path in
/// this protocol at all, so a controller announcing audio is announcing
/// something the host would otherwise have to work out what to do with.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_controller_cannot_announce_the_hosts_own_sound() {
    let (_home, host, host_key, red_home) = facing("sound", &[Permission::View]);
    let mut red = Peer::open(&red_home, &host, host_key, &[Permission::View]).await;

    red.sends(&MediaMessage::Audio(esper_proto::AudioConfig {
        codec: esper_proto::AudioCodec::Opus,
        sample_rate_hz: 48_000,
        channels: 2,
        frame_ms: 20,
        ssrc: 7,
    }))
    .await;
    assert!(
        host.waits_to_say("refused a message from the controller"),
        "a controller described the host's own sound: {:?}",
        host.everything()
    );
}

/// **A number out of range does not reach the thing that acts on it.**
///
/// The host's session loop reads a controller's link report and hands the
/// capacity to the transport's pacer — and until this pass, it did that
/// *before* anything checked the number was in range: the pipeline thread
/// refused the message a moment later, on the other side of a channel, by
/// which time the host was already pacing to whatever had arrived. A host
/// paced to an absurd capacity floods its own uplink.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_capacity_out_of_range_never_reaches_the_pacer() {
    let (_home, host, host_key, red_home) = facing("pacing", &[Permission::View]);
    let mut red = Peer::open(&red_home, &host, host_key, &[Permission::View]).await;

    red.sends(&MediaMessage::Report(esper_proto::LinkReport {
        capacity_kbit: Some(u32::MAX),
    }))
    .await;
    assert!(
        host.waits_to_say("refused a message from the controller"),
        "an out-of-range capacity was not refused: {:?}",
        host.everything()
    );
    assert!(
        !host.has_said("measures this link at 4294967295"),
        "the pacer was told a capacity the protocol does not allow: {:?}",
        host.everything()
    );
}

/// **One keyboard, and a second stream gets a refusal rather than it.**
///
/// What this can separate and what it cannot is worth saying: the host has two
/// different refusals here — "a second input stream" when it already gave the
/// keyboard away, and "nothing on this machine to inject" when there was never
/// one — and headless it is always the second. So what is asserted is the
/// thing that matters either way: **no input stream was served**, which two
/// refusals prove. Which of the two sentences a host with a display would use
/// is its own comment's business.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_input_stream_gets_a_refusal_and_not_a_keyboard() {
    let (_home, host, host_key, red_home) =
        facing("two-keyboards", &[Permission::View, Permission::Control]);
    let red = Peer::open(
        &red_home,
        &host,
        host_key,
        &[Permission::View, Permission::Control],
    )
    .await;
    assert!(red.granted.contains(&Permission::Control));

    let _first = red.opens(StreamPurpose::Input).await;
    let _second = red.opens(StreamPurpose::Input).await;
    let refused = |host: &Host| {
        host.everything()
            .iter()
            .filter(|l| {
                l.contains("refused a second input stream")
                    || l.contains("with nothing on this machine to inject")
            })
            .count()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while refused(&host) < 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        refused(&host) >= 2,
        "a session opened two input streams and one of them was served: {:?}",
        host.everything()
    );
}

/// **A stream that says something that is not a purpose is dropped, and the
/// session carries on.**
///
/// A peer's bad stream must not be a way to end somebody's session — and it
/// must not be served either, since a stream is checked against the grant by
/// the purpose it names.
///
/// **What could not be attacked from here, and why it is worth writing down:**
/// the host also has a ten-second patience for a stream that is opened and
/// then says *nothing*, and that cannot be reached through this transport's
/// own API. QUIC does not surface a stream to the far end until bytes arrive
/// on it, so a stream opened and left silent is invisible to the host — there
/// is nothing to time out. Reaching that path needs a hand-rolled QUIC client
/// writing a partial frame, which is a different tool than this file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stream_that_names_no_purpose_is_dropped_and_the_session_lives() {
    let (_home, host, host_key, red_home) = facing("nonsense", &[Permission::View]);
    let red = Peer::open(&red_home, &host, host_key, &[Permission::View]).await;

    let mut nonsense = red
        .session
        .open_control()
        .await
        .expect("a stream to the host");
    // A well-formed frame carrying something that is not a purpose.
    nonsense.send(&true).await.expect("the host took the bytes");

    // The session is still there and still refusing by the grant, which is
    // what a peer's bad stream must not be able to change.
    let _clipboard = red.opens(StreamPurpose::Clipboard).await;
    assert!(
        host.waits_to_say("refused a clipboard stream"),
        "the session stopped serving streams after one that named no purpose: {:?}",
        host.everything()
    );
}

/// **A device that connects and then says nothing cannot stall the session
/// somebody else is in.**
///
/// A host serves one interactive session at a time and now *answers* a second
/// device rather than leaving it in the accept queue — which means the accept
/// loop does two things at once, and the second one talks to a stranger. Both
/// halves of that conversation wait on the far end: a handshake, and then up
/// to `REQUEST_TIMEOUT` for a request that may never come. Run on the same
/// task as the live session, that is ten seconds of somebody's screen not
/// being served, for the price of one connection and no bytes — and a paired
/// device that repeated it would hold a session frozen for as long as it
/// liked.
///
/// What is measured is the live session still being *answered*, because that
/// is a round trip through the same loop that would have been blocked. The
/// answer here is a refusal (headless: nobody to ask), and a refusal that
/// arrives is the proof — what it says is another test's business.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_second_device_does_not_stall_the_session_being_served() {
    let (_home, host, host_key, red_home) = facing("silent-second", &[Permission::View]);
    let mut red = Peer::open(&red_home, &host, host_key, &[Permission::View]).await;

    // Through the door, and then nothing.
    let _quiet = Silent::connect(&red_home, &host, host_key).await;

    // Comfortably under the ten seconds the host will wait for a request, and
    // comfortably over a round trip on loopback.
    const PATIENCE: Duration = Duration::from_secs(3);
    let asked = std::time::Instant::now();
    red.sends(&MediaMessage::Ask(esper_proto::PermissionAsk {
        wanted: Permission::Control,
    }))
    .await;
    let answer = red.next_answer(PATIENCE).await;

    assert!(
        answer.is_some(),
        "the live session went unanswered for {:?} while the host talked to a \
         device that said nothing — one connection is enough to freeze it",
        asked.elapsed()
    );
}
