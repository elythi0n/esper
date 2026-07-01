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

    /// The class this code belongs to, derived from its hundreds digit.
    #[must_use]
    pub const fn class(self) -> Class {
        match self.0 / 100 {
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

/// The class of a reason code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
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
