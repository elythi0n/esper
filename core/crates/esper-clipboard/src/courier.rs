use std::time::{Duration, Instant};

use esper_proto::{
    ClipboardData, ClipboardKind, ClipboardMessage, ClipboardOffer, ClipboardRefusal,
    ClipboardReply, ClipboardRequest, ClipboardWay, Offered, Permission,
};

use crate::{Clipboard, ClipboardError, Held};

/// How quiet the clipboard has to be before it is read.
///
/// A program that copies in a loop — a script, a badly written editor, a peer
/// probing — would otherwise make this process read and hold its content on
/// every tick. Waiting for the changes to stop costs a person nothing they can
/// perceive and turns an unbounded amount of reading into one read.
const SETTLE: Duration = Duration::from_millis(150);

/// What the far end is permitted to do to **this machine's** clipboard.
///
/// One-sided on purpose. A host grants a controller device the right to read
/// its clipboard, or to write to it, or neither, or both; the controller grants
/// the same about its own. Neither side takes the other's word for what it may
/// do, because the whole arrangement is worth nothing if it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Grants {
    /// The far end may read what is on this machine's clipboard.
    pub read: bool,
    /// The far end may put things on this machine's clipboard.
    pub write: bool,
}

impl Grants {
    /// Nothing allowed. The default a session starts from.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            read: false,
            write: false,
        }
    }

    /// Whatever these permissions say, and nothing else.
    #[must_use]
    pub fn from_permissions(held: &[Permission]) -> Self {
        Self {
            read: held.contains(&Permission::ClipboardOut),
            write: held.contains(&Permission::ClipboardIn),
        }
    }

    /// The same grant seen from the other machine.
    ///
    /// # Why the far end's permissions decide this end's
    ///
    /// The permissions are written from the host's point of view: `clipboard
    /// out` means *the controller may read the host's clipboard*. For that to
    /// be worth anything, the controller has to be willing to put what comes
    /// back onto its own clipboard — which, from the controller, is the far
    /// end writing to it.
    ///
    /// So one negotiated grant settles both ends, and the controller needs no
    /// second set of switches that could disagree with the first. A session
    /// where the host permits a paste and the controller quietly refuses to
    /// receive it is a session where copy does nothing and nothing says why.
    #[must_use]
    pub fn mirrored(held: &[Permission]) -> Self {
        let straight = Self::from_permissions(held);
        Self {
            read: straight.write,
            write: straight.read,
        }
    }

    /// Whether this direction may proceed.
    #[must_use]
    pub const fn allows(self, way: ClipboardWay) -> bool {
        match way {
            ClipboardWay::Out => self.read,
            ClipboardWay::In => self.write,
        }
    }
}

/// The clipboard half of a session: what to say, what to answer, what to take.
///
/// # What it will not do
///
/// - It will not read the local clipboard at all unless the far end is
///   permitted to see it. Announcing "there are 43 bytes of text here" to a
///   peer that may not have them is both a leak and a lie, and holding the
///   content in this process for nobody is worse than either.
/// - It will not accept content it did not ask for. The protocol has no push,
///   and a peer that sends one anyway is refused by the same code that would
///   refuse it if the protocol grew one.
/// - It will not keep anything past [`Courier::forget`], and what it keeps in
///   the meantime is wiped rather than dropped.
pub struct Courier<C> {
    clipboard: C,
    grants: Grants,
    /// The local clipboard's content, read when the last offer was made, and
    /// the only copy this process holds.
    held: Vec<Held>,
    /// The generation the held content was read at.
    seen: u64,
    /// When the generation last changed, if it has not settled yet.
    changed_at: Option<Instant>,
    /// What has been asked of the far end and not yet answered.
    ///
    /// **This is what makes "no push" enforceable.** Content arriving that
    /// nothing asked for has nowhere to be matched, so it is refused.
    asked: Option<ClipboardKind>,
}

impl<C: Clipboard> Courier<C> {
    /// A courier over this clipboard, allowed to do what these grants say.
    pub fn new(clipboard: C, grants: Grants) -> Self {
        Self {
            clipboard,
            grants,
            held: Vec::new(),
            seen: 0,
            changed_at: None,
            asked: None,
        }
    }

    /// What the far end may do to this machine's clipboard.
    #[must_use]
    pub const fn grants(&self) -> Grants {
        self.grants
    }

