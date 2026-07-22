//! QUIC transport: endpoints, the TLS 1.3 handshake, and the connection tiers.

pub mod auth;
pub mod authorize;
pub mod backlog;
pub mod control;
pub mod crypto_suite;
pub mod endpoint;
pub mod forward;
pub mod pair;
pub mod parked;
pub mod session;
pub mod terminal;

pub use auth::{AnyOf, NoPeers, OnlyPeer, PeerAuthority};
pub use authorize::{
    Answered, AuthorizeError, GrantStoredPermissions, Policy, REQUEST_TIMEOUT, RefuseAll,
    SessionRegistry, Verdict, answer_session, answer_session_within, decide_and_clamp,
    request_session, response,
};
pub use backlog::{Backlog, DEFAULT_BACKLOG};
pub use control::{Control, ControlError, FrameReader};
pub use crypto_suite::{KX_GROUP, TLS_VERSIONS, provider};
pub use endpoint::{ALPN, DIAL_TIMEOUT, Endpoint, IDLE_TIMEOUT, KEEPALIVE, TransportError};
pub use forward::{Allowlist, Decision, ForwardError, Forwarded, Slot, Slots};
pub use pair::{AnyKeyForPairingOnly, PairError, pairing_authority};
pub use parked::{Limits, LiveTerminal, Parked, ParkedSessions, Resumed};
pub use session::{ControlAcceptor, HELLO_TIMEOUT, MediaChannel, Session, SessionError, Stats};
pub use terminal::{
    Ended, EscapeWatcher, Grants, TerminalError, TerminalPolicy, TerminalService, Typed,
    send_resize,
};
