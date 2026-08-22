//! `esper-engine pair` — the two sides of a pairing.
//!
//! # Why pairing exists at all
//!
//! There are no passwords in this product. A device is either paired — its
//! public key recorded in this machine's device list — or it cannot connect,
//! and the TLS handshake is where that is enforced, so a refused peer never
//! reaches session code.
//!
//! Pairing is the one moment a machine will talk to a device it does not know,
//! and it is deliberately narrow: a short-lived code, a handful of attempts,
//! and a SPAKE2 exchange bound to the connection so a relay cannot sit in the
//! middle of it.
//!
//! # The code is read aloud, and that shapes everything
//!
//! Somebody reads it from one screen and types it into another, so it is
//! short — which means it is guessable, which is why the offer expires and
//! counts attempts. The security does not come from the code's length; it
//! comes from a PAKE that reveals nothing on a wrong guess and from there
//! being only three of them.
//!
//! # Both sides print the fingerprint, and that is not decoration
//!
//! A PAKE proves both ends used the same code. It cannot prove the code went
//! to the person you meant. Reading the fingerprint aloud is what closes that
//! gap, so both sides print it in the same format and a mismatch is something
//! a person can act on.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use esper_crypto::{Device, DeviceStore, Offer, PairingCode};
use esper_proto::{Hello, Role};
use esper_transport::{Control, Endpoint, Session, pair, pairing_authority};

use crate::paths;

/// How long to wait for the other side.
///
/// Generous, because a person is reading a code aloud in the middle of it.
const WAIT: Duration = Duration::from_secs(180);

/// What `pair` was asked to do.
#[derive(Debug, Clone)]
pub enum Options {
    /// Offer a code and wait for a device to use it.
    Offer { listen: SocketAddr },
    /// Use a code somebody read out.
    Accept { addr: SocketAddr, code: String },
}

/// Parse the flags.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut listen: Option<SocketAddr> = None;
    let mut addr: Option<SocketAddr> = None;
    let mut code: Option<String> = None;
    let mut i = 0usize;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--listen" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--listen needs an address".to_owned())?;
                listen = Some(crate::net::parse_listen(raw)?);
                i += 2;
            }
            "--code" => {
                code =
                    Some(args.get(i + 1).cloned().ok_or_else(|| {
                        "--code needs the code the other side printed".to_owned()
                    })?);
                i += 2;
            }
            other if !other.starts_with('-') && addr.is_none() => {
                addr = Some(crate::net::parse_addr(other)?);
                i += 1;
            }
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    match (listen, addr, code) {
        (Some(listen), None, None) => Ok(Options::Offer { listen }),
        (None, Some(addr), Some(code)) => Ok(Options::Accept { addr, code }),
        (None, Some(_), None) => {
            Err("pairing with an address needs --code, which the other side printed".to_owned())
        }
        _ => Err("either `pair --listen <addr>` to offer a code, or \
                  `pair <addr> --code <code>` to use one"
            .to_owned()),
    }
}

/// Run whichever side was asked for.
pub fn run(options: &Options) -> Result<(), String> {
    crate::net::runtime()?.block_on(async {
        match options {
            Options::Offer { listen } => offer(*listen).await,
            Options::Accept { addr, code } => accept(*addr, code).await,
        }
    })
}

