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
    /// Lock the screen at the host.
    ///
    /// **Its own permission, and not part of `control`.** A device that can
    /// drive the keyboard could lock the screen by pressing whatever the
    /// person's shortcut is, so this grants nothing new to a device holding
    /// `control` — but it is exactly what a device *without* `control` should
    /// be able to be given: "you may put my machine away when you are done,
    /// and you may not touch anything else".
    ///
    /// It locks the display the host is serving and nothing further out. A
    /// remote request is not somebody standing at the machine, so it must not
    /// reach past the screen it was made about.
    Lock,
    /// End the login session at the host, closing what is running in it.
    ///
    /// **Sensitive**, and separate from [`Permission::Reboot`]: they are
    /// different amounts of damage. This closes somebody's applications;
    /// that takes the machine away and every other session on it with it.
    Logoff,
    /// Restart the host.
    ///
    /// **The most consequential thing a device can be granted**, and the only
    /// one whose effect outlives the session that asked for it. Sensitive,
    /// and its own permission for the same reason: a helper who may log
    /// somebody out has not been given the machine's power button.
    Reboot,
}

impl Permission {
    /// Every permission there is, in the order a listing should show them.
    ///
    /// Exists so help text, listings and a name parser cannot fall behind the
    /// enum: adding a variant without adding it here is a compile error at the
    /// exhaustive match in [`Permission::name`] and a failing test here.
    pub const ALL: [Self; 14] = [
        Self::View,
        Self::Control,
        Self::ClipboardOut,
        Self::ClipboardIn,
        Self::FilesOut,
        Self::FilesIn,
        Self::Audio,
        Self::Terminal,
        Self::Forward,
        Self::Lock,
        Self::Logoff,
        Self::Reboot,
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
            Self::Lock => "lock",
            Self::Logoff => "logoff",
            Self::Reboot => "reboot",
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
    ///
    /// # Why `terminal` is on this list, and is the one that matters
    ///
    /// A shell on this machine runs as the user who owns `~/.esper`. That user
    /// can read the identity key and the local API's token, **rewrite the
    /// device list**, and read or edit the record of what happened. So a
    /// device holding `terminal` already has every permission below this line
    /// whether or not anybody granted them — it does not need `esper grant`
    /// at all, and a store it rewrites by hand leaves no line in the trail.
    ///
    /// That cannot be closed by asking a better question: the confirmation is
    /// answered at a terminal, and a served shell *is* a terminal — measured,
    /// not assumed. **So the gate belongs where the escalation starts.**
    /// Handing out the shell is the deliberate, confirmed, recorded act;
    /// everything after it is out of this machine's hands.
    #[must_use]
    pub const fn is_sensitive(self) -> bool {
        matches!(
            self,
            // A shell as the owning user: every permission below, and more.
            Self::Terminal
                // Removes the question a session is otherwise held up by.
                | Self::Unattended
                // Writes to this machine's filesystem.
                | Self::FilesIn
                // Keeps a copy of somebody's screen after the session.
                | Self::Record
                // Closes what somebody is working on.
                | Self::Logoff
                // Takes the machine away, and every other session on it.
                | Self::Reboot
        )
    }

    /// What granting this actually hands over, where that is more than its
    /// name suggests. `None` where the name is the whole of it.
    ///
    /// Shown at the moment somebody is asked to confirm, because a question
    /// naming only the permission is a question they cannot answer well.
    #[must_use]
    pub const fn what_it_really_grants(self) -> Option<&'static str> {
        match self {
            Self::Terminal => Some(
                "a shell on this machine, as the user who owns it — which can read this \
                 machine's identity key and the local API's token, rewrite the device list \
                 without asking, and edit the record of what happened. Granting it grants \
                 every other permission, whether or not you grant them",
            ),
            Self::Unattended => Some(
                "sessions with nobody here to agree to them, held until somebody takes it away",
            ),
            _ => None,
        }
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
    /// Moving files, and nothing else.
    ///
    /// **Not a screen session that happens to carry a file.** A copy over a
    /// screen session would start capture, an encoder and a presenter on a
    /// host that was asked to hand over one file — and would need the `view`
    /// permission to do it, which is a much larger grant than moving a file
    /// into a directory.
    Files,
    /// Lock the screen at the host, and nothing else.
    ///
    /// **A session rather than a message on one**, because it is the one thing
    /// a device might be allowed to do without being allowed to watch. A
    /// message on a screen session would mean asking for `view` to lock a
    /// screen, which is a much larger grant than putting somebody's machine
    /// away for them.
    ///
    /// It carries no video, needs no codec, and ends as soon as the host has
    /// answered.
    Lock,
    /// End the login session at the host, and nothing else.
    Logoff,
    /// Restart the host, and nothing else.
    ///
    /// The one session whose effect outlives it: the host is gone before the
    /// close frame can be read, so a controller learns it worked by the
    /// machine going away.
    Reboot,
}

impl SessionKind {
    /// The permissions a session of this kind cannot run without — **any one
    /// of them is enough**.
    ///
    /// One place, so the check the host makes and the request the controller
    /// sends cannot disagree about what a kind requires. A list rather than
    /// one, because moving files is two permissions that are granted
    /// separately and either of them is a reason to open a session.
    /// Whether a session of this kind carries video, and so needs a codec
    /// both ends understand.
    ///
    /// One place, for the same reason [`Self::requires`] is one place: the
    /// check the host makes and the list the controller sends must not
    /// disagree about whether a codec matters. A shell and a file transfer
    /// carry no pictures, and refusing one for a codec would refuse it for
    /// something it was never going to use.
    #[must_use]
    pub const fn carries_video(self) -> bool {
        match self {
            Self::Screen => true,
            Self::Terminal
            | Self::Forward
            | Self::Files
            | Self::Lock
            | Self::Logoff
            | Self::Reboot => false,
        }
    }

    #[must_use]
    pub const fn requires(self) -> &'static [Permission] {
        match self {
            Self::Screen => &[Permission::View],
            Self::Terminal => &[Permission::Terminal],
            Self::Forward => &[Permission::Forward],
            Self::Files => &[Permission::FilesOut, Permission::FilesIn],
            Self::Lock => &[Permission::Lock],
            Self::Logoff => &[Permission::Logoff],
            Self::Reboot => &[Permission::Reboot],
        }
    }

    /// What to tell a device that holds none of them.
    #[must_use]
    pub fn needs_said(self) -> String {
        let names: Vec<&str> = self.requires().iter().map(|p| p.name()).collect();
        names.join(" or ")
    }

    /// The word for it, for a log line or a prompt.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Terminal => "terminal",
            Self::Forward => "forward",
            Self::Files => "file",
            Self::Lock => "lock",
            Self::Logoff => "logoff",
            Self::Reboot => "reboot",
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

/// The host holding the picture, or letting it go again.
///
/// **Not an ending, and not a revocation.** The connection stays up, the
/// permissions stand, the clipboard and any file transfer carry on — what
/// stops is the picture. Somebody at the host needed a moment, and the
/// alternative products offer for that moment is ending the session and
/// setting it up again.
///
/// Sent whenever it changes, and it is the *state* rather than a toggle: two
/// ends that each flip a boolean on a message drift apart the first time one
/// is lost, and the state cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paused {
    /// Whether the host is holding the picture right now.
    pub paused: bool,
}

/// The controller saying whether it is recording the session to a file.
///
/// **Sent by the side doing it, which is the only side that knows.** A
/// recording is a copy of somebody else's screen that outlives the session,
/// and the person whose screen it is has a right to know it is being made.
///
/// What this cannot do is *enforce* that: a modified controller can record
/// without saying so, and no message can prevent that any more than a message
/// could stop somebody pointing a camera at the screen. What it does is make
/// an honest client honest by construction — this product always sends it, and
/// the host shows it for as long as it lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recording {
    /// Whether a recording is being written right now.
    pub recording: bool,
}

/// A codec for sound.
///
/// One variant, and it is still an enum: a receiver has to check what it is
/// being sent rather than assume, and a field that can only hold one value
/// today is how the second one arrives without a wire break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioCodec {
    /// Opus. Every browser decodes it, it is the only codec in the WebRTC
    /// mandatory set for speech *and* music, and it is good at both.
    Opus,
}

impl AudioCodec {
    /// The name a person reads.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Opus => "opus",
        }
    }
}

/// What the host will send as sound, announced when the sound starts.
///
/// **Its own message, not part of [`StreamConfig`].** Audio is permissioned
/// separately, starts and stops separately, and a session can be all of one
/// and none of the other: a terminal session with sound is nonsense, and a
/// screen with no sound is the ordinary case. Bundling the two would mean
/// re-announcing the picture to say something about the sound.
///
/// A host that cannot deliver sound at all does not send this and narrows the
/// session instead — see [`PermissionChange`] — because "the sound will not be
/// coming" is a statement about what the session may do, and there is already
/// one message for that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioConfig {
    pub codec: AudioCodec,
    /// Samples per second, per channel.
    pub sample_rate_hz: u32,
    /// 1 for mono, 2 for stereo.
    pub channels: u8,
    /// How much sound is in one packet, in milliseconds.
    ///
    /// **The receiver's whole buffering plan comes from this**, so it is on
    /// the wire rather than assumed: at 20 ms a lost packet is 20 ms of
    /// silence, and a buffer sized for 20 ms packets fed 60 ms ones stutters
    /// three times a second.
    pub frame_ms: u8,
    /// The stream's identity, so audio and video can be told apart in one
    /// media channel — and in a packet capture.
    pub ssrc: u32,
}

impl AudioConfig {
    /// The rates Opus itself accepts. Anything else is a config a decoder
    /// cannot be built for.
    const RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

    /// The frame lengths Opus itself accepts, in milliseconds.
    ///
    /// Opus also has 2.5 and 5 ms frames, which are not whole numbers of
    /// milliseconds in the 2.5 case and are for latency budgets no remote
    /// desktop has. A whole-millisecond field cannot express 2.5, which is
    /// the honest reason to leave it out rather than round it.
    const FRAMES_MS: [u8; 4] = [10, 20, 40, 60];

