//! Listing paired devices, and changing what they may do.
//!
//! Pairing establishes identity and grants **view and control, nothing else**.
//! Anything beyond that — a terminal, the clipboard, files, unattended access
//! — is a deliberate act by somebody at this machine, which means there has to
//! be a way to perform it. Without one, the permissions exist in the store and
//! in the protocol and are unreachable, and a separate permission that can
//! never be granted is the same as a feature that does not work.
//!
//! # Why a fingerprint prefix
//!
//! Devices are named by key, and a key is 64 hex characters nobody will type.
//! A prefix is matched instead, and an **ambiguous prefix is refused rather
//! than resolved** — picking one of two devices for somebody who was clearly
//! not sure which they meant is how a permission ends up on the wrong machine.

use esper_audit::{Entry, Event, Log};
use esper_crypto::{Device, DeviceStore};
use esper_proto::Permission;

use crate::paths;

/// What to do to a device's permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Add them.
    Grant,
    /// Take them away.
    Revoke,
}

/// Print every paired device, with what it may do.
pub fn list() -> Result<(), String> {
    let (store, dropped) = paths::devices()?;
    for line in &dropped {
        eprintln!("  · {line}");
    }

    println!("📇 esper devices");
    println!();
    if store.is_empty() {
        println!(
            "  no paired devices — run `{} pair` on both machines",
            crate::net::me()
        );
        println!();
        return Ok(());
    }

    let now = crate::paths::now_secs();
    let mut any_once = false;
    let mut any_unattended = false;
    for device in store.iter() {
        println!("  {}  {}", device.fingerprint().hex(), device.name);
        println!("      may  {}", crate::net::permissions(device));
        if device.pairing.is_once() {
            any_once = true;
            println!("      for  {}", device.pairing.describe(now));
        }
        // **A device that can walk in unasked is never a quiet setting.** It
        // is the most consequential thing in this list, so it says so on a
        // line of its own — with the hours when it is bounded, and without
        // them when it is not.
        if device.may(Permission::Unattended) {
            any_unattended = true;
            match device.unattended_when.as_ref() {
                Some(schedule) => println!("      when {} (local time)", schedule.spelled()),
                None => println!("      when any time, day or night"),
            }
        }
    }
    println!();
    // **The count, because a long list hides the total.** Every device that
    // can walk in unasked says so on a line of its own, which is right and is
    // not enough: an operator with twenty devices has to read twenty entries
    // to find out how many of them that is. This is the same fact at a glance,
    // and it names the unbounded ones separately because those are the ones
    // with no hours to be outside of.
    if let Some(count) = how_many_walk_in(&store) {
        println!("  {count}");
        println!();
    }
    println!(
        "  grant more with:  {} grant <fingerprint> <permission>...",
        crate::net::me()
    );
    println!("  permissions:      {}", every_permission());
    if any_once {
        println!(
            "  keep a one-session pairing:  {} keep <fingerprint>",
            crate::net::me()
        );
    }
    if any_unattended {
        println!(
            "  bound unattended access:     {} schedule <fingerprint> <when>",
            crate::net::me()
        );
        // Which zone those hours are in, because a schedule read in the wrong
        // timezone is a rule nobody wrote.
        if let Ok(zone) = esper_clock::Zone::local() {
            println!("  this host's local time:      {}", zone.name());
        }
    }
    Ok(())
}

/// How many devices may walk in unasked, in one line, or `None` when none can.
///
/// A function rather than three `println!`s so it can be tested: the
/// interesting cases are none, some with hours, and some without, and the
/// third is the one an operator most needs to see.
fn how_many_walk_in(store: &esper_crypto::DeviceStore) -> Option<String> {
    let total = store.iter().count();
    let unasked: Vec<&esper_crypto::Device> = store
        .iter()
        .filter(|d| d.may(Permission::Unattended))
        .collect();
    if unasked.is_empty() {
        return None;
    }
    let any_time = unasked
        .iter()
        .filter(|d| d.unattended_when.is_none())
        .count();
    let devices = if total == 1 { "device" } else { "devices" };
    let mut line = format!("{} of {total} {devices} may walk in unasked", unasked.len());
    if any_time > 0 {
        // Said as a share of the ones that can, not of the whole list: the
        // question an operator is asking is "how many of those are unbounded".
        line.push_str(&format!(", {any_time} of them at any time, day or night"));
    }
    Some(line)
}

