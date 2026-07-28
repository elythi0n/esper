#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Two machines, one clipboard crossing between them.
//!
//! # What this is for
//!
//! Every other test in this crate looks at one side of the arrangement. The
//! thing a person actually does — copy over here, paste over there — is a
//! conversation, and a conversation can be wrong in ways neither half is: a
//! side that offers what the other will not ask for, an ask nobody answers, an
//! answer that arrives and is refused.
//!
//! So this runs two real couriers against each other and passes the messages
//! between them, exactly as a session's two ends do. There is no transport and
//! no display here, which is why it can check the things that are hard to
//! provoke on a real one.

use std::time::{Duration, Instant};

use esper_clipboard::{Clipboard, ClipboardKind, Courier, Grants, NullClipboard};
use esper_proto::{
    ClipboardData, ClipboardMessage, ClipboardOffer, ClipboardRefusal, ClipboardReply,
    ClipboardRequest, Offered, Permission,
};

/// Long enough for the courier to have stopped waiting for the copying to
/// settle. The exact figure is the courier's; this only has to be past it.
const SETTLED: Duration = Duration::from_millis(500);

/// A clipboard that learns of its own changes the way a real one does.
///
/// **The delay is the point.** A display server tells a process that the
/// clipboard changed some time *after* the process changed it, so a session
/// that took a paste and then looked would see what looks exactly like the
/// person here copying something — and offer the far end its own paste back.
/// A clipboard that noticed its own writes instantly would let that through.
struct LikeADisplay {
    inner: NullClipboard,
    /// How many looks until the notice about the last write arrives.
    notice: u8,
    /// What the notice has added so far, so the count never goes backwards.
    since: u64,
}

impl LikeADisplay {
    fn new() -> Self {
        Self {
            inner: NullClipboard::new(),
            notice: 0,
            since: 0,
        }
    }
}

impl Clipboard for LikeADisplay {
    fn read(
        &mut self,
        kind: ClipboardKind,
    ) -> Result<Option<Vec<u8>>, esper_clipboard::ClipboardError> {
        self.inner.read(kind)
    }

    fn write(
        &mut self,
        kind: ClipboardKind,
        content: &[u8],
    ) -> Result<(), esper_clipboard::ClipboardError> {
        self.inner.write(kind, content)?;
        // Not on the next look — the one after. The next look is the session
        // taking note of what it just did.
        self.notice = 2;
        Ok(())
    }

    fn generation(&mut self) -> u64 {
        if self.notice > 0 {
            self.notice -= 1;
            if self.notice == 0 {
                self.since += 1;
            }
        }
        self.inner.generation().saturating_add(self.since)
    }

    fn clear(&mut self) -> Result<(), esper_clipboard::ClipboardError> {
        self.inner.clear()
    }
}

/// One end of a session.
struct End {
    courier: Courier<LikeADisplay>,
}

impl End {
    fn new(grants: Grants) -> Self {
        Self {
            courier: Courier::new(LikeADisplay::new(), grants),
        }
    }

    /// What a person at this machine copied.
    fn copies(&mut self, kind: ClipboardKind, content: &[u8]) {
        self.courier.clipboard_mut().inner.set(kind, content);
    }

    /// What is on this machine's clipboard now.
    fn on_the_clipboard(&mut self, kind: ClipboardKind) -> Option<Vec<u8>> {
        self.courier.clipboard_mut().read(kind).expect("a read")
    }
}

/// Run the two ends until neither has anything more to say.
///
/// Returns every message that crossed, so a test can check what did *not*
/// happen as easily as what did.
fn talk(a: &mut End, b: &mut End, at: Instant) -> Vec<ClipboardMessage> {
    let mut crossed = Vec::new();
    let mut pending: Vec<(bool, ClipboardMessage)> = Vec::new();
    // Whatever each side wants to announce, before anything is answered.
    if let Some(offer) = a.courier.poll(at) {
        pending.push((true, ClipboardMessage::Offer(offer)));
    }
    if let Some(offer) = b.courier.poll(at) {
        pending.push((false, ClipboardMessage::Offer(offer)));
    }
    // A clipboard conversation is a handful of messages; anything longer is a
    // loop, and a loop is the failure this bound exists to catch.
    for _ in 0..64 {
        let Some((from_a, message)) = pending.pop() else {
            return crossed;
        };
        crossed.push(message.clone());
        let listener = if from_a { &mut *b } else { &mut *a };
        let heard = listener.courier.hear(&message);
        if let Some(say) = heard.say {
            pending.push((!from_a, say));
        }
    }
    panic!("the two ends never stopped talking: {crossed:#?}");
}

fn both() -> Grants {
    Grants::from_permissions(&[Permission::ClipboardOut, Permission::ClipboardIn])
}