    /// The clipboard underneath, for a caller that has to end the session.
    pub fn clipboard_mut(&mut self) -> &mut C {
        &mut self.clipboard
    }

    /// Whether anything is being waited for from the far end.
    #[must_use]
    pub const fn awaiting(&self) -> Option<ClipboardKind> {
        self.asked
    }

    /// Look at the local clipboard, and say what is on it if that has changed.
    ///
    /// Returns the offer to send, or nothing — which is the usual answer,
    /// because a clipboard changes a few times an hour and this is called a
    /// few times a second.
    pub fn poll(&mut self, now: Instant) -> Option<ClipboardOffer> {
        // Nothing is read from a clipboard the far end may not see. This is
        // the cheapest check and the most important one.
        if !self.grants.read {
            return None;
        }
        let generation = self.clipboard.generation();
        if generation != self.seen {
            self.seen = generation;
            self.changed_at = Some(now);
            return None;
        }
        let since = self.changed_at?;
        if now.saturating_duration_since(since) < SETTLE {
            return None;
        }
        self.changed_at = None;
        self.survey()
    }

    /// Read the clipboard and hold what is worth offering.
    fn survey(&mut self) -> Option<ClipboardOffer> {
        let mut fresh = Vec::new();
        let mut kinds = Vec::new();
        for kind in ClipboardKind::ALL {
            let bytes = match self.clipboard.read(kind) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => continue,
                Err(e) => {
                    // Not fatal, and not worth telling the far end about: one
                    // kind being unreadable does not stop the other.
                    tracing::debug!(kind = kind.name(), error = %e, "the clipboard would not answer");
                    continue;
                }
            };
            // Checked against what this protocol can actually carry, here,
            // rather than offered and refused later. A peer told about a
            // twenty megabyte image would ask for it and get nothing.
            let candidate = ClipboardData {
                kind,
                content: bytes,
            };
            if !candidate.is_well_formed() {
                tracing::debug!(
                    kind = kind.name(),
                    size = candidate.content.len(),
                    "not offering clipboard content this session cannot carry"
                );
                continue;
            }
            let Ok(size) = u32::try_from(candidate.content.len()) else {
                continue;
            };
            kinds.push(Offered { kind, bytes: size });
            fresh.push(Held::new(kind, candidate.content));
        }

