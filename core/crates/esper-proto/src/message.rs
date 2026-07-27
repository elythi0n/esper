//! Control messages.
//!
//! Field names are stable: they may be added, never renamed, never repurposed.
//! Unknown fields are ignored on decode so a newer peer can talk to an older
//! one without a flag day — that is why nothing here is `deny_unknown_fields`.

use serde::{Deserialize, Serialize};

use crate::codes::Reason;

/// Which side of a session a peer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The machine being viewed and controlled.
    Host,
    /// The machine doing the viewing and controlling.
    Controller,
}

/// Length of an Ed25519 public key.
pub const DEVICE_KEY_LEN: usize = 32;

/// Longest accepted peer-supplied display name, in bytes.
///
/// Peer-supplied and therefore untrusted: it is bounded here so a hostile name
/// cannot become a memory or layout problem, and it is never interpreted as
/// markup, a path, or a shell word.
pub const MAX_NAME_LEN: usize = 64;

/// The first control message in both directions.
///
/// Feature evolution happens through [`Hello::caps`], which is intersected —
/// never through a version comparison in feature code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Wire protocol version. Must match exactly.
    pub proto: u16,
    /// Engine version. Informational; never used for feature decisions.
    pub engine: String,
    /// Ed25519 public key identifying this device.
    #[serde(with = "serde_bytes_array")]
    pub device: [u8; DEVICE_KEY_LEN],
    /// Human label. Peer-supplied and untrusted.
    pub name: String,
    /// Which side this peer is.
    pub role: Role,
    /// Capability tokens, e.g. `"clipboard"`, `"files"`, `"audio"`.
    pub caps: Vec<String>,
}

impl Hello {
    /// Build a `Hello` for this build, truncating an over-long name to
    /// [`MAX_NAME_LEN`] on a character boundary.
    #[must_use]
    pub fn new(role: Role, device: [u8; DEVICE_KEY_LEN], name: &str) -> Self {
        Self {
            proto: crate::PROTOCOL_VERSION,
            engine: env!("CARGO_PKG_VERSION").to_owned(),
            device,
            name: truncate_on_char_boundary(name, MAX_NAME_LEN).to_owned(),
            role,
            caps: Vec::new(),
        }
    }

    /// Whether this peer's protocol version matches ours.
    #[must_use]
    pub const fn version_matches(&self) -> bool {
        self.proto == crate::PROTOCOL_VERSION
    }

    /// Capabilities both peers advertise, sorted and deduplicated.
    ///
    /// Set intersection is the only sanctioned way to decide what a session
    /// supports. Comparing versions in feature code is how a protocol rots.
    #[must_use]
    pub fn common_caps(&self, other: &Self) -> Vec<String> {
        let theirs: std::collections::BTreeSet<&str> =
            other.caps.iter().map(String::as_str).collect();
        let mut common: Vec<String> = self
            .caps
            .iter()
            .filter(|c| theirs.contains(c.as_str()))
            .cloned()
            .collect();
        common.sort();
        common.dedup();
        common
    }

    /// Reject a name that is over-long or contains control characters.
    ///
    /// Applied to a *received* `Hello`: the sender is not trusted to have used
    /// [`Hello::new`].
    #[must_use]
    pub fn name_is_acceptable(&self) -> bool {
        self.name.len() <= MAX_NAME_LEN && !self.name.chars().any(char::is_control)
    }
}

/// Truncate to at most `max` bytes without splitting a UTF-8 character.
fn truncate_on_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("")
}

/// What a paired device is allowed to do.
///
/// Each is granted separately, and each direction of a two-way capability is
/// its own permission: a device permitted to *read* this machine's clipboard is
/// not thereby permitted to *write* to it. Two-way capabilities that are one
/// permission are how a feature nobody asked for gets enabled by a feature
/// somebody did.
///
/// This lives in the protocol crate because it is both halves of the same
/// thing: the wire vocabulary a controller uses to request access, and the
/// policy vocabulary a host uses to grant it. Defining it twice would let the
/// two drift, and a permission that means one thing on the wire and another in
/// the store is a security bug waiting for a rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// See the screen.
    View,
    /// Inject keyboard and mouse.
    Control,
    /// Read this machine's clipboard.
    ClipboardOut,
    /// Write to this machine's clipboard.
    ClipboardIn,
    /// Receive files from this machine.
    FilesOut,
    /// Send files to this machine.
    FilesIn,
    /// Hear this machine's audio.
    Audio,
    /// Open a terminal session.
    Terminal,
    /// Forward a TCP connection to a target on the host.
    ///
    /// Granted to nobody by default, and separate from [`Permission::Terminal`]
    /// on purpose: a shell already reaches everything a forward could, but a
    /// forward can be given *without* a shell, and that is the point — a
    /// database client on the laptop and no login on the server.
    Forward,
    /// Connect with nobody present to approve.
    Unattended,
    /// Record the session locally.
    Record,
}

impl Permission {
    /// Every permission there is, in the order a listing should show them.
    ///
    /// Exists so help text, listings and a name parser cannot fall behind the
    /// enum: adding a variant without adding it here is a compile error at the
    /// exhaustive match in [`Permission::name`] and a failing test here.
    pub const ALL: [Self; 11] = [
        Self::View,
        Self::Control,
        Self::ClipboardOut,
        Self::ClipboardIn,
        Self::FilesOut,
        Self::FilesIn,
        Self::Audio,
        Self::Terminal,
        Self::Forward,
        Self::Unattended,
        Self::Record,
    ];

    /// The name this permission goes by.
    ///
    /// **The same string the wire uses**, deliberately: the serde
    /// representation is `snake_case`, and a permission that is `terminal` on
    /// the wire and `Terminal` in a log is one rename away from a support
    /// conversation about two different things.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Control => "control",
            Self::ClipboardOut => "clipboard_out",
            Self::ClipboardIn => "clipboard_in",
            Self::FilesOut => "files_out",
            Self::FilesIn => "files_in",
            Self::Audio => "audio",
            Self::Terminal => "terminal",
            Self::Forward => "forward",
            Self::Unattended => "unattended",
            Self::Record => "record",
        }
    }

    /// Read a permission from a name a person typed.
    ///
    /// Accepts a hyphen where the name has an underscore, because a person
    /// typing on a command line will use whichever they are used to and being
    /// told "unknown permission clipboard-out" is a poor answer to a request
    /// that was perfectly clear.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let wanted = name.trim().to_ascii_lowercase().replace('-', "_");
        Self::ALL.into_iter().find(|p| p.name() == wanted)
    }

    /// Permissions a freshly paired device receives.
    ///
    /// View and control, prompt on connect, nothing else — and emphatically not
    /// [`Permission::Unattended`], which is the dangerous one and is granted
    /// only by a human at the host.
    #[must_use]
    pub fn defaults() -> Vec<Self> {
        vec![Self::View, Self::Control]
    }

    /// Whether granting this permission should require explicit confirmation at
    /// the host rather than a checkbox.
    #[must_use]
    pub const fn is_sensitive(self) -> bool {
        matches!(self, Self::Unattended | Self::FilesIn | Self::Record)
    }
}

