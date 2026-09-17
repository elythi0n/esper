//! `esper ls` — what is in a directory on a paired host.
//!
//! # Why this is a command before it is a screen
//!
//! The two-pane file manager and drag-and-drop both need one thing the
//! protocol did not have: a way to ask what is there. Building that underneath
//! a window would make the first thing anybody can do with it require a
//! window — and this product's file transfer arrived as `esper cp` for exactly
//! the opposite reason. The fleet is headless; the machines where moving a
//! file matters most have no screen to drag onto.
//!
//! So the listing is a command. A window that wants a two-pane view has the
//! message it needs, and a person with a terminal has something useful today.
//!
//! # It is `files-out`, not a permission of its own
//!
//! Knowing a file exists is reading it. A host that will not send a file has
//! no business saying it has one, and a device granted only `files-in` — the
//! right to *put* things somewhere — must not be able to enumerate what is
//! already there. Splitting listing into its own permission would invite
//! exactly that mistake, by making "can list" look like a smaller thing than
//! "can read".

use esper_proto::{FileList, FileListing, FileMessage, FileWhere, Permission};
use esper_transport::Control;

/// What `esper ls` was asked for.
#[derive(Debug, Clone)]
pub struct ListOptions {
    pub device: String,
    pub path: String,
    pub via: Option<esper_rendezvous::Via>,
    pub mdns: bool,
    /// List the session's file box rather than the file directory.
    pub the_box: bool,
    /// One name per line, for a script.
    ///
    /// **A separate flag rather than guessing from whether stdout is a
    /// terminal.** A program whose output changes shape depending on where it
    /// is pointed is a program every script has to defend against.
    pub plain: bool,
}

/// Read the arguments.
///
/// # Errors
///
/// With a sentence and the usage, for anything it cannot make sense of.
pub fn parse(args: &[String]) -> Result<ListOptions, String> {
    let mut plain = Vec::new();
    let mut via_addr = None;
    let mut via_key = None;
    let mut mdns = crate::settings::flag("discovery.mdns", true);
    let mut the_box = false;
    let mut one_per_line = false;
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--no-mdns" => {
                mdns = false;
                i += 1;
            }
            "--box" => {
                the_box = true;
                i += 1;
            }
            "-1" | "--plain" => {
                one_per_line = true;
                i += 1;
            }
            "--via" => {
                let text = args
                    .get(i + 1)
                    .ok_or_else(|| "--via needs a broker address".to_owned())?;
                via_addr = Some(crate::net::parse_addr(text)?);
                i += 2;
            }
            "--broker-key" => {
                let text = args
                    .get(i + 1)
                    .ok_or_else(|| "--broker-key needs a key".to_owned())?;
                via_key = Some(crate::broker_cmd::parse_key(text)?);
                i += 2;
            }
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown option {other:?}\n{}", usage()));
            }
            other => {
                plain.push(other.to_owned());
                i += 1;
            }
        }
    }
    let [target] = plain.as_slice() else {
        return Err(format!("ls takes one <device>:<path>\n{}", usage()));
    };
    // The last colon, for the same reason `cp` splits there: a device can be
    // named by an address, and an address has a colon in it.
    let (device, path) = match target.rsplit_once(':') {
        Some((device, path)) if !device.is_empty() => (device.to_owned(), path.to_owned()),
        _ => {
            return Err(format!(
                "ls needs a device and a path, as in `elythion:reports`\n{}",
                usage()
            ));
        }
    };
    let via = match crate::session_cmd::broker_from(via_addr, via_key)? {
        Some(via) => Some(via),
        None => crate::settings::broker()?,
    };
    Ok(ListOptions {
        device,
        path,
        via,
        mdns,
        the_box,
        plain: one_per_line,
    })
}

/// What `esper ls` takes.
#[must_use]
pub fn usage() -> String {
    let me = crate::net::me();
    format!(
        "usage: {me} ls <device>:<path>\n\
         \n\
         List a directory on a paired host, relative to that machine's file\n\
         directory. An empty path lists the top of it.\n\
         \n\
         \x20 --box                list the session's file box instead\n\
         \x20 -1, --plain          one name per line, for a script\n\
         \x20 --via <broker>       be introduced by a broker when nothing direct works\n\
         \x20 --broker-key <key>   the broker's key, when it is not remembered\n\
         \x20 --no-mdns            do not ask the local network where the host is\n\
         \n\
         for example: {me} ls elythion:\n\
         \x20            {me} ls elythion:reports"
    )
}