        // Identical content is not news. A generation can tick without
        // anything changing — an application re-asserting the selection, this
        // process writing a paste it was handed — and re-announcing would have
        // the far end ask for what it already has.
        if fresh == self.held {
            return None;
        }
        self.forget_held();
        self.held = fresh;
        if kinds.is_empty() {
            return None;
        }
        Some(ClipboardOffer { kinds })
    }

    /// Answer the far end asking for something.
    ///
    /// Always answers. A request met with silence is indistinguishable from a
    /// host that has stopped, and the far end would wait for a paste that is
    /// never coming.
    pub fn answer(&mut self, request: &ClipboardRequest) -> ClipboardReply {
        if !self.grants.allows(ClipboardWay::Out) {
            return ClipboardReply::Refused(ClipboardRefusal::NotPermitted);
        }
        let Some(held) = self.held.iter().find(|h| h.kind() == request.kind) else {
            // Ordinary: an offer describes a moment, and a person can copy
            // something else before the far end gets around to asking.
            return ClipboardReply::Refused(ClipboardRefusal::NotOffered);
        };
        let data = ClipboardData {
            kind: held.kind(),
            content: held.bytes().to_vec(),
        };
        // Checked again on the way out, though `survey` checked it on the way
        // in. The cost is a pass over at most a megabyte; the thing it stops is
        // a change to either path quietly making the other one wrong.
        if !data.is_well_formed() {
            return ClipboardReply::Refused(ClipboardRefusal::Unavailable);
        }
        ClipboardReply::Sent(data)
    }

    /// Ask for what the far end has offered, if it is worth having.
    ///
    /// Returns the request to send, and remembers it so the answer can be
    /// matched to it.
    pub fn wants(&mut self, offer: &ClipboardOffer) -> Option<ClipboardRequest> {
        // No point asking for what would be refused on arrival.
        if !self.grants.allows(ClipboardWay::In) || !offer.is_well_formed() {
            return None;
        }
        // In the order the kinds are declared, which is most useful first.
        let kind = ClipboardKind::ALL
            .into_iter()
            .find(|k| offer.of(*k).is_some_and(|o| o.is_well_formed()))?;
        self.asked = Some(kind);
        Some(ClipboardRequest { kind })
    }

    /// Take content the far end sent, and put it on this machine's clipboard.
    ///
    /// # Errors
    ///
    /// With the refusal to send back, if this is not something to act on.
    pub fn accept(&mut self, data: &ClipboardData) -> Result<(), ClipboardRefusal> {
        if !self.grants.allows(ClipboardWay::In) {
            return Err(ClipboardRefusal::NotPermitted);
        }
        // **Nothing arrives unasked for.** The protocol is offer-then-request,
        // so content nothing asked for is either a peer that has invented a
        // push or a peer answering a question twice.
        if self.asked != Some(data.kind) {
            return Err(ClipboardRefusal::NotOffered);
        }
        if !data.is_well_formed() {
            self.asked = None;
            return Err(ClipboardRefusal::Malformed);
        }
        self.asked = None;
        if let Err(e) = self.clipboard.write(data.kind, &data.content) {
            tracing::warn!(error = %e, "could not put the paste on the clipboard");
            return Err(match e {
                ClipboardError::TooLarge { .. } => ClipboardRefusal::Malformed,
                _ => ClipboardRefusal::Unavailable,
            });
        }
        // **This is what stops the echo.** The write changed the clipboard,
        // which from here is indistinguishable from the person at this machine
        // copying something — so without it the next poll would offer the far
        // end its own paste, it would ask for it, and the two would trade the
        // same clipboard forever.
        //
        // Recorded as *content* rather than as a generation, because a real
        // backend learns of the change from the display server a moment after
        // the write returns, and a generation captured before that notice
        // arrives would already be stale. Content compares equal whenever it
        // arrives.
        if self.grants.read {
            self.forget_held();
            self.held = vec![Held::new(data.kind, data.content.clone())];
        }
        self.seen = self.clipboard.generation();
        self.changed_at = None;
        Ok(())
    }

    /// Act on one thing the far end said.
    ///
    /// **The whole conversation is here** rather than in whatever is holding
    /// the stream. A session's clipboard code is then only I/O — read a
    /// message, hand it over, send what comes back — and the part with the
    /// decisions in it can be tested by running two of these against each
    /// other, with no transport and no display anywhere near it.
    pub fn hear(&mut self, message: &ClipboardMessage) -> Heard {
        // Checked here as well as at the boundary, because "the caller
        // checked" is a property of today's caller.
        if !message.is_well_formed() {
            return Heard::just(format!("refused {} from the far end", message.name()));
        }
        match message {
            ClipboardMessage::Offer(offer) => match self.wants(offer) {
                Some(ask) => Heard {
                    say: Some(ClipboardMessage::Ask(ask)),
                    note: None,
                },
                None => Heard::nothing(),
            },
            ClipboardMessage::Ask(request) => {
                let reply = self.answer(request);
                let note = match &reply {
                    ClipboardReply::Sent(data) => {
                        format!("sent {} bytes of {}", data.content.len(), data.kind.name())
                    }
                    ClipboardReply::Refused(why) => why.message().to_owned(),
                };
                Heard {
                    say: Some(ClipboardMessage::Reply(reply)),
                    note: Some(note),
                }
            }
            ClipboardMessage::Reply(ClipboardReply::Sent(data)) => {
                // **Nothing goes back.** The far end asked nothing; a refusal
                // sent here would be this side answering a question that was
                // not put, and a peer that could make this side speak by
                // sending content is a peer with a way to make it speak.
                Heard::just(match self.accept(data) {
                    Ok(()) => format!("took {} bytes of {}", data.content.len(), data.kind.name()),
                    Err(why) => format!("did not take it: {}", why.message()),
                })
            }
            // The far end's policy answering, which is the arrangement
            // working. Said once, quietly.
            ClipboardMessage::Reply(ClipboardReply::Refused(why)) => {
                Heard::just(why.message().to_owned())
            }
        }
    }

    /// Forget everything, at the end of a session.
    ///
    /// Wipes what this process is holding. **The machine's own clipboard is
    /// left alone**: by the time a session ends, what is on it is either
    /// something the person here chose to accept or something they copied
    /// themselves since, and taking either away is a worse surprise than
    /// leaving it.
    ///
    /// # What the platform does anyway
    ///
    /// On a display server where nothing *stores* a clipboard and a process
    /// answers for one — X11 — a paste this session made does not outlive it,
    /// because the thing answering for it goes when the session does. That is
    /// not this method's doing and it is not undone here: content that arrived
    /// over a session lasting only as long as the session is the safe
    /// direction, and a person who wants to keep it can copy it again.
    pub fn forget(&mut self) {
        self.forget_held();
        self.asked = None;
        self.changed_at = None;
    }

    fn forget_held(&mut self) {
        for held in &mut self.held {
            held.wipe();
        }
        self.held.clear();
    }
}

