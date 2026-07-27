//! The thread that owns the connection, the window, and the selection.
//!
//! Everything here runs on one thread with one X connection. That is not an
//! implementation detail: a selection owner must keep answering, and a client
//! that shares a connection between a request loop and an answer loop
//! eventually blocks one on the other.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use esper_clipboard::{ClipboardError, Held};
use esper_proto::ClipboardKind;
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask,
    PropMode, Property, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent,
    Timestamp, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::Job;

/// How long to wait for whichever program owns the clipboard to answer.
///
/// Two seconds is a long time to a person and a short time to an application
/// that is swapping. Past it, the honest answer is that the clipboard did not
/// answer — not a longer wait.
const ANSWER: Duration = Duration::from_secs(2);

/// How long a whole piece-by-piece transfer may take.
const TRANSFER: Duration = Duration::from_secs(15);

/// How long a transfer *this* process is sending may sit untouched before it
/// is abandoned.
///
/// A requestor that asks for a large paste and then exits would otherwise
/// leave its bytes here for as long as the session lasts — which is the exact
/// content this project is careful about.
const ABANDONED: Duration = Duration::from_secs(30);

/// How often the loop looks for work while it owns a selection.
///
/// A paste from another program arrives as an event, and how long the person
/// who pressed paste waits is how long this takes to notice. Eight
/// milliseconds is under a frame.
const BUSY_TICK: Duration = Duration::from_millis(8);

/// And how often while it owns nothing.
///
/// Nothing will ask this process for a selection it does not hold, so the only
/// events worth noticing are the ones saying somebody else's clipboard
/// changed — and those are read by a poll that waits longer than this anyway.
const IDLE_TICK: Duration = Duration::from_millis(50);

/// And how often while a piece-by-piece transfer is running.
///
/// One piece costs one round trip, so the tick is what a large paste spends
/// most of its time in: at eight milliseconds a sixteen megabyte image would
/// take a second of nothing but waiting.
const TRANSFER_TICK: Duration = Duration::from_millis(1);

/// The most that is read from a property in one request, in four-byte units.
const READ_UNITS: u32 = 64 * 1024;

/// The most bytes a single `ChangeProperty` carries, past which a transfer is
/// broken into pieces.
///
/// Well under the smallest maximum request length any server offers, which is
/// 256 KB when the BIG-REQUESTS extension is absent.
const CHUNK: usize = 128 * 1024;

/// The most that will be accepted from another program, whatever it claims.
///
/// A paste is not a file transfer, and a peer that sends a gigabyte is not
/// offering to paste it.
fn ceiling(kind: ClipboardKind) -> usize {
    kind.cap()
}

/// The atoms this backend needs, interned once.
struct Atoms {
    clipboard: Atom,
    targets: Atom,
    timestamp: Atom,
    incr: Atom,
    utf8: Atom,
    text_plain_utf8: Atom,
    text: Atom,
    png: Atom,
    /// The property a conversion is delivered into.
    prop: Atom,
    /// A property touched only to learn the server's clock.
    clock: Atom,
}

impl Atoms {
    fn intern(conn: &RustConnection) -> Result<Self, ClipboardError> {
        let ask = |name: &str| -> Result<Atom, ClipboardError> {
            conn.intern_atom(false, name.as_bytes())
                .map_err(failed)?
                .reply()
                .map(|r| r.atom)
                .map_err(failed)
        };
        Ok(Self {
            clipboard: ask("CLIPBOARD")?,
            targets: ask("TARGETS")?,
            timestamp: ask("TIMESTAMP")?,
            incr: ask("INCR")?,
            utf8: ask("UTF8_STRING")?,
            text_plain_utf8: ask("text/plain;charset=utf-8")?,
            text: ask("TEXT")?,
            png: ask("image/png")?,
            prop: ask("ESPER_CLIPBOARD")?,
            clock: ask("ESPER_CLIPBOARD_CLOCK")?,
        })
    }
}

