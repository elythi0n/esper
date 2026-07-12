//! QUIC transport: endpoints, the TLS 1.3 handshake, and the connection tiers.

pub mod auth;
pub mod authorize;
pub mod backlog;
pub mod control;
pub mod crypto_suite;
pub mod endpoint;
pub mod pair;
pub mod parked;
pub mod session;
pub mod terminal;

pub use auth::{AnyOf, NoPeers, OnlyPeer, PeerAuthority};
pub use authorize::{
    GrantStoredPermissions, Policy, RefuseAll, SessionRegistry, Verdict, decide_and_clamp, response,
};
pub use backlog::{Backlog, DEFAULT_BACKLOG};
pub use control::{Control, ControlError, FrameReader};
pub use crypto_suite::{KX_GROUP, TLS_VERSIONS, provider};
pub use endpoint::{ALPN, Endpoint, IDLE_TIMEOUT, KEEPALIVE, TransportError};
pub use pair::{AnyKeyForPairingOnly, PairError, pairing_authority};
pub use parked::{Limits, LiveTerminal, Parked, ParkedSessions, Resumed};
pub use session::{HELLO_TIMEOUT, Session, SessionError, Stats};
pub use terminal::{Ended, EscapeWatcher, TerminalError, TerminalPolicy, TerminalService, Typed};
