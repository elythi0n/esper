//! Numeric reason codes.
//!
//! Codes exist so clients can branch; the accompanying string exists so the
//! person reading the UI understands. Every failure carries both.
//!
//! | Range | Meaning |
//! |-------|---------|
//! | 1xx   | Protocol — version mismatch, malformed frame |
//! | 2xx   | Authentication — unknown key, changed key, pairing failed |
//! | 3xx   | Authorization — permission denied, revoked, out of schedule |
//! | 4xx   | Capability — no common codec, no capture backend, no display |
//! | 5xx   | Transport — tiers exhausted, relay full, migration failed |

use serde::{Deserialize, Serialize};

/// A protocol reason code. Unknown values are preserved rather than rejected,
/// so a newer peer's code reaches a human even when this build cannot name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Reason(pub u16);

impl Reason {
    // 0xx — a session that ended, rather than failed.
    //
    // Every other class is a failure. Without a class for endings, a session
    // closing normally has to pick whichever failure is nearest, and a
    // controller cannot tell "the person closed the window" from "the host
    // refused you".
    /// Somebody closed the viewer, or interrupted it.
    pub const CLOSED_BY_USER: Self = Self(1);
    /// The session ran for as long as it was asked to.
    pub const SESSION_COMPLETE: Self = Self(2);
    /// The host is going away — shutting down, or losing its display.
    pub const HOST_SHUTTING_DOWN: Self = Self(3);
    /// A newer session for the same device replaced this one.
    pub const SUPERSEDED: Self = Self(4);
    /// The peer stopped responding, and the session was given up on.
    ///
    /// Not a failure of either side's making, which is why it lives here: a
    /// laptop that closed its lid is not a peer that did something wrong.
    pub const PEER_GONE: Self = Self(5);
    /// The person at the host ended it.
    ///
    /// **Its own code because it is its own event.** A controller told "the
    /// host is shutting down" would wait for it to come back; told "you were
    /// disconnected" it would reconnect. Neither is what happened: somebody
    /// standing at the machine decided they had seen enough, and the honest
    /// thing to show the person at the other end is that a person did this.
    ///
    /// It revokes nothing. The pairing is intact and a reconnection is
    /// allowed — what stops it being useful is that the screen is now locked.
    pub const STOPPED_AT_HOST: Self = Self(6);

    // 1xx — protocol
    pub const PROTOCOL_VERSION_MISMATCH: Self = Self(100);
    pub const MALFORMED_FRAME: Self = Self(101);
    pub const FRAME_TOO_LARGE: Self = Self(102);
    pub const UNKNOWN_MESSAGE: Self = Self(103);

    // 2xx — authentication
    pub const UNKNOWN_PEER_KEY: Self = Self(200);
    pub const PEER_KEY_CHANGED: Self = Self(201);
    pub const PAIRING_FAILED: Self = Self(202);
    pub const PAIRING_EXPIRED: Self = Self(203);
    pub const PAIRING_RATE_LIMITED: Self = Self(204);

    // 3xx — authorization
    pub const PERMISSION_DENIED: Self = Self(300);
    pub const REVOKED: Self = Self(301);
    pub const OUT_OF_SCHEDULE: Self = Self(302);
    pub const HOST_DISABLED: Self = Self(303);
    pub const PROMPT_TIMEOUT: Self = Self(304);
    /// Somebody at the host was asked and said no.
    ///
    /// Its own code rather than a timeout, because the two mean opposite
    /// things to whoever is waiting: nobody answered is worth trying again
    /// when they are back, and somebody declined is not.
    pub const PROMPT_REFUSED: Self = Self(305);
    /// The host had nobody to ask, and this device may not connect unattended.
    ///
    /// The fail-closed answer, and one a real deployment meets: a host running
    /// as a service has no terminal to ask at, so a device that was never
    /// granted `unattended` cannot open a session there. Its own code because
    /// the fix is a specific one — grant that permission — and a device told
    /// "permission denied" would go looking at the wrong ones.
    pub const NOBODY_TO_ASK: Self = Self(306);

    // 4xx — capability
    pub const NO_COMMON_CODEC: Self = Self(400);
    pub const NO_CAPTURE_BACKEND: Self = Self(401);
    pub const NO_DISPLAY: Self = Self(402);
    pub const NO_INPUT_BACKEND: Self = Self(403);

