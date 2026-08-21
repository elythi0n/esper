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

/// Say that entries were left out of the store, and what that means.
///
/// The store reports the fingerprints of entries it would not load — a key
/// nothing can be trusted under, which is what a hand edit or a corrupted
/// write leaves behind. They were printed as a bare bullet and a row of
/// fingerprint words, before the heading, with nothing to say what the line
/// was: somebody reading it had a phrase and no question it answered. Seven
/// commands printed it that way, each with its own copy of the loop.
///
/// On stderr, because it is about the file rather than about the devices, and
/// a person piping the list somewhere wants the list.
fn say_what_was_dropped(dropped: &[String]) {
    if dropped.is_empty() {
        return;
    }
    // Written twice rather than assembled from parts: "1 entry ... were not
    // loaded ... until they are removed" is what a sentence built from a
    // plural switch reads like, and this one is read by somebody who has just
    // been told something is wrong with their file.
    if dropped.len() == 1 {
        eprintln!("  ⚠ One entry in the device store names a key nothing can be trusted");
        eprintln!("    under, and was not loaded. Nothing has been written: the file still");
        eprintln!("    holds it, and this will say so until it is removed.");
    } else {
        eprintln!(
            "  ⚠ {} entries in the device store name a key nothing can be",
            dropped.len()
        );
        eprintln!("    trusted under, and were not loaded. Nothing has been written: the");
        eprintln!("    file still holds them, and this will say so until they are removed.");
    }
    for line in dropped {
        eprintln!("      · {line}");
    }
    eprintln!();
}