/// A SPAKE2 message, sent once in each direction during pairing.
///
/// Carries the sender's public key alongside the PAKE message. The key is a
/// *claim* at this point — pairing is what establishes whether to believe it,
/// and the confirmation MAC covers it so it cannot be swapped afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairSpake {
    /// The SPAKE2 message.
    #[serde(with = "serde_bytes")]
    pub spake: Vec<u8>,
    /// The sender's Ed25519 public key.
    #[serde(with = "serde_bytes_array")]
    pub device: [u8; DEVICE_KEY_LEN],
}

/// Longest SPAKE2 message accepted.
///
/// An Ed25519-group SPAKE2 message is 33 bytes. The cap is generous enough for
/// a future group and small enough that a peer cannot make us allocate: this is
/// pairing, so the peer is unauthenticated by definition.
pub const MAX_SPAKE_LEN: usize = 256;

impl PairSpake {
    /// Whether this message is within its bounds.
    ///
    /// Checked before the message reaches any cryptography. A peer at this
    /// stage has proven nothing at all.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.spake.is_empty() && self.spake.len() <= MAX_SPAKE_LEN
    }
}

/// A key confirmation MAC, proving the sender derived the same SPAKE2 key.
///
/// Without this step a wrong code is indistinguishable from success: SPAKE2
/// returns a key either way. See the pairing module in `esper-crypto`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairConfirm {
    /// HMAC-SHA-256 over the pairing transcript.
    #[serde(with = "serde_bytes_array")]
    pub mac: [u8; 32],
}

/// A controller asking for a shell.
///
/// Terminal mode is not a lesser version of screen sharing. For a headless
/// server it is the *correct* mode: no capture, no codec, no display anywhere
/// in the path, and a few kilobytes per second.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalOpen {
    /// Initial window size, so the shell starts at the right width rather than
    /// redrawing once the first resize arrives.
    pub cols: u16,
    pub rows: u16,
    /// The controller's `TERM`, passed through **as the controller reported it**.
    ///
    /// Not rewritten and not upgraded. Claiming a terminal type the controller
    /// cannot actually render produces a shell that emits escape sequences the
    /// user sees as garbage — so a `TERM` we do not recognise is passed along
    /// and left to the far side's terminfo, which knows more about it than we
    /// do.
    pub term: String,
}

/// Largest accepted `TERM` value.
///
/// `TERM` reaches a child process's environment, so it is bounded and screened
/// before it gets there.
pub const MAX_TERM_LEN: usize = 64;

/// Largest window dimension accepted.
///
/// Generous — a very wide terminal is legitimate — but bounded, because the
/// value reaches a `TIOCSWINSZ` ioctl and a shell's own line-wrapping
/// arithmetic.
pub const MAX_WINDOW: u16 = 10_000;

impl TerminalOpen {
    /// Whether this request is within its bounds and safe to act on.
    ///
    /// `TERM` is checked against a conservative character set rather than just
    /// a length: it becomes an environment variable in a child process, and a
    /// value containing a separator or a control character is not a terminal
    /// name, it is an attempt.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.cols > 0
            && self.rows > 0
            && self.cols <= MAX_WINDOW
            && self.rows <= MAX_WINDOW
            && !self.term.is_empty()
            && self.term.len() <= MAX_TERM_LEN
            && self
                .term
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    }
}

/// Bytes in either direction on a terminal session.
///
/// Deliberately opaque. **These bytes are never logged.** What a person types
/// into a shell includes passwords, and a remote desktop that records its own
/// terminal traffic is a keylogger with a licence. The audit log records that a
/// terminal session happened, never what went through it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalData {
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}

/// Largest terminal payload in one message.
///
/// 64 KiB is well above a screenful and well below the frame cap, so a `cat` of
/// a large file is chunked rather than rejected, and a peer cannot make us
/// buffer a megabyte per message.
pub const MAX_TERMINAL_CHUNK: usize = 64 * 1024;

impl TerminalData {
    /// Whether this chunk is within its bounds.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.bytes.is_empty() && self.bytes.len() <= MAX_TERMINAL_CHUNK
    }
}

/// The controller's window changed size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalResize {
    pub cols: u16,
    pub rows: u16,
}

impl TerminalResize {
    /// Whether this resize is within bounds.
    #[must_use]
    pub const fn is_well_formed(&self) -> bool {
        self.cols > 0 && self.rows > 0 && self.cols <= MAX_WINDOW && self.rows <= MAX_WINDOW
    }
}

/// The shell ended.
///
/// Carries the real exit status, because a controller that always reports 0 is
/// useless in a script — and terminal mode's whole point is being usable on a
/// server, where things are scripted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalExit {
    /// Exit code, if the process exited normally.
    pub code: Option<i32>,
    /// Signal number, if it was killed.
    pub signal: Option<i32>,
}

impl TerminalExit {
    /// The exit code a shell would report for this outcome.
    ///
    /// Follows the shell convention of `128 + signal` for a killed process, so
    /// a script on the controller sees what it would have seen running locally.
    #[must_use]
    pub fn shell_code(&self) -> i32 {
        match (self.code, self.signal) {
            (Some(c), _) => c,
            (None, Some(s)) => 128i32.saturating_add(s),
            (None, None) => 1,
        }
    }
}

/// Anything that can travel on a terminal session, in either direction.
///
/// One tagged enum rather than separate streams: a resize that overtook the
/// output it applies to would make the shell wrap at the wrong width, and a
/// single ordered stream cannot reorder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalMessage {
    /// Bytes to or from the shell.
    Data(TerminalData),
    /// The window changed size. Controller to host only.
    Resize(TerminalResize),
    /// The shell ended. Host to controller only.
    Exit(TerminalExit),
}