    /// Whether this describes sound a receiver can be built for.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        Self::RATES.contains(&self.sample_rate_hz)
            && (self.channels == 1 || self.channels == 2)
            && Self::FRAMES_MS.contains(&self.frame_ms)
    }

    /// How many samples per channel are in one packet.
    ///
    /// The number a decoder's output buffer has to hold, and the number a
    /// capture has to gather before it can encode. Derived in one place
    /// because the two ends must agree exactly: a buffer one sample short is
    /// a decode that fails every packet.
    #[must_use]
    pub const fn samples_per_frame(&self) -> usize {
        (self.sample_rate_hz as usize).saturating_mul(self.frame_ms as usize) / 1000
    }
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

/// A session asking to be allowed something it did not open with.
///
/// Controller to host. **Asking is all it is**: nothing about this message
/// changes what a session may do, and a host that acted on one without a
/// person answering would have built the escalation path this exists to make
/// impossible. What it buys is that the person at the machine gets the
/// question, instead of the person at the far end having to telephone them and
/// talk them through `esper grant`.
///
/// **One permission per ask.** A person answers one question at a time, and a
/// prompt offering three things at once is one where the answer says nothing
/// about which of them mattered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionAsk {
    /// What the session would like to be allowed.
    ///
    /// Which permissions may be asked for at all is the **host's** rule and is
    /// not on the wire: a set named here would be a controller describing its
    /// own limits. `check-host-decides.py` holds that mechanically.
    pub wanted: Permission,
}

/// The host's answer to a [`PermissionAsk`].
///
/// Host to controller, and always sent: a session left waiting on an answer
/// that never comes is a person watching a screen wondering whether anybody
/// saw the question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionAnswer {
    /// Somebody at the machine said yes.
    ///
    /// **What the session may now do arrives separately**, in a
    /// [`PermissionChange`]: that message is the one statement of what is in
    /// force, and two carriers for one fact are two things that can disagree.
    Allowed,
    /// No — and which kind of no.
    Refused {
        /// The kind of refusal, as a code the far end can act on: a person
        /// declining, nobody there to ask, a host that never asks, or one
        /// question too many.
        reason: Reason,
        /// The same thing as a sentence, for whoever is reading.
        detail: String,
    },
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

    /// Whether a controller listing these names can decode this.
    ///
    /// Case-insensitive, because the list is text a peer wrote and `H264` is
    /// the same codec as `h264`. Not a *prefix* match: a peer offering
    /// `h264-something-else` is offering something else.
    #[must_use]
    pub fn is_named_in(self, offered: &[String]) -> bool {
        offered.iter().any(|n| n.eq_ignore_ascii_case(self.name()))
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
    /// Resolutions this monitor can be changed to, largest first.
    ///
    /// **Empty means the host will not change it**, which is the honest answer
    /// on a Wayland compositor that does not offer the protocol, on a host
    /// whose display server has one fixed mode, and on a terminal-only host
    /// with no display at all. A controller shows no picker rather than a
    /// picker that always fails.
    ///
    /// Defaulted and skipped when empty so that a host built before this
    /// existed, and every recorded fixture, stay byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<DisplayMode>,
}

/// A resolution a monitor can be set to.
///
/// Sent so a controller can offer the list without a round trip, and so it can
/// say *which* it is asking for rather than "smaller".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    /// Refresh rate in millihertz, so it stays an integer on the wire and a
    /// fixture stays reproducible. 60,000 is 60 Hz; **zero means the host did
    /// not say**, which every virtual display does and which a controller
    /// shows as a resolution with no rate rather than as 0 Hz.
    pub refresh_mhz: u32,
}

impl DisplayMode {
    /// Whether this is a resolution worth putting in front of somebody.
    #[must_use]
    pub const fn is_well_formed(&self) -> bool {
        self.width >= 640
            && self.height >= 480
            && self.width <= MAX_PICTURE_EDGE
            && self.height <= MAX_PICTURE_EDGE
            // 1000 Hz. Above that is not a display, it is a parsing error.
            && self.refresh_mhz <= 1_000_000
    }
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
            // A list longer than this is not a monitor's mode list; it is
            // something that got in on the wire.
            && self.modes.len() <= 64
            && self.modes.iter().all(DisplayMode::is_well_formed)
    }
}

/// Set one monitor to one of the resolutions it offered.
///
/// Naming the monitor rather than assuming the selected one, because a host
/// serving two screens has two, and "the resolution" would then be ambiguous
/// in exactly the arrangement where changing it matters most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizeRequest {
    /// An id from [`StreamConfig::monitors`].
    pub monitor: u32,
    /// One of that monitor's [`MonitorDescription::modes`].
    ///
    /// A host that is offered anything else refuses rather than choosing
    /// something close: a resolution nobody picked is worse than none.
    pub mode: DisplayMode,
}

impl ResizeRequest {
    /// Whether this asks for something a host could act on.
    ///
    /// Shape only. Whether *this* monitor offers *that* mode is the host's to
    /// judge, because it is the end that owns the screen.
    #[must_use]
    pub const fn is_well_formed(&self) -> bool {
        self.mode.is_well_formed()
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
    ///
    /// With several screens tiled into one picture this is the *first* of
    /// them, in the order [`Self::serving`] gives — the one a controller
    /// showing a single screen would have asked for. It is kept because
    /// everything that is per-stream rather than per-screen keys on it: the
    /// SSRC, the cursor's frame of reference, a keyframe after a switch.
    pub selected: u32,
    /// Every screen in the picture, in the order they were asked for.
    ///
    /// **One picture, several screens.** A host serving more than one lays
    /// them out in the arrangement the desktop already has — each screen at
    /// its own position relative to the top-left corner of the set — and sends
    /// the result as a single stream. So a controller does not reassemble
    /// anything: the picture it decodes *is* the arrangement, and any gap
    /// between screens is black rather than the screen that happens to sit
    /// there.
    ///
    /// Repeated in every config rather than sent once, for the reason
    /// [`PermissionChange`] gives: a set both ends have to agree about must
    /// not be reconstructed from a history of deltas, because one lost message
    /// makes the two disagree for the rest of the session.
    ///
    /// It always contains [`Self::selected`] and is never empty — a session
    /// serving nothing is a session that should have been closed.
    pub serving: Vec<u32>,
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
    /// What somebody standing at the host would see of this session.
    ///
    /// **The one field in this message that is not about the picture**, and it
    /// is here because the person at the *other* end is party to whatever the
    /// answer is. A host that cannot show its own indicator will not serve at
    /// all by default; the owner of that machine can decide otherwise, and a
    /// controller should be able to say so rather than let somebody find out
    /// afterwards.
    #[serde(default)]
    pub seen: Seen,
}

/// What somebody at the host would see of a session.
///
/// **Three states rather than a flag**, because a machine with no screen and a
/// machine whose screen shows nothing are opposite situations wearing the same
/// answer: the first is every server this product was built for, and the
/// second is the one thing the indicator exists to prevent. A single boolean
/// forces a controller to warn about both or neither.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Seen {
    /// An indicator is up, confirmed by reading the host's own screen.
    ///
    /// The default, for a peer built before this field existed: an old host is
    /// not a host with a broken indicator, and warning about every one of them
    /// is how a warning stops being read.
    #[default]
    Shown,
    /// The host has no screen, so there is nobody there to show anything to.
    NoScreen,
    /// The host has a screen and cannot show an indicator on it.
    ///
    /// Only reachable where somebody set `host.serve_without_indicator`,
    /// because that is the one case where this host serves anyway.
    NotShown,
}

impl Seen {
    /// What a controller should tell whoever is watching.
    ///
    /// `None` when there is nothing worth saying — the ordinary case, where
    /// the host is showing what it should.
    #[must_use]
    pub const fn warning(self) -> Option<&'static str> {
        match self {
            Self::Shown => None,
            Self::NoScreen => Some("this host has no screen, so nothing there shows a session"),
            Self::NotShown => Some(
                "THIS HOST CANNOT SHOW THAT IT IS BEING WATCHED: nobody sitting at it will see \
                 an indicator",
            ),
        }
    }
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
            && self.serving_is_a_set()
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
    /// Whether `serving` names real screens, each once, including `selected`.
    ///
    /// A config that fails this describes a picture nobody can lay out: a
    /// screen that is not there has no rectangle, a repeated one would be
    /// placed twice, and a set without `selected` contradicts the field every
    /// per-stream decision keys on.
    fn serving_is_a_set(&self) -> bool {
        if self.serving.is_empty() || self.serving.len() > MAX_MONITORS {
            return false;
        }
        if !self.serving.contains(&self.selected) {
            return false;
        }
        if !self
            .serving
            .iter()
            .all(|id| self.monitors.iter().any(|m| m.id == *id))
        {
            return false;
        }
        let mut seen = self.serving.clone();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        seen.len() == before
    }

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

/// A controller asking for different monitors.
///
/// **A set, because "show me both screens" is the same request as "show me
/// that one" with a different number of answers.** Two messages for one job
/// would be two things to keep in step, and the host's answer is the same
/// either way: a config saying what it is now sending, or a refusal naming
/// what it is keeping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorSelect {
    /// Ids from [`StreamConfig::monitors`], in the order they should be laid
    /// out. Never empty: a controller that wants no picture has a close for
    /// that, and a session serving nothing would be a window showing black
    /// with a connection still paid for.
    pub monitors: Vec<u32>,
}

impl MonitorSelect {
    /// Ask for one screen.
    #[must_use]
    pub fn just(monitor: u32) -> Self {
        Self {
            monitors: vec![monitor],
        }
    }

    /// Whether this asks for something a host could serve.
    ///
    /// The ids themselves are the host's to judge — it is the only end that
    /// knows what is plugged in — so this is shape only: a set, not empty, and
    /// no longer than a desktop can be.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        if self.monitors.is_empty() || self.monitors.len() > MAX_MONITORS {
            return false;
        }
        let mut seen = self.monitors.clone();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        seen.len() == before
    }
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

/// What kind of thing is on a clipboard.
///
/// **A closed set, and small on purpose.** A clipboard on a real desktop can
/// carry anything a program cares to invent — file lists, application-private
/// formats, HTML with remote references, RTF with embedded objects. Forwarding
/// arbitrary types would turn a paste into a transfer, and turn a parser the
/// far end never chose into an attack surface reachable by copying.
///
/// So: text and images, and nothing else, and a peer naming anything else is
/// simply not understood.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardKind {
    /// UTF-8 text. The overwhelming majority of what anybody copies.
    Text,
    /// A PNG image. One encoding rather than several, so a receiver decodes
    /// what it expects rather than sniffing.
    Image,
}