/// A transfer this process is sending one piece at a time.
///
/// The bytes are a [`Held`] rather than a `Vec`, like everything else here
/// that holds clipboard content: a transfer to a program that exits halfway
/// through is abandoned, and what is abandoned should not be left in the
/// allocator.
struct Outgoing {
    requestor: Window,
    property: Atom,
    kind: Atom,
    content: Held,
    sent: usize,
    touched: Instant,
}

/// What the loop is waiting for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    /// An answer to a conversion this process asked for.
    Answer,
    /// The next piece of one arriving.
    Piece,
}

struct Worker {
    conn: RustConnection,
    window: Window,
    atoms: Atoms,
    /// What this process is currently answering for, by kind.
    ///
    /// **[`Held`], so that giving it up wipes it.** This is a second copy of
    /// whatever a person last copied — very often a password — and a plain
    /// `Vec` handed back to the allocator is readable by whatever allocates
    /// next, which in this process is a buffer on its way somewhere.
    owned: Vec<Held>,
    /// The timestamp ownership was taken at, which is what `TIMESTAMP` answers.
    owned_at: Timestamp,
    generation: Arc<AtomicU64>,
    sending: Vec<Outgoing>,
}

/// Start a worker and serve it until the last handle is dropped.
pub(crate) fn run(
    inbox: &mpsc::Receiver<Job>,
    ready: &mpsc::Sender<Result<(), ClipboardError>>,
    generation: &Arc<AtomicU64>,
) {
    match Worker::start(generation) {
        Err(e) => {
            let _ = ready.send(Err(e));
        }
        Ok(mut worker) => {
            if ready.send(Ok(())).is_ok() {
                worker.serve(inbox);
            }
            worker.give_up();
        }
    }
}

impl Worker {
    fn start(generation: &Arc<AtomicU64>) -> Result<Self, ClipboardError> {
        let (conn, screen_index) = x11rb::connect(None)
            .map_err(|e| ClipboardError::Unavailable(format!("no X display: {e}")))?;
        let root = conn
            .setup()
            .roots
            .get(screen_index)
            .ok_or_else(|| ClipboardError::Unavailable("the display has no screen".to_owned()))?
            .root;
        let atoms = Atoms::intern(&conn)?;

        // XFIXES is what turns "has the clipboard changed?" from a question
        // asked several times a second into an event.
        conn.xfixes_query_version(5, 0)
            .map_err(failed)?
            .reply()
            .map_err(|e| {
                ClipboardError::Unavailable(format!("this X server has no XFIXES: {e}"))
            })?;

        let window = conn.generate_id().map_err(failed)?;
        // Input-only, one pixel, never mapped. It exists to be a name the
        // server can deliver selection events to, and it must never be
        // something a person can see or click.
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(failed)?;

        conn.xfixes_select_selection_input(
            window,
            atoms.clipboard,
            SelectionEventMask::SET_SELECTION_OWNER
                | SelectionEventMask::SELECTION_WINDOW_DESTROY
                | SelectionEventMask::SELECTION_CLIENT_CLOSE,
        )
        .map_err(failed)?;
        conn.flush().map_err(failed)?;

        Ok(Self {
            conn,
            window,
            atoms,
            owned: Vec::new(),
            owned_at: x11rb::CURRENT_TIME,
            generation: Arc::clone(generation),
            sending: Vec::new(),
        })
    }