/// Offer a code and wait for one device to use it.
async fn offer(listen: SocketAddr) -> Result<(), String> {
    crate::net::install_crypto();
    let identity = paths::identity()?;
    let mut offer = Offer::new();

    // **The pairing authority accepts any key, and only for this.** It is the
    // one place in the product that does, which is why it is a separate
    // endpoint bound for the pairing and dropped afterwards rather than a mode
    // the session endpoint can be put into. A session endpoint that could be
    // switched into accepting anyone is a session endpoint that will be.
    let endpoint = Endpoint::bind(&identity, listen, pairing_authority(), pairing_authority())
        .map_err(|e| format!("binding {listen}: {e}"))?;
    let bound = endpoint
        .local_addr()
        .map_err(|e| format!("reading the bound address: {e}"))?;

    println!("🔗 pairing");
    println!();
    println!("  code       {}", offer.code().display());
    println!("  listening  {bound}");
    println!(
        "  expires    in {} seconds",
        offer.remaining(Instant::now()).map_or(0, |d| d.as_secs())
    );
    println!();
    println!("  On the other machine:");
    println!(
        "      {} pair <this-machine>:{} --code {}",
        crate::net::me(),
        bound.port(),
        offer.code().display()
    );
    println!();
    println!("  This device's fingerprint — read it aloud and check it matches:");
    for line in identity.fingerprint().display().lines() {
        println!("      {line}");
    }
    println!();

    let conn = tokio::time::timeout(WAIT, endpoint.accept())
        .await
        .map_err(|_| format!("nobody connected within {} seconds", WAIT.as_secs()))?
        .ok_or("the endpoint closed")?
        .map_err(|e| format!("accepting: {e}"))?;

    let mut session = Session::open(
        conn.clone(),
        Hello::new(
            Role::Host,
            identity.public_key(),
            &crate::net::this_device(),
        ),
    )
    .await
    .map_err(|(e, _)| format!("opening the session: {e}"))?;
    let mut control: Control = session
        .take_control()
        .ok_or("the session has no control stream")?;

    let peer_key = pair::host(&conn, &mut control, &identity, &mut offer, Instant::now())
        .await
        .map_err(|e| format!("pairing failed: {e}"))?;

    let name = session.peer_hello().name.clone();
    let device = record(peer_key, &name)?;
    report(&device);

    // **The host writes last, so the host must not close first.**
    //
    // The exchange ends with a confirmation the controller is still waiting to
    // read. Closing here cuts it off mid-read, and the controller reports a
    // lost connection on a pairing that in fact succeeded — both sides did the
    // right thing and one of them says it failed. `pair::host` says so in its
    // own documentation, and this code did it anyway on the first attempt.
    //
    // So the controller drives the teardown and this waits for it.
    tokio::time::timeout(Duration::from_secs(5), conn.closed())
        .await
        .ok();
    tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle())
        .await
        .ok();
    Ok(())
}

/// Use a code somebody read out.
async fn accept(addr: SocketAddr, code: &str) -> Result<(), String> {
    crate::net::install_crypto();
    let identity = paths::identity()?;
    let code = PairingCode::parse(code).map_err(|e| format!("that code cannot be used: {e}"))?;

    let endpoint = Endpoint::bind(
        &identity,
        crate::net::any_port(addr),
        pairing_authority(),
        pairing_authority(),
    )
    .map_err(|e| format!("binding a local port: {e}"))?;

    println!("🔗 pairing with {addr}");
    println!();
    let conn = tokio::time::timeout(WAIT, endpoint.connect(addr))
        .await
        .map_err(|_| format!("{addr} did not answer within {} seconds", WAIT.as_secs()))?
        .map_err(|e| e.to_string())?;

    let mut session = Session::open(
        conn.clone(),
        Hello::new(
            Role::Controller,
            identity.public_key(),
            &crate::net::this_device(),
        ),
    )
    .await
    .map_err(|(e, _)| format!("opening the session: {e}"))?;
    let mut control: Control = session
        .take_control()
        .ok_or("the session has no control stream")?;

    let peer_key = pair::controller(&conn, &mut control, &identity, &code)
        .await
        .map_err(|e| format!("pairing failed: {e}"))?;

    let name = session.peer_hello().name.clone();
    let device = record(peer_key, &name)?;
    report(&device);

    // The controller read last, so it closes — which is what releases the
    // host from the wait above.
    conn.close(0u32.into(), b"paired");
    tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle())
        .await
        .ok();
    Ok(())
}