impl ClipboardKind {
    /// Every kind, for a caller enumerating what it can offer.
    pub const ALL: [Self; 2] = [Self::Text, Self::Image];

    /// The most bytes worth carrying, for this kind.
    ///
    /// **Text and images are capped differently because they are used
    /// differently.** A megabyte of text is a large file pretending to be a
    /// paste; a megabyte of image is a screenshot, which is the second most
    /// ordinary thing anybody copies.
    #[must_use]
    pub const fn cap(self) -> usize {
        match self {
            // Comfortably a long document, and far below the point where a
            // paste becomes a transfer by another name.
            Self::Text => 1024 * 1024,
            // A full-screen PNG screenshot at 4K, with room.
            Self::Image => 16 * 1024 * 1024,
        }
    }

    /// A name for a person.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "an image",
        }
    }
}

/// Which way a clipboard is being crossed, from the point of view of the
/// machine deciding.
///
/// The two directions are separate permissions because they are separate
/// risks. Letting a controller *read* this machine's clipboard exports
/// whatever was last copied here — very often a password. Letting it *write*
/// puts content a person did not choose in front of the next paste they make.
/// A support session usually wants one and not the other, and which one
/// depends on who is helping whom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardWay {
    /// Leaving this machine, because the far end asked for what is here.
    Out,
    /// Arriving at this machine, to be put on the clipboard here.
    In,
}

impl ClipboardWay {
    /// The permission this direction cannot proceed without.
    #[must_use]
    pub const fn permission(self) -> Permission {
        match self {
            Self::Out => Permission::ClipboardOut,
            Self::In => Permission::ClipboardIn,
        }
    }

    /// The same crossing seen from the other machine.
    ///
    /// One side's `Out` is the other side's `In`, and both sides have to agree
    /// for anything to move: a controller reading a host's clipboard needs its
    /// own `clipboard-in` *and* the host's `clipboard-out`.
    #[must_use]
    pub const fn reversed(self) -> Self {
        match self {
            Self::Out => Self::In,
            Self::In => Self::Out,
        }
    }

    /// A phrase for a log line or a refusal.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Out => "read from this machine's clipboard",
            Self::In => "written to this machine's clipboard",
        }
    }
}

/// What is on a peer's clipboard — **its shape, never its content**.
///
/// # Why an offer rather than a push
///
/// A clipboard holds whatever was copied last, and what people copy last is
/// very often a password. A session that pushed every clipboard change to the
/// far end would export the contents of a password manager to anyone who
/// happened to have a window open, silently, at the moment somebody pressed
/// copy.
///
/// So a change announces its *shape* — what kind, how big — and nothing moves
/// until the other side asks for it and this side's policy allows it. The
/// asking is what makes the taking visible, and the policy is what makes it
/// refusable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardOffer {
    /// What is available, and how large each is.
    ///
    /// Ordered by the sender's preference, most useful first, so a receiver
    /// that wants only one thing takes the first it understands.
    pub kinds: Vec<Offered>,
}

/// One thing on offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offered {
    pub kind: ClipboardKind,
    /// How many bytes it is, so the far end can decide before asking rather
    /// than after receiving.
    pub bytes: u32,
}

impl ClipboardOffer {
    /// The most kinds an offer may name.
    ///
    /// There are two kinds, so anything longer is either a peer repeating
    /// itself or a peer probing. Bounded because this is the first thing a
    /// session parses from an unpaired-in-spirit peer.
    pub const MOST_KINDS: usize = 8;

    /// Whether this offer is within bounds and self-consistent.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.kinds.is_empty()
            && self.kinds.len() <= Self::MOST_KINDS
            && self.kinds.iter().all(Offered::is_well_formed)
    }

    /// What is offered of this kind, if anything.
    #[must_use]
    pub fn of(&self, kind: ClipboardKind) -> Option<Offered> {
        self.kinds.iter().copied().find(|o| o.kind == kind)
    }
}

impl Offered {
    /// Whether this is worth asking for.
    ///
    /// **An empty offer is refused rather than carried.** Nothing is gained by
    /// asking for zero bytes, and a peer that offers them is either confused
    /// or counting how this side responds.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.bytes > 0 && self.bytes as usize <= self.kind.cap()
    }
}

/// Asking for something that was offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardRequest {
    pub kind: ClipboardKind,
}

/// The content, at last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardData {
    pub kind: ClipboardKind,
    /// The bytes. UTF-8 for [`ClipboardKind::Text`], PNG for
    /// [`ClipboardKind::Image`].
    #[serde(with = "serde_bytes")]
    pub content: Vec<u8>,
}

impl ClipboardData {
    /// Whether this is within its kind's cap and actually of its kind.
    ///
    /// **Checked here rather than trusted**, because this is content from the
    /// far end on its way to a local clipboard, where every program on the
    /// machine can read it. Text that is not UTF-8 would be handed to a
    /// clipboard as though it were; an image that is not a PNG would be handed
    /// to whatever decodes PNGs.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        if self.content.is_empty() || self.content.len() > self.kind.cap() {
            return false;
        }
        match self.kind {
            ClipboardKind::Text => std::str::from_utf8(&self.content).is_ok(),
            ClipboardKind::Image => self.content.starts_with(&PNG_MAGIC),
        }
    }
}

/// The eight bytes every PNG begins with.
///
/// Checked so that "an image" means a PNG rather than whatever the far end
/// felt like sending to this machine's image decoders.
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Why a clipboard request came back with nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardRefusal {
    /// The peer does not hold the permission this direction needs.
    NotPermitted,
    /// Nothing of that kind is on the clipboard.
    ///
    /// Ordinary rather than exceptional: an offer describes the clipboard at
    /// the moment it was sent, and a person can copy something else before the
    /// far end gets around to asking.
    NotOffered,
    /// The clipboard could not be read at all.
    ///
    /// Deliberately not distinguished from a clipboard that is simply empty.
    /// Whether this machine has a display, whether anything owns the
    /// selection, and whether the owning program answered are all facts about
    /// the host, and the asker's next move is the same for all three.
    Unavailable,
    /// The request was outside its bounds.
    Malformed,
}

impl ClipboardRefusal {
    /// A sentence for a user, not a code for a machine.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NotPermitted => "this device is not permitted to use that machine's clipboard",
            Self::NotOffered => "there is nothing of that kind on the clipboard now",
            Self::Unavailable => "that machine's clipboard could not be read",
            Self::Malformed => "the clipboard request was malformed",
        }
    }
}

/// The answer to a [`ClipboardRequest`], and the only thing that ever carries
/// clipboard content.
///
/// **On a stream of its own, never on the media control stream.** A sixteen
/// megabyte image on the control stream would sit in front of every keyframe
/// request behind it, so a paste would cost the session seconds of frozen
/// picture. Its own stream means a large paste is slow at being a paste and
/// nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardReply {
    /// Here it is.
    Sent(ClipboardData),
    /// Refused, with a reason.
    Refused(ClipboardRefusal),
}

/// Anything that travels on a session's clipboard stream, in either direction.
///
/// # Why a stream of its own
///
/// The obvious place for a clipboard offer is the media control stream, which
/// already exists and already carries small things. Two reasons it does not go
/// there. A sixteen megabyte image would sit in front of every keyframe
/// request behind it, so a paste would cost the session seconds of frozen
/// picture — and a clipboard is the one part of a session that is **the same
/// in both directions**, where everything on the media stream is a controller
/// asking a host about its screen. One stream, one vocabulary, and both ends
/// run the same code over it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardMessage {
    /// Something was copied here. Its shape, never its content.
    Offer(ClipboardOffer),
    /// Asking for something that was offered.
    Ask(ClipboardRequest),
    /// The answer to an ask, which is the only thing that carries content.
    Reply(ClipboardReply),
}

impl ClipboardMessage {
    /// Whether this message is within bounds.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        match self {
            Self::Offer(o) => o.is_well_formed(),
            Self::Ask(_) => true,
            // A refusal carries nothing to be wrong; content is checked by
            // whatever is about to put it on a clipboard, which checks it
            // again on the way in.
            Self::Reply(ClipboardReply::Sent(d)) => d.is_well_formed(),
            Self::Reply(ClipboardReply::Refused(_)) => true,
        }
    }

    /// A name for a person.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Offer(_) => "a clipboard offer",
            Self::Ask(_) => "a clipboard request",
            Self::Reply(ClipboardReply::Sent(_)) => "clipboard content",
            Self::Reply(ClipboardReply::Refused(_)) => "a clipboard refusal",
        }
    }
}

/// The most bytes a path on a file stream may be.
///
/// The receiving side has its own, stricter rules about what a name may
/// contain; this is only the bound the framing enforces before any of them
/// run.
pub const MAX_FILE_PATH_LEN: usize = 1024;

/// The size and shape of a file, offered or asked for.
///
/// **The hash is here, in the offer, rather than at the end of the bytes.** It
/// costs the sender one extra read of the file and buys three things worth
/// much more: a receiver can refuse a file it already has, a resumed transfer
/// can prove the partial data belongs to the same content, and there is no
/// trailing frame to send after a raw body — which there is no clean way to
/// do.
/// Which of a host's two file directories a transfer is about.
///
/// **A session's file box is not the file directory**, and the difference is
/// the whole point of having one. The file directory is a place somebody chose
/// to share and that keeps what is put in it; the box is opened for one
/// session, is empty when it starts, and is emptied when it ends — so dropping
/// something into it is a smaller decision than writing into a directory that
/// persists.
///
/// Defaulted and skipped when it is the directory, so a host built before this
/// existed, and every recorded fixture, stay byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileWhere {
    /// The host's file directory, which persists.
    #[default]
    Directory,
    /// This session's box, which does not.
    Box,
}