fn later(from: Instant) -> Instant {
    from.checked_add(SETTLED)
        .expect("a clock that goes forward")
}

/// **The thing a person does.** Copy on one machine, paste on the other.
#[test]
fn a_copy_on_one_machine_becomes_a_paste_on_the_other() {
    let mut host = End::new(both());
    let mut viewer = End::new(both());
    let now = Instant::now();

    host.copies(ClipboardKind::Text, b"the password is not this");
    host.courier.poll(now);
    let crossed = talk(&mut host, &mut viewer, later(now));

    assert_eq!(
        viewer.on_the_clipboard(ClipboardKind::Text).as_deref(),
        Some(b"the password is not this".as_slice()),
        "the paste never arrived"
    );
    // Offer, ask, answer. Nothing else, and in that order.
    assert!(
        matches!(
            crossed.as_slice(),
            [
                ClipboardMessage::Offer(_),
                ClipboardMessage::Ask(_),
                ClipboardMessage::Reply(ClipboardReply::Sent(_))
            ]
        ),
        "{crossed:#?}"
    );
}

/// **The offer carries no content, and this is the test that says so.**
#[test]
fn nothing_leaves_the_machine_until_the_other_end_asks() {
    let mut host = End::new(both());
    let mut viewer = End::new(Grants::none());
    let now = Instant::now();

    host.copies(ClipboardKind::Text, b"hunter2");
    host.courier.poll(now);
    let crossed = talk(&mut host, &mut viewer, later(now));

    // The viewer may not take anything, so it asks for nothing — and the
    // password stays where it was copied.
    assert!(matches!(crossed.as_slice(), [ClipboardMessage::Offer(_)]));
    for message in &crossed {
        let mut bytes = Vec::new();
        esper_proto::frame::write_frame(&mut bytes, message).expect("encodes");
        let as_text: String = bytes.iter().map(|&b| b as char).collect();
        assert!(
            !as_text.contains("hunter2"),
            "the content was on the wire in {}",
            message.name()
        );
    }
    assert_eq!(viewer.on_the_clipboard(ClipboardKind::Text), None);
}

/// A host that may not be read is not read, and says nothing about what is on
/// it — because saying "there are 43 bytes of text here" is itself something.
#[test]
fn a_host_that_may_not_be_read_offers_nothing_at_all() {
    let mut host = End::new(Grants {
        read: false,
        write: true,
    });
    let mut viewer = End::new(both());
    let now = Instant::now();

    host.copies(ClipboardKind::Text, b"hunter2");
    host.courier.poll(now);
    assert!(
        talk(&mut host, &mut viewer, later(now)).is_empty(),
        "a clipboard nobody may see was described anyway"
    );

    // And a viewer that asks anyway, having never been offered, is refused by
    // name rather than answered.
    let heard = host.courier.hear(&ClipboardMessage::Ask(ClipboardRequest {
        kind: ClipboardKind::Text,
    }));
    assert_eq!(
        heard.say,
        Some(ClipboardMessage::Reply(ClipboardReply::Refused(
            ClipboardRefusal::NotPermitted
        )))
    );
}

/// **A peer cannot make this machine's clipboard say what it likes.** The
/// protocol has no push, so content nobody asked for goes nowhere — and this
/// end does not answer it either, which would be a way to make it speak.
#[test]
fn a_peer_that_invents_a_push_is_refused_and_gets_no_answer() {
    let mut host = End::new(both());
    let pushed = ClipboardMessage::Reply(ClipboardReply::Sent(ClipboardData {
        kind: ClipboardKind::Text,
        content: b"you did not ask for this".to_vec(),
    }));
    let heard = host.courier.hear(&pushed);
    assert_eq!(heard.say, None, "an unasked-for push was answered");
    assert_eq!(host.on_the_clipboard(ClipboardKind::Text), None);
    assert!(
        heard.note.is_some_and(|n| n.contains("did not take it")),
        "it was taken, or nothing was said about it"
    );
}

/// The permissions are one-sided, and a session can be one-sided with them.
#[test]
fn a_one_way_grant_carries_one_way() {
    // The host will let the viewer read it and not write to it.
    let mut host = End::new(Grants {
        read: true,
        write: false,
    });
    let mut viewer = End::new(Grants::mirrored(&[Permission::ClipboardOut]));
    let now = Instant::now();

    // Host to viewer: works.
    host.copies(ClipboardKind::Text, b"from the host");
    host.courier.poll(now);
    talk(&mut host, &mut viewer, later(now));
    assert_eq!(
        viewer.on_the_clipboard(ClipboardKind::Text).as_deref(),
        Some(b"from the host".as_slice())
    );

    // Viewer to host: the viewer will not even offer, because its own grant
    // says the host may not read it.
    let then = later(later(now));
    viewer.copies(ClipboardKind::Text, b"from the viewer");
    viewer.courier.poll(then);
    let crossed = talk(&mut viewer, &mut host, later(then));
    assert!(
        crossed.is_empty(),
        "the viewer's clipboard was described to a host that may not read it: {crossed:#?}"
    );
    assert_eq!(
        host.on_the_clipboard(ClipboardKind::Text).as_deref(),
        Some(b"from the host".as_slice()),
        "the host's own clipboard was replaced"
    );
}