    fn serve(&mut self, inbox: &mpsc::Receiver<Job>) {
        loop {
            if self.drain().is_err() {
                tracing::warn!("the connection to the display was lost");
                return;
            }
            self.prune();
            let tick = if !self.sending.is_empty() {
                TRANSFER_TICK
            } else if self.owned.is_empty() {
                IDLE_TICK
            } else {
                BUSY_TICK
            };
            match inbox.recv_timeout(tick) {
                Ok(job) => self.serve_one(job),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn serve_one(&mut self, job: Job) {
        match job {
            Job::Read(kind, reply) => {
                let answer = self.fetch(kind);
                let _ = reply.send(answer);
            }
            Job::Write(kind, bytes, reply) => {
                let answer = self.publish(kind, bytes);
                let _ = reply.send(answer);
            }
            Job::Clear(reply) => {
                let answer = self.release();
                let _ = reply.send(answer);
            }
        }
    }

    /// Handle everything the server has already sent.
    fn drain(&mut self) -> Result<(), ClipboardError> {
        while let Some(event) = self.conn.poll_for_event().map_err(failed)? {
            self.dispatch(event);
        }
        Ok(())
    }

    fn dispatch(&mut self, event: Event) {
        match event {
            // The clipboard changed hands. **Noted, and nothing more.** This
            // notice is not ordered against this process's own requests: it
            // describes a moment that has already passed, so giving up what is
            // held here on the strength of it would throw away content copied
            // since. Losing ownership is said properly by `SelectionClear`,
            // which the server sends to the client it is taking it from.
            Event::XfixesSelectionNotify(e) if e.selection == self.atoms.clipboard => self.moved(),
            // Somebody is pasting what this process copied.
            Event::SelectionRequest(e) => self.answer(&e),
            // Somebody else took the clipboard from this process.
            Event::SelectionClear(e) if e.selection == self.atoms.clipboard => {
                self.forget(|_| true);
                self.moved();
            }
            // A piece of an outgoing transfer was taken.
            Event::PropertyNotify(e) if e.state == Property::DELETE => {
                self.push_piece(e.window, e.atom);
            }
            Event::Error(e) => tracing::debug!(?e, "the display refused a clipboard request"),
            _ => {}
        }
    }

    /// The clipboard changed hands.
    fn moved(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Give up what this process is answering for, wiping it on the way.
    fn forget(&mut self, which: impl Fn(&Held) -> bool) {
        let mut kept = Vec::new();
        for mut held in std::mem::take(&mut self.owned) {
            if which(&held) {
                held.wipe();
            } else {
                kept.push(held);
            }
        }
        self.owned = kept;
    }

    /// Forget transfers whose requestor has stopped taking pieces.
    fn prune(&mut self) {
        let now = Instant::now();
        let before = self.sending.len();
        for transfer in &mut self.sending {
            if now.saturating_duration_since(transfer.touched) >= ABANDONED {
                transfer.content.wipe();
            }
        }
        self.sending
            .retain(|t| now.saturating_duration_since(t.touched) < ABANDONED);
        for _ in self.sending.len()..before {
            tracing::debug!("abandoned a clipboard transfer nobody was taking");
        }
    }

    // ---- answering, when this process owns the selection -------------------

    fn answer(&mut self, request: &SelectionRequestEvent) {
        if request.selection != self.atoms.clipboard {
            self.refuse(request);
            return;
        }
        // A property of `None` is what very old clients send, meaning "put it
        // where the target says". Honoured because it costs one line and the
        // alternative is a paste that silently does nothing.
        let property = if request.property == x11rb::NONE {
            request.target
        } else {
            request.property
        };
        if self.fill(request.requestor, property, request.target) {
            self.tell(request, property);
        } else {
            self.refuse(request);
        }
    }

    fn refuse(&mut self, request: &SelectionRequestEvent) {
        self.tell(request, x11rb::NONE);
    }

    fn tell(&mut self, request: &SelectionRequestEvent, property: Atom) {
        let event = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property,
        };
        if let Err(e) = self
            .conn
            .send_event(false, request.requestor, EventMask::NO_EVENT, event)
        {
            tracing::debug!(error = %e, "could not answer a paste");
        }
        let _ = self.conn.flush();
    }

    /// Put the answer on the requestor's window. Says whether it could.
    fn fill(&mut self, requestor: Window, property: Atom, target: Atom) -> bool {
        if target == self.atoms.targets {
            let mut list = vec![self.atoms.targets, self.atoms.timestamp];
            for held in &self.owned {
                list.extend(offered_targets(&self.atoms, held.kind()));
            }
            return self
                .conn
                .change_property32(
                    PropMode::REPLACE,
                    requestor,
                    property,
                    AtomEnum::ATOM,
                    &list,
                )
                .is_ok();
        }
        if target == self.atoms.timestamp {
            return self
                .conn
                .change_property32(
                    PropMode::REPLACE,
                    requestor,
                    property,
                    AtomEnum::INTEGER,
                    &[self.owned_at],
                )
                .is_ok();
        }
        let Some(kind) = self.kind_for(target) else {
            return false;
        };
        let Some(held) = self.owned.iter().find(|h| h.kind() == kind) else {
            return false;
        };
        let bytes = held.bytes().to_vec();
        // `TEXT` means "whatever you think is best", and what is best is UTF-8.
        let announced = if target == self.atoms.text {
            self.atoms.utf8
        } else {
            target
        };
        if bytes.len() <= CHUNK {
            return self
                .conn
                .change_property8(PropMode::REPLACE, requestor, property, announced, &bytes)
                .is_ok();
        }
        self.begin_piecewise(requestor, property, announced, Held::new(kind, bytes))
    }

    /// Start handing a large paste over one piece at a time.
    fn begin_piecewise(
        &mut self,
        requestor: Window,
        property: Atom,
        kind: Atom,
        content: Held,
    ) -> bool {
        // **Before the header, not after.** The requestor deletes the property
        // as soon as it sees the header, and a mask set afterwards would miss
        // the notice and the transfer would stall forever.
        if self
            .conn
            .change_window_attributes(
                requestor,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )
            .is_err()
        {
            return false;
        }
        let Ok(total) = u32::try_from(content.len()) else {
            return false;
        };
        if self
            .conn
            .change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                self.atoms.incr,
                &[total],
            )
            .is_err()
        {
            return false;
        }
        self.sending.push(Outgoing {
            requestor,
            property,
            kind,
            content,
            sent: 0,
            touched: Instant::now(),
        });
        true
    }

    /// The requestor took the last piece; give it the next one.
    fn push_piece(&mut self, requestor: Window, property: Atom) {
        let Some(at) = self
            .sending
            .iter()
            .position(|t| t.requestor == requestor && t.property == property)
        else {
            return;
        };
        let Some(transfer) = self.sending.get_mut(at) else {
            return;
        };
        transfer.touched = Instant::now();
        let from = transfer.sent;
        let to = from.saturating_add(CHUNK).min(transfer.content.len());
        let piece = transfer
            .content
            .bytes()
            .get(from..to)
            .unwrap_or_default()
            .to_vec();
        let kind = transfer.kind;
        let done = piece.is_empty();
        transfer.sent = to;

        // A zero-length write is how the end of a transfer is said.
        let sent = self
            .conn
            .change_property8(PropMode::REPLACE, requestor, property, kind, &piece)
            .is_ok();
        let _ = self.conn.flush();
        if done || !sent {
            // The requestor's window is left as it was found: this process
            // asked for property events on somebody else's window, and leaving
            // that on would keep sending them events forever.
            let _ = self.conn.change_window_attributes(
                requestor,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
            );
            let _ = self.conn.flush();
            for t in &mut self.sending {
                if t.requestor == requestor && t.property == property {
                    t.content.wipe();
                }
            }
            self.sending
                .retain(|t| !(t.requestor == requestor && t.property == property));
        }
    }

    // ---- reading somebody else's -------------------------------------------

    fn fetch(&mut self, kind: ClipboardKind) -> Result<Option<Vec<u8>>, ClipboardError> {
        // Anything the server has already said about ownership is worth
        // knowing before deciding whether this process still owns the
        // clipboard.
        self.drain()?;
        if let Some(held) = self.owned.iter().find(|h| h.kind() == kind) {
            return Ok(Some(held.bytes().to_vec()));
        }
        for target in offered_targets(&self.atoms, kind) {
            if let Some(bytes) = self.convert(kind, target)? {
                return Ok(Some(normalise(target, bytes)));
            }
        }
        Ok(None)
    }

    /// Ask the owner for one target. `None` means it cannot give that one.
    fn convert(
        &mut self,
        kind: ClipboardKind,
        target: Atom,
    ) -> Result<Option<Vec<u8>>, ClipboardError> {
        self.conn
            .delete_property(self.window, self.atoms.prop)
            .map_err(failed)?;
        self.conn
            .convert_selection(
                self.window,
                self.atoms.clipboard,
                target,
                self.atoms.prop,
                x11rb::CURRENT_TIME,
            )
            .map_err(failed)?;
        self.conn.flush().map_err(failed)?;

        let deadline = Instant::now().checked_add(ANSWER);
        let Some(Event::SelectionNotify(notify)) = self.pump(Want::Answer, deadline)? else {
            return Err(ClipboardError::NoAnswer);
        };
        // No owner, or an owner that cannot produce this target. Both are the
        // same answer here: try the next target, then give up.
        if notify.property == x11rb::NONE {
            return Ok(None);
        }
        let (found, bytes) = self.read_property(kind)?;
        self.conn
            .delete_property(self.window, self.atoms.prop)
            .map_err(failed)?;
        self.conn.flush().map_err(failed)?;
        if found == self.atoms.incr {
            return self.take_pieces(kind).map(Some);
        }
        Ok(Some(bytes))
    }

    /// Collect a transfer arriving one piece at a time.
    fn take_pieces(&mut self, kind: ClipboardKind) -> Result<Vec<u8>, ClipboardError> {
        let mut whole = Vec::new();
        let deadline = Instant::now().checked_add(TRANSFER);
        loop {
            if self.pump(Want::Piece, deadline)?.is_none() {
                return Err(ClipboardError::NoAnswer);
            }
            let (_, piece) = self.read_property(kind)?;
            self.conn
                .delete_property(self.window, self.atoms.prop)
                .map_err(failed)?;
            self.conn.flush().map_err(failed)?;
            if piece.is_empty() {
                return Ok(whole);
            }
            if whole.len().saturating_add(piece.len()) > ceiling(kind) {
                // Read no further. The rest is not wanted and the owner will
                // stop when the property stops being taken.
                return Err(ClipboardError::TooLarge {
                    kind,
                    size: whole.len().saturating_add(piece.len()),
                    cap: ceiling(kind),
                });
            }
            whole.extend_from_slice(&piece);
        }
    }

    /// Everything in the delivery property, and what type it claims to be.
    fn read_property(&self, kind: ClipboardKind) -> Result<(Atom, Vec<u8>), ClipboardError> {
        let head = self
            .conn
            .get_property(
                false,
                self.window,
                self.atoms.prop,
                AtomEnum::ANY,
                0,
                READ_UNITS,
            )
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        let claimed = head.type_;
        if claimed == x11rb::NONE {
            return Ok((claimed, Vec::new()));
        }
        let mut whole = head.value;
        let mut more = head.bytes_after;
        // In four-byte units, which is how the protocol counts offsets.
        let mut offset = u32::try_from(whole.len() / 4).unwrap_or(u32::MAX);
        while more > 0 {
            if whole.len() > ceiling(kind) {
                return Err(ClipboardError::TooLarge {
                    kind,
                    size: whole.len().saturating_add(more as usize),
                    cap: ceiling(kind),
                });
            }
            let next = self
                .conn
                .get_property(
                    false,
                    self.window,
                    self.atoms.prop,
                    AtomEnum::ANY,
                    offset,
                    READ_UNITS,
                )
                .map_err(failed)?
                .reply()
                .map_err(failed)?;
            if next.value.is_empty() {
                break;
            }
            offset = offset.saturating_add(u32::try_from(next.value.len() / 4).unwrap_or(0));
            more = next.bytes_after;
            whole.extend_from_slice(&next.value);
        }
        Ok((claimed, whole))
    }

    /// Wait for one thing while still answering everything else.
    ///
    /// **The "while still answering" is the point.** This process can be
    /// waiting for one program to hand over a paste at the moment another asks
    /// it for one, and a loop that only looked for its own answer would leave
    /// the second program hanging until the deadline.
    fn pump(
        &mut self,
        want: Want,
        deadline: Option<Instant>,
    ) -> Result<Option<Event>, ClipboardError> {
        loop {
            match self.conn.poll_for_event().map_err(failed)? {
                Some(event) => {
                    if self.is_wanted(want, &event) {
                        return Ok(Some(event));
                    }
                    self.dispatch(event);
                }
                None => {
                    if deadline.is_some_and(|d| Instant::now() >= d) {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    fn is_wanted(&self, want: Want, event: &Event) -> bool {
        match (want, event) {
            (Want::Answer, Event::SelectionNotify(e)) => {
                e.requestor == self.window && e.selection == self.atoms.clipboard
            }
            (Want::Piece, Event::PropertyNotify(e)) => {
                e.window == self.window
                    && e.atom == self.atoms.prop
                    && e.state == Property::NEW_VALUE
            }
            _ => false,
        }
    }

    // ---- taking and giving up ownership ------------------------------------

    fn publish(&mut self, kind: ClipboardKind, bytes: Vec<u8>) -> Result<(), ClipboardError> {
        if bytes.len() > ceiling(kind) {
            return Err(ClipboardError::TooLarge {
                kind,
                size: bytes.len(),
                cap: ceiling(kind),
            });
        }
        self.forget(|held| held.kind() == kind);
        self.owned.push(Held::new(kind, bytes));

        let time = self.server_time();
        self.conn
            .set_selection_owner(self.window, self.atoms.clipboard, time)
            .map_err(failed)?;
        self.conn.flush().map_err(failed)?;
        let owner = self
            .conn
            .get_selection_owner(self.atoms.clipboard)
            .map_err(failed)?
            .reply()
            .map_err(failed)?
            .owner;
        if owner != self.window {
            self.forget(|_| true);
            return Err(ClipboardError::Failed(
                "the display would not make this process the clipboard's owner".to_owned(),
            ));
        }
        self.owned_at = time;
        // **Said here rather than waited for.** XFIXES will say the same thing
        // in a moment, and a caller that asked for the clipboard to change and
        // then looked would otherwise see that it had not yet.
        self.moved();
        Ok(())
    }

    fn release(&mut self) -> Result<(), ClipboardError> {
        self.forget(|_| true);
        for transfer in &mut self.sending {
            transfer.content.wipe();
        }
        self.sending.clear();
        self.conn
            .set_selection_owner(x11rb::NONE, self.atoms.clipboard, x11rb::CURRENT_TIME)
            .map_err(failed)?;
        self.conn.flush().map_err(failed)?;
        self.moved();
        Ok(())
    }

    /// A timestamp from the server, rather than `CurrentTime`.
    ///
    /// ICCCM asks for a real one: two clients racing for the selection are
    /// settled by comparing timestamps, and `CurrentTime` cannot be compared
    /// with anything. Obtained the way everything obtains it — by touching a
    /// property and reading the time off the notice that comes back.
    ///
    /// Falls back to `CurrentTime` rather than failing. Losing a race that
    /// almost never happens is better than refusing to copy.
    fn server_time(&mut self) -> Timestamp {
        let touched = self
            .conn
            .change_property8(
                PropMode::APPEND,
                self.window,
                self.atoms.clock,
                AtomEnum::STRING,
                &[],
            )
            .is_ok();
        if !touched || self.conn.flush().is_err() {
            return x11rb::CURRENT_TIME;
        }
        let deadline = Instant::now().checked_add(ANSWER);
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(Event::PropertyNotify(e)))
                    if e.window == self.window && e.atom == self.atoms.clock =>
                {
                    return e.time;
                }
                Ok(Some(event)) => self.dispatch(event),
                Ok(None) => {
                    if deadline.is_some_and(|d| Instant::now() >= d) {
                        return x11rb::CURRENT_TIME;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => return x11rb::CURRENT_TIME,
            }
        }
    }

    /// Stop being the owner of anything, on the way out.
    ///
    /// The server would drop the selection anyway when this connection closes,
    /// so the visible half of this is belt and braces. The half that is not:
    /// releasing wipes what is held here, and a connection closing does not.
    fn give_up(&mut self) {
        let _ = self.release();
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }

    fn kind_for(&self, target: Atom) -> Option<ClipboardKind> {
        if target == self.atoms.utf8
            || target == self.atoms.text_plain_utf8
            || target == self.atoms.text
            || target == Atom::from(AtomEnum::STRING)
        {
            return Some(ClipboardKind::Text);
        }
        (target == self.atoms.png).then_some(ClipboardKind::Image)
    }
}

/// The targets this backend will use for a kind, best first.
///
/// `STRING` is last and is only ever *accepted*: it means Latin-1 by
/// definition, so offering it would be promising to produce bytes that cannot
/// represent most of what people copy. Accepting it costs a transcode and
/// makes pasting from an old program work.
fn offered_targets(atoms: &Atoms, kind: ClipboardKind) -> Vec<Atom> {
    match kind {
        ClipboardKind::Text => vec![
            atoms.utf8,
            atoms.text_plain_utf8,
            atoms.text,
            AtomEnum::STRING.into(),
        ],
        ClipboardKind::Image => vec![atoms.png],
    }
}

/// Turn what arrived into what this protocol carries.
fn normalise(target: Atom, bytes: Vec<u8>) -> Vec<u8> {
    if target == Atom::from(AtomEnum::STRING) {
        // Latin-1 by definition, and every byte of it is a character, so this
        // cannot fail and cannot lose anything.
        return bytes
            .iter()
            .map(|&b| char::from(b))
            .collect::<String>()
            .into_bytes();
    }
    bytes
}

fn failed(e: impl std::fmt::Display) -> ClipboardError {
    ClipboardError::Failed(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `STRING` means Latin-1 by definition, and a program old enough to offer
    /// only that is exactly the program whose text has an accent in it.
    /// Pasting `café` and getting `cafÃ©` — or four bytes that are not text at
    /// all — is the failure this exists to stop.
    #[test]
    fn latin_one_becomes_utf_eight_and_nothing_else_is_touched() {
        let string: Atom = AtomEnum::STRING.into();
        // `café` in Latin-1: the é is one byte, 0xE9.
        let latin = vec![b'c', b'a', b'f', 0xE9];
        let got = normalise(string, latin);
        assert_eq!(String::from_utf8(got.clone()).as_deref(), Ok("café"));
        assert_eq!(got.len(), 5, "the é should have become two bytes");

        // Every byte is a character in Latin-1, so nothing can fail to convert.
        let every = (0u8..=255).collect::<Vec<_>>();
        let all = normalise(string, every);
        assert!(std::str::from_utf8(&all).is_ok());

        // And a target that already means UTF-8 is left exactly alone, or text
        // would be transcoded twice.
        let already = "café 日本語".as_bytes().to_vec();
        assert_eq!(normalise(Atom::from(9999u32), already.clone()), already);
    }

    /// `STRING` is accepted and never offered: offering it would be promising
    /// to produce bytes that cannot represent most of what people copy.
    #[test]
    fn the_targets_offered_are_the_ones_that_can_carry_what_people_copy() {
        let atoms = Atoms {
            clipboard: 1,
            targets: 2,
            timestamp: 3,
            incr: 4,
            utf8: 5,
            text_plain_utf8: 6,
            text: 7,
            png: 8,
            prop: 9,
            clock: 10,
        };
        let text = offered_targets(&atoms, ClipboardKind::Text);
        assert_eq!(text.first(), Some(&atoms.utf8), "UTF-8 is asked for first");
        assert_eq!(
            text.last(),
            Some(&Atom::from(AtomEnum::STRING)),
            "Latin-1 is the last resort"
        );
        assert_eq!(
            offered_targets(&atoms, ClipboardKind::Image),
            vec![atoms.png]
        );
        // The two kinds share nothing, or asking for one could answer with the
        // other.
        for target in offered_targets(&atoms, ClipboardKind::Image) {
            assert!(
                !text.contains(&target),
                "an image target is also a text one"
            );
        }
    }
}