impl FileWhere {
    /// A phrase for a person, and for the record.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Directory => "the file directory",
            Self::Box => "this session's box",
        }
    }

    /// Whether this names the box.
    #[must_use]
    pub const fn is_box(self) -> bool {
        matches!(self, Self::Box)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOffer {
    /// Where, relative to the far end's file directory. Never absolute.
    pub path: String,
    /// How many bytes it is.
    pub bytes: u64,
    /// SHA-256 of the whole thing.
    #[serde(with = "serde_bytes_array")]
    pub hash: [u8; 32],
    /// Whether to replace something already there.
    ///
    /// **Off unless somebody said so**, and the receiver may still refuse. A
    /// transfer that silently replaces is a transfer that eats work.
    #[serde(default)]
    pub replace: bool,
    /// Which directory it is going into.
    #[serde(default, skip_serializing_if = "is_the_directory")]
    pub to: FileWhere,
}

/// Whether a destination is the ordinary one, for `skip_serializing_if`.
fn is_the_directory(where_to: &FileWhere) -> bool {
    matches!(where_to, FileWhere::Directory)
}

impl FileOffer {
    /// Whether this is within the bounds the framing sets.
    ///
    /// What the *name* may be is [`crate::Permission`]-independent and lives
    /// with the code that opens files: this is the wire's own bound, checked
    /// before a string of any length reaches a path parser.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.path.is_empty() && self.path.len() <= MAX_FILE_PATH_LEN
    }
}

/// Asking the far end to send something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileWant {
    /// Where, relative to the far end's file directory.
    pub path: String,
    /// Which directory to take it from.
    #[serde(default, skip_serializing_if = "is_the_directory")]
    pub from: FileWhere,
}

impl FileWant {
    /// Whether this is within the bounds the framing sets.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.path.is_empty() && self.path.len() <= MAX_FILE_PATH_LEN
    }
}

/// Ask what is in a directory.
///
/// **A listing is not a transfer**, and it is governed by the same permission:
/// `files-out` is the right to read what is there, and knowing a file exists is
/// reading. A host that will not send a file will not say it has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileList {
    /// Which directory, relative to the far end's file directory. Empty means
    /// the top of it.
    pub path: String,
    /// Which of the host's two directories.
    #[serde(default, skip_serializing_if = "is_the_directory")]
    pub from: FileWhere,
}

impl FileList {
    /// Whether this is within the bounds the framing sets.
    ///
    /// The empty path is allowed and means the root — which is the one case
    /// [`FileWant`] refuses, because wanting nothing is not a request and
    /// listing everything is.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.path.len() <= MAX_FILE_PATH_LEN
    }
}

/// One thing in a directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Its name, with no path in it. **A single component**, so a listing
    /// cannot be used to describe somewhere else.
    pub name: String,
    /// How many bytes, for a plain file. Zero for a directory, which is what
    /// the filesystem says and not a count of what is inside.
    pub bytes: u64,
    /// Whether it is a directory.
    pub directory: bool,
}

impl FileEntry {
    /// Whether this names something a listing may describe.
    ///
    /// **Nothing that is neither a file nor a directory appears at all**, so
    /// this does not need a kind for a socket or a device node — a host does
    /// not put them in a listing, and a controller that received one would be
    /// being told about something it can do nothing with.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.name.is_empty()
            && self.name.len() <= MAX_NAME_LEN
            && !self.name.contains('/')
            && !self.name.contains('\\')
            && self.name != "."
            && self.name != ".."
            && !self.name.chars().any(char::is_control)
    }
}

/// What is in a directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileListing {
    /// What the peer asked about, echoed so a controller listing several
    /// directories at once can tell the answers apart.
    pub path: String,
    /// What is in it, in no particular order — sorting is a decision for
    /// whoever is showing it, and a host that sorted would be imposing its
    /// own collation on somebody else's language.
    pub entries: Vec<FileEntry>,
    /// Whether the host stopped short of the end.
    ///
    /// **A bound rather than pagination.** A directory with a million files in
    /// it is a thing that exists, and a host that tried to describe it whole
    /// would spend a session's memory on one message. The controller is told
    /// it is seeing part of it rather than being left to believe a directory
    /// is smaller than it is.
    #[serde(default)]
    pub truncated: bool,
}

/// The most entries one listing carries.
///
/// Two thousand: enough that an ordinary directory is never cut, small enough
/// that the message stays well under the framing's limit even with the longest
/// names the protocol allows.
pub const MAX_LISTING: usize = 2000;

impl FileListing {
    /// Whether this is within the bounds the framing sets.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.path.len() <= MAX_FILE_PATH_LEN
            && self.entries.len() <= MAX_LISTING
            && self.entries.iter().all(FileEntry::is_well_formed)
    }
}

/// Yes, and start here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAccept {
    /// The first byte still wanted.
    ///
    /// Zero for a fresh transfer, and the length of what is already held for
    /// one being resumed. **The receiver decides this**, because the receiver
    /// is the one that knows what it has.
    pub from: u64,
}

/// Why a transfer will not happen.
///
/// Note what these do *not* separate. There is no code for "the path leaves
/// the file directory" against "there is no such file": which of the two it
/// was is information about a filesystem the asking end cannot see, and its
/// next move is the same either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRefusal {
    /// The device does not hold the permission this direction needs.
    NotPermitted,
    /// This machine does not do file transfer at all.
    NotEnabled,
    /// The name is not one this protocol will carry.
    BadName,
    /// There is no such file, or it is not one this end may reach.
    Missing,
    /// Something is already there, and the offer did not say to replace it.
    Exists,
    /// Not enough room.
    NoRoom,
    /// Larger than this machine accepts.
    TooLarge,
    /// This session is already moving a file in that direction.
    Busy,
    /// It went wrong in a way worth one line rather than a code.
    Failed,
}

impl FileRefusal {
    /// A sentence for a user, not a code for a machine.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NotPermitted => "this device is not permitted to move files that way",
            Self::NotEnabled => "that machine does not have file transfer switched on",
            Self::BadName => "that name is not one this can carry",
            Self::Missing => "there is no such file in that machine's file directory",
            Self::Exists => "there is already a file there",
            Self::NoRoom => "there is not enough room for it",
            Self::TooLarge => "it is larger than that machine accepts",
            Self::Busy => "that session is already moving a file in that direction",
            Self::Failed => "the transfer could not be started",
        }
    }
}

/// How a transfer ended, said by whichever end received it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileEnd {
    /// Every byte arrived, the hash matched, and it is under its real name.
    Stored,
    /// Every byte arrived and the hash did not match. Nothing was stored.
    Mismatch,
    /// The bytes stopped before the end. What arrived is kept, to resume.
    Incomplete,
    /// Something else went wrong at the receiving end.
    Failed,
}

impl FileEnd {
    /// Whether the file is now where it was meant to go.
    #[must_use]
    pub const fn is_stored(self) -> bool {
        matches!(self, Self::Stored)
    }

    /// Whether what arrived was kept, so a later attempt can carry on.
    ///
    /// **The one ending worth reconnecting for.** Every other failure has
    /// already removed the partial, because a file that hashed wrong would
    /// hash wrong again from the same bytes — so this is what separates a
    /// transfer that was interrupted from one that was refused.
    #[must_use]
    pub const fn is_incomplete(self) -> bool {
        matches!(self, Self::Incomplete)
    }

    /// A sentence for a user.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Stored => "stored",
            Self::Mismatch => "what arrived was not what was sent; nothing was stored",
            Self::Incomplete => "it stopped part way; what arrived is kept to carry on from",
            Self::Failed => "it could not be written",
        }
    }
}

/// Anything that travels on a file stream, in either direction.
///
/// # The shape of a transfer
///
/// One stream per file, opened by the controller whichever way the file is
/// going. Framed control at the front, **raw bytes for the body** — the same
/// division the port forwarder makes, and for the same reason: re-framing a
/// bulk payload would be inventing a second flow-control scheme on top of the
/// one the stream already has.
///
/// Pushing, controller to host:
///
/// ```text
/// controller  Offer  ──────────────►  host
///             ◄────────────── Accept | Refuse
///             the bytes ──────────►
///             ◄────────────── Done
/// ```
///
/// Pulling, host to controller:
///
/// ```text
/// controller  Want  ───────────────►  host
///             ◄────────────── Have | Refuse
///             Accept | Refuse ────►
///             ◄────────────── the bytes
///             Done ───────────────►
/// ```
///
/// **A half of the stream carries frames until it starts carrying a body, and
/// only the sending half ever does.** That is what makes one vocabulary work
/// in both directions without either end having to know which of them opened
/// the stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMessage {
    /// I have this and would like to send it to you.
    Offer(FileOffer),
    /// I would like you to send me this.
    Want(FileWant),
    /// Here is what you asked for; say where to start.
    Have(FileOffer),
    /// Send it, from here. Said by whichever end is receiving.
    Accept(FileAccept),
    /// No, and why.
    Refuse(FileRefusal),
    /// That was all of it, and this is how it went. Said by the receiver.
    Done(FileEnd),
    /// What is in this directory?
    List(FileList),
    /// This is what is in it.
    Listing(FileListing),
}

impl FileMessage {
    /// Whether this message is within bounds.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        match self {
            Self::Offer(o) | Self::Have(o) => o.is_well_formed(),
            Self::Want(w) => w.is_well_formed(),
            Self::List(l) => l.is_well_formed(),
            Self::Listing(l) => l.is_well_formed(),
            Self::Accept(_) | Self::Refuse(_) | Self::Done(_) => true,
        }
    }

    /// A name for a person.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Offer(_) => "a file offer",
            Self::Want(_) => "a request for a file",
            Self::Have(_) => "the shape of a file",
            Self::Accept(_) => "an acceptance",
            Self::Refuse(_) => "a refusal",
            Self::Done(_) => "the end of a transfer",
            Self::List(_) => "a request for a directory listing",
            Self::Listing(_) => "a directory listing",
        }
    }
}

/// What a control stream a peer just opened is for.
///
/// **The first frame on every stream a screen session opens, and the only
/// thing that says what the rest of it is.** Before this existed there was one
/// kind of extra stream — input — so the one that arrived was the one it had
/// to be. A second kind makes that an assumption rather than a fact, and a
/// clipboard stream mistaken for an input stream would be a session where
/// neither works and nothing says why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamPurpose {
    /// Keys and pointer, controller to host. See [`crate::InputMessage`].
    Input,
    /// The clipboard, both ways. See [`ClipboardMessage`].
    Clipboard,
    /// One file, either way. See [`FileMessage`].
    ///
    /// **One stream per file, not one per session.** A transfer holds its
    /// stream for as long as the file takes, and a second file waiting behind
    /// the first on a shared stream would be a second file that cannot be
    /// cancelled without cancelling the first.
    Files,
}