/// What came of hearing one message.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Heard {
    /// What to send back, if anything.
    pub say: Option<ClipboardMessage>,
    /// What happened, in a sentence, for whoever is watching the session.
    pub note: Option<String>,
}

impl Heard {
    const fn nothing() -> Self {
        Self {
            say: None,
            note: None,
        }
    }

    fn just(note: String) -> Self {
        Self {
            say: None,
            note: Some(note),
        }
    }
}

impl<C> Drop for Courier<C> {
    fn drop(&mut self) {
        for held in &mut self.held {
            held.wipe();
        }
    }
}

impl<C> std::fmt::Debug for Courier<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Courier")
            .field("grants", &self.grants)
            .field("held", &self.held)
            .field("awaiting", &self.asked)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NullClipboard;

    /// A clipboard that counts what was asked of it, so a test can show that
    /// something was *not* read rather than only that nothing came back.
    #[derive(Default)]
    struct Spy {
        inner: NullClipboard,
        reads: u32,
        writes: u32,
        /// Handed back instead of the real content, when set.
        instead: Option<(ClipboardKind, Vec<u8>)>,
    }

    impl Clipboard for Spy {
        fn read(&mut self, kind: ClipboardKind) -> Result<Option<Vec<u8>>, ClipboardError> {
            self.reads = self.reads.saturating_add(1);
            if let Some((k, bytes)) = &self.instead
                && *k == kind
            {
                return Ok(Some(bytes.clone()));
            }
            self.inner.read(kind)
        }
        fn write(&mut self, kind: ClipboardKind, content: &[u8]) -> Result<(), ClipboardError> {
            self.writes = self.writes.saturating_add(1);
            self.inner.write(kind, content)
        }
        fn generation(&mut self) -> u64 {
            self.inner.generation()
        }
        fn clear(&mut self) -> Result<(), ClipboardError> {
            self.inner.clear()
        }
    }

    const BOTH: Grants = Grants {
        read: true,
        write: true,
    };

    fn later(now: Instant) -> Instant {
        now.checked_add(SETTLE * 2).unwrap_or(now)
    }

    /// Drive a courier to the point where it has surveyed a settled clipboard.
    fn settled(courier: &mut Courier<Spy>, now: Instant) -> Option<ClipboardOffer> {
        courier.poll(now);
        courier.poll(later(now))
    }

    fn with(content: &[u8]) -> (Courier<Spy>, Instant) {
        let mut spy = Spy::default();
        spy.inner.set(ClipboardKind::Text, content);
        (Courier::new(spy, BOTH), Instant::now())
    }

    /// **The check that matters most.** A peer that may not see this
    /// machine's clipboard must not cause it to be read — not for an offer,
    /// not to measure it, not at all. Reading is what puts the content in this
    /// process, and what is never read cannot leak.
    #[test]
    fn a_clipboard_the_far_end_may_not_see_is_never_read() {
        let mut spy = Spy::default();
        spy.inner.set(ClipboardKind::Text, b"hunter2");
        let mut courier = Courier::new(
            spy,
            Grants {
                read: false,
                write: true,
            },
        );
        let now = Instant::now();
        for step in 0..8 {
            assert_eq!(
                courier.poll(now.checked_add(SETTLE * step).unwrap_or(now)),
                None
            );
        }
        assert_eq!(
            courier.clipboard_mut().reads,
            0,
            "the clipboard was read for a peer that may not have it"
        );
    }

    /// A program that copies in a loop must not make this read in a loop.
    #[test]
    fn a_change_has_to_settle_before_it_is_read() {
        let (mut courier, now) = with(b"first");
        assert_eq!(courier.poll(now), None, "read before it settled");
        assert_eq!(courier.clipboard_mut().reads, 0);

        let halfway = now.checked_add(SETTLE / 2).unwrap_or(now);
        assert_eq!(courier.poll(halfway), None, "read before it settled");
        assert_eq!(courier.clipboard_mut().reads, 0);

        let offer = courier.poll(later(now)).expect("a settled change is news");
        assert_eq!(
            offer.of(ClipboardKind::Text).map(|o| o.bytes),
            Some(5),
            "the offer says the shape"
        );
        // And once announced, a clipboard nobody touched is not announced again.
        assert_eq!(courier.poll(later(later(now))), None);
    }

    /// An offer names shapes. The content is what a request is for.
    #[test]
    fn an_offer_carries_no_content() {
        let (mut courier, now) = with(b"hunter2");
        let offer = settled(&mut courier, now).expect("something was copied");
        let mut encoded = Vec::new();
        esper_proto::frame::write_frame(&mut encoded, &offer).expect("encodes");
        let as_text: String = encoded.iter().map(|&b| b as char).collect();
        assert!(
            !as_text.contains("hunter2"),
            "the content travelled in the offer: {as_text}"
        );
    }

    #[test]
    fn a_request_is_answered_with_exactly_what_was_offered() {
        let (mut courier, now) = with(b"hunter2");
        settled(&mut courier, now).expect("something was copied");
        let reply = courier.answer(&ClipboardRequest {
            kind: ClipboardKind::Text,
        });
        match reply {
            ClipboardReply::Sent(data) => {
                assert_eq!(data.kind, ClipboardKind::Text);
                assert_eq!(data.content, b"hunter2");
            }
            other => panic!("{other:?}"),
        }
        // A kind that was never offered is refused rather than invented.
        assert_eq!(
            courier.answer(&ClipboardRequest {
                kind: ClipboardKind::Image
            }),
            ClipboardReply::Refused(ClipboardRefusal::NotOffered)
        );
    }

    /// The permission is checked at the answer as well as at the poll, because
    /// a request can arrive from a peer that never saw an offer.
    #[test]
    fn a_request_from_a_peer_that_may_not_read_is_refused() {
        let mut spy = Spy::default();
        spy.inner.set(ClipboardKind::Text, b"hunter2");
        let mut courier = Courier::new(
            spy,
            Grants {
                read: false,
                write: true,
            },
        );
        assert_eq!(
            courier.answer(&ClipboardRequest {
                kind: ClipboardKind::Text
            }),
            ClipboardReply::Refused(ClipboardRefusal::NotPermitted)
        );
        assert_eq!(courier.clipboard_mut().reads, 0);
    }

    /// **The protocol has no push, and this is where that is enforced.**
    /// Content nothing asked for is refused, whatever it claims to be.
    #[test]
    fn content_nobody_asked_for_is_refused_and_not_written() {
        let mut courier = Courier::new(Spy::default(), BOTH);
        assert_eq!(courier.awaiting(), None);
        let pushed = ClipboardData {
            kind: ClipboardKind::Text,
            content: b"paste me".to_vec(),
        };
        assert_eq!(courier.accept(&pushed), Err(ClipboardRefusal::NotOffered));
        assert_eq!(
            courier.clipboard_mut().writes,
            0,
            "an unasked-for paste reached the clipboard"
        );

        // Asking makes exactly one answer acceptable.
        let offer = ClipboardOffer {
            kinds: vec![Offered {
                kind: ClipboardKind::Text,
                bytes: 8,
            }],
        };
        assert_eq!(
            courier.wants(&offer),
            Some(ClipboardRequest {
                kind: ClipboardKind::Text
            })
        );
        assert_eq!(courier.awaiting(), Some(ClipboardKind::Text));
        assert_eq!(courier.accept(&pushed), Ok(()));
        assert_eq!(courier.clipboard_mut().writes, 1);
        // And the second copy of the same answer is not a second paste.
        assert_eq!(courier.accept(&pushed), Err(ClipboardRefusal::NotOffered));
        assert_eq!(courier.clipboard_mut().writes, 1);
    }

    /// Answering a question with a different answer does not count.
    #[test]
    fn an_answer_of_the_wrong_kind_is_refused() {
        let mut courier = Courier::new(Spy::default(), BOTH);
        courier
            .wants(&ClipboardOffer {
                kinds: vec![Offered {
                    kind: ClipboardKind::Text,
                    bytes: 4,
                }],
            })
            .expect("text was offered");
        let wrong = ClipboardData {
            kind: ClipboardKind::Image,
            content: crate::conformance_png(),
        };
        assert_eq!(courier.accept(&wrong), Err(ClipboardRefusal::NotOffered));
        assert_eq!(courier.clipboard_mut().writes, 0);
        assert_eq!(
            courier.awaiting(),
            Some(ClipboardKind::Text),
            "the question stands"
        );
    }

    #[test]
    fn a_peer_that_may_not_write_is_refused_and_is_never_asked() {
        let mut courier = Courier::new(
            Spy::default(),
            Grants {
                read: true,
                write: false,
            },
        );
        let offer = ClipboardOffer {
            kinds: vec![Offered {
                kind: ClipboardKind::Text,
                bytes: 4,
            }],
        };
        assert_eq!(
            courier.wants(&offer),
            None,
            "asked for what it would refuse"
        );
        assert_eq!(
            courier.accept(&ClipboardData {
                kind: ClipboardKind::Text,
                content: b"nope".to_vec(),
            }),
            Err(ClipboardRefusal::NotPermitted)
        );
        assert_eq!(courier.clipboard_mut().writes, 0);
    }

    /// Content that is not what it says it is never reaches the clipboard,
    /// where every program on this machine could read it.
    #[test]
    fn malformed_content_is_refused_even_though_it_was_asked_for() {
        let mut courier = Courier::new(Spy::default(), BOTH);
        courier
            .wants(&ClipboardOffer {
                kinds: vec![Offered {
                    kind: ClipboardKind::Text,
                    bytes: 3,
                }],
            })
            .expect("text was offered");
        assert_eq!(
            courier.accept(&ClipboardData {
                kind: ClipboardKind::Text,
                content: vec![0xFF, 0xFE, 0xFD],
            }),
            Err(ClipboardRefusal::Malformed)
        );
        assert_eq!(courier.clipboard_mut().writes, 0);
    }

    /// **The echo that would never stop.** Accepting a paste changes the local
    /// clipboard, which looks exactly like the person here copying something —
    /// so without this the next poll would offer the far end its own paste, it
    /// would ask for it, and the two would trade it forever.
    #[test]
    fn a_paste_is_not_offered_back_to_the_peer_that_sent_it() {
        let mut courier = Courier::new(Spy::default(), BOTH);
        let now = Instant::now();
        // Settle first, so the courier is not merely waiting.
        settled(&mut courier, now);

        courier
            .wants(&ClipboardOffer {
                kinds: vec![Offered {
                    kind: ClipboardKind::Text,
                    bytes: 8,
                }],
            })
            .expect("text was offered");
        courier
            .accept(&ClipboardData {
                kind: ClipboardKind::Text,
                content: b"paste me".to_vec(),
            })
            .expect("it was asked for and is well formed");

        // A real display server tells this process about the change *after*
        // the write returns, so the generation moves again here. Guarding on
        // the generation alone would miss exactly this.
        courier
            .clipboard_mut()
            .inner
            .set(ClipboardKind::Text, b"paste me");

        for step in 1..6 {
            let at = now.checked_add(SETTLE * step * 2).unwrap_or(now);
            assert_eq!(
                courier.poll(at),
                None,
                "the paste was offered back to whoever sent it"
            );
        }

        // And a copy the person here actually makes is still announced.
        courier
            .clipboard_mut()
            .inner
            .set(ClipboardKind::Text, b"mine");
        let at = now.checked_add(SETTLE * 20).unwrap_or(now);
        courier.poll(at);
        let offer = courier
            .poll(later(at))
            .expect("a real copy after a paste is still news");
        assert_eq!(offer.of(ClipboardKind::Text).map(|o| o.bytes), Some(4));
    }

    /// What this machine cannot carry is not announced, so the far end never
    /// asks for something it can only be refused.
    #[test]
    fn content_this_session_cannot_carry_is_not_offered() {
        let mut spy = Spy::default();
        spy.inner.set(ClipboardKind::Text, b"placeholder");
        spy.instead = Some((
            ClipboardKind::Text,
            vec![b'a'; ClipboardKind::Text.cap() + 1],
        ));
        let mut courier = Courier::new(spy, BOTH);
        let now = Instant::now();
        assert_eq!(
            settled(&mut courier, now),
            None,
            "more than the cap was offered"
        );
        assert_eq!(
            courier.answer(&ClipboardRequest {
                kind: ClipboardKind::Text
            }),
            ClipboardReply::Refused(ClipboardRefusal::NotOffered)
        );
    }

    /// An X11 selection can hand back anything at all. Text that is not text
    /// is not offered as text.
    #[test]
    fn local_content_that_is_not_what_it_claims_is_not_offered() {
        let mut spy = Spy::default();
        spy.inner.set(ClipboardKind::Text, b"placeholder");
        spy.instead = Some((ClipboardKind::Text, vec![0xFF, 0xFE, 0xFD]));
        let mut courier = Courier::new(spy, BOTH);
        assert_eq!(settled(&mut courier, Instant::now()), None);
    }

    /// Text first, because text is what people copy.
    #[test]
    fn the_most_useful_kind_offered_is_the_one_asked_for() {
        let mut courier = Courier::new(Spy::default(), BOTH);
        let offer = ClipboardOffer {
            kinds: vec![
                Offered {
                    kind: ClipboardKind::Image,
                    bytes: 4096,
                },
                Offered {
                    kind: ClipboardKind::Text,
                    bytes: 6,
                },
            ],
        };
        assert_eq!(
            courier.wants(&offer),
            Some(ClipboardRequest {
                kind: ClipboardKind::Text
            })
        );
        // An offer outside its bounds is not asked about at all.
        let mut courier = Courier::new(Spy::default(), BOTH);
        assert_eq!(courier.wants(&ClipboardOffer { kinds: Vec::new() }), None);
        assert_eq!(courier.awaiting(), None);
    }

    /// A session that ended leaves nothing here for the next one to answer
    /// with.
    #[test]
    fn forgetting_leaves_nothing_to_answer_a_request_with() {
        let (mut courier, now) = with(b"hunter2");
        settled(&mut courier, now).expect("something was copied");
        assert!(matches!(
            courier.answer(&ClipboardRequest {
                kind: ClipboardKind::Text
            }),
            ClipboardReply::Sent(_)
        ));
        assert!(format!("{courier:?}").contains("Held"));

        courier.forget();
        assert_eq!(
            courier.answer(&ClipboardRequest {
                kind: ClipboardKind::Text
            }),
            ClipboardReply::Refused(ClipboardRefusal::NotOffered),
            "a forgotten session still had the content"
        );
        assert_eq!(courier.awaiting(), None);
        assert!(
            !format!("{courier:?}").contains("hunter2"),
            "the content reached a log line"
        );
    }

    #[test]
    fn grants_come_from_the_permissions_and_nothing_else() {
        assert_eq!(Grants::from_permissions(&[]), Grants::none());
        assert_eq!(
            Grants::from_permissions(&[Permission::ClipboardOut]),
            Grants {
                read: true,
                write: false
            }
        );
        assert_eq!(
            Grants::from_permissions(&[Permission::ClipboardIn, Permission::View]),
            Grants {
                read: false,
                write: true
            }
        );
        assert_eq!(
            Grants::from_permissions(&[Permission::Control, Permission::View]),
            Grants::none(),
            "an unrelated permission granted a clipboard"
        );
        assert!(BOTH.allows(ClipboardWay::Out) && BOTH.allows(ClipboardWay::In));
        assert!(!Grants::none().allows(ClipboardWay::Out));
        assert!(!Grants::none().allows(ClipboardWay::In));
        assert_eq!(Courier::new(Spy::default(), BOTH).grants(), BOTH);
    }

    /// One negotiated grant, two ends, and the same content moving.
    #[test]
    fn a_grant_read_from_the_other_end_is_the_same_grant() {
        // The host lets the controller read its clipboard, so the controller
        // has to be willing to take what comes back.
        let out = [Permission::ClipboardOut];
        assert_eq!(
            Grants::from_permissions(&out),
            Grants {
                read: true,
                write: false
            }
        );
        assert_eq!(
            Grants::mirrored(&out),
            Grants {
                read: false,
                write: true
            },
            "the controller would have refused the paste it asked for"
        );
        // And the other direction, the other way round.
        assert_eq!(
            Grants::mirrored(&[Permission::ClipboardIn]),
            Grants {
                read: true,
                write: false
            }
        );
        // Neither becomes neither, and both stay both.
        assert_eq!(Grants::mirrored(&[]), Grants::none());
        assert_eq!(
            Grants::mirrored(&[Permission::ClipboardIn, Permission::ClipboardOut]),
            BOTH
        );
    }
}