/// A controller asking the host to dial one TCP target.
///
/// The first frame on a forwarded stream and the **last framed thing on it**:
/// once the host replies [`ForwardReply::Opened`], the stream carries the
/// connection's own bytes with nothing of ours around them. A tunnel that
/// re-framed the payload would be inventing a second flow-control scheme on
/// top of the one QUIC already gives each stream, and getting it wrong is how
/// a slow reader stalls a whole session instead of its own connection.
///
/// `host` is what the *controller* typed. It is matched against the host's
/// allowlist as written, before anything is resolved or dialled, so a name is
/// never even looked up unless the host's owner named it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardOpen {
    /// The target host, as a name or an IP literal. No brackets: `::1`, not
    /// `[::1]`.
    pub host: String,
    /// The target port.
    pub port: u16,
}

/// Longest accepted target host.
///
/// A DNS name cannot be longer, and the value reaches a resolver.
pub const MAX_FORWARD_HOST_LEN: usize = 255;

impl ForwardOpen {
    /// Whether this request is within its bounds and safe to act on.
    ///
    /// The character set is deliberately narrow — a host is a name or an
    /// address, and anything with a slash, a space, a `@` or a control
    /// character in it is not either of those. Screened here so the string
    /// that reaches a resolver has already been established to be a host.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.port != 0
            && !self.host.is_empty()
            && self.host.len() <= MAX_FORWARD_HOST_LEN
            && self
                .host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
    }

    /// The target as a person would write it, for a log line or a refusal.
    ///
    /// Bracketed when it is an IPv6 literal, because `::1:5432` is unreadable
    /// and `[::1]:5432` is what everything else prints.
    #[must_use]
    pub fn target(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Why a forward could not be opened.
///
/// Note what these do *not* distinguish: there is no separate code for "no
/// such host" against "nothing listening". Both are [`ForwardRefusal::DialFailed`],
/// because the difference is only ever information about the host's network,
/// and the controller's next move is the same either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardRefusal {
    /// The device does not hold [`Permission::Forward`].
    NotPermitted,
    /// The target is not on this host's allowlist.
    ///
    /// **Sent without dialling anything.** The refusal is the whole point of
    /// the allowlist: a forward grant must not be usable to find out what is
    /// listening on addresses its owner never named.
    NotAllowed,
    /// The target was allowed, and the host could not connect to it.
    ///
    /// This does tell a permitted peer whether an allowlisted port is
    /// listening. Accepted deliberately, and bounded by the allowlist: the
    /// reach of the probe is exactly the list the host's owner wrote down.
    DialFailed,
    /// The session already has as many forwarded connections as it may.
    TooMany,
    /// The request was outside its bounds.
    Malformed,
}

impl ForwardRefusal {
    /// A sentence for a user, not a code for a machine.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NotPermitted => "this device is not permitted to forward ports to this machine",
            Self::NotAllowed => "that target is not on the host's forwarding allowlist",
            Self::DialFailed => "the host could not connect to that target",
            Self::TooMany => "this session is already forwarding as many connections as it may",
            Self::Malformed => "the forward request was malformed",
        }
    }
}

/// The host's answer to a [`ForwardOpen`].
///
/// Tagged, and sent before any payload byte, so the controller never has to
/// infer from the first bytes whether it has a connection — and a refusal
/// arrives as a refusal rather than as a stream that closes.
///
/// [`ForwardReply::Opened`] carries nothing. There is nothing honest to put in
/// it: the controller knows what it asked for, and what the host resolved the
/// name to is the host's business.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardReply {
    /// Connected. Everything after this frame is the connection.
    Opened,
    /// Refused, with a reason.
    Refused(ForwardRefusal),
}

/// Which kind of session a controller is asking for.
///
/// **The request says which, rather than the host inferring it.** Before this
/// existed a host guessed from the shape of the permission set — a device with
/// `terminal` and not `view` must want a shell — which meant a device holding
/// both could never open one, and a third session type had nothing left to be
/// inferred from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// The screen, with input if it was granted.
    #[default]
    Screen,
    /// A shell.
    Terminal,
    /// TCP connections forwarded to a target on the host.
    Forward,
}

impl SessionKind {
    /// The permission a session of this kind cannot run without.
    ///
    /// One place, so the check the host makes and the request the controller
    /// sends cannot disagree about what a kind requires.
    #[must_use]
    pub const fn requires(self) -> Permission {
        match self {
            Self::Screen => Permission::View,
            Self::Terminal => Permission::Terminal,
            Self::Forward => Permission::Forward,
        }
    }

    /// The word for it, for a log line or a prompt.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Terminal => "terminal",
            Self::Forward => "forward",
        }
    }
}

/// A controller asking to open a session.
///
/// Requests are a *maximum*, never a demand. The host grants some subset — see
/// [`SessionGrant`] — and the controller renders what it was given rather than
/// what it asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRequest {
    /// What the controller means to do with the session.
    ///
    /// Defaulted rather than required, so a peer built before session kinds
    /// existed asks for a screen — which is what it meant.
    #[serde(default)]
    pub kind: SessionKind,
    /// The permissions the controller would like.
    pub want: Vec<Permission>,
    /// Video codecs the controller can decode, in preference order.
    pub codecs: Vec<String>,
    /// A short human reason, shown on the host's prompt. Peer-supplied and
    /// untrusted: bounded, and never interpreted as markup.
    pub reason: String,
}

/// Longest accepted session reason, in bytes.
pub const MAX_REASON_LEN: usize = 120;

/// Most permissions a single request may name.
///
/// The set is small and fixed, so a request naming more than all of them twice
/// over is either broken or probing. Bounded because a request arrives from an
/// authenticated peer but an authenticated peer is not a trusted one.
pub const MAX_REQUESTED_PERMISSIONS: usize = 32;

impl SessionRequest {
    /// Whether this request is within its bounds.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.reason.len() <= MAX_REASON_LEN
            && !self.reason.chars().any(char::is_control)
            && self.want.len() <= MAX_REQUESTED_PERMISSIONS
            && self.codecs.len() <= 16
    }
}

/// The host opening a session, with the permissions it actually granted.
///
/// `granted` may be smaller than what was requested, and frequently is. **The
/// controller must render what it holds, never what it asked for**: a client
/// that assumes its request was honoured will offer a control cursor on a
/// view-only session and confuse the person using it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGrant {
    /// An opaque identifier for this session, for logs and the audit trail.
    #[serde(with = "serde_bytes_array")]
    pub session_id: [u8; 16],
    /// What the controller may actually do.
    pub granted: Vec<Permission>,
}