/// How long a pairing made from an invitation lasts if it is never used.
///
/// The session it exists for usually follows within minutes. A day is
/// generous — it covers "call me back after lunch" — and short enough that a
/// key nobody used is gone by tomorrow rather than sitting in a device list
/// for ever.
pub(crate) const INVITED_LASTS: u64 = 24 * 60 * 60;

/// Remove one-session pairings that are used up or have lapsed.
///
/// Run when a host starts. Ordinarily the session that spends a pairing also
/// removes it, but the case this exists for is the one where that did not
/// happen: a host that crashed mid-session, or an invitation nobody ever took
/// up. Neither can be used — a spent or lapsed pairing admits nothing — so
/// this is tidying rather than enforcement, and an operator should not have to
/// do it by hand.
///
/// Returns what was removed, so the host can say so.
pub(crate) fn sweep(now: u64) -> Vec<String> {
    let Ok((mut store, _)) = paths::devices() else {
        return Vec::new();
    };
    let names = sweep_store(&mut store, now);
    if !names.is_empty() && paths::save_devices(&store).is_err() {
        // Nothing is lost by failing: the pairings still admit nothing, and
        // the next start tries again.
        return Vec::new();
    }
    names
}

/// The part of [`sweep`] that is a decision rather than a file.
fn sweep_store(store: &mut DeviceStore, now: u64) -> Vec<String> {
    let over: Vec<[u8; 32]> = store
        .iter()
        .filter(|d| d.pairing.is_once() && !d.admits(now))
        .map(|d| d.public_key)
        .collect();
    over.iter()
        .filter_map(|key| store.remove(key).map(|gone| gone.name))
        .collect()
}

/// Record a newly paired device.
pub(crate) fn record(public_key: [u8; 32], name: &str) -> Result<Device, String> {
    record_as(public_key, name, false)
}

/// Record a device that was let in by an invitation: **one session, and gone**.
pub(crate) fn record_invited(public_key: [u8; 32], name: &str) -> Result<Device, String> {
    record_as(public_key, name, true)
}

/// Record a newly paired device, kept or invited.
fn record_as(public_key: [u8; 32], name: &str, invited: bool) -> Result<Device, String> {
    let (mut store, dropped) = paths::devices()?;
    for note in &dropped {
        eprintln!("  · the device list needed correcting: {note}");
    }
    // A peer-supplied name, so it is shown and never interpreted. The
    // fingerprint is the identity; the name is a label for a person.
    let shown = if name.trim().is_empty() {
        "an unnamed device".to_owned()
    } else {
        name.to_owned()
    };
    let now = paths::now_secs();
    let device = if invited {
        Device::invited(public_key, shown, now, now.saturating_add(INVITED_LASTS))
    } else {
        Device::new(public_key, shown, now)
    };
    // **A key change is a new device, and the old one's grants do not follow
    // it here.** That is the property somebody wants: a machine that was
    // reinstalled, or whose key was rotated, pairs afresh and starts with the
    // defaults — so `unattended` has to be granted again by somebody at this
    // host rather than inherited by whatever now holds that name.
    //
    // What does *not* happen on its own is the other half. The old record is
    // still there, still holding whatever it was granted, and this host cannot
    // link the two: a key is the only stable identifier a device has, so a
    // changed key is indistinguishable from an unknown one. A name is a label
    // a peer chose and two machines may share it.
    //
    // So it is said rather than guessed at. Removing the old record would be
    // this host deciding that two devices calling themselves `laptop` are the
    // same machine, and the case where that is wrong is the case where it
    // matters.
    for older in also_called(&store, &device.name, &public_key) {
        eprintln!(
            "  · a device called {:?} is already paired, with a different key: {}",
            device.name,
            older.fingerprint().hex()
        );
        eprintln!(
            "    If that is this machine, the old record still holds {} — remove it with:",
            granted_by(older)
        );
        eprintln!(
            "      {} forget {}",
            crate::net::me(),
            older.fingerprint().hex()
        );
    }

    store
        .insert(device.clone())
        .map_err(|e| format!("recording the device: {e}"))?;
    paths::save_devices(&store)?;

    // **Recorded, because a new device is the most consequential change
    // anybody makes to this machine's trust.** The counterpart is
    // `Event::Unpaired`, and a log with one and not the other reads as a
    // machine that has only ever gained devices.
    if let Ok(path) = paths::audit_log() {
        let entry = esper_audit::Entry::now(
            &public_key,
            esper_audit::Event::Paired {
                name: device.name.clone(),
            },
        );
        if let Err(e) = esper_audit::Log::at(path).append(&entry) {
            eprintln!("  · not recorded in the audit log: {e}");
        }
    } else {
        eprintln!("  · not recorded: there is nowhere to write the audit log");
    }

    Ok(device)
}

