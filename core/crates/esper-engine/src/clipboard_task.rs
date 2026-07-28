//! The clipboard half of a session — the same code at both ends.
//!
//! # Why a thread
//!
//! Reading a clipboard is a conversation with whichever program owns it, and
//! that program can be busy. On X11 it has two seconds to answer. Two seconds
//! on the engine's runtime is two seconds of frozen video, so the courier sits
//! on a thread of its own and the session talks to it through channels — the
//! same arrangement the pointer and the pipeline already have, for the same
//! reason.
//!
//! # Why one implementation for host and controller
//!
//! A clipboard is the one part of a session that is the same in both
//! directions: both machines have one, both may offer, both may ask. So there
//! is one message vocabulary, one policy, and one loop, and the only thing
//! that differs between the two ends is the grant — which
//! [`esper_clipboard::Grants::mirrored`] derives from the same negotiated
//! permissions the host end uses.

// Nothing here runs on a platform with no backend for it: `open` says so and
// the session carries on without a clipboard. It is still compiled everywhere,
// because the loop below is the part a new platform reuses rather than
// rewrites.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use esper_clipboard::{Clipboard, Courier, Grants};
use esper_proto::{ClipboardMessage, ClipboardOffer};

/// How often the local clipboard is looked at.
///
/// A person copies a few times an hour, and the courier waits for a burst of
/// changes to settle before it reads anything, so this is how long a copy
/// takes to be *noticed* rather than how often anything is read.
const TICK: Duration = Duration::from_millis(100);

/// What the session hands the thread: whatever the far end said, verbatim.
///
/// Not picked apart here. What a message means is the courier's, in one place,
/// so that this end is I/O and nothing else.
struct Errand(ClipboardMessage);

/// What the thread hands back.
pub enum Word {
    /// Put this on the wire.
    Send(ClipboardMessage),
    /// Say this to whoever is watching the session.
    Say(String),
}

/// The session's end of the clipboard.
pub struct Attached {
    errands: Sender<Errand>,
    words: tokio::sync::mpsc::UnboundedReceiver<Word>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The backend in use, for the session's own log.
    pub backend: &'static str,
}

impl std::fmt::Debug for Attached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attached")
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

/// Open a clipboard for this session, if the grant asks for one and this
/// machine has one.
///
/// Returns the reason when it cannot, so the session can say it out loud
/// rather than being silently without a clipboard — which from the far end
/// looks exactly like a clipboard that is empty.
pub fn open(grants: Grants) -> Result<Attached, String> {
    if !grants.read && !grants.write {
        return Err("not granted for this session".to_owned());
    }
    let (clipboard, backend) = platform()?;
    let courier = Courier::new(clipboard, grants);
    let (errands, inbox) = std::sync::mpsc::channel();
    let (out, words) = tokio::sync::mpsc::unbounded_channel();
    let thread = std::thread::Builder::new()
        .name("esper-clipboard".to_owned())
        .spawn(move || run(courier, &inbox, &out))
        .map_err(|e| format!("could not start a clipboard thread: {e}"))?;
    Ok(Attached {
        errands,
        words,
        thread: Some(thread),
        backend,
    })
}

#[cfg(target_os = "linux")]
fn platform() -> Result<(Box<dyn Clipboard + Send>, &'static str), String> {
    match esper_clipboard_x11::X11Clipboard::open() {
        Ok(c) => Ok((Box::new(c), esper_clipboard_x11::BACKEND)),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(not(target_os = "linux"))]
fn platform() -> Result<(Box<dyn Clipboard + Send>, &'static str), String> {
    Err("no clipboard backend is compiled in for this platform".to_owned())
}

impl Attached {
    /// Act on something the far end said. Says whether the thread is still
    /// there.
    fn take(&mut self, message: ClipboardMessage) -> bool {
        // **Checked at the boundary**, before anything reaches the policy or
        // this machine's clipboard. The courier checks again on the way in;
        // this is what keeps a malformed message from being work at all.
        if !message.is_well_formed() {
            eprintln!("      · refused {} from the far end", message.name());
            return true;
        }
        self.errands.send(Errand(message)).is_ok()
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        // Dropping the sender ends the thread's loop, and the thread wipes
        // what it holds on its way out. Joined rather than detached, so a
        // session that has ended has really let go of what it was holding.
        let (errands, _) = std::sync::mpsc::channel();
        drop(std::mem::replace(&mut self.errands, errands));
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("the clipboard thread ended badly");
        }
    }
}

/// Carry the clipboard for as long as the session lasts.
pub async fn serve(
    mut control: esper_transport::Control,
    mut attached: Attached,
    mut stopping: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = stopping.changed() => break,
            word = attached.words.recv() => match word {
                Some(Word::Send(message)) => {
                    if let Err(e) = control.send(&message).await {
                        tracing::debug!(error = %e, "the clipboard stream ended");
                        break;
                    }
                }
                Some(Word::Say(line)) => println!("      {line}"),
                None => break,
            },
            incoming = control.recv::<ClipboardMessage>() => match incoming {
                Ok(message) => {
                    if !attached.take(message) {
                        break;
                    }
                }
                // The ordinary end of a session.
                Err(_) => break,
            },
        }
    }
}

/// The courier's thread.
fn run<C: Clipboard>(
    mut courier: Courier<C>,
    errands: &Receiver<Errand>,
    out: &tokio::sync::mpsc::UnboundedSender<Word>,
) {
    loop {
        if let Some(offer) = courier.poll(Instant::now()) {
            let said = describe(&offer);
            if out
                .send(Word::Send(ClipboardMessage::Offer(offer)))
                .is_err()
            {
                break;
            }
            let _ = out.send(Word::Say(format!(
                "clipboard  {said} is here to be asked for"
            )));
        }
        match errands.recv_timeout(TICK) {
            Ok(Errand(message)) => {
                let heard = courier.hear(&message);
                if let Some(say) = heard.say
                    && out.send(Word::Send(say)).is_err()
                {
                    break;
                }
                if let Some(note) = heard.note {
                    let _ = out.send(Word::Say(format!("clipboard  {note}")));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // **The end of the session is where everything held goes.** Not on the
    // next copy, not when the process exits — here, whichever way the loop
    // ended.
    courier.forget();
}

/// An offer as a person would say it.
fn describe(offer: &ClipboardOffer) -> String {
    let mut parts = Vec::new();
    for o in &offer.kinds {
        parts.push(format!("{} bytes of {}", o.bytes, o.kind.name()));
    }
    parts.join(" and ")
}
