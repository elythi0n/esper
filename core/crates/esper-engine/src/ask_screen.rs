//! Asking on the host's own screen.
//!
//! # What this closes
//!
//! Until now a host asked on its terminal or it did not ask at all — so a
//! machine started from a desktop launcher, or as a user service, refused
//! every session that needed somebody's agreement. That answer is honest and
//! it makes the product unusable for the person most likely to want it: the
//! one sitting in front of the machine, with no terminal open.
//!
//! The question itself is drawn by [`esper_dialog`] and put on the screen by
//! [`esper_ask_x11`]; what is here is which of them a host uses, and the one
//! rule about the choice.

use std::time::Duration;

use crate::consent::{Answer, Ask, AskAtTheHost, Asked, Console};

/// Ask wherever somebody actually is.
///
/// **A terminal wins when there is one.** Somebody who ran `esper host` in a
/// terminal is looking at that terminal, and drawing a window over their
/// screen instead would put the question where they are not. A host with no
/// terminal has nobody watching one, and then the screen is the only place
/// left.
///
/// Neither, and the host says so and serves only devices holding
/// `unattended` — which is what it did in both cases before this existed.
#[must_use]
pub fn wherever_somebody_is() -> AskAtTheHost {
    let within = crate::settings::whole("consent.answer_within")
        .map_or(crate::consent::ANSWER_WITHIN, Duration::from_secs);
    let console = Console::open();
    if console.can_ask() {
        return AskAtTheHost::asking_for(Box::new(console), within);
    }
    #[cfg(target_os = "linux")]
    if esper_ask_x11::can_ask() {
        return AskAtTheHost::asking_for(Box::new(OnTheScreen), within);
    }
    // Nothing to ask at. The console reports that for itself, and the host's
    // own output already has words for it.
    AskAtTheHost::asking_for(Box::new(console), within)
}

/// A question drawn on the screen in front of the machine.
#[cfg(target_os = "linux")]
struct OnTheScreen;

#[cfg(target_os = "linux")]
impl Ask for OnTheScreen {
    fn can_ask(&self) -> bool {
        esper_ask_x11::can_ask()
    }

    /// Draw the question and wait.
    ///
    /// **A display that has gone away is not a refusal.** Unplugging a screen,
    /// or an X server that stopped answering, is nobody saying no — it is
    /// nobody being asked, and [`Answer::Silence`] is what this vocabulary has
    /// for that. The difference reaches the controller as a different reason
    /// code, and it is the difference between "somebody said no" and "nobody
    /// was there".
    fn ask(&self, about: &Asked, within: Duration) -> Answer {
        let question = esper_dialog::Question {
            kind: about.kind.to_owned(),
            name: about.name.clone(),
            fingerprint: about.words.clone(),
            granted: about.granted.iter().map(|p| p.name().to_owned()).collect(),
            seconds: within.as_secs(),
        };
        match esper_ask_x11::ask(&question, within) {
            Ok(esper_ask_x11::Answer::Yes) => Answer::Yes,
            Ok(esper_ask_x11::Answer::No) => Answer::No,
            Ok(esper_ask_x11::Answer::Silence) => Answer::Silence,
            Err(e) => {
                eprintln!("  · the prompt could not be drawn: {e}");
                Answer::Silence
            }
        }
    }

    fn asked_at(&self) -> &'static str {
        "this screen"
    }
}
