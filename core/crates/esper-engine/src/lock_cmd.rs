//! Locking a paired host's screen, logging it out, or restarting it.
//!
//! # Why this is a verb rather than a key in a session
//!
//! "Put my machine away when you are done" is a smaller thing to ask for than
//! "watch my screen", and it should be grantable on its own. A key inside a
//! screen session would mean asking for `view` to lock a screen — so the
//! permission and the session kind are both their own, and a device can hold
//! `lock` and nothing else.
//!
//! It is also the shape a script wants. `esper lock elythion` at the end of a
//! job, or from a cron entry on a laptop that should not be left unlocked, is
//! the whole use — and neither of those has a session to press a key in.
//!
//! # What it reaches
//!
//! The display the host is serving, and nothing further out. A remote request
//! is not somebody standing at the machine, so it must not lock a login
//! session the way the panic key deliberately does — see `esper_presence`'s
//! `Reach`, and the finding that a host on a scratch display would otherwise
//! lock the developer's own desktop.

use std::net::SocketAddr;

use crate::session_cmd::{Reached, Reaching, reach};

/// Which host, what to ask it for, and how to get to it.
#[derive(Debug)]
pub struct LockOptions {
    pub addr: Option<SocketAddr>,
    pub named: Option<String>,
    pub via: Option<esper_rendezvous::Via>,
    pub mdns: bool,
    /// What to ask for. Each is its own permission at the host.
    pub what: What,
}

/// What a host is being asked to do to itself.
///
/// **Three verbs and one path**, because the difference between them is
/// entirely in the permission and the session kind — everything about
/// reaching the host, agreeing what may happen and reading the answer is the
/// same. Three copies of it would be three places to fix one bug in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    /// Lock the screen.
    Lock,
    /// End the login session.
    Logoff,
    /// Restart the machine.
    Reboot,
}

impl What {
    /// The verb somebody typed.
    #[must_use]
    pub const fn verb(self) -> &'static str {
        match self {
            Self::Lock => "lock",
            Self::Logoff => "logoff",
            Self::Reboot => "reboot",
        }
    }

    /// The permission it needs at the host.
    #[must_use]
    pub const fn needs(self) -> esper_proto::Permission {
        match self {
            Self::Lock => esper_proto::Permission::Lock,
            Self::Logoff => esper_proto::Permission::Logoff,
            Self::Reboot => esper_proto::Permission::Reboot,
        }
    }

    /// The kind of session it opens.
    #[must_use]
    pub const fn kind(self) -> esper_proto::SessionKind {
        match self {
            Self::Lock => esper_proto::SessionKind::Lock,
            Self::Logoff => esper_proto::SessionKind::Logoff,
            Self::Reboot => esper_proto::SessionKind::Reboot,
        }
    }

    /// What to say when it worked.
    #[must_use]
    pub const fn done(self) -> &'static str {
        match self {
            Self::Lock => "locked that host's screen",
            Self::Logoff => "asked that host to log out",
            Self::Reboot => "asked that host to restart",
        }
    }

    /// Read it from the word somebody typed.
    #[must_use]
    pub fn parse(verb: &str) -> Option<Self> {
        match verb {
            "lock" => Some(Self::Lock),
            "logoff" => Some(Self::Logoff),
            "reboot" => Some(Self::Reboot),
            _ => None,
        }
    }
}

/// Ask a host to lock its screen.
///
/// # Errors
///
/// If the host cannot be reached, refuses, or could not lock.
pub fn run(o: &LockOptions) -> Result<(), String> {
    crate::net::install_crypto();
    let identity = crate::paths::identity()?;

    crate::net::runtime()?.block_on(async {
        let reached = reach(
            &identity,
            &Reaching {
                label: &format!("🔒 esper {}", o.what.verb()),
                addr: o.addr,
                named: o.named.as_deref(),
                via: o.via,
                mdns: o.mdns,
                aside: false,
            },
        )
        .await?;
        let Reached {
            endpoint,
            mut session,
            ..
        } = reached;

        let (media, mut control) = esper_link::parts(&mut session).map_err(|e| e.to_string())?;
        let grant = esper_transport::request_session(
            &mut control,
            &esper_proto::SessionRequest {
                kind: o.what.kind(),
                // **`lock` and nothing else.** Asking for a permission this
                // will not use is asking somebody to agree to more than is
                // happening — and the whole point of the permission is that
                // it can be held alone.
                want: vec![o.what.needs()],
                // No pictures, so no codec: a host refusing this for a codec
                // would refuse it for something it was never going to use.
                codecs: Vec::new(),
                reason: o.what.verb().to_owned(),
            },
        )
        .await
        .map_err(|e| match e {
            esper_transport::AuthorizeError::Refused(d) => {
                format!("the host refused: {} ({})", d.detail, d.reason.0)
            }
            other => format!("agreeing what the session may do: {other}"),
        })?;
        if !grant.allows(o.what.needs()) {
            return Err(format!("this device may not {} that host", o.what.verb()));
        }

        // **In the trail as a session, because it is one.** The shortest thing
        // this product can do to somebody else's machine, and the easiest to
        // do without leaving a trace — which is precisely why it leaves one.
        let far_end = *session.peer_key();
        crate::trail::went_out(
            &far_end,
            esper_audit::Event::SessionOpened {
                granted: grant.granted.clone(),
                tier: o.what.verb().to_owned(),
                // This end asked and was allowed; the host's reason is the
                // host's to record.
                allowed: None,
            },
        );

        // **The host closes this, and the code is the answer.** There is
        // nothing to wait for on the wire: the host locks, says so, and hangs
        // up — so what it hung up with is the whole result.
        let said = wait_for_the_answer(&media).await;
        // The close frames are queued rather than sent, so a process that
        // exited here would leave the host waiting out its idle timeout for a
        // session that is over.
        drop(control);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), endpoint.wait_idle()).await;

        match said {
            Answered::Locked => {
                println!();
                println!("  done       {}", o.what.done());
                Ok(())
            }
            Answered::Refused(code, text) => {
                let described = code.describe().unwrap_or(&text);
                Err(format!("the host did not: {described}"))
            }
            // **Silence is the answer for a restart.** The machine is gone
            // before the close frame can be read, so nothing arriving is what
            // success looks like — and for the other two it is a link that
            // broke, which is worth saying differently.
            Answered::Nothing if o.what == What::Reboot => {
                println!();
                println!("  done       asked that host to restart, and it stopped answering");
                Ok(())
            }
            Answered::Nothing => Err(format!(
                "the host said nothing about the {} it was asked for",
                o.what.verb()
            )),
        }
    })
}