impl SessionGrant {
    /// Whether a permission was granted.
    #[must_use]
    pub fn allows(&self, p: Permission) -> bool {
        self.granted.contains(&p)
    }
}

/// The host refusing a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDeny {
    /// Why, as a code the controller can branch on.
    pub reason: Reason,
    /// Why, as a sentence a person can act on.
    pub detail: String,
}

/// The host narrowing or widening a live session.
///
/// Sent whenever policy changes mid-session — a human revoking control, a
/// schedule ending. The controller must apply it immediately; there is no
/// negotiation and no acknowledgement, because a permission the host has
/// withdrawn is withdrawn whether or not the controller replies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionChange {
    /// The complete new set, not a delta.
    ///
    /// A delta invites a controller and host that disagree about the current
    /// state to drift further apart with every message. The full set cannot
    /// drift.
    pub granted: Vec<Permission>,
}

/// The host's response to a session request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionResponse {
    /// Granted, with the permissions actually given.
    Grant(SessionGrant),
    /// Refused, with a reason.
    Deny(SessionDeny),
}

/// The host's reply to the controller's opening message.
///
/// Tagged, because the host has two honest answers and the controller must be
/// able to read either. It can continue — supplying its own SPAKE2 message — or
/// refuse before the exchange begins, which is what an expired or exhausted
/// code produces.
///
/// Without the tag the controller would be waiting for a [`PairSpake`] while
/// the host sent a [`PairResult`], and the mismatch would surface as a decode
/// failure rather than the reason the host actually gave.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairResponse {
    /// Continuing. Here is the host's SPAKE2 message and public key.
    Spake(PairSpake),
    /// Refusing before the exchange starts — an expired or exhausted code.
    Refused(PairResult),
}

/// The outcome the host reports, so the controller can tell a typo from a dead
/// code without guessing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairResult {
    /// The host's confirmation verified, and here is the host's own MAC.
    ///
    /// **The MAC is the point, not the variant.** A man in the middle can send
    /// `Ok` as easily as the host can; only a MAC over a transcript covering
    /// this connection's channel binding proves the sender knew the code. The
    /// controller must verify it before storing anything.
    Ok {
        #[serde(with = "serde_bytes_array")]
        mac: [u8; 32],
    },
    /// The confirmation did not verify — a wrong code, or interference.
    /// Deliberately does not distinguish, because nothing can.
    Failed { attempts_remaining: u8 },
    /// The code's sixty seconds elapsed.
    Expired,
    /// The attempt budget is spent; the host must show a new code.
    Exhausted,
}

/// CBOR-encodes a fixed-size byte array as a byte string rather than an array
/// of integers. Without this a 32-byte key costs ~33 bytes instead of ~34 as an
/// array of 32 separately-tagged integers, and — more importantly — the Go side
/// would have to agree on which representation was meant.
/// Length of a terminal session identifier.
///
/// 16 bytes. The identifier is not a capability — resume is authorised by
/// device key — but an enumerable identifier would let one paired device learn
/// that another has a live session, and 128 random bits costs nothing.
pub const SESSION_ID_LEN: usize = 16;

/// Identifies a terminal session that can be resumed after a dropped link.
///
/// Opaque, compared whole, and printed as hex for logs. **Holding one grants
/// nothing:** a session is looked up by identifier *and* owning device key, so
/// a leaked identifier is not a way in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionId(#[serde(with = "serde_bytes_array")] [u8; SESSION_ID_LEN]);

impl SessionId {
    /// Wrap raw bytes.
    #[must_use]
    pub const fn from_bytes(b: [u8; SESSION_ID_LEN]) -> Self {
        Self(b)
    }

    /// The raw bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SESSION_ID_LEN] {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for SessionId {
    /// Same as `Display`.
    ///
    /// The derived form prints sixteen decimal numbers inside a tuple struct,
    /// which is unreadable in a log line and impossible to match against
    /// another log line by eye.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SessionId({self})")
    }
}

/// Return to a terminal session that is still running on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalResume {
    /// Which session. Meaningless without the asking device's key.
    pub session: SessionId,
    /// The window size *now*, which may differ from when the link dropped —
    /// the user may have resized, or be on a different machine entirely.
    pub cols: u16,
    pub rows: u16,
}

impl TerminalResume {
    /// Whether this request is within its bounds.
    #[must_use]
    pub const fn is_well_formed(&self) -> bool {
        self.cols > 0 && self.rows > 0 && self.cols <= MAX_WINDOW && self.rows <= MAX_WINDOW
    }
}

/// How a controller asks for a terminal: a new one, or one it left behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalStart {
    /// Start a fresh shell.
    Open(TerminalOpen),
    /// Reattach to a shell still running from an earlier connection.
    Resume(TerminalResume),
}

impl TerminalStart {
    /// Whether this request is within its bounds.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        match self {
            Self::Open(o) => o.is_well_formed(),
            Self::Resume(r) => r.is_well_formed(),
        }
    }
}

/// Why a terminal could not be started.
///
/// Note what is *not* here: there is no separate code for "that session
/// belongs to another device" or "that session expired". Both are
/// [`Refusal::Unknown`], because distinguishing them would confirm to the
/// asking device that some other device has a live session — a fact it has no
/// business learning. A resume finds a session only when the identifier and
/// the asking device's key both match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    /// The device does not hold [`Permission::Terminal`].
    NotPermitted,
    /// No resumable session for that identifier and this device.
    Unknown,
    /// The request was outside its bounds.
    Malformed,
    /// The host is already holding as many parked sessions as it will.
    TooMany,
    /// The host has no pseudo-terminal support at all.
    ///
    /// Permanent, and separate from [`Refusal::Unavailable`] for that reason:
    /// one says "not on this machine, ever" and the other says "not right
    /// now". Retrying is right for exactly one of them, and a controller that
    /// cannot tell them apart either retries forever or gives up too early.
    Unsupported,
    /// The host could not start a shell.
    ///
    /// Something on the host ran out or failed — no free pseudo-terminal, no
    /// shell where one was expected. **A host that could not start a shell
    /// says so**: the alternative is a control stream that simply closes,
    /// which leaves the controller to guess between a refusal, a crash and a
    /// network fault.
    Unavailable,
}

impl Refusal {
    /// A sentence for a user, not a code for a machine.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NotPermitted => "this device is not permitted to open a terminal on this machine",
            Self::Unknown => "that terminal session is no longer available",
            Self::Malformed => "the terminal request was malformed",
            Self::TooMany => "the host is holding too many disconnected terminal sessions",
            Self::Unsupported => "this machine cannot open a terminal",
            Self::Unavailable => "the host could not start a shell",
        }
    }
}