/// Every permission, for help text.
fn every_permission() -> String {
    Permission::ALL
        .iter()
        .map(|p| p.name())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Apply a change to one device's permissions.
///
/// The arguments are a fingerprint prefix and one or more permission names.
pub fn change(args: &[String], what: Change) -> Result<(), String> {
    let verb = match what {
        Change::Grant => "grant",
        Change::Revoke => "revoke",
    };
    // **Answering the confirmation in advance, for a machine that has nobody
    // to ask.** Provisioning a host over a pipeline has no terminal, and the
    // alternative to a flag is somebody editing the device store by hand —
    // which skips the validation and the audit entry as well as the question.
    //
    // Taken out before the positional arguments are read, so its position does
    // not matter and it cannot be mistaken for a fingerprint.
    let answered_already = args.iter().any(|a| a == "--yes");
    let args: Vec<String> = args.iter().filter(|a| *a != "--yes").cloned().collect();
    let [prefix, names @ ..] = args.as_slice() else {
        return Err(format!(
            "usage: {} {verb} <fingerprint> <permission>... [--yes]\n\
             permissions: {}",
            crate::net::me(),
            every_permission()
        ));
    };
    if names.is_empty() {
        return Err(format!(
            "{} {verb} needs at least one permission\npermissions: {}",
            crate::net::me(),
            every_permission()
        ));
    }

    // Parsed before anything is written, so a typo in the third name does not
    // leave the first two applied.
    let mut wanted = Vec::new();
    for name in names {
        let p = Permission::parse(name).ok_or_else(|| {
            format!(
                "unknown permission {name:?}\npermissions: {}",
                every_permission()
            )
        })?;
        if !wanted.contains(&p) {
            wanted.push(p);
        }
    }

    // **A sensitive permission is confirmed before it is written.**
    //
    // `security.md` has always said so — "`unattended` is the dangerous one
    // and is treated as such: granting it requires confirmation at the host" —
    // and `Permission::is_sensitive` was written to express it. Every
    // reference to that function was a test, so the rule was stated,
    // expressed, and enforced nowhere: `grant <fp> unattended` wrote
    // permanent unattended access and printed a success line.
    //
    // It matters most for that one permission, because `unattended` is what
    // removes the prompt a *session* is otherwise held up by. Granting it
    // without a word is a way to switch off consent without being asked.
    if matches!(what, Change::Grant) {
        let sensitive: Vec<Permission> = wanted
            .iter()
            .copied()
            .filter(|p| p.is_sensitive())
            .collect();
        if !sensitive.is_empty() && !answered_already {
            confirm_sensitive(prefix, &sensitive)?;
        }
    }

    let (mut store, dropped) = paths::devices()?;
    for line in &dropped {
        eprintln!("  · {line}");
    }
    let key = *find(&store, prefix)?;
    let Some(device) = store.get_mut(&key) else {
        return Err("the device disappeared between finding it and changing it".to_owned());
    };

    for p in &wanted {
        match what {
            Change::Grant => device.grant(*p),
            Change::Revoke => device.revoke(*p),
        }
    }
    let name = device.name.clone();
    let now = crate::net::permissions(device);
    let permissions = device.permissions.clone();
    paths::save_devices(&store)?;

    // Recorded, because changing what a device may do is one of the few things
    // a person does *to* this machine's security posture, and the audit trail
    // is where somebody looks when they want to know who widened what.
    record(
        &key,
        Event::PermissionsChanged {
            granted: permissions,
        },
    );

    let done = match what {
        Change::Grant => "granted",
        Change::Revoke => "revoked",
    };
    println!(
        "  {done} {} on {name}",
        wanted
            .iter()
            .map(|p| p.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("  may     {now}");
    if what == Change::Revoke {
        // A running host re-reads the store while it runs, so this reaches
        // the session that device already has. Worth saying, because the
        // alternative — a revocation that waits for the revoked device to
        // choose to leave — is what people expect from a config file.
        println!("  note    a session that device already has will close within a second");
    }
    Ok(())
}

/// Turn a one-session pairing into one that lasts.
///
/// The deliberate act the one-session rule leaves room for. It exists because
/// the alternative to "invited pairings lapse" is not "people remember to
/// unpair" — it is "people do not" — and the alternative to *this* is somebody
/// who genuinely wants their helper to come back having to run the whole
/// invitation again.
/// Bound a device's unattended access to a schedule, or take the bound off.
///
/// **The permission is granted separately.** This says *when* it applies, and
/// setting one on a device without the permission is allowed on purpose: the
/// order somebody does two things in should not decide whether both took
/// effect, and a schedule waiting for a permission is harmless where a
/// refusal would be a puzzle.
///
/// # Errors
///
/// If the arguments are not a fingerprint and a schedule, the fingerprint
/// names no device or more than one, the schedule cannot be read, or the store
/// cannot be written.
pub fn schedule(args: &[String]) -> Result<(), String> {
    let [prefix, rest @ ..] = args else {
        return Err(schedule_usage());
    };
    if rest.is_empty() {
        return Err(schedule_usage());
    }
    // Joined, so `schedule <fp> weekdays 08:00-18:00` works without quoting.
    // Somebody typing a schedule has enough to think about.
    let spec = rest.join(" ");

    let (mut store, dropped) = paths::devices()?;
    for line in &dropped {
        eprintln!("  · {line}");
    }
    let key = *find(&store, prefix)?;

    let wanted = if matches!(
        spec.trim().to_ascii_lowercase().as_str(),
        "clear" | "none" | "off" | "any" | "anytime"
    ) {
        None
    } else {
        Some(esper_clock::Schedule::parse(&spec).map_err(|e| e.to_string())?)
    };

    let Some(device) = store.get_mut(&key) else {
        return Err("the device disappeared between finding it and scheduling it".to_owned());
    };
    let name = device.name.clone();
    let had_permission = device.may(esper_proto::Permission::Unattended);
    let fingerprint = device.fingerprint().hex();
    device.unattended_when = wanted.clone();
    paths::save_devices(&store)?;

    // **Recorded like every other change to what this machine trusts.**
    // Narrowing or widening when a device may walk in unasked is exactly the
    // kind of thing somebody should be able to find afterwards, and the audit
    // trail is the only place it would be.
    record(
        &key,
        Event::PermissionsChanged {
            granted: store
                .get(&key)
                .map(|d| d.permissions.clone())
                .unwrap_or_default(),
        },
    );

    match wanted.as_ref() {
        Some(schedule) => {
            println!(
                "  scheduled  {name} may connect unattended {}",
                schedule.spelled()
            );
            // The zone, because a schedule is in local time and "local" is a
            // decision the host makes — see `esper_clock`, which reads the
            // machine's own answer and ignores `$TZ`.
            match esper_clock::Zone::local() {
                Ok(zone) => println!("  local      those hours are {} time", zone.name()),
                Err(e) => println!(
                    "  warning    this host cannot tell what time it is locally ({e}), so the \
                     schedule will never allow anything until that is fixed"
                ),
            }
        }
        None => println!("  scheduled  {name} may connect unattended at any time"),
    }
    if !had_permission {
        println!(
            "  note      it does not hold `unattended` yet: {} grant {} unattended",
            crate::net::me(),
            fingerprint
        );
    }
    Ok(())
}

/// How to use the schedule command.
///
/// Public because `--help` is answered centrally, on stdout and with a
/// successful exit: asking how something works is not a failure. See the
/// dispatch in `main`.
#[must_use]
pub fn schedule_usage() -> String {
    let me = crate::net::me();
    format!(
        "usage: {me} schedule <fingerprint> <when>\n\
         \n\
         When a device may connect unattended — without anybody at the host\n\
         being asked. Outside those hours it is an ordinary paired device and\n\
         somebody at the host can still allow it.\n\
         \n\
         \x20 weekdays 08:00-18:00   Monday to Friday, working hours\n\
         \x20 weekends 10:00-23:00   Saturday and Sunday\n\
         \x20 daily 22:00-06:00      every night, across midnight\n\
         \x20 mon,wed,fri 9-17       named days\n\
         \x20 always                 every hour of every day\n\
         \x20 clear                  take the schedule off, leaving `always`\n\
         \n\
         The hours are this host's own local time, from its own timezone data.\n\
         Enforced here, at the host, and never by the far end."
    )
}

pub fn keep(args: &[String]) -> Result<(), String> {
    let [prefix] = args else {
        return Err(format!("usage: {} keep <fingerprint>", crate::net::me()));
    };

    let (mut store, dropped) = paths::devices()?;
    for line in &dropped {
        eprintln!("  · {line}");
    }
    let key = *find(&store, prefix)?;
    let Some(device) = store.get_mut(&key) else {
        return Err("the device disappeared between finding it and keeping it".to_owned());
    };
    if !device.pairing.is_once() {
        println!(
            "  {} is already paired for good — nothing to do",
            device.name
        );
        return Ok(());
    }
    device.keep();
    let name = device.name.clone();
    paths::save_devices(&store)?;

    // Recorded like every other change to what this machine trusts. Widening
    // a one-session pairing into a permanent one is exactly the kind of thing
    // somebody should be able to find afterwards.
    record(
        &key,
        Event::PermissionsChanged {
            granted: store
                .get(&key)
                .map(|d| d.permissions.clone())
                .unwrap_or_default(),
        },
    );

    println!("  kept    {name} is now paired like any other device");
    println!(
        "  note    remove it with `{} forget <fingerprint>`",
        crate::net::me()
    );
    Ok(())
}

/// Forget a device entirely: unpair it.
///
/// Removing the key is what makes the *next* connection impossible. Closing
/// the one it already has is the host's job — it re-reads this file while it
/// runs — and that is the half people forget, because a revocation that
/// leaves the revoked device connected is a request rather than a revocation.
pub fn forget(args: &[String]) -> Result<(), String> {
    let [prefix] = args else {
        return Err(format!("usage: {} forget <fingerprint>", crate::net::me()));
    };

    let (mut store, dropped) = paths::devices()?;
    for line in &dropped {
        eprintln!("  · {line}");
    }
    let key = *find(&store, prefix)?;
    let Some(gone) = store.remove(&key) else {
        return Err("the device disappeared between finding it and removing it".to_owned());
    };
    paths::save_devices(&store)?;

    record(
        &key,
        Event::Unpaired {
            name: gone.name.clone(),
        },
    );

    println!("  forgot  {}  {}", gone.fingerprint().hex(), gone.name);
    println!("  note    that device must pair again to connect at all");
    println!("  note    a session it already has will close within a second");
    Ok(())
}

/// Put one line in the audit trail.
///
/// Best-effort and *said* when it fails, rather than silently: a change made
/// and not recorded is worse than either, because the record is what somebody
/// will trust later.
fn record(key: &[u8; 32], event: Event) {
    let Ok(path) = paths::audit_log() else {
        eprintln!("  · not recorded: there is nowhere to write the audit log");
        return;
    };
    if let Err(e) = Log::at(path).append(&Entry::now(key, event)) {
        eprintln!("  · not recorded in the audit log: {e}");
    }
}

/// The one device whose fingerprint starts with `prefix`.
fn find<'a>(store: &'a DeviceStore, prefix: &str) -> Result<&'a [u8; 32], String> {
    let wanted = prefix.trim().to_ascii_lowercase().replace([' ', ':'], "");
    if wanted.is_empty() {
        return Err("no fingerprint given".to_owned());
    }
    let matches: Vec<&Device> = store
        .iter()
        .filter(|d| d.fingerprint().hex().starts_with(&wanted))
        .collect();
    match matches.as_slice() {
        [only] => Ok(&only.public_key),
        [] => Err(format!(
            "no paired device's fingerprint starts with {wanted:?} — `{} devices` lists them",
            crate::net::me()
        )),
        several => Err(format!(
            "{} paired devices start with {wanted:?}; give more of it:\n{}",
            several.len(),
            several
                .iter()
                .map(|d| format!("      {}  {}", d.fingerprint().hex(), d.name))
                .collect::<Vec<_>>()
                .join("\n")
        )),
    }
}

/// How long somebody has to confirm a sensitive grant.
///
/// Shorter than a session prompt, because whoever typed the command is at the
/// keyboard: they are answering a question they just caused.
const CONFIRM_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);