impl StreamPurpose {
    /// The permission a stream of this kind cannot run without.
    ///
    /// The clipboard needs *either* direction: a session where the controller
    /// may paste into the host but not read from it is a session with a
    /// clipboard stream that carries traffic one way.
    #[must_use]
    pub fn permitted_by(self, held: &[Permission]) -> bool {
        match self {
            Self::Input => held.contains(&Permission::Control),
            Self::Clipboard => {
                held.contains(&Permission::ClipboardOut) || held.contains(&Permission::ClipboardIn)
            }
            // Either direction is enough to open one; which way this
            // particular file is going is checked when the offer arrives.
            Self::Files => {
                held.contains(&Permission::FilesOut) || held.contains(&Permission::FilesIn)
            }
        }
    }

    /// A name for a person.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Clipboard => "clipboard",
            Self::Files => "files",
        }
    }
}

/// What the receiving end has measured about the link.
///
/// # Why a sender cannot work this out for itself
///
/// Measured, three ways, all wrong. The congestion window over the round trip
/// reads two to three times high. A sender's own send rate measures the
/// *overload* rather than the capacity — it is what this end chose to offer,
/// not what the wire took. And the delivered rate is `min(offered, capacity)`,
/// so a settled session reported 68 kbit/s on a link carrying 800.
///
/// What can measure it is the end the bytes arrive at. **Every frame is a
/// capacity probe**: a sender hands a frame's datagrams over at once, so the
/// spread they arrive with is the wire serialising them, and `bytes ÷ spread`
/// is what the link can do. Measured within 8–15% across a fourfold range,
/// while demand over the same sessions varied four-fold with no relationship
/// to it.
///
/// So the receiver measures and says so, and the sender's transport paces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkReport {
    /// What the link carries, in kbit/s.
    ///
    /// Absent until enough frames have arrived to judge — and absent is the
    /// honest answer rather than a guess, because a sender told a number it
    /// cannot rely on paces at a rate the link does not have.
    pub capacity_kbit: Option<u32>,
}

/// The most a report may claim, in kbit/s.
///
/// A hundred gigabits. Past anything real and short of where the arithmetic
/// on the other side stops being comfortable.
pub const MOST_REPORTED_KBIT: u32 = 100_000_000;

impl LinkReport {
    /// Whether this is within bounds.
    ///
    /// A report only ever makes the sender **more** cautious, so a hostile one
    /// is a peer throttling the session it is itself watching. Bounded anyway:
    /// a number outside the plausible range is a peer that is confused, and
    /// acting on confusion is how a session ends up paced at nothing.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.capacity_kbit
            .is_none_or(|kbit| kbit > 0 && kbit <= MOST_REPORTED_KBIT)
    }
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
    /// Change the screen's resolution. Controller to host.
    ///
    /// **The host is left holding an obligation by this**, which no other
    /// message on this stream does: it has changed a screen somebody is
    /// sitting in front of, and it owes them the resolution back when the
    /// session ends — including if it dies first. See `esper-display-mode`.
    Resize(ResizeRequest),
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
    /// What the controller has measured about the link. Controller to host.
    ///
    /// On this stream because it is a handful of bytes a few times a second
    /// and the only other traffic here is rare. See [`LinkReport`] for why the
    /// measuring end is the receiving one.
    Report(LinkReport),
    /// The picture is held, or let go again. Host to controller.
    ///
    /// **A controller has to be told, or a held picture is a broken one.** A
    /// viewer whose frames simply stopped shows the last one it had, which is
    /// indistinguishable from a link that died — and the person watching would
    /// reconnect, which is precisely what pausing exists to avoid.
    Paused(Paused),
    Recording(Recording),
    /// What the host will send as sound. Host to controller.
    ///
    /// Sent when the sound starts, which is not when the session does: audio
    /// is permissioned on its own and a session can gain it later. See
    /// [`AudioConfig`].
    Audio(AudioConfig),
    /// What this session may now do. Host to controller.
    ///
    /// **On this stream because it is the only one the host can push on.**
    /// Every other stream in a session is opened by the controller for a
    /// purpose it names, so there is none the host could start to say this —
    /// and a session-level message is not a reason to invent one, being a
    /// handful of bytes that happen at most a few times in a session.
    ///
    /// It is not a media message, and this enum is not a claim that it is:
    /// what this type means is "anything that can travel on a media session's
    /// control stream", which is what its own documentation says.
    Permissions(PermissionChange),
    /// May this session do one more thing? Controller to host.
    ///
    /// On this stream for the same reason [`MediaMessage::Permissions`] is:
    /// it is a question about the session rather than about any one of its
    /// streams, and the answer comes back the same way.
    Ask(PermissionAsk),
    /// What somebody at the host said to that. Host to controller.
    Answer(PermissionAnswer),
    /// Whether the viewer wants the sound at all. Controller to host.
    ///
    /// **The protocol has always announced sound separately from the picture;
    /// what was missing was somewhere to ask.** A viewer who did not want it
    /// could say so at connect time and not afterwards, so somebody who
    /// walked into an open-plan office had one lever: close the session and
    /// open another without the `audio` permission. That is a different
    /// session — a new consent prompt, a new keyframe, and a gap in the audit
    /// trail where one session became two.
    ///
    /// A **request**, not a command. A host that was never granted `audio`,
    /// or that has no sound server, does not start it; the answer is the
    /// [`MediaMessage::Audio`] announcement that does or does not follow.
    /// Asking for sound a session may not have is not an error and is not
    /// refused — it is answered by silence, the same way it is answered when
    /// the machine has no sound card.
    Sound(SoundWanted),
    /// Something a person typed. Either direction.
    ///
    /// **One of two messages that either end may send**, and the only one
    /// carrying words a person wrote. During a support call the two people
    /// are often on a telephone; when they are not, the alternative to this
    /// is the helper typing into the *host's* own applications, which is
    /// putting text into somebody's machine to talk to them.
    ///
    /// It goes on the control stream because it is a session-level thing that
    /// happens a handful of times, and it is [`Said`] rather than a bare
    /// string so the bound and the refusal of control characters live with
    /// the type.
    Say(Said),
}

/// The longest thing anybody may say in one message.
///
/// Two thousand characters is a long paragraph and far more than anybody
/// types into a session. It is a bound rather than a feature: the text is
/// peer-supplied and reaches a terminal, and an unbounded string on a control
/// stream is a way to spend a host's memory a message at a time.
pub const MAX_SAID: usize = 2000;

/// Something a person typed, for the person at the other end.
///
/// **Never persisted off the machine that received it.** It is not written to
/// the audit trail and not written to disk: the trail records what a session
/// *did* — what was granted, what was refused, when it ended — and a
/// conversation is not that. A support call where the customer typed a
/// password into the chat window should not leave it in a file the helper can
/// read afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Said {
    /// What was typed.
    pub text: String,
}

impl Said {
    /// Whether this is something to show a person.
    ///
    /// **Control characters are refused rather than stripped.** The text goes
    /// to a terminal, where an escape sequence can move the cursor, recolour
    /// the screen or — on some terminals — put characters into the reader's
    /// own input. Stripping them would mean showing a message the sender did
    /// not send; refusing means the sender is told, which is the honest
    /// answer for something a person typed by hand.
    ///
    /// A newline is not excepted. A message is a line.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.text.is_empty()
            && self.text.chars().count() <= MAX_SAID
            && !self.text.chars().any(char::is_control)
    }
}

/// Whether the viewer wants the host's sound. See [`MediaMessage::Sound`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoundWanted {
    /// True to start it, false to stop it.
    pub wanted: bool,
}