/// The host's answer to a [`TerminalStart`].
///
/// Sent before any terminal bytes, so a controller always knows whether it has
/// a session and which one, and never has to infer it from the first output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalStarted {
    /// A new shell is running.
    Opened { session: SessionId },
    /// Reattached to the shell that was already running.
    Resumed {
        session: SessionId,
        /// Bytes of output dropped from the backlog while nobody was attached.
        ///
        /// Non-zero means there is a hole in what follows. The user is told
        /// inline as well, at the point of the loss, because a hole in a
        /// terminal transcript that is only reported in a status field is a
        /// hole nobody notices.
        lost: u64,
    },
    /// No session. The reason is deliberately coarse.
    Refused(Refusal),
}

mod serde_bytes_array {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer, const N: usize>(v: &[u8; N], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(v)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        d: D,
    ) -> Result<[u8; N], D::Error> {
        let v = serde_bytes::ByteBuf::deserialize(d)?;
        <[u8; N]>::try_from(v.as_ref()).map_err(|_| {
            serde::de::Error::invalid_length(v.len(), &"a byte string of the expected length")
        })
    }
}

// ── Media ────────────────────────────────────────────────────────────────────
//
// Negotiation for the video stream. These travel on the control stream; the
// pixels travel as datagrams and are RTP, not CBOR.
//
// Engine to engine, unlike everything above. The Go control plane supervises
// the engine and does not negotiate media, so there are deliberately no
// cross-language fixtures for these — a fixture nobody on the other side reads
// is a test of one implementation pretending to be a test of two.

/// Most monitors a host will describe.
///
/// Sixteen is far past any real desk and far below the point where a
/// controller's monitor list becomes an allocation a peer chooses the size of.
pub const MAX_MONITORS: usize = 16;

/// Largest picture edge this protocol will carry.
///
/// 8192 is two generations past 4K on the long edge. It exists so a peer
/// cannot announce a stream whose buffers would be gigabytes before anything
/// checks — the decoder enforces its own limit as well, and both being present
/// is deliberate.
pub const MAX_PICTURE_EDGE: u32 = 8192;

/// Highest framerate ceiling worth accepting.
pub const MAX_FPS: u16 = 240;

/// Which video codec a stream carries.
///
/// An enum with one variant today. It is here so a peer that only speaks
/// H.264 refuses an AV1 stream by *name* rather than by failing to decode it,
/// which is the difference between a clear message and a black window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    /// H.264, baseline or main. The floor: hardware everywhere, and the only
    /// codec a browser can be relied on to decode.
    H264,
}

impl Codec {
    /// The RTP payload type this codec is offered as.
    #[must_use]
    pub const fn payload_type(self) -> u8 {
        match self {
            Self::H264 => 96,
        }
    }

    /// A name for logs and the session HUD.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
        }
    }
}

/// One monitor, as the host describes it.
///
/// Geometry is included because a controller showing two monitors has to lay
/// them out the way the host does, and because pointer coordinates are
/// normalised per monitor — a controller that guessed the arrangement would
/// put the cursor in the wrong place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorDescription {
    /// The host's id for it. Opaque: a controller echoes it back in
    /// [`MonitorSelect`] and never interprets it.
    pub id: u32,
    /// What the host calls it, for showing a person.
    pub name: String,
    /// Position in the host's desktop coordinates. Can be negative — a monitor
    /// left of the primary one starts at a negative x, and pretending
    /// otherwise is how multi-monitor bugs start.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Backing-store scale in percent, so it stays an integer on the wire.
    ///
    /// A float would round differently in two languages and make a fixture
    /// non-reproducible. 100 is a normal display, 200 a typical HiDPI one, and
    /// 150 a fractional Wayland one.
    pub scale_percent: u16,
    /// Whether the host calls this its primary display.
    pub primary: bool,
}

impl MonitorDescription {
    /// The narrowest and widest scale worth believing.
    const SCALE: std::ops::RangeInclusive<u16> = 25..=800;

    /// Whether this description is within bounds.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.name.is_empty()
            && self.name.len() <= MAX_NAME_LEN
            && self.width > 0
            && self.height > 0
            && self.width <= MAX_PICTURE_EDGE
            && self.height <= MAX_PICTURE_EDGE
            && Self::SCALE.contains(&self.scale_percent)
    }
}

/// What the host is going to send, sent once the session is granted.
///
/// **Not a negotiation in the sense of a handshake.** The host states what it
/// will do; a controller that cannot cope closes with a reason. Two rounds of
/// offer and counter-offer would add a round trip to session setup to settle
/// something the host decides anyway — it owns the screen, the encoder and the
/// CPU budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamConfig {
    /// Every monitor the host can send.
    pub monitors: Vec<MonitorDescription>,
    /// Which one it is sending now. One of `monitors`' ids.
    pub selected: u32,
    pub codec: Codec,
    /// The size of picture that will arrive.
    ///
    /// **Not the monitor's size.** A resolution ceiling and the CPU budget's
    /// ladder both reduce it, so a controller sizing a window from the
    /// monitor's dimensions would letterbox every session on a large display.
    pub width: u32,
    pub height: u32,
    /// The most frames a second the host will send. A ceiling, not a promise:
    /// a static screen sends almost none.
    pub max_fps: u16,
    /// The RTP timestamp clock, in Hz. 90 000 for video, and stated rather
    /// than assumed so a receiver never has to guess what a timestamp means.
    pub clock_hz: u32,
    /// The RTP synchronisation source for this stream, which is how a
    /// datagram is matched to a monitor.
    pub ssrc: u32,
}

impl StreamConfig {
    /// Whether this configuration is within bounds and self-consistent.
    ///
    /// Self-consistency is the part worth having: a config whose `selected`
    /// names no monitor it listed is not a config a well-behaved host sends,
    /// and a controller acting on it would ask for pictures of nothing.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.monitors.is_empty()
            && self.monitors.len() <= MAX_MONITORS
            && self.monitors.iter().all(MonitorDescription::is_well_formed)
            && self.monitors.iter().any(|m| m.id == self.selected)
            && self.width > 0
            && self.height > 0
            && self.width <= MAX_PICTURE_EDGE
            && self.height <= MAX_PICTURE_EDGE
            && self.max_fps > 0
            && self.max_fps <= MAX_FPS
            && self.clock_hz > 0
            && self.no_duplicate_monitor_ids()
    }

    /// Ids have to be unique, or `selected` is ambiguous and a controller
    /// cannot tell two monitors apart.
    fn no_duplicate_monitor_ids(&self) -> bool {
        let mut seen: Vec<u32> = self.monitors.iter().map(|m| m.id).collect();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        seen.len() == before
    }

    /// The monitor being sent, if the config names one it listed.
    #[must_use]
    pub fn selected_monitor(&self) -> Option<&MonitorDescription> {
        self.monitors.iter().find(|m| m.id == self.selected)
    }
}