/// Tell the person what happened, and what to check.
pub(crate) fn report(device: &Device) {
    println!("  ✓ paired with {}", device.name);
    println!();
    println!("  Their fingerprint — it must match what the other screen shows:");
    for line in device.fingerprint().display().lines() {
        println!("      {line}");
    }
    println!();
    println!("  granted   {}", crate::net::permissions(device));
    if device.pairing.is_once() {
        // Said plainly at the moment it is created, because it is the
        // difference between this pairing and every other one on the machine.
        println!("  lasts     one session, and then it is gone");
        println!(
            "            keep it with:  {} keep {}",
            crate::net::me(),
            short(device)
        );
    }
    println!();
    println!(
        "  A pairing proves both sides used the same code. It cannot prove the \
         code reached the person you meant, which is what the fingerprint is for."
    );
}

/// Enough of a fingerprint to name a device on a command line.
fn short(device: &Device) -> String {
    device.fingerprint().hex().chars().take(12).collect()
}

/// Records with this name and a different key.
///
/// The one hint a host has that a device was re-paired rather than newly
/// paired, and it is only a hint: the name is peer-supplied and two machines
/// may share one. So it is used to *say something*, never to remove anything.
fn also_called<'a>(
    store: &'a esper_crypto::DeviceStore,
    name: &str,
    key: &[u8; 32],
) -> Vec<&'a esper_crypto::Device> {
    store
        .iter()
        .filter(|d| d.name == name && &d.public_key != key)
        .collect()
}