/// Ask, and print what comes back.
///
/// # Errors
///
/// With a sentence for a person, as every command here does.
pub fn run(o: &ListOptions) -> Result<(), String> {
    let identity = crate::paths::identity()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("could not start: {e}"))?;

    runtime.block_on(async move {
        let at = crate::net::parse_addr(&o.device).ok();
        let reached = crate::session_cmd::reach(
            &identity,
            &crate::session_cmd::Reaching {
                label: "📄 esper ls",
                addr: at,
                named: if at.is_some() { None } else { Some(&o.device) },
                via: o.via,
                mdns: o.mdns,
                // The listing is this command's output; everything else goes
                // to standard error so a script's standard output is its own.
                aside: true,
            },
        )
        .await?;
        let crate::session_cmd::Reached {
            endpoint,
            mut session,
            ..
        } = reached;

        let (media, mut control) = esper_link::parts(&mut session).map_err(|e| e.to_string())?;
        let grant = esper_transport::request_session(
            &mut control,
            &esper_proto::SessionRequest {
                kind: esper_proto::SessionKind::Files,
                want: vec![Permission::FilesOut],
                codecs: Vec::new(),
                reason: "a directory listing".to_owned(),
            },
        )
        .await
        .map_err(|e| match e {
            esper_transport::AuthorizeError::Refused(d) => {
                format!("the host refused: {} ({})", d.detail, d.reason.0)
            }
            other => format!("agreeing what the session may do: {other}"),
        })?;
        if !grant.allows(Permission::FilesOut) {
            return Err(format!(
                "that host has not granted this device {}",
                Permission::FilesOut.name()
            ));
        }

        let mut stream = session
            .open_control()
            .await
            .map_err(|e| format!("could not open a file stream: {e}"))?;
        stream
            .send(&esper_proto::StreamPurpose::Files)
            .await
            .map_err(|e| format!("could not say what the stream was for: {e}"))?;

        let listing = ask(&mut stream, o).await;

        media.close(esper_proto::Reason::SESSION_COMPLETE);
        media.linger(std::time::Duration::from_millis(200)).await;
        drop(endpoint);
        show(&listing?, o);
        Ok(())
    })
}

/// One question and one answer.
async fn ask(stream: &mut Control, o: &ListOptions) -> Result<FileListing, String> {
    stream
        .send(&FileMessage::List(FileList {
            path: o.path.clone(),
            from: if o.the_box {
                FileWhere::Box
            } else {
                FileWhere::Directory
            },
        }))
        .await
        .map_err(|e| e.to_string())?;
    match stream.recv::<FileMessage>().await {
        Ok(FileMessage::Listing(listing)) => Ok(listing),
        Ok(FileMessage::Refuse(why)) => Err(crate::cp_cmd::advice(why, &o.path)),
        Ok(other) => Err(format!("the host answered with {}", other.name())),
        Err(e) => Err(format!("the host said nothing: {e}")),
    }
}

/// Print it.
///
/// **Directories first, then by name**, which is the order a person reading a
/// listing expects — and the sorting happens here rather than on the host,
/// because collation belongs to whoever is reading it.
fn show(listing: &FileListing, o: &ListOptions) {
    let mut entries = listing.entries.clone();
    entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.name.cmp(&b.name))
    });

    if o.plain {
        for entry in &entries {
            println!("{}", entry.name);
        }
    } else {
        for entry in &entries {
            if entry.directory {
                println!("  {:>12}  {}/", "-", entry.name);
            } else {
                println!("  {:>12}  {}", entry.bytes, entry.name);
            }
        }
        if entries.is_empty() {
            eprintln!("  (nothing here)");
        }
    }
    if listing.truncated {
        eprintln!(
            "  · only the first {} are shown; there are more in this directory",
            entries.len()
        );
    }
}