    // 5xx — transport
    pub const ALL_TIERS_EXHAUSTED: Self = Self(500);
    pub const RELAY_FULL: Self = Self(501);
    pub const MIGRATION_FAILED: Self = Self(502);
    pub const IDLE_TIMEOUT: Self = Self(503);

    /// Whether this code describes a failure rather than an ending.
    ///
    /// The distinction a caller actually needs: a viewer that closed cleanly
    /// should say so and exit, and one that was refused should say why and
    /// tell somebody. Reported by class rather than by listing codes, so a
    /// code from a newer peer is classified correctly without this build
    /// knowing its name.
    #[must_use]
    pub const fn is_failure(self) -> bool {
        !matches!(self.class(), Class::Normal)
    }

    /// The class this code belongs to, derived from its hundreds digit.
    #[must_use]
    pub const fn class(self) -> Class {
        match self.0 / 100 {
            0 => Class::Normal,
            1 => Class::Protocol,
            2 => Class::Authentication,
            3 => Class::Authorization,
            4 => Class::Capability,
            5 => Class::Transport,
            _ => Class::Unknown,
        }
    }

    /// A short human description, or `None` for a code this build does not know.
    ///
    /// Callers must render the peer-supplied string when this returns `None`,
    /// which is how a newer peer's failures stay legible.
    #[must_use]
    pub const fn describe(self) -> Option<&'static str> {
        Some(match self {
            Self::CLOSED_BY_USER => "closed",
            Self::SESSION_COMPLETE => "session complete",
            Self::HOST_SHUTTING_DOWN => "the host is shutting down",
            Self::SUPERSEDED => "replaced by a newer session",
            Self::PEER_GONE => "the peer stopped responding",
            Self::STOPPED_AT_HOST => "the person at the host ended it",
            Self::PROTOCOL_VERSION_MISMATCH => "protocol version mismatch",
            Self::MALFORMED_FRAME => "malformed frame",
            Self::FRAME_TOO_LARGE => "frame exceeds the maximum length",
            Self::UNKNOWN_MESSAGE => "unknown message type",
            Self::UNKNOWN_PEER_KEY => "peer key is not paired with this device",
            Self::PEER_KEY_CHANGED => "peer presented a different key; re-pairing required",
            Self::PAIRING_FAILED => "pairing code did not match",
            Self::PAIRING_EXPIRED => "pairing code has expired",
            Self::PAIRING_RATE_LIMITED => "too many pairing attempts",
            Self::PERMISSION_DENIED => "permission denied",
            Self::REVOKED => "this device has been revoked",
            Self::OUT_OF_SCHEDULE => "outside the permitted schedule",
            Self::HOST_DISABLED => "this machine is not accepting sessions",
            Self::PROMPT_TIMEOUT => "nobody responded to the connection prompt",
            Self::PROMPT_REFUSED => "somebody at the host declined this session",
            Self::NOBODY_TO_ASK => {
                "this host has nobody to ask, and this device may not connect unattended"
            }
            Self::NO_COMMON_CODEC => "no codec in common",
            Self::NO_CAPTURE_BACKEND => "no screen capture backend available",
            Self::NO_DISPLAY => "no display to capture",
            Self::NO_INPUT_BACKEND => "no input injection backend available",
            Self::ALL_TIERS_EXHAUSTED => "every connection tier failed",
            Self::RELAY_FULL => "relay is at capacity",
            Self::MIGRATION_FAILED => "connection migration failed",
            Self::IDLE_TIMEOUT => "session was idle too long",
            _ => return None,
        })
    }
}

impl std::fmt::Display for Reason {
    /// The human description, or a note that this build does not know the code.
    ///
    /// Mirrors the Go implementation's `String()` exactly, including the
    /// fallback text: the two are compared by a cross-language test, and a
    /// close reason that differs between the halves is a confusing bug report.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.describe().unwrap_or("unrecognized reason code"))
    }
}