/// What a device is allowed to do, for a sentence about it.
fn granted_by(device: &esper_crypto::Device) -> String {
    if device.permissions.is_empty() {
        return "nothing".to_owned();
    }
    device
        .permissions
        .iter()
        .map(|p| p.name())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use esper_crypto::Identity;

    use super::*;

    fn device_named(name: &str) -> Device {
        let id = Identity::generate().expect("a key");
        Device::new(id.public_key(), name.to_owned(), 0)
    }

    fn invited_named(name: &str, expires_at: u64) -> Device {
        let id = Identity::generate().expect("a key");
        Device::invited(id.public_key(), name.to_owned(), 0, expires_at)
    }

    #[test]
    fn a_sweep_takes_only_the_invitations_that_are_over() {
        let mut store = DeviceStore::new();
        store.insert(device_named("laptop")).unwrap();
        store.insert(invited_named("still-good", 100)).unwrap();
        store.insert(invited_named("lapsed", 10)).unwrap();
        let mut used = invited_named("used", 100);
        used.spend();
        store.insert(used).unwrap();

        let gone = sweep_store(&mut store, 50);
        assert_eq!(gone.len(), 2, "{gone:?}");
        assert!(gone.contains(&"lapsed".to_owned()));
        assert!(gone.contains(&"used".to_owned()));

        let left: Vec<String> = store.iter().map(|d| d.name.clone()).collect();
        assert!(
            left.contains(&"laptop".to_owned()),
            "a kept pairing was swept"
        );
        assert!(
            left.contains(&"still-good".to_owned()),
            "an invitation nobody has used yet was swept"
        );
    }

    #[test]
    fn a_sweep_of_nothing_removes_nothing() {
        let mut store = DeviceStore::new();
        store.insert(device_named("laptop")).unwrap();
        assert!(sweep_store(&mut store, u64::MAX).is_empty());
        assert_eq!(store.iter().count(), 1);
    }

    /// A real key, because the store refuses one that is not a curve point —
    /// which is the right refusal and means a test cannot use `[3u8; 32]`.
    fn a_key() -> [u8; 32] {
        esper_crypto::Identity::generate()
            .expect("a key")
            .public_key()
    }

    /// **A key change does not carry the grants over, because it is a new
    /// device.**
    ///
    /// The property the tracker asks for, and it holds by construction rather
    /// than by a check: the store is keyed by the public key, so a device that
    /// pairs with a different one gets a fresh record with the defaults. What
    /// this test is for is that "by construction" stays true — a store keyed
    /// by anything else, or a pairing that copied an existing record, would
    /// hand `unattended` to whatever now holds that name.
    #[test]
    fn a_device_that_pairs_with_a_new_key_starts_from_the_defaults() {
        let (old, new) = (a_key(), a_key());
        let mut store = esper_crypto::DeviceStore::default();
        let mut before = esper_crypto::Device::new(old, "laptop".to_owned(), 0);
        before.permissions.push(esper_proto::Permission::Unattended);
        store.insert(before).expect("a usable key");

        // The same machine, reinstalled: same name, different key.
        store
            .insert(esper_crypto::Device::new(new, "laptop".to_owned(), 1))
            .expect("a usable key");

        let now = store.get(&new).expect("the new record");
        assert!(
            !now.may(esper_proto::Permission::Unattended),
            "a re-paired device inherited unattended: {:?}",
            now.permissions
        );
        assert!(
            now.may(esper_proto::Permission::View),
            "{:?}",
            now.permissions
        );
    }

    /// And the old record is still there, still granted, which is the half a
    /// host cannot fix on its own.
    ///
    /// A key is the only stable identifier a device has, so a changed key is
    /// indistinguishable from an unknown one — see the ledger entry for
    /// `Reason::PEER_KEY_CHANGED`. If the old private key survives anywhere, a
    /// backup or a cloned disk, it still walks in. So the name collision is
    /// *said*, with the fingerprint and the command that removes it.
    #[test]
    fn the_record_for_the_old_key_is_found_and_named() {
        let (old, new, other) = (a_key(), a_key(), a_key());
        let mut store = esper_crypto::DeviceStore::default();
        let mut before = esper_crypto::Device::new(old, "laptop".to_owned(), 0);
        before.permissions.push(esper_proto::Permission::Unattended);
        store.insert(before).expect("a usable key");
        store
            .insert(esper_crypto::Device::new(other, "desktop".to_owned(), 0))
            .expect("a usable key");

        let older = also_called(&store, "laptop", &new);
        assert_eq!(older.len(), 1, "the old record was not found");
        let found = older.first().expect("one");
        assert_eq!(found.public_key, old);
        assert!(
            granted_by(found).contains("unattended"),
            "{}",
            granted_by(found)
        );

        // The device that is pairing is not itself a collision, or every
        // pairing would warn about the record it is replacing.
        assert!(
            also_called(&store, "laptop", &old).is_empty(),
            "a device collided with its own record"
        );
        // And a different name is not a collision.
        assert!(also_called(&store, "phone", &new).is_empty());
    }

    /// A record granted nothing says "nothing" rather than an empty sentence.
    #[test]
    fn a_record_with_no_permissions_reads_as_nothing() {
        let mut bare = esper_crypto::Device::new(a_key(), "bare".to_owned(), 0);
        bare.permissions.clear();
        assert_eq!(granted_by(&bare), "nothing");
    }
}