/// **The failure that would never stop.** Taking a paste changes the local
/// clipboard, which is what copying looks like from here.
#[test]
fn a_paste_does_not_bounce_between_the_two_machines_forever() {
    let mut host = End::new(both());
    let mut viewer = End::new(both());
    let now = Instant::now();

    host.copies(ClipboardKind::Text, b"round and round");
    host.courier.poll(now);
    let mut at = later(now);
    let first = talk(&mut host, &mut viewer, at);
    assert_eq!(first.len(), 3);

    // Now let both ends look at their clipboards again, several times over.
    // Neither should have anything new to say: they hold the same thing.
    for _ in 0..5 {
        at = later(at);
        let crossed = talk(&mut host, &mut viewer, at);
        assert!(
            crossed.is_empty(),
            "the two ends traded the same clipboard again: {crossed:#?}"
        );
    }
}

/// A clipboard offers what is there now, not what was there before.
#[test]
fn a_second_copy_supersedes_the_first_and_no_history_is_kept() {
    let mut host = End::new(both());
    let mut viewer = End::new(both());
    let now = Instant::now();

    host.copies(ClipboardKind::Text, b"first thing");
    host.courier.poll(now);
    let at = later(now);
    talk(&mut host, &mut viewer, at);

    host.copies(ClipboardKind::Text, b"second thing");
    host.courier.poll(at);
    talk(&mut host, &mut viewer, later(at));
    assert_eq!(
        viewer.on_the_clipboard(ClipboardKind::Text).as_deref(),
        Some(b"second thing".as_slice())
    );

    // The first thing is not obtainable any more, from either end.
    let heard = host.courier.hear(&ClipboardMessage::Ask(ClipboardRequest {
        kind: ClipboardKind::Text,
    }));
    match heard.say {
        Some(ClipboardMessage::Reply(ClipboardReply::Sent(data))) => {
            assert_eq!(data.content, b"second thing");
        }
        other => panic!("{other:?}"),
    }
}

/// And when the session ends, what was crossing it is gone.
#[test]
fn the_end_of_a_session_takes_what_it_was_carrying() {
    let mut host = End::new(both());
    let mut viewer = End::new(both());
    let now = Instant::now();

    host.copies(ClipboardKind::Text, b"only for this session");
    host.courier.poll(now);
    talk(&mut host, &mut viewer, later(now));

    host.courier.forget();
    let heard = host.courier.hear(&ClipboardMessage::Ask(ClipboardRequest {
        kind: ClipboardKind::Text,
    }));
    assert_eq!(
        heard.say,
        Some(ClipboardMessage::Reply(ClipboardReply::Refused(
            ClipboardRefusal::NotOffered
        ))),
        "a finished session could still answer for what it had been carrying"
    );
    // What a person actually copied is still theirs. Ending a session must not
    // take away what was on the machine before it started.
    assert_eq!(
        host.on_the_clipboard(ClipboardKind::Text).as_deref(),
        Some(b"only for this session".as_slice())
    );
}

/// Content that is not what it says it is never reaches a clipboard.
#[test]
fn a_peer_cannot_put_something_that_is_not_text_onto_a_clipboard_as_text() {
    let mut viewer = End::new(both());

    // The viewer asks for what the host offered, honestly.
    let offer = ClipboardOffer {
        kinds: vec![Offered {
            kind: ClipboardKind::Text,
            bytes: 3,
        }],
    };
    let heard = viewer.courier.hear(&ClipboardMessage::Offer(offer));
    assert!(matches!(heard.say, Some(ClipboardMessage::Ask(_))));

    // And the answer is not text at all.
    let lie = ClipboardMessage::Reply(ClipboardReply::Sent(ClipboardData {
        kind: ClipboardKind::Text,
        content: vec![0xFF, 0xFE, 0xFD],
    }));
    // Refused before it is even work: the message is not well formed, so it
    // never reaches the policy.
    assert!(!lie.is_well_formed());
    let heard = viewer.courier.hear(&lie);
    assert_eq!(heard.say, None);
    assert_eq!(viewer.on_the_clipboard(ClipboardKind::Text), None);
}