/// A controller asking for a different monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorSelect {
    /// One of the ids from [`StreamConfig::monitors`].
    pub monitor: u32,
}

/// What a controller would prefer, if the host has the headroom.
///
/// **A hint, never a command.** The host owns its CPU budget: a controller
/// asking for 60 fps on a machine that can afford 15 gets 15, and a protocol
/// that let the controller demand otherwise would let a viewer make somebody
/// else's laptop unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityHint {
    /// A framerate the controller would be happy with. `None` means no
    /// preference — usually because the viewer is not visible.
    pub max_fps: Option<u16>,
    /// A long edge the controller would be happy with, in pixels. A small
    /// window does not need a 4K stream.
    pub max_long_edge: Option<u32>,
    /// What the controller is doing, which is the only thing it knows that the
    /// host does not.
    pub profile: Profile,
    /// How sharp a picture the controller is willing to settle for.
    ///
    /// **The middle rung of the degradation order, and for a long time the
    /// only one a controller could not reach.** It could ask for fewer frames
    /// and for smaller ones; it could not ask for cheaper ones. So a session
    /// short of bandwidth skipped from framerate straight to resolution — and
    /// resolution is the most damaging lever there is for the thing this
    /// product is mostly used for, which is reading somebody's screen.
    ///
    /// A ceiling like the other two: `None` is no preference, and the host
    /// takes the coarser of this and what it would have chosen. Nothing here
    /// can ask a host to spend *more*.
    #[serde(default)]
    pub sharpness: Option<Sharpness>,
}

impl QualityHint {
    /// The smallest long edge worth asking for.
    const EDGE: std::ops::RangeInclusive<u32> = 160..=MAX_PICTURE_EDGE;

    /// Whether this hint is within bounds.
    ///
    /// A malformed hint is *ignored*, not an error: it asks for something and
    /// the host was never obliged to agree anyway.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.max_fps.is_none_or(|f| f > 0 && f <= MAX_FPS)
            && self.max_long_edge.is_none_or(|e| Self::EDGE.contains(&e))
    }
}

/// How coarsely a controller is willing to have its pictures quantised.
///
/// The protocol's own vocabulary, deliberately not the encoder's. What a
/// quantiser parameter range should be for a given answer is the encoder's
/// business and changes with the codec; what a viewer is willing to look at
/// does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sharpness {
    /// Text, diagrams, code. Every glyph edge intact.
    Sharp,
    /// The default.
    #[default]
    Balanced,
    /// Visibly softer, and much cheaper. What video can afford to be.
    Soft,
}

/// What the person at the controller is doing.
///
/// The host cannot work this out from the pixels: a slideshow and a video look
/// the same to a damage tracker until it is too late to have chosen
/// differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// Reading and typing. Low framerate, high quality — cheaper *and* better
    /// for the most common content.
    Text,
    /// The default, and what to pick when unsure.
    #[default]
    Balanced,
    /// Video or a game. Framerate before fidelity.
    Motion,
}

/// A controller asking for a picture it can start from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyframeRequest {
    /// Why, so a host can tell a controller that lost one packet from one that
    /// is asking every frame — the second is a link that needs its bitrate
    /// reduced, not more keyframes.
    pub reason: KeyframeReason,
}

/// Why a keyframe is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyframeReason {
    /// A frame was lost and the decoder cannot continue from what it has.
    LostSync,
    /// The controller has just started listening and has no reference picture.
    Joined,
    /// The stream's size changed, so the decoder was reset.
    Reconfigured,
    /// The viewer became visible again after being hidden.
    Revealed,
}

/// Anything that can travel on a media session's control stream.
///
/// One tagged enum, ordered, for the same reason the terminal's is: a monitor
/// selection that overtook the config describing the monitors would name an id
/// the controller has never heard of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaMessage {
    /// What the host will send. Host to controller.
    Config(StreamConfig),
    /// Send a different monitor. Controller to host.
    Select(MonitorSelect),
    /// What the controller would prefer. Controller to host.
    Hint(QualityHint),
    /// Send a picture I can start from. Controller to host.
    Keyframe(KeyframeRequest),
    /// What the pointer looks like and where it is. Host to controller.
    ///
    /// On this stream rather than one of its own: a cursor message is a few
    /// bytes and the only other traffic here is a `Config`, which is rare and
    /// small. Input got its own stream because a keystroke queued behind a
    /// monitor list would be a keystroke a person notices; a cursor position
    /// queued behind one is a position that is about to be superseded anyway.
    Cursor(crate::CursorMessage),
}