impl MediaMessage {
    /// Whether this message is within bounds.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        match self {
            Self::Config(c) => c.is_well_formed(),
            // **A selection has a shape now that it is a set.** It used to
            // be one number, and every number was a well-formed request the
            // host would either honour or refuse by name. An empty set is
            // not: it asks for a session with no picture, which is a close
            // rather than a selection.
            Self::Select(s) => s.is_well_formed(),
            Self::Resize(r) => r.is_well_formed(),
            // A receiver sizes every buffer in its playback path from this, so
            // a config it cannot build a decoder for has to be refused rather
            // than approximated.
            Self::Audio(a) => a.is_well_formed(),
            Self::Keyframe(_) => true,
            Self::Hint(h) => h.is_well_formed(),
            Self::Cursor(c) => c.is_well_formed(),
            Self::Report(r) => r.is_well_formed(),
            // **Always, and that is a security decision rather than laziness.**
            // A `PermissionChange` only ever *removes* what a session may do,
            // and a caller refuses a message this returns false for — so a
            // validation rule here would be a way for a malformed narrowing to
            // be discarded, leaving the wider permissions in place. The
            // dangerous outcome for this one message is ignoring it, so there
            // is nothing to fail. The set is bounded by the enum: it cannot
            // name a permission that does not exist, and a repeat of one it
            // already named grants nothing, so applying it is idempotent.
            Self::Permissions(_) => true,
            // **A question is within bounds by naming a permission that
            // exists**, which the enum already guarantees — a name this build
            // does not know fails to decode long before this. Whether it may
            // be *asked* is the host's rule and lives at the host: a check
            // here would be this crate deciding what somebody at a machine is
            // allowed to be asked.
            Self::Ask(_) => true,
            // An answer carries a reason code, and an unrecognised code is
            // deliberately preserved rather than refused so a newer host's
            // reason still reaches a person.
            Self::Answer(_) => true,
            // A boolean is within bounds by being one.
            Self::Paused(_) => true,
            Self::Recording(_) => true,
            Self::Sound(_) => true,
            Self::Say(s) => s.is_well_formed(),
        }
    }

    /// A name for a person, for an error message or a log line.
    ///
    /// Here rather than at each end, so a host refusing a message and a
    /// controller refusing the same message call it the same thing.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Config(_) => "a stream config",
            Self::Select(_) => "a monitor selection",
            Self::Resize(_) => "a request to change the screen's resolution",
            Self::Hint(_) => "a quality hint",
            Self::Keyframe(_) => "a keyframe request",
            Self::Cursor(c) => c.name(),
            Self::Report(_) => "a link report",
            Self::Permissions(_) => "a permission change",
            Self::Ask(_) => "a request for one more permission",
            Self::Answer(_) => "an answer about a requested permission",
            Self::Paused(_) => "a pause",
            Self::Recording(_) => "a recording notice",
            Self::Audio(_) => "an audio config",
            Self::Sound(_) => "a request for the sound to start or stop",
            Self::Say(_) => "something somebody typed",
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
            // A controller announcing its own permissions would be a
            // controller granting itself permissions, which is the whole
            // reason this is checked at the boundary rather than trusted.
            // A controller that could pause the host's own screen would be
            // deciding for the person at it, which is the opposite of what
            // this is for.
            Self::Config(_)
            | Self::Cursor(_)
            | Self::Permissions(_)
            | Self::Answer(_)
            | Self::Paused(_)
            | Self::Audio(_) => matches!(role, Role::Host),
            // **A recording notice comes from the side making the recording**,
            // which is the controller and the only side that knows. This
            // table had it the other way round and nothing noticed, because
            // for a long time nothing consulted the table for this variant:
            // the host accepted it in its own arm and the controller refused
            // it in its own arm, and both were right about the direction while
            // the table was wrong. The first thing to rely on the table
            // refused every honest recording notice, which is how it was
            // found.
            Self::Select(_)
            // **Controller to host, and it is the message on this stream
            // that leaves the host owing something.** A host changes its own
            // screen only because a controller asked; a host that could
            // resize the *controller's* screen would be reaching through the
            // session in the direction nothing else does.
            | Self::Resize(_)
            | Self::Hint(_)
            | Self::Keyframe(_)
            | Self::Report(_)
            | Self::Recording(_)
            | Self::Ask(_)
            // **The viewer's own ears.** Whether a controller wants to hear
            // the host is a decision about the room the *controller* is in,
            // which is the one thing the host cannot see.
            | Self::Sound(_) => {
                matches!(role, Role::Controller)
            }
            // **The only message either side may send.** A conversation with
            // one direction is not one, and both ends have a person who might
            // be the one to speak first.
            Self::Say(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {

    /// **What may be shown to a person, and what may not.**
    ///
    /// The text is peer-supplied and reaches a terminal, where an escape
    /// sequence can move the cursor, recolour the screen or on some terminals
    /// put characters into the reader's own input. Refused rather than
    /// stripped: stripping shows a message the sender did not send, and the
    /// sender is the one who can fix it.
    #[test]
    fn something_said_is_bounded_and_carries_no_control_characters() {
        use super::{MAX_SAID, Said};

        let fine = Said {
            text: "can you open the terminal?".to_owned(),
        };
        assert!(fine.is_well_formed());

        // Nothing to show.
        assert!(
            !Said {
                text: String::new()
            }
            .is_well_formed()
        );

        // An escape sequence, which is the whole reason for the rule.
        assert!(
            !Said {
                text: "\u{1b}[2Jgone".to_owned()
            }
            .is_well_formed(),
            "an escape sequence would reach a terminal"
        );
        // A newline is a control character and a message is a line.
        assert!(
            !Said {
                text: "two\nlines".to_owned()
            }
            .is_well_formed()
        );

        // The bound, either side of it.
        assert!(
            Said {
                text: "a".repeat(MAX_SAID)
            }
            .is_well_formed()
        );
        assert!(
            !Said {
                text: "a".repeat(MAX_SAID + 1)
            }
            .is_well_formed(),
            "an unbounded string on a control stream spends a host's memory"
        );
        // Counted in characters, not bytes, or the bound moves with the
        // language somebody is typing.
        assert!(
            Said {
                text: "é".repeat(MAX_SAID)
            }
            .is_well_formed(),
            "the bound must be characters, not bytes"
        );
    }

    /// Either end may speak, which is what makes it a conversation.
    #[test]
    fn either_end_may_say_something() {
        use super::{MediaMessage, Role, Said};
        let said = MediaMessage::Say(Said {
            text: "hello".to_owned(),
        });
        assert!(said.may_be_sent_by(Role::Host));
        assert!(said.may_be_sent_by(Role::Controller));
    }

    /// **A narrowing must never be discardable, and that is why it has no
    /// validation rule.**
    ///
    /// Every other message on this stream is checked and refused if it is out
    /// of bounds, which is right: the dangerous outcome for a config, a hint
    /// or a pointer position is *acting* on a bad one. A `PermissionChange` is
    /// the one message where the dangerous outcome is the opposite. It only
    /// ever removes what a session may do, and both ends refuse a message that
    /// is not well formed — so any rule here would be a way for a host's
    /// withdrawal to be thrown away, leaving the *wider* permissions standing.
    ///
    /// So the sets that a rule might have caught are checked to be accepted:
    /// an empty one, a repeated one, and every permission at once.
    #[test]
    fn a_permission_change_is_always_well_formed_so_it_cannot_be_discarded() {
        use super::{MediaMessage, PermissionChange};
        for granted in [
            vec![],
            vec![Permission::View],
            vec![Permission::View, Permission::View],
            Permission::ALL.to_vec(),
        ] {
            let m = MediaMessage::Permissions(PermissionChange { granted });
            assert!(
                m.is_well_formed(),
                "a withdrawal a receiver refuses is a withdrawal that did not \
                 happen: {m:?}"
            );
        }
    }

    /// **Sound is host to controller only.** A controller that could announce
    /// an audio config would be telling the host what its own microphone is —
    /// and v1 deliberately has no microphone path at all.
    #[test]
    fn only_a_host_may_say_what_the_sound_is() {
        use super::{AudioCodec, AudioConfig, MediaMessage};
        let m = MediaMessage::Audio(AudioConfig {
            codec: AudioCodec::Opus,
            sample_rate_hz: 48_000,
            channels: 2,
            frame_ms: 20,
            ssrc: 7,
        });
        assert!(m.may_be_sent_by(super::Role::Host));
        assert!(!m.may_be_sent_by(super::Role::Controller));
        assert!(m.is_well_formed());
    }

    /// **Every variant's direction, in one place.** The table was wrong about
    /// one of them — a recording notice, which comes from the controller — for
    /// as long as nothing consulted it: each end had an arm that happened to
    /// be right, so the mistake could only surface the day something relied on
    /// the table, and then it refused every honest notice. A list is cheap and
    /// it cannot be right by coincidence.
    #[test]
    fn every_message_says_which_end_sends_it() {
        use super::{
            AudioCodec, AudioConfig, MediaMessage, Permission, PermissionAnswer, PermissionAsk,
            PermissionChange, Role,
        };

        let from_the_host = [
            MediaMessage::Cursor(crate::CursorMessage::Moved(crate::CursorPos {
                x: 1,
                y: 1,
                monitor: 0,
            })),
            MediaMessage::Permissions(PermissionChange {
                granted: vec![Permission::View],
            }),
            MediaMessage::Answer(PermissionAnswer::Allowed),
            MediaMessage::Paused(super::Paused { paused: true }),
            MediaMessage::Audio(AudioConfig {
                codec: AudioCodec::Opus,
                sample_rate_hz: 48_000,
                channels: 2,
                frame_ms: 20,
                ssrc: 1,
            }),
        ];
        for m in &from_the_host {
            assert!(m.may_be_sent_by(Role::Host), "{}", m.name());
            assert!(!m.may_be_sent_by(Role::Controller), "{}", m.name());
        }

        let from_the_controller = [
            MediaMessage::Hint(super::QualityHint {
                max_fps: None,
                max_long_edge: None,
                profile: super::Profile::Balanced,
                sharpness: None,
            }),
            MediaMessage::Keyframe(super::KeyframeRequest {
                reason: super::KeyframeReason::Joined,
            }),
            MediaMessage::Report(super::LinkReport {
                capacity_kbit: None,
            }),
            MediaMessage::Recording(super::Recording { recording: true }),
            MediaMessage::Ask(PermissionAsk {
                wanted: Permission::Control,
            }),
        ];
        for m in &from_the_controller {
            assert!(m.may_be_sent_by(Role::Controller), "{}", m.name());
            assert!(!m.may_be_sent_by(Role::Host), "{}", m.name());
        }
    }

    /// **A controller may ask and may not answer**, which is the whole
    /// arrangement: a controller that could send its own answer would be a
    /// controller granting itself permissions with one extra step, and one
    /// that could send a `Permissions` would not even need the step.
    #[test]
    fn a_controller_asks_and_a_host_answers_and_never_the_other_way() {
        use super::{MediaMessage, Permission, PermissionAnswer, PermissionAsk, Reason, Role};

        let ask = MediaMessage::Ask(PermissionAsk {
            wanted: Permission::Control,
        });
        assert!(ask.may_be_sent_by(Role::Controller));
        assert!(!ask.may_be_sent_by(Role::Host));
        assert!(ask.is_well_formed());

        for answer in [
            PermissionAnswer::Allowed,
            PermissionAnswer::Refused {
                reason: Reason::PROMPT_REFUSED,
                detail: "somebody at the host said no".to_owned(),
            },
        ] {
            let m = MediaMessage::Answer(answer);
            assert!(m.may_be_sent_by(Role::Host));
            assert!(!m.may_be_sent_by(Role::Controller));
            assert!(m.is_well_formed());
        }
    }

    /// The two survive a round trip, including the reason on a refusal — a
    /// code the far end branches on is not a code to lose in encoding.
    #[test]
    fn asking_and_answering_survive_the_wire() {
        use super::{MediaMessage, Permission, PermissionAnswer, PermissionAsk, Reason};

        for m in [
            MediaMessage::Ask(PermissionAsk {
                wanted: Permission::FilesOut,
            }),
            MediaMessage::Answer(PermissionAnswer::Allowed),
            MediaMessage::Answer(PermissionAnswer::Refused {
                reason: Reason::ASKED_ENOUGH,
                detail: "as often as it may".to_owned(),
            }),
        ] {
            let bytes = crate::encode(&m).expect("encode");
            let back: MediaMessage = crate::decode(&bytes).expect("decode");
            assert_eq!(back, m);
        }
    }

    /// **A config a decoder cannot be built for is refused, not rounded.**
    ///
    /// The receiver sizes its playback buffers from these three numbers, and
    /// Opus accepts only certain ones: a rate it does not have is a decoder
    /// that cannot be constructed, and a frame length it does not have is a
    /// decode that fails on every packet.
    #[test]
    fn an_audio_config_outside_what_opus_accepts_is_refused() {
        use super::{AudioCodec, AudioConfig};
        let good = AudioConfig {
            codec: AudioCodec::Opus,
            sample_rate_hz: 48_000,
            channels: 2,
            frame_ms: 20,
            ssrc: 1,
        };
        assert!(good.is_well_formed());
        assert_eq!(good.samples_per_frame(), 960);

        for bad in [
            AudioConfig {
                sample_rate_hz: 44_100,
                ..good
            },
            AudioConfig {
                sample_rate_hz: 0,
                ..good
            },
            AudioConfig {
                channels: 0,
                ..good
            },
            AudioConfig {
                channels: 6,
                ..good
            },
            AudioConfig {
                frame_ms: 0,
                ..good
            },
            AudioConfig {
                frame_ms: 33,
                ..good
            },
        ] {
            assert!(!bad.is_well_formed(), "{bad:?} should be refused");
        }
    }

    /// The sample count is what both ends size a buffer from, so it is
    /// checked at every rate rather than at the one we send.
    #[test]
    fn samples_per_frame_is_the_same_arithmetic_at_every_rate() {
        use super::{AudioCodec, AudioConfig};
        for (rate, frame_ms, want) in [
            (48_000u32, 20u8, 960usize),
            (48_000, 10, 480),
            (48_000, 60, 2880),
            (24_000, 20, 480),
            (8_000, 10, 80),
        ] {
            let c = AudioConfig {
                codec: AudioCodec::Opus,
                sample_rate_hz: rate,
                channels: 2,
                frame_ms,
                ssrc: 1,
            };
            assert!(c.is_well_formed(), "{c:?}");
            assert_eq!(c.samples_per_frame(), want, "{c:?}");
        }
    }

    /// **A pause is host to controller only, and always well formed.**
    ///
    /// Only the person at the host decides whether their screen is being
    /// shown; a controller that could send this could tell a host to stop
    /// sending, which is a way of hiding what is happening on it. And, like a
    /// withdrawal, it may not be discarded: a receiver that dropped the
    /// release would show a held picture for the rest of the session.
    #[test]
    fn only_a_host_may_hold_the_picture() {
        use super::{MediaMessage, Paused};
        for paused in [true, false] {
            let m = MediaMessage::Paused(Paused { paused });
            assert!(m.may_be_sent_by(super::Role::Host));
            assert!(
                !m.may_be_sent_by(super::Role::Controller),
                "a controller could tell the host to stop showing its screen"
            );
            assert!(
                m.is_well_formed(),
                "a pause a receiver refuses is a lie: {m:?}"
            );
        }
    }

    /// And it is host to controller only. A controller that could state its
    /// own permissions could grant itself any of them.
    #[test]
    fn only_a_host_may_say_what_a_session_holds() {
        use super::{MediaMessage, PermissionChange};
        let m = MediaMessage::Permissions(PermissionChange {
            granted: vec![Permission::View, Permission::Control],
        });
        assert!(m.may_be_sent_by(super::Role::Host));
        assert!(!m.may_be_sent_by(super::Role::Controller));
    }

    #[test]
    fn every_permission_is_in_all_and_parses_back_from_its_name() {
        // The list is what help text and the name parser iterate; a variant
        // missing from it would be a permission nobody could grant.
        assert_eq!(
            Permission::ALL.len(),
            14,
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
            modes: Vec::new(),
        }
    }

    fn config() -> StreamConfig {
        StreamConfig {
            monitors: vec![monitor(0, "eDP-1"), monitor(1, "DP-3")],
            selected: 1,
            serving: vec![1],
            codec: Codec::H264,
            width: 1920,
            height: 1080,
            max_fps: 30,
            clock_hz: 90_000,
            ssrc: 0xDEAD_BEEF,
            seen: Seen::Shown,
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
            MediaMessage::Select(MonitorSelect::just(1)),
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
            MediaMessage::Select(MonitorSelect::just(0)),
            MediaMessage::Hint(QualityHint {
                max_fps: None,
                max_long_edge: None,
                profile: Profile::Balanced,
                sharpness: None,
            }),
            MediaMessage::Keyframe(KeyframeRequest {
                reason: KeyframeReason::Joined,
            }),
            // Whether a viewer wants to hear the host is a decision about the
            // room the viewer is in, which the host cannot see.
            MediaMessage::Sound(SoundWanted { wanted: false }),
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
            (MediaMessage::Select(MonitorSelect::just(0)), "select"),
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

#[cfg(test)]
mod clipboard_tests {
    use super::*;

    fn roundtrip<T>(value: &T) -> T
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let mut bytes = Vec::new();
        crate::frame::write_frame(&mut bytes, value).expect("encodes");
        crate::frame::read_frame(&mut bytes.as_slice()).expect("decodes")
    }

    fn png(extra: usize) -> Vec<u8> {
        let mut v = PNG_MAGIC.to_vec();
        v.extend(std::iter::repeat_n(0u8, extra));
        v
    }

    #[test]
    fn every_clipboard_message_round_trips_through_the_framing() {
        let offer = ClipboardOffer {
            kinds: vec![
                Offered {
                    kind: ClipboardKind::Text,
                    bytes: 12,
                },
                Offered {
                    kind: ClipboardKind::Image,
                    bytes: 4096,
                },
            ],
        };
        assert_eq!(roundtrip(&offer), offer);
        assert!(offer.is_well_formed());

        let request = ClipboardRequest {
            kind: ClipboardKind::Image,
        };
        assert_eq!(roundtrip(&request), request);

        for reply in [
            ClipboardReply::Sent(ClipboardData {
                kind: ClipboardKind::Text,
                content: b"hello".to_vec(),
            }),
            ClipboardReply::Sent(ClipboardData {
                kind: ClipboardKind::Image,
                content: png(16),
            }),
            ClipboardReply::Refused(ClipboardRefusal::NotPermitted),
            ClipboardReply::Refused(ClipboardRefusal::NotOffered),
        ] {
            assert_eq!(roundtrip(&reply), reply);
        }
    }

    /// **The check that keeps a paste a paste.** Content arriving from the far
    /// end is on its way to a clipboard every program on this machine can
    /// read, so what it claims to be has to be what it is.
    #[test]
    fn content_that_is_not_what_it_says_it_is_gets_refused() {
        for bad in [
            // Not UTF-8, offered as text.
            ClipboardData {
                kind: ClipboardKind::Text,
                content: vec![0xFF, 0xFE, 0xFD],
            },
            // Not a PNG, offered as an image.
            ClipboardData {
                kind: ClipboardKind::Image,
                content: b"GIF89a and then some".to_vec(),
            },
            // A PNG header is not enough on its own to be a nothing.
            ClipboardData {
                kind: ClipboardKind::Text,
                content: Vec::new(),
            },
            ClipboardData {
                kind: ClipboardKind::Image,
                content: Vec::new(),
            },
        ] {
            assert!(!bad.is_well_formed(), "{:?} was accepted", bad.kind);
        }
        assert!(
            ClipboardData {
                kind: ClipboardKind::Text,
                content: "héllo — unicode".as_bytes().to_vec(),
            }
            .is_well_formed()
        );
        assert!(
            ClipboardData {
                kind: ClipboardKind::Image,
                content: png(1),
            }
            .is_well_formed()
        );
    }

    /// A cap that is not checked at its own edge is a cap that is off by one.
    #[test]
    fn the_cap_holds_exactly_at_its_edge() {
        for kind in ClipboardKind::ALL {
            let content = match kind {
                ClipboardKind::Text => vec![b'a'; kind.cap()],
                ClipboardKind::Image => png(kind.cap() - PNG_MAGIC.len()),
            };
            assert_eq!(content.len(), kind.cap());
            let mut data = ClipboardData { kind, content };
            assert!(data.is_well_formed(), "{kind:?} at its cap was refused");
            data.content
                .push(if kind == ClipboardKind::Text { b'a' } else { 0 });
            assert!(
                !data.is_well_formed(),
                "{kind:?} one byte over was accepted"
            );

            // And the offer describing it is bounded the same way, or a peer
            // could announce more than could ever be carried.
            let at = u32::try_from(kind.cap()).expect("a cap fits in a u32");
            assert!(Offered { kind, bytes: at }.is_well_formed());
            assert!(
                !Offered {
                    kind,
                    bytes: at + 1
                }
                .is_well_formed()
            );
            assert!(!Offered { kind, bytes: 0 }.is_well_formed());
        }
        // Text is capped far below images on purpose: a megabyte of text is a
        // file pretending to be a paste.
        assert!(ClipboardKind::Text.cap() < ClipboardKind::Image.cap());
    }

    #[test]
    fn an_offer_outside_its_bounds_is_refused() {
        let ok = Offered {
            kind: ClipboardKind::Text,
            bytes: 4,
        };
        assert!(!ClipboardOffer { kinds: Vec::new() }.is_well_formed());
        assert!(
            !ClipboardOffer {
                kinds: vec![ok; ClipboardOffer::MOST_KINDS + 1],
            }
            .is_well_formed()
        );
        assert!(
            ClipboardOffer {
                kinds: vec![ok; ClipboardOffer::MOST_KINDS],
            }
            .is_well_formed()
        );
        // One bad entry spoils the offer rather than being skipped over.
        assert!(
            !ClipboardOffer {
                kinds: vec![
                    ok,
                    Offered {
                        kind: ClipboardKind::Image,
                        bytes: 0
                    }
                ],
            }
            .is_well_formed()
        );
    }

    #[test]
    fn an_offer_says_what_it_has_of_a_kind() {
        let offer = ClipboardOffer {
            kinds: vec![Offered {
                kind: ClipboardKind::Text,
                bytes: 7,
            }],
        };
        assert_eq!(offer.of(ClipboardKind::Text).map(|o| o.bytes), Some(7));
        assert_eq!(offer.of(ClipboardKind::Image), None);
    }

    /// **The two directions must not collapse into one permission.** Granting
    /// a controller the right to paste *into* this machine is not granting it
    /// the right to read what is already here, and a mapping that returned the
    /// same permission for both would silently make it so.
    #[test]
    fn each_direction_needs_its_own_permission() {
        assert_eq!(ClipboardWay::Out.permission(), Permission::ClipboardOut);
        assert_eq!(ClipboardWay::In.permission(), Permission::ClipboardIn);
        assert_ne!(
            ClipboardWay::Out.permission(),
            ClipboardWay::In.permission()
        );
        for way in [ClipboardWay::Out, ClipboardWay::In] {
            assert_eq!(way.reversed().reversed(), way);
            assert_ne!(way.reversed(), way);
            assert!(!way.describe().is_empty());
        }
    }

    /// The clipboard is the one part of a session that is the same in both
    /// directions, so its messages have no direction rule at all — what may be
    /// taken is the permissions' business.
    #[test]
    fn every_clipboard_message_is_checked_before_it_is_acted_on() {
        let good = ClipboardMessage::Offer(ClipboardOffer {
            kinds: vec![Offered {
                kind: ClipboardKind::Text,
                bytes: 3,
            }],
        });
        assert!(good.is_well_formed());
        assert_eq!(roundtrip(&good), good);
        assert_eq!(good.name(), "a clipboard offer");

        for bad in [
            ClipboardMessage::Offer(ClipboardOffer { kinds: Vec::new() }),
            ClipboardMessage::Reply(ClipboardReply::Sent(ClipboardData {
                kind: ClipboardKind::Text,
                content: vec![0xFF],
            })),
        ] {
            assert!(!bad.is_well_formed(), "{} was accepted", bad.name());
        }
        // A refusal has nothing in it to be wrong.
        let refused =
            ClipboardMessage::Reply(ClipboardReply::Refused(ClipboardRefusal::NotOffered));
        assert!(refused.is_well_formed());
        assert_eq!(refused.name(), "a clipboard refusal");
        assert_eq!(
            ClipboardMessage::Ask(ClipboardRequest {
                kind: ClipboardKind::Image
            })
            .name(),
            "a clipboard request"
        );
    }

    /// **A stream says what it is before it says anything else.** There was
    /// one kind, so the one that arrived was the one it had to be; a second
    /// kind turns that from a fact into an assumption.
    #[test]
    fn a_stream_says_what_it_is_and_what_it_needs() {
        for purpose in [StreamPurpose::Input, StreamPurpose::Clipboard] {
            assert_eq!(roundtrip(&purpose), purpose);
            assert!(!purpose.permitted_by(&[]), "{purpose:?} needed nothing");
            assert!(!purpose.name().is_empty());
        }
        assert!(StreamPurpose::Input.permitted_by(&[Permission::Control]));
        assert!(!StreamPurpose::Input.permitted_by(&[Permission::ClipboardIn]));
        // Either direction is enough for a clipboard stream to be worth having.
        assert!(StreamPurpose::Clipboard.permitted_by(&[Permission::ClipboardIn]));
        assert!(StreamPurpose::Clipboard.permitted_by(&[Permission::ClipboardOut]));
        assert!(!StreamPurpose::Clipboard.permitted_by(&[Permission::Control]));
    }

    /// The wire names are the contract, and a rename is a protocol break.
    #[test]
    fn the_clipboard_wire_names_are_pinned() {
        for (bytes, tag) in [
            (
                encode(&ClipboardMessage::Offer(ClipboardOffer {
                    kinds: vec![Offered {
                        kind: ClipboardKind::Text,
                        bytes: 1,
                    }],
                })),
                "offer",
            ),
            (encode(&StreamPurpose::Clipboard), "clipboard"),
            (
                encode(&ClipboardReply::Refused(ClipboardRefusal::NotPermitted)),
                "not_permitted",
            ),
            (
                encode(&ClipboardRequest {
                    kind: ClipboardKind::Image,
                }),
                "image",
            ),
            (encode(&ClipboardWay::Out), "out"),
        ] {
            let as_text: String = bytes.iter().map(|&b| b as char).collect();
            assert!(
                as_text.contains(tag),
                "the wire form no longer contains {tag:?}"
            );
        }
        assert_eq!(ClipboardKind::Text.name(), "text");
        assert_eq!(ClipboardKind::Image.name(), "an image");
        for r in [
            ClipboardRefusal::NotPermitted,
            ClipboardRefusal::NotOffered,
            ClipboardRefusal::Unavailable,
            ClipboardRefusal::Malformed,
        ] {
            assert!(!r.message().is_empty());
        }
    }

    fn encode<T: serde::Serialize>(value: &T) -> Vec<u8> {
        let mut bytes = Vec::new();
        crate::frame::write_frame(&mut bytes, value).expect("encodes");
        bytes
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;

    fn roundtrip<T>(value: &T) -> T
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let mut bytes = Vec::new();
        crate::frame::write_frame(&mut bytes, value).expect("encodes");
        crate::frame::read_frame(&mut bytes.as_slice()).expect("decodes")
    }

    fn offer(path: &str) -> FileOffer {
        FileOffer {
            path: path.to_owned(),
            bytes: 4096,
            hash: [7u8; 32],
            replace: false,
            to: FileWhere::Directory,
        }
    }

    #[test]
    fn every_file_message_round_trips_through_the_framing() {
        for message in [
            FileMessage::Offer(offer("reports/q3.pdf")),
            FileMessage::Have(offer("logs/build.log")),
            FileMessage::Want(FileWant {
                path: "logs/build.log".to_owned(),
                from: FileWhere::Directory,
            }),
            FileMessage::Accept(FileAccept { from: 1_048_576 }),
            FileMessage::Refuse(FileRefusal::NoRoom),
            FileMessage::Done(FileEnd::Stored),
        ] {
            assert_eq!(roundtrip(&message), message, "{}", message.name());
            assert!(message.is_well_formed(), "{}", message.name());
            assert!(!message.name().is_empty());
        }
    }

    /// The hash is thirty-two bytes and has to arrive as thirty-two bytes.
    /// Serialised as a list of numbers rather than a byte string it would be
    /// four times the size and would still decode, which is the kind of thing
    /// that is only noticed by measuring.
    #[test]
    fn the_hash_crosses_as_bytes_and_comes_back_the_same() {
        let mut o = offer("x");
        o.hash = std::array::from_fn(|i| u8::try_from(i).unwrap_or(0));
        let back = roundtrip(&o);
        assert_eq!(back.hash, o.hash);

        let mut encoded = Vec::new();
        crate::frame::write_frame(&mut encoded, &o).expect("encodes");
        // A 32-byte CBOR byte string is 2 bytes of header plus 32 of payload.
        // As an array of small integers it would be at least 33 plus the
        // header, and the ones above 23 would take two bytes each.
        assert!(
            encoded.len() < 120,
            "the hash is not travelling as bytes: {} bytes for the whole frame",
            encoded.len()
        );
    }

    /// **The wire's own bound, before a string of any length reaches a path
    /// parser.** What a name may *contain* is a stricter question answered
    /// where files are opened; this is the one the framing can answer.
    #[test]
    fn a_path_outside_the_wire_bound_is_refused() {
        assert!(FileMessage::Offer(offer("a")).is_well_formed());
        assert!(!FileMessage::Offer(offer("")).is_well_formed());
        assert!(!FileMessage::Have(offer("")).is_well_formed());
        assert!(!FileMessage::Offer(offer(&"a".repeat(MAX_FILE_PATH_LEN + 1))).is_well_formed());
        assert!(FileMessage::Offer(offer(&"a".repeat(MAX_FILE_PATH_LEN))).is_well_formed());
        assert!(
            !FileMessage::Want(FileWant {
                path: String::new(),
                from: FileWhere::Directory,
            })
            .is_well_formed()
        );
    }

    /// **Replacing is off unless somebody said so**, including for a peer old
    /// enough not to have the field at all.
    #[test]
    fn a_transfer_does_not_replace_unless_it_says_it_does() {
        assert!(!offer("x").replace);
        let older = serde_json::json!({
            "path": "x",
            "bytes": 1,
            "hash": vec![0u8; 32],
        });
        let mut buf = Vec::new();
        ciborium::into_writer(&older, &mut buf).expect("encode");
        let back: FileOffer = ciborium::from_reader(buf.as_slice()).expect("an offer without it");
        assert!(
            !back.replace,
            "a peer that said nothing was taken to mean yes"
        );
    }

    /// Every refusal and every ending has something to say.
    #[test]
    fn every_outcome_says_something() {
        for r in [
            FileRefusal::NotPermitted,
            FileRefusal::NotEnabled,
            FileRefusal::BadName,
            FileRefusal::Missing,
            FileRefusal::Exists,
            FileRefusal::NoRoom,
            FileRefusal::TooLarge,
            FileRefusal::Busy,
            FileRefusal::Failed,
        ] {
            assert!(!r.message().is_empty(), "{r:?}");
        }
        for e in [
            FileEnd::Stored,
            FileEnd::Mismatch,
            FileEnd::Incomplete,
            FileEnd::Failed,
        ] {
            assert!(!e.message().is_empty(), "{e:?}");
            assert_eq!(e.is_stored(), e == FileEnd::Stored);
        }
    }

    /// A file stream needs a file permission, either direction, and nothing
    /// else opens one.
    #[test]
    fn a_file_stream_needs_a_file_permission() {
        assert!(StreamPurpose::Files.permitted_by(&[Permission::FilesOut]));
        assert!(StreamPurpose::Files.permitted_by(&[Permission::FilesIn]));
        assert!(!StreamPurpose::Files.permitted_by(&[Permission::Control]));
        assert!(!StreamPurpose::Files.permitted_by(&[Permission::ClipboardIn]));
        assert!(!StreamPurpose::Files.permitted_by(&[]));
        assert_eq!(StreamPurpose::Files.name(), "files");
        assert_eq!(roundtrip(&StreamPurpose::Files), StreamPurpose::Files);
    }

    /// The wire names are the contract.
    #[test]
    fn the_file_wire_names_are_pinned() {
        for (bytes, tag) in [
            (encoded(&FileMessage::Offer(offer("x"))), "offer"),
            (
                encoded(&FileMessage::Want(FileWant {
                    path: "x".to_owned(),
                    from: FileWhere::Directory,
                })),
                "want",
            ),
            (encoded(&FileMessage::Done(FileEnd::Mismatch)), "mismatch"),
            (
                encoded(&FileMessage::Refuse(FileRefusal::NotPermitted)),
                "not_permitted",
            ),
            (encoded(&StreamPurpose::Files), "files"),
        ] {
            let text: String = bytes.iter().map(|&b| b as char).collect();
            assert!(
                text.contains(tag),
                "the wire form no longer contains {tag:?}"
            );
        }
    }

    fn encoded<T: serde::Serialize>(value: &T) -> Vec<u8> {
        let mut bytes = Vec::new();
        crate::frame::write_frame(&mut bytes, value).expect("encodes");
        bytes
    }
}