/// The class of a reason code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// The session ended rather than failed.
    Normal,
    Protocol,
    Authentication,
    Authorization,
    Capability,
    Transport,
    /// A code from a newer peer that this build does not recognize.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_derive_from_the_hundreds_digit() {
        assert_eq!(Reason::MALFORMED_FRAME.class(), Class::Protocol);
        assert_eq!(Reason::UNKNOWN_PEER_KEY.class(), Class::Authentication);
        assert_eq!(Reason::PERMISSION_DENIED.class(), Class::Authorization);
        assert_eq!(Reason::NO_COMMON_CODEC.class(), Class::Capability);
        assert_eq!(Reason::ALL_TIERS_EXHAUSTED.class(), Class::Transport);
    }

    #[test]
    fn unknown_codes_are_preserved_not_rejected() {
        let future = Reason(999);
        assert_eq!(future.class(), Class::Unknown);
        assert_eq!(future.describe(), None);
        assert_eq!(future.0, 999, "the raw code survives for display");
    }

    /// The fallback text must match the Go implementation's, because a close
    /// reason that differs between the two halves is a confusing bug report.
    #[test]
    fn display_matches_the_go_implementation() {
        assert_eq!(Reason::MALFORMED_FRAME.to_string(), "malformed frame");
        assert_eq!(Reason(9999).to_string(), "unrecognized reason code");
    }

    #[test]
    fn every_named_code_has_a_description() {
        for r in [
            Reason::PROTOCOL_VERSION_MISMATCH,
            Reason::MALFORMED_FRAME,
            Reason::FRAME_TOO_LARGE,
            Reason::UNKNOWN_MESSAGE,
            Reason::UNKNOWN_PEER_KEY,
            Reason::PEER_KEY_CHANGED,
            Reason::PAIRING_FAILED,
            Reason::PAIRING_EXPIRED,
            Reason::PAIRING_RATE_LIMITED,
            Reason::PERMISSION_DENIED,
            Reason::REVOKED,
            Reason::OUT_OF_SCHEDULE,
            Reason::HOST_DISABLED,
            Reason::PROMPT_TIMEOUT,
            Reason::PROMPT_REFUSED,
            Reason::NOBODY_TO_ASK,
            Reason::NO_COMMON_CODEC,
            Reason::NO_CAPTURE_BACKEND,
            Reason::NO_DISPLAY,
            Reason::NO_INPUT_BACKEND,
            Reason::ALL_TIERS_EXHAUSTED,
            Reason::RELAY_FULL,
            Reason::MIGRATION_FAILED,
            Reason::IDLE_TIMEOUT,
        ] {
            assert!(r.describe().is_some(), "code {} has no description", r.0);
            assert_ne!(r.class(), Class::Unknown, "code {} has no class", r.0);
        }
    }
}

#[cfg(test)]
mod normal_close_tests {
    use super::*;

    /// **Every other class is a failure.** Without this one, a session ending
    /// normally has to close with whatever code is nearest, and a controller
    /// cannot tell "the person closed the window" from "the host refused you".
    #[test]
    fn an_ending_is_not_a_failure() {
        for r in [
            Reason::CLOSED_BY_USER,
            Reason::SESSION_COMPLETE,
            Reason::HOST_SHUTTING_DOWN,
            Reason::SUPERSEDED,
            Reason::PEER_GONE,
            Reason::STOPPED_AT_HOST,
        ] {
            assert_eq!(r.class(), Class::Normal, "{r:?}");
            assert!(!r.is_failure(), "{r:?} reads as a failure");
            assert!(r.describe().is_some(), "{r:?} has no description");
        }
    }

    #[test]
    fn every_other_class_is_a_failure() {
        for r in [
            Reason::PROTOCOL_VERSION_MISMATCH,
            Reason::UNKNOWN_PEER_KEY,
            Reason::PERMISSION_DENIED,
            Reason::NO_CAPTURE_BACKEND,
            Reason::IDLE_TIMEOUT,
        ] {
            assert!(r.is_failure(), "{r:?} reads as an ending");
        }
    }

    /// A code this build has never heard of must still be classified, because
    /// a newer peer will send one and "unknown" is not the same as "fine".
    #[test]
    fn an_unknown_code_is_classified_without_being_known() {
        let future = Reason(999);
        assert_eq!(future.class(), Class::Unknown);
        assert!(
            future.is_failure(),
            "an unrecognised code must not be assumed harmless"
        );
        assert_eq!(future.describe(), None);

        // And an unrecognised code in the normal range is an ending, because
        // the class is what carries the meaning rather than the name.
        let future_normal = Reason(42);
        assert_eq!(future_normal.class(), Class::Normal);
        assert!(!future_normal.is_failure());
    }

    /// Zero is what a caller passes when it has nothing to say, and it must
    /// not read as a failure.
    #[test]
    fn zero_is_an_ending() {
        assert!(!Reason(0).is_failure());
    }
}