impl MediaMessage {
    /// Whether this message is within bounds.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        match self {
            Self::Config(c) => c.is_well_formed(),
            Self::Select(_) | Self::Keyframe(_) => true,
            Self::Hint(h) => h.is_well_formed(),
            Self::Cursor(c) => c.is_well_formed(),
        }
    }

    /// Whether the given role is allowed to send this.
    ///
    /// Direction is part of the protocol, not a convention. A controller
    /// sending a `Config` would be telling the host what its own screen is,
    /// and a host sending a `Keyframe` request would be asking the controller
    /// for a picture. Both are nonsense, and nonsense from a peer is worth
    /// refusing by name rather than acting on.
    #[must_use]
    pub const fn may_be_sent_by(&self, role: Role) -> bool {
        match self {
            Self::Config(_) | Self::Cursor(_) => matches!(role, Role::Host),
            Self::Select(_) | Self::Hint(_) | Self::Keyframe(_) => {
                matches!(role, Role::Controller)
            }
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn every_permission_is_in_all_and_parses_back_from_its_name() {
        // The list is what help text and the name parser iterate; a variant
        // missing from it would be a permission nobody could grant.
        assert_eq!(
            Permission::ALL.len(),
            11,
            "a permission was added to the enum and not to ALL"
        );
        for p in Permission::ALL {
            assert_eq!(Permission::parse(p.name()), Some(p), "{p:?}");
            // The name is the wire form, so it must round-trip through serde
            // as well. Two vocabularies that drift is the thing the doc on
            // this enum argues against.
            let cbor = {
                let mut buf = Vec::new();
                ciborium::into_writer(&p, &mut buf).expect("encode");
                buf
            };
            let back: String = ciborium::from_reader(&cbor[..]).expect("a permission is a string");
            assert_eq!(back, p.name(), "the wire name and the printed name differ");
        }
    }

    #[test]
    fn a_permission_name_is_read_the_way_a_person_types_it() {
        assert_eq!(
            Permission::parse("clipboard-out"),
            Some(Permission::ClipboardOut)
        );
        assert_eq!(
            Permission::parse("  Clipboard_Out "),
            Some(Permission::ClipboardOut)
        );
        assert_eq!(Permission::parse("nonsense"), None);
        assert_eq!(Permission::parse(""), None);
    }
    use super::*;

    fn hello(caps: &[&str]) -> Hello {
        let mut h = Hello::new(Role::Host, [1u8; 32], "host");
        h.caps = caps.iter().map(|s| (*s).to_owned()).collect();
        h
    }

    #[test]
    fn new_sets_the_current_protocol_version() {
        assert!(Hello::new(Role::Host, [0u8; 32], "x").version_matches());
    }

    #[test]
    fn version_mismatch_is_detected() {
        let mut h = Hello::new(Role::Controller, [0u8; 32], "x");
        h.proto = crate::PROTOCOL_VERSION + 1;
        assert!(!h.version_matches());
    }

    #[test]
    fn caps_intersect_and_are_sorted() {
        let a = hello(&["files", "audio", "clipboard"]);
        let b = hello(&["clipboard", "audio", "multimon"]);
        assert_eq!(a.common_caps(&b), vec!["audio", "clipboard"]);
    }

    #[test]
    fn caps_intersection_is_symmetric() {
        let a = hello(&["files", "audio"]);
        let b = hello(&["audio", "multimon"]);
        assert_eq!(a.common_caps(&b), b.common_caps(&a));
    }

    #[test]
    fn an_unknown_cap_is_simply_absent_from_the_intersection() {
        let a = hello(&["telepathy"]);
        let b = hello(&["audio"]);
        assert!(a.common_caps(&b).is_empty());
    }

    #[test]
    fn long_names_are_truncated_not_rejected_at_construction() {
        let h = Hello::new(Role::Host, [0u8; 32], &"a".repeat(500));
        assert_eq!(h.name.len(), MAX_NAME_LEN);
        assert!(h.name_is_acceptable());
    }

    #[test]
    fn truncation_never_splits_a_utf8_character() {
        // 'é' is two bytes, so 64 bytes lands mid-character on odd boundaries.
        for n in 1..80 {
            let h = Hello::new(Role::Host, [0u8; 32], &"é".repeat(n));
            assert!(h.name.len() <= MAX_NAME_LEN);
            assert!(std::str::from_utf8(h.name.as_bytes()).is_ok());
        }
    }

    #[test]
    fn a_received_name_with_control_characters_is_rejected() {
        let mut h = Hello::new(Role::Host, [0u8; 32], "ok");
        h.name = "evil\u{7}\nname".to_owned();
        assert!(!h.name_is_acceptable());
    }

    #[test]
    fn a_received_over_long_name_is_rejected() {
        let mut h = Hello::new(Role::Host, [0u8; 32], "ok");
        h.name = "a".repeat(MAX_NAME_LEN + 1);
        assert!(!h.name_is_acceptable());
    }

    #[test]
    fn device_key_survives_a_cbor_round_trip() {
        let key = [0xABu8; 32];
        let h = Hello::new(Role::Controller, key, "k");
        let mut buf = Vec::new();
        ciborium::into_writer(&h, &mut buf).expect("encode");
        let back: Hello = ciborium::from_reader(buf.as_slice()).expect("decode");
        assert_eq!(back.device, key);
    }

    #[test]
    fn unknown_fields_are_ignored_so_a_newer_peer_can_connect() {
        // A future peer sends a field this build has never heard of.
        let cbor = serde_json::json!({
            "proto": crate::PROTOCOL_VERSION,
            "engine": "9.9.9",
            "device": vec![0u8; 32],
            "name": "future",
            "role": "host",
            "caps": ["audio"],
            "quantum_tunnel": true,
        });
        let mut buf = Vec::new();
        ciborium::into_writer(&cbor, &mut buf).expect("encode");
        let back: Hello = ciborium::from_reader(buf.as_slice()).expect("must ignore unknown field");
        assert_eq!(back.name, "future");
    }
}

#[cfg(test)]
mod media_tests {
    use super::*;

    fn monitor(id: u32, name: &str) -> MonitorDescription {
        MonitorDescription {
            id,
            name: name.to_owned(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale_percent: 100,
            primary: id == 0,
        }
    }

    fn config() -> StreamConfig {
        StreamConfig {
            monitors: vec![monitor(0, "eDP-1"), monitor(1, "DP-3")],
            selected: 1,
            codec: Codec::H264,
            width: 1920,
            height: 1080,
            max_fps: 30,
            clock_hz: 90_000,
            ssrc: 0xDEAD_BEEF,
        }
    }

    /// Through the real framing, so this tests the wire and not just serde.
    fn roundtrip<T>(value: &T) -> T
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let mut bytes = Vec::new();
        crate::frame::write_frame(&mut bytes, value).expect("encodes");
        crate::frame::read_frame(&mut bytes.as_slice()).expect("decodes")
    }

    /// The bytes a message becomes on the wire.
    fn encoded(value: &MediaMessage) -> Vec<u8> {
        let mut bytes = Vec::new();
        crate::frame::write_frame(&mut bytes, value).expect("encodes");
        bytes
    }

    #[test]
    fn every_media_message_round_trips_through_the_framing() {
        for message in [
            MediaMessage::Config(config()),
            MediaMessage::Select(MonitorSelect { monitor: 1 }),
            MediaMessage::Hint(QualityHint {
                max_fps: Some(15),
                max_long_edge: Some(1280),
                profile: Profile::Text,
                sharpness: None,
            }),
            MediaMessage::Hint(QualityHint {
                max_fps: None,
                max_long_edge: None,
                profile: Profile::Balanced,
                sharpness: None,
            }),
            MediaMessage::Keyframe(KeyframeRequest {
                reason: KeyframeReason::LostSync,
            }),
        ] {
            assert_eq!(roundtrip(&message), message);
            assert!(message.is_well_formed(), "{message:?} is not well formed");
        }
    }

    /// A negative origin has to survive the wire. A monitor to the left of the
    /// primary one has one, and a field that silently became unsigned would put
    /// every pointer event on the wrong screen.
    #[test]
    fn a_negative_monitor_origin_survives() {
        let mut m = monitor(0, "left");
        m.x = -1920;
        m.y = -300;
        let got = roundtrip(&m);
        assert_eq!((got.x, got.y), (-1920, -300));
    }

    /// **The check worth having.** A config whose `selected` names no monitor
    /// it listed is not a config a well-behaved host sends, and a controller
    /// acting on it would ask for pictures of nothing.
    #[test]
    fn a_config_selecting_a_monitor_it_does_not_list_is_refused() {
        let mut c = config();
        c.selected = 99;
        assert!(!c.is_well_formed());
        assert!(c.selected_monitor().is_none());
        // And the well-formed one does name it.
        assert_eq!(
            config().selected_monitor().map(|m| m.name.as_str()),
            Some("DP-3")
        );
    }

    #[test]
    fn duplicate_monitor_ids_are_refused() {
        let mut c = config();
        c.monitors = vec![monitor(4, "a"), monitor(4, "b")];
        c.selected = 4;
        assert!(
            !c.is_well_formed(),
            "two monitors with one id make `selected` ambiguous"
        );
    }

    #[test]
    fn a_config_outside_its_bounds_is_refused() {
        for mutate in [
            (|c: &mut StreamConfig| c.monitors.clear()) as fn(&mut StreamConfig),
            |c| c.width = 0,
            |c| c.height = 0,
            |c| c.width = MAX_PICTURE_EDGE + 1,
            |c| c.height = MAX_PICTURE_EDGE + 1,
            |c| c.max_fps = 0,
            |c| c.max_fps = MAX_FPS + 1,
            |c| c.clock_hz = 0,
            |c| {
                c.monitors = (0..=u32::try_from(MAX_MONITORS).unwrap_or(16))
                    .map(|i| {
                        let mut m = monitor(i, "m");
                        m.id = i;
                        m
                    })
                    .collect()
            },
        ] {
            let mut c = config();
            mutate(&mut c);
            assert!(!c.is_well_formed(), "{c:?} was accepted");
        }
        // And the unmutated one is fine, or the test proves nothing.
        assert!(config().is_well_formed());
    }

    #[test]
    fn a_monitor_outside_its_bounds_is_refused() {
        for mutate in [
            (|m: &mut MonitorDescription| m.name.clear()) as fn(&mut MonitorDescription),
            |m| m.name = "x".repeat(MAX_NAME_LEN + 1),
            |m| m.width = 0,
            |m| m.height = 0,
            |m| m.width = MAX_PICTURE_EDGE + 1,
            |m| m.scale_percent = 0,
            |m| m.scale_percent = 24,
            |m| m.scale_percent = 801,
        ] {
            let mut m = monitor(0, "eDP-1");
            mutate(&mut m);
            assert!(!m.is_well_formed(), "{m:?} was accepted");
        }
        assert!(monitor(0, "eDP-1").is_well_formed());
        // The scale range's own edges are fine.
        for scale in [25u16, 100, 150, 200, 800] {
            let mut m = monitor(0, "eDP-1");
            m.scale_percent = scale;
            assert!(m.is_well_formed(), "scale {scale} was refused");
        }
    }

    #[test]
    fn a_hint_outside_its_bounds_is_refused_rather_than_acted_on() {
        let ok = QualityHint {
            max_fps: Some(30),
            max_long_edge: Some(1920),
            profile: Profile::Balanced,
            sharpness: None,
        };
        assert!(ok.is_well_formed());
        for bad in [
            QualityHint {
                max_fps: Some(0),
                ..ok
            },
            QualityHint {
                max_fps: Some(MAX_FPS + 1),
                ..ok
            },
            QualityHint {
                max_long_edge: Some(0),
                ..ok
            },
            QualityHint {
                max_long_edge: Some(159),
                ..ok
            },
            QualityHint {
                max_long_edge: Some(MAX_PICTURE_EDGE + 1),
                ..ok
            },
        ] {
            assert!(!bad.is_well_formed(), "{bad:?} was accepted");
        }
        // No preference is not the same as a bad preference.
        assert!(
            QualityHint {
                max_fps: None,
                max_long_edge: None,
                profile: Profile::Motion,
                sharpness: None,
            }
            .is_well_formed()
        );
    }

    /// **Direction is part of the protocol.** A controller sending a `Config`
    /// is telling the host what its own screen is; a host sending a keyframe
    /// request is asking the controller for a picture. Both are nonsense, and
    /// nonsense from a peer is worth refusing by name.
    #[test]
    fn each_media_message_may_only_travel_one_way() {
        let config = MediaMessage::Config(config());
        assert!(config.may_be_sent_by(Role::Host));
        assert!(!config.may_be_sent_by(Role::Controller));

        for from_controller in [
            MediaMessage::Select(MonitorSelect { monitor: 0 }),
            MediaMessage::Hint(QualityHint {
                max_fps: None,
                max_long_edge: None,
                profile: Profile::Balanced,
                sharpness: None,
            }),
            MediaMessage::Keyframe(KeyframeRequest {
                reason: KeyframeReason::Joined,
            }),
        ] {
            assert!(
                from_controller.may_be_sent_by(Role::Controller),
                "{from_controller:?} must be allowed from a controller"
            );
            assert!(
                !from_controller.may_be_sent_by(Role::Host),
                "{from_controller:?} must not be allowed from a host"
            );
        }
    }

    /// The wire names are the contract. A rename is a protocol break, so it
    /// should take a test failure to do it by accident.
    #[test]
    fn the_wire_names_are_pinned() {
        for (message, tag) in [
            (
                MediaMessage::Keyframe(KeyframeRequest {
                    reason: KeyframeReason::LostSync,
                }),
                "keyframe",
            ),
            (MediaMessage::Select(MonitorSelect { monitor: 0 }), "select"),
        ] {
            let text: String = encoded(&message).iter().map(|&b| b as char).collect();
            assert!(
                text.contains(tag),
                "the wire form of {message:?} no longer contains {tag:?}"
            );
        }
        // And the codec and profile names.
        assert_eq!(Codec::H264.name(), "h264");
        assert_eq!(Codec::H264.payload_type(), 96);
        assert_eq!(Profile::default(), Profile::Balanced);
    }
}