/// What the host said by hanging up.
enum Answered {
    /// It locked.
    Locked,
    /// It did not, and this is why.
    Refused(esper_proto::Reason, String),
    /// It closed with nothing, or the link broke.
    Nothing,
}

/// How long to wait for the host to answer.
///
/// The host locks and closes, which takes as long as starting a locker takes.
/// Five seconds is generous for that and short enough that a script does not
/// hang on a machine that is not going to answer.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

/// Wait for the host to close, and read what it closed with.
async fn wait_for_the_answer(media: &esper_transport::MediaChannel) -> Answered {
    let deadline = std::time::Instant::now() + PATIENCE;
    while std::time::Instant::now() < deadline {
        if let Some((code, text)) = media.peer_close_reason() {
            return if code == esper_proto::Reason::SESSION_COMPLETE {
                Answered::Locked
            } else {
                Answered::Refused(code, text)
            };
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    Answered::Nothing
}

/// What to say when somebody asks, or gets it wrong.
#[must_use]
pub fn usage(what: What) -> String {
    let me = crate::net::me();
    let verb = what.verb();
    let about = match what {
        What::Lock => {
            "Lock a paired host's screen. Nothing else: a device may hold `lock`\n\
             without holding `view`, so this works on a host it may not watch.\n\
             \n\
             It locks the display that host is serving and nothing further out."
        }
        What::Logoff => {
            "End the login session on a paired host, closing what is running in\n\
             it. Its own permission, and a sensitive one: granting it needs\n\
             confirming at that host."
        }
        What::Reboot => {
            "Restart a paired host. Its own permission, and the most\n\
             consequential one there is — the effect outlives the session that\n\
             asked for it, so granting it needs confirming at that host.\n\
             \n\
             Nothing answers a restart: the machine is gone before it could."
        }
    };
    format!(
        "usage: {me} {verb} [<address>|<name>] [options]\n\
         \n\
         {about}\n\
         \n\
         options:\n  \
         \x20 --via <addr>         a broker to be introduced through\n  \
         \x20 --broker-key <key>   the broker's key\n  \
         \x20 --no-mdns            do not ask the segment where the host is\n\
         \n\
         for example: {me} {verb} elythion"
    )
}

/// Read the arguments.
///
/// # Errors
///
/// On an unknown option, or an option with no value.
pub fn parse(what: What, args: &[String]) -> Result<LockOptions, String> {
    let mut addr: Option<SocketAddr> = None;
    let mut named: Option<String> = None;
    let mut via_addr: Option<SocketAddr> = None;
    let mut via_key: Option<[u8; 32]> = None;
    let mut mdns = crate::settings::flag("discovery.mdns", true);
    let mut i = 0usize;
    // No `--help` arm: `main` answers it before any parser runs, on stdout
    // with a zero status, and no other parser here has one either.
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--via" => {
                via_addr = Some(crate::net::parse_addr(
                    args.get(i + 1)
                        .ok_or_else(|| "--via needs the broker's address".to_owned())?,
                )?);
                i += 2;
            }
            "--broker-key" => {
                via_key = Some(crate::broker_cmd::parse_key(
                    args.get(i + 1)
                        .ok_or_else(|| "--broker-key needs the broker's key".to_owned())?,
                )?);
                i += 2;
            }
            "--no-mdns" => {
                mdns = false;
                i += 1;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other}\n\n{}", usage(what)));
            }
            other => {
                // An address if it parses as one, otherwise a paired host's
                // name — the same rule `connect` and `screenshot` follow.
                match crate::net::parse_addr(other) {
                    Ok(a) => addr = Some(a),
                    Err(_) => named = Some(other.to_owned()),
                }
                i += 1;
            }
        }
    }
    Ok(LockOptions {
        addr,
        named,
        via: crate::session_cmd::broker_from(via_addr, via_key)?,
        mdns,
        what,
    })
}