/// Ask before writing a permission that is dangerous to hold.
///
/// **Fail closed, and say what to do instead.** A grant run by a script, or in
/// a pipeline, or anywhere with no terminal, has nobody to ask — so it is
/// refused rather than assumed. The way to grant one deliberately is to be at
/// the machine, which is the whole point of the rule.
///
/// # What this does not defend against
///
/// A shell **on** this machine, including one Esper itself is serving to a
/// device holding `terminal`. That shell has a terminal of its own, so a
/// confirmation printed into it is answered by whoever is driving it. Closing
/// that needs something the remote side cannot supply, which is a Phase 14
/// question and is recorded as one. What this closes is the accident and the
/// unattended script, which is worth closing on its own.
fn confirm_sensitive(prefix: &str, sensitive: &[Permission]) -> Result<(), String> {
    let names = sensitive
        .iter()
        .map(|p| p.name())
        .collect::<Vec<_>>()
        .join(", ");
    let console = crate::consent::Console::open();
    if !console.can_ask() {
        return Err(format!(
            "granting {names} needs confirming at the host, and there is no terminal here \
             to confirm at.\nRun it where somebody can answer, or say so in the command:\n    \
             {} grant {prefix} {names} --yes",
            crate::net::me()
        ));
    }
    let lines = vec![
        format!("about to grant {names}"),
        "this is held until somebody takes it away, and applies to every session".to_owned(),
    ];
    match console.confirm(&lines, "grant it?", CONFIRM_WITHIN) {
        crate::consent::Answer::Yes => Ok(()),
        crate::consent::Answer::No => Err("nothing was changed".to_owned()),
        crate::consent::Answer::Silence => {
            Err("nobody answered, so nothing was changed".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// **Which permissions need confirming is one list, and it is the
    /// protocol's.**
    ///
    /// Not a second copy here: a list in this file that drifted from
    /// `is_sensitive` would mean the rule the design states and the rule the
    /// command applies were two different rules, and the one that mattered
    /// would be whichever was written last.
    #[test]
    fn the_dangerous_permissions_are_the_ones_the_protocol_says_are() {
        // Each of these is dangerous for a different reason, and each is worth
        // a question: `unattended` removes the prompt a session is otherwise
        // held up by, `files_in` writes to somebody's machine, and `record`
        // keeps a copy of what happened on it.
        for p in [
            Permission::Unattended,
            Permission::FilesIn,
            Permission::Record,
        ] {
            assert!(p.is_sensitive(), "{} must be confirmed", p.name());
        }
        // And the ordinary ones are not, or the confirmation becomes a thing
        // people learn to type through.
        for p in [
            Permission::View,
            Permission::Control,
            Permission::ClipboardOut,
            Permission::FilesOut,
            Permission::Terminal,
        ] {
            assert!(!p.is_sensitive(), "{} must not need a question", p.name());
        }
    }

    /// `--yes` is taken out wherever it appears, so it cannot be read as a
    /// fingerprint or as a permission name.
    #[test]
    fn the_confirmation_flag_is_not_a_positional_argument() {
        for args in [
            vec!["--yes", "abc", "unattended"],
            vec!["abc", "--yes", "unattended"],
            vec!["abc", "unattended", "--yes"],
        ] {
            let owned: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            let answered = owned.iter().any(|a| a == "--yes");
            let rest: Vec<String> = owned.into_iter().filter(|a| a != "--yes").collect();
            assert!(answered);
            assert_eq!(rest, vec!["abc".to_owned(), "unattended".to_owned()]);
        }
    }

    /// A device with a real key.
    ///
    /// Real because the store checks what it is given: a key that is not a
    /// point on the curve is refused on the way in, which is the Ed25519
    /// weak-key finding doing its job.
    fn device(name: &str) -> Device {
        let id = esper_crypto::Identity::generate().expect("a key");
        Device::new(id.public_key(), name.to_owned(), 0)
    }

    fn store_with(names: &[&str]) -> DeviceStore {
        let mut s = DeviceStore::new();
        for name in names {
            s.insert(device(name)).unwrap();
        }
        s
    }

    #[test]
    fn a_prefix_finds_the_one_device_it_names() {
        let s = store_with(&["laptop", "phone"]);
        let laptop = s.iter().find(|d| d.name == "laptop").unwrap();
        let hex = laptop.fingerprint().hex();
        let eight: String = hex.chars().take(8).collect();
        assert_eq!(find(&s, &eight).unwrap(), &laptop.public_key);
        // And the whole thing, which is what a script would paste.
        assert_eq!(find(&s, &hex).unwrap(), &laptop.public_key);
    }

    #[test]
    fn an_empty_prefix_matches_nothing_rather_than_everything() {
        // `starts_with("")` is true of every string, so without the check a
        // bare fingerprint argument would pick whichever device came first.
        let s = store_with(&["laptop"]);
        assert!(find(&s, "").is_err());
        assert!(find(&s, "   ").is_err());
    }

    #[test]
    fn an_ambiguous_prefix_is_refused_and_lists_the_candidates() {
        // A fingerprint is a hash, so a shared prefix cannot be constructed —
        // it is found. Sixteen possible first characters means a handful of
        // keys is plenty, and the loop is bounded so a failure fails rather
        // than hangs.
        let mut s = DeviceStore::new();
        let mut shared = None;
        for n in 0..64 {
            let d = device(&format!("device-{n}"));
            let hex = d.fingerprint().hex();
            let first: String = hex.chars().take(1).collect();
            if s.iter()
                .any(|other| other.fingerprint().hex().starts_with(&first))
            {
                shared = Some(first);
                s.insert(d).unwrap();
                break;
            }
            s.insert(d).unwrap();
        }
        let shared = shared.expect("64 random fingerprints without a shared first character");

        let e = find(&s, &shared).unwrap_err();
        assert!(e.contains("give more of it"), "{e}");
        // The candidates are listed, because "ambiguous" without them leaves
        // somebody guessing which two devices they meant.
        assert!(e.contains(&shared), "{e}");
    }

    #[test]
    fn an_unknown_prefix_says_how_to_find_the_right_one() {
        let s = store_with(&["laptop"]);
        // 'z' is not a hex digit, so no fingerprint can start with it.
        let e = find(&s, "zzzz").unwrap_err();
        assert!(e.contains("esper-engine devices"), "{e}");
    }

    /// A real key, because the store refuses one that is not a curve point.
    fn a_key() -> [u8; 32] {
        esper_crypto::Identity::generate()
            .expect("a key")
            .public_key()
    }

    fn paired(names: &[(&str, bool, bool)]) -> esper_crypto::DeviceStore {
        let mut store = esper_crypto::DeviceStore::default();
        for (name, unasked, bounded) in names {
            let mut d = esper_crypto::Device::new(a_key(), (*name).to_owned(), 0);
            if *unasked {
                d.permissions.push(Permission::Unattended);
                if *bounded {
                    d.unattended_when = Some(
                        esper_clock::Schedule::parse("weekdays 08:00-18:00").expect("a schedule"),
                    );
                }
            }
            store.insert(d).expect("a usable key");
        }
        store
    }

    /// **Nothing is said when nothing can walk in.**
    ///
    /// The commonest case, and a line saying "0 of 3 devices" would be noise
    /// on every machine that has not granted the permission — which is the
    /// state this product ships in.
    #[test]
    fn a_list_with_no_unattended_devices_says_nothing_about_it() {
        let store = paired(&[("elythion", false, false), ("desk", false, false)]);
        assert_eq!(how_many_walk_in(&store), None);
        assert_eq!(
            how_many_walk_in(&esper_crypto::DeviceStore::default()),
            None
        );
    }

    /// And the count is there when it is not zero.
    #[test]
    fn the_count_says_how_many_and_how_many_are_unbounded() {
        let store = paired(&[
            ("elythion", true, false),
            ("desk", true, true),
            ("phone", false, false),
        ]);
        let line = how_many_walk_in(&store).expect("a count");
        assert!(
            line.starts_with("2 of 3 devices may walk in unasked"),
            "{line}"
        );
        assert!(line.contains("1 of them at any time"), "{line}");
    }

    /// A device with hours is counted, and is not counted as unbounded.
    ///
    /// The distinction the line exists for: a schedule is the difference
    /// between "always" and "a choice", and a count that lumped them together
    /// would make bounding one pointless.
    #[test]
    fn a_bounded_device_is_not_counted_as_any_time() {
        let store = paired(&[("elythion", true, true)]);
        let line = how_many_walk_in(&store).expect("a count");
        assert!(
            line.starts_with("1 of 1 device may walk in unasked"),
            "{line}"
        );
        assert!(!line.contains("any time"), "{line}");
    }
}