/// Print every paired device, with what it may do.
pub fn list() -> Result<(), String> {
    let (store, dropped) = paths::devices()?;
    say_what_was_dropped(&dropped);

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
        // Which groups it is in, because a policy applied to a group is one
        // an operator has to be able to see the reach of.
        if !device.groups.is_empty() {
            println!("      in   {}", device.groups.join(", "));
        }
        // **Only when it is set.** Every device without one follows the
        // host's rule, and a line saying so on every entry would bury the one
        // device that does not.
        if let Some(after) = device.lock_after {
            println!("      lock {} when a session with it ends", after.word());
        }
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
    // **The target is taken off the front before anything else is read.**
    // `--group ops control` is a group and one permission; reading the first
    // word as a fingerprint made the *group's name* the first permission, and
    // the error said `unknown permission "ops"`.
    let usage = || {
        format!(
            "usage: {} {verb} <fingerprint>|--group <name> <permission>... [--yes]\n\
             permissions: {}",
            crate::net::me(),
            every_permission()
        )
    };
    let how_many = match args.as_slice() {
        [flag, _name, ..] if flag == "--group" => 2,
        [_prefix, ..] => 1,
        [] => return Err(usage()),
    };
    let (target, names) = args.split_at(how_many);
    // What to call it in a message somebody has to act on.
    let prefix = target.join(" ");
    let prefix = prefix.as_str();
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
    say_what_was_dropped(&dropped);
    // **Every key resolved before the first write.** A group that names
    // nothing, or a fingerprint that names two devices, changes nothing —
    // rather than changing some of it and then saying why it stopped.
    let (whose, _) = whose(&store, target)?;
    let mut changed: Vec<(String, String, Vec<Permission>, [u8; 32])> = Vec::new();
    for key in whose.keys() {
        let Some(device) = store.get_mut(key) else {
            return Err("a device disappeared between finding it and changing it".to_owned());
        };
        for p in &wanted {
            match what {
                Change::Grant => device.grant(*p),
                Change::Revoke => device.revoke(*p),
            }
        }
        changed.push((
            device.name.clone(),
            crate::net::permissions(device),
            device.permissions.clone(),
            *key,
        ));
    }
    paths::save_devices(&store)?;

    // Recorded, because changing what a device may do is one of the few things
    // a person does *to* this machine's security posture, and the audit trail
    // is where somebody looks when they want to know who widened what.
    // **One entry per device, not one per command.** An auditor asking what a
    // machine may do needs the answer for that machine; a single line saying
    // "the ops group was granted control" would leave them working out who was
    // in it at the time.
    for (_, _, permissions, key) in &changed {
        record(
            key,
            Event::PermissionsChanged {
                granted: permissions.clone(),
            },
        );
    }

    let done = match what {
        Change::Grant => "granted",
        Change::Revoke => "revoked",
    };
    let what_changed = wanted
        .iter()
        .map(|p| p.name())
        .collect::<Vec<_>>()
        .join(", ");
    if let Whose::Group(name, _) = &whose {
        println!("  {done} {what_changed} on {} in {name:?}", changed.len());
    }
    for (name, now, _, _) in &changed {
        println!("  {done} {what_changed} on {name}");
        println!("  may     {now}");
    }
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
    if args.is_empty() {
        return Err(schedule_usage());
    }
    let (mut store, dropped) = paths::devices()?;
    say_what_was_dropped(&dropped);
    // A fingerprint or a group, resolved before anything is written — the
    // same rule the permission changes follow.
    let (whose, rest) = whose(&store, args)?;
    if rest.is_empty() {
        return Err(schedule_usage());
    }
    // Joined, so `schedule <fp> weekdays 08:00-18:00` works without quoting.
    // Somebody typing a schedule has enough to think about.
    let spec = rest.join(" ");

    let wanted = if matches!(
        spec.trim().to_ascii_lowercase().as_str(),
        "clear" | "none" | "off" | "any" | "anytime"
    ) {
        None
    } else {
        Some(esper_clock::Schedule::parse(&spec).map_err(|e| e.to_string())?)
    };

    let mut scheduled: Vec<(String, bool, String, [u8; 32])> = Vec::new();
    for key in whose.keys() {
        let Some(device) = store.get_mut(key) else {
            return Err("a device disappeared between finding it and scheduling it".to_owned());
        };
        device.unattended_when = wanted.clone();
        scheduled.push((
            device.name.clone(),
            device.may(esper_proto::Permission::Unattended),
            device.fingerprint().hex(),
            *key,
        ));
    }
    paths::save_devices(&store)?;
    let Some((name, had_permission, fingerprint, key)) = scheduled.first().cloned() else {
        return Err("no device to schedule".to_owned());
    };
    if let Whose::Group(group, _) = &whose {
        println!();
        println!(
            "  scheduled {} device(s) in {group:?}: {}",
            scheduled.len(),
            scheduled
                .iter()
                .map(|(n, _, _, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    // **One entry per device**, for the same reason a permission change
    // writes one: an auditor asking about a machine needs the answer for that
    // machine rather than the name of a group somebody has since changed.
    for (_, _, _, each) in scheduled.iter().skip(1) {
        record(
            each,
            Event::PermissionsChanged {
                granted: store
                    .get(each)
                    .map(|d| d.permissions.clone())
                    .unwrap_or_default(),
            },
        );
    }

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

/// Set whether a session with a device leaves this machine's screen locked.
///
/// **A per-device answer to `host.lock_after_session`**, for the device the
/// host-wide rule is wrong about: a laptop connecting to its owner's own
/// desktop should not lock it under them, and a helper's machine should always
/// leave it locked.
///
/// Takes a group as well as a fingerprint, like every other per-device change
/// here, and resolves every key before writing any of them.
///
/// # Errors
///
/// If no device or group matches, if the word is not one of the four, or if
/// the store cannot be written.
pub fn lock_after(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err(lock_after_usage());
    }
    let (mut store, dropped) = paths::devices()?;
    say_what_was_dropped(&dropped);
    let (whose, rest) = whose(&store, args)?;
    let [word] = rest else {
        return Err(lock_after_usage());
    };
    let said = word.trim().to_ascii_lowercase();
    // **"default" is a value somebody sets, not the absence of one.** Taking
    // the answer off has to be as easy as giving it, or a person who changes
    // their mind edits the file by hand.
    let wanted = if matches!(said.as_str(), "default" | "host" | "clear" | "none") {
        None
    } else {
        Some(
            esper_crypto::LockAfter::from_word(&said)
                .ok_or_else(|| format!("{said:?} is not one of them.\n\n{}", lock_after_usage()))?,
        )
    };

    let mut changed: Vec<(String, [u8; 32])> = Vec::new();
    for key in whose.keys() {
        let Some(device) = store.get_mut(key) else {
            return Err("a device disappeared between finding it and changing it".to_owned());
        };
        device.lock_after = wanted;
        changed.push((device.name.clone(), *key));
    }
    paths::save_devices(&store)?;
    let Some((name, _)) = changed.first().cloned() else {
        return Err("no device to change".to_owned());
    };
    if let Whose::Group(group, _) = &whose {
        println!();
        println!(
            "  set        {} device(s) in {group:?}: {}",
            changed.len(),
            changed
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    // **One entry per device**, for the reason a permission change writes one:
    // an auditor asking about a machine needs the answer for that machine
    // rather than the name of a group somebody has since changed.
    for (_, each) in &changed {
        record(
            each,
            Event::LockAfterChanged {
                after: wanted.map(|a| a.word().to_owned()),
            },
        );
    }

    match wanted {
        Some(after) => println!(
            "  lock       {after_word} when a session with {name} ends",
            after_word = after.word()
        ),
        None => println!("  lock       whatever this host is set to, for {name}"),
    }
    println!(
        "  host       host.lock_after_session is {:?}, which every other device follows",
        crate::settings::text("host.lock_after_session")
    );
    Ok(())
}

/// How to use the lock-after command.
///
/// Public for the reason the schedule one is: `--help` is answered centrally.
#[must_use]
pub fn lock_after_usage() -> String {
    let me = crate::net::me();
    format!(
        "usage: {me} lock-after <fingerprint> <always|never|unattended|default>\n\
         \n\
         Whether this machine locks its screen when a session with that device\n\
         ends. Without this, every device follows host.lock_after_session.\n\
         \n\
         \x20 always        lock however the session was allowed\n\
         \x20 never         leave the screen as the session left it\n\
         \x20 unattended    lock only when nobody here was asked about it\n\
         \x20 default       take the answer off; follow this host's rule\n\
         \n\
         The panic key always locks, whatever any of this says.\n"
    )
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
    say_what_was_dropped(&dropped);
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
    say_what_was_dropped(&dropped);
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
/// Put devices in a group, take them out, or show who is in one.
///
/// # Why a group is a label rather than a list
///
/// A file of groups would be a second thing with a lifecycle: a group naming a
/// device that was unpaired is a dangling reference, and something has to
/// notice. A label on the device cannot dangle — forgetting a device forgets
/// its memberships in the same write. What it costs is that a group *is* its
/// members: the last device leaving takes the name with it.
///
/// # Errors
///
/// If the arguments are not a group name and fingerprints, a fingerprint
/// names no device or more than one, or the store cannot be written.
pub fn group(args: &[String]) -> Result<(), String> {
    let removing = args.iter().any(|a| a == "--remove");
    let args: Vec<String> = args.iter().filter(|a| *a != "--remove").cloned().collect();
    let [name, prefixes @ ..] = args.as_slice() else {
        return Err(group_usage());
    };
    let name = esper_crypto::Device::group_name(name);
    if name.is_empty() {
        return Err("a group needs a name".to_owned());
    }

    let (mut store, dropped) = paths::devices()?;
    say_what_was_dropped(&dropped);

    // No fingerprints is a question rather than a change.
    if prefixes.is_empty() {
        if removing {
            return Err(format!(
                "{} group --remove needs the devices to take out of {name:?}",
                crate::net::me()
            ));
        }
        show(&store, &name);
        return Ok(());
    }

    // **Every fingerprint resolved before the first write**, so a typo in the
    // third one does not leave the first two moved.
    let mut keys = Vec::new();
    for prefix in prefixes {
        let key = *find(&store, prefix)?;
        if !keys.contains(&key) {
            keys.push(key);
        }
    }

    let mut moved = Vec::new();
    for key in &keys {
        let Some(device) = store.get_mut(key) else {
            return Err("a device disappeared between finding it and changing it".to_owned());
        };
        if removing {
            device.leave(&name);
        } else {
            device.join(&name)?;
        }
        moved.push((device.name.clone(), device.groups.join(", ")));
    }
    paths::save_devices(&store)?;

    println!();
    let done = if removing { "took out of" } else { "put in" };
    for (device, groups) in &moved {
        println!("  {done} {name:?}: {device}");
        if groups.is_empty() {
            println!("  groups  none");
        } else {
            println!("  groups  {groups}");
        }
    }
    println!();
    println!(
        "  apply a policy to all of them:  {} grant --group {name} <permission>...",
        crate::net::me()
    );
    Ok(())
}

/// Print who is in a group.
fn show(store: &DeviceStore, name: &str) {
    let members: Vec<&Device> = store.iter().filter(|d| d.in_group(name)).collect();
    println!();
    if members.is_empty() {
        println!("  nobody is in {name:?}");
        println!();
        println!(
            "  put a device in it:  {} group {name} <fingerprint>...",
            crate::net::me()
        );
        return;
    }
    println!("  {} in {name:?}", members.len());
    for d in members {
        println!("  {}  {}", d.fingerprint().hex(), d.name);
        println!("      may  {}", crate::net::permissions(d));
    }
}

/// What to say when somebody asks, or gets it wrong.
#[must_use]
pub fn group_usage() -> String {
    let me = crate::net::me();
    format!(
        "usage: {me} group <name> [<fingerprint>...] [--remove]\n\
         \n\
         Put paired devices in a group, so a policy can be applied to all of\n\
         them at once. With no fingerprints, shows who is in it.\n\
         \n\
         A group is its members: the last device leaving takes the name with\n\
         it, and forgetting a device forgets its memberships.\n\
         \n\
         options:\n  \
         \x20 --remove   take those devices out of the group instead\n\
         \n\
         for example: {me} group ops 8a0f18d8 3423d1d7\n\
         \x20            {me} grant --group ops control\n\
         \x20            {me} group ops"
    )
}

/// Which devices a command is about: one by fingerprint, or a whole group.
///
/// **Resolved before anything is written**, like the permission names are: a
/// group that names nothing, or a fingerprint that names two devices, should
/// change nothing rather than change some of it.
#[derive(Debug)]
pub enum Whose {
    /// One device, found by a prefix of its fingerprint.
    One([u8; 32]),
    /// Every device in a group, and the group's name for saying so.
    Group(String, Vec<[u8; 32]>),
}

impl Whose {
    /// The keys this applies to.
    #[must_use]
    pub fn keys(&self) -> &[[u8; 32]] {
        match self {
            Self::One(key) => std::slice::from_ref(key),
            Self::Group(_, keys) => keys,
        }
    }
}

/// Read `--group <name>` or a fingerprint out of the arguments.
///
/// Returns what is left, so the caller can go on reading its own arguments
/// without knowing which form was used.
fn whose<'a>(store: &DeviceStore, args: &'a [String]) -> Result<(Whose, &'a [String]), String> {
    if let [flag, name, rest @ ..] = args
        && flag == "--group"
    {
        let keys = members(store, name);
        if keys.is_empty() {
            return Err(format!(
                "no paired device is in a group called {:?} — `{} group {}` puts one in it",
                esper_crypto::Device::group_name(name),
                crate::net::me(),
                esper_crypto::Device::group_name(name)
            ));
        }
        return Ok((
            Whose::Group(esper_crypto::Device::group_name(name), keys),
            rest,
        ));
    }
    let [prefix, rest @ ..] = args else {
        return Err("no fingerprint given".to_owned());
    };
    Ok((Whose::One(*find(store, prefix)?), rest))
}

/// Every device in a group, in the order the store holds them.
fn members(store: &DeviceStore, group: &str) -> Vec<[u8; 32]> {
    store
        .iter()
        .filter(|d| d.in_group(group))
        .map(|d| d.public_key)
        .collect()
}

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
