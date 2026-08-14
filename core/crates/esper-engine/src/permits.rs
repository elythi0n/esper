//! What every paired device may do, watched while sessions are open.
//!
//! # Why a session has to be told twice
//!
//! Permissions are checked when a session opens. That is not enough: taking a
//! permission away from a device that is *currently connected* has to reach the
//! connection it already has. A revocation that leaves the revoked device
//! driving the machine until it chooses to leave is not a revocation, it is a
//! request.
//!
//! # Why the store, rather than a signal or a socket
//!
//! `esper-engine revoke` runs in a different process from `esper-engine host`,
//! and the only thing the two already share is the device store — which is the
//! source of truth for both of them. So the host reads it while it runs, and a
//! change made from any process, by any means, including an editor, takes
//! effect. Nothing has to know where the host is or that it is running.
//!
//! The cost is a stat and a small read twice a second, and the benefit is that
//! there is no second path by which permissions can change and no daemon
//! protocol to keep in step with the file.

use std::sync::Arc;
use std::time::Duration;

use esper_crypto::DeviceStore;
use esper_crypto::identity::KEY_LEN;
use esper_proto::Permission;

use crate::paths;

/// How often the store is re-read.
///
/// Half a second: far below what a person notices when they click "revoke",
/// and far above what the cost of reading a few hundred bytes of JSON could
/// matter at. Polling rather than watching the filesystem because it is the
/// same on every platform and cannot miss an event.
const INTERVAL: Duration = Duration::from_millis(500);

/// What every paired device may do, as of one reading of the store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Permits {
    devices: Vec<Entry>,
}

/// One device in a reading of the store.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    key: [u8; KEY_LEN],
    permissions: Vec<Permission>,
    /// How long the pairing lasts. Kept apart from the permissions because it
    /// answers a different question: **whether a new connection is allowed at
    /// all**, rather than what one may do once it is open. A pairing that has
    /// been spent still holds its permissions for the session it is running,
    /// and admits nothing further.
    pairing: esper_crypto::Pairing,
}

/// What the store now says about a session that is already open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// Everything it was granted, it still holds.
    Unchanged,
    /// These were taken away. The session holds the rest.
    Narrowed(Vec<Permission>),
    /// The device is not paired any more.
    Forgotten,
}

impl Permits {
    /// Read the store.
    ///
    /// A store that is **absent** is an empty one: `paths::devices` says the
    /// same, and it is what deleting the file means. A store that cannot be
    /// *parsed* is an error, and the caller keeps the last good reading rather
    /// than treating a broken file as "nobody is paired" — one bad character
    /// must not end every session on the machine.
    pub fn load() -> Result<Self, String> {
        let path = paths::devices_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let (store, _corrected) =
            DeviceStore::load(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        Ok(Self::of_store(&store))
    }

    /// The snapshot a store makes.
    fn of_store(store: &DeviceStore) -> Self {
        Self {
            devices: store
                .iter()
                .map(|d| Entry {
                    key: d.public_key,
                    permissions: d.permissions.clone(),
                    pairing: d.pairing,
                })
                .collect(),
        }
    }

    /// What one device may do, or `None` if it is not paired.
    #[must_use]
    pub fn of(&self, key: &[u8; KEY_LEN]) -> Option<&[Permission]> {
        self.devices
            .iter()
            .find(|e| e.key == *key)
            .map(|e| e.permissions.as_slice())
    }

    /// Whether this device may open a **new** connection, as of `now`.
    ///
    /// A different question from [`Permits::of`], and asked at a different
    /// moment: this one gates the handshake, and `of` describes a session that
    /// is already running. A one-session pairing that has been spent answers
    /// no here and still answers with its permissions there — which is what
    /// lets the session it was spent on finish.
    #[must_use]
    pub fn admits(&self, key: &[u8; KEY_LEN], now: u64) -> bool {
        self.devices
            .iter()
            .find(|e| e.key == *key)
            .is_some_and(|e| e.pairing.admits(now))
    }

    /// Devices that held a permission in this reading and do not in `now`.
    ///
    /// Includes devices that are no longer there at all: unpairing takes away
    /// everything, and a caller acting on "may no longer do X" wants both.
    #[must_use]
    pub fn lost_since(&self, now: &Self, p: Permission) -> Vec<[u8; KEY_LEN]> {
        self.devices
            .iter()
            .filter(|e| e.permissions.contains(&p))
            .filter(|e| !now.of(&e.key).is_some_and(|has| has.contains(&p)))
            .map(|e| e.key)
            .collect()
    }

    /// How an open session stands against this reading.
    ///
    /// **Only removals count.** A device that gained a permission mid-session
    /// keeps the session it was granted: widening a live session without the
    /// controller asking would hand it something it never requested, and the
    /// next session gets the wider set anyway.
    #[must_use]
    pub fn standing(&self, key: &[u8; KEY_LEN], granted: &[Permission]) -> Standing {
        let Some(now) = self.of(key) else {
            return Standing::Forgotten;
        };
        let lost: Vec<Permission> = granted
            .iter()
            .copied()
            .filter(|p| !now.contains(p))
            .collect();
        if lost.is_empty() {
            Standing::Unchanged
        } else {
            Standing::Narrowed(lost)
        }
    }
}

/// A live view of the store, updated while the host runs.
#[derive(Debug, Clone)]
pub struct Watching {
    now: tokio::sync::watch::Receiver<Arc<Permits>>,
    /// Where a reading taken outside the poll is published.
    ///
    /// Held alongside the receiver rather than kept in the polling task
    /// because the moment a session opens is a moment worth reading at, and
    /// only the session knows when that is — see [`Watching::reread`].
    tell: Arc<tokio::sync::watch::Sender<Arc<Permits>>>,
}

impl Watching {
    /// The store as of the last reading.
    #[must_use]
    pub fn borrow(&self) -> tokio::sync::watch::Ref<'_, Arc<Permits>> {
        self.now.borrow()
    }

    /// The store as of the last reading, and mark it seen.
    pub fn borrow_and_update(&mut self) -> tokio::sync::watch::Ref<'_, Arc<Permits>> {
        self.now.borrow_and_update()
    }

    /// Wait until the reading changes.
    ///
    /// # Errors
    ///
    /// When the host has stopped reading the store.
    pub async fn changed(&mut self) -> Result<(), tokio::sync::watch::error::RecvError> {
        self.now.changed().await
    }

    /// Read the store **now**, rather than waiting for the next poll.
    ///
    /// **For the moment a session opens, which is the one moment the poll is
    /// not good enough for.** A session is agreed against its own fresh read
    /// of the store and then *enforced* against this reading, which is up to
    /// [`INTERVAL`] behind it. So a permission granted a moment before the
    /// session opened is in the agreement and not yet in the enforcement: the
    /// host tells the device it holds `forward`, and then tells it that it is
    /// not permitted to forward. Half a second of a host contradicting itself
    /// to the one device in a position to notice.
    ///
    /// Reading here closes that window by construction rather than by being
    /// quick about it — this reading is taken *after* the one the agreement
    /// used, so it cannot be older than the decision it is enforcing.
    ///
    /// A store that cannot be read leaves the last reading in place, which is
    /// what the poll does with the same problem and for the same reason.
    pub fn reread(&self) {
        let Ok(now) = Permits::load() else {
            return;
        };
        publish(&self.tell, now);
    }
}

/// Publish a reading, waking anybody waiting only if it says something new.
///
/// Otherwise every session watching the store is woken twice a second for as
/// long as the host runs.
fn publish(tell: &tokio::sync::watch::Sender<Arc<Permits>>, now: Permits) {
    tell.send_if_modified(|held| {
        if **held == now {
            false
        } else {
            *held = Arc::new(now);
            true
        }
    });
}

/// Start reading the store, and publish every change.
///
/// The first reading is taken **before** this returns, so a session that
/// starts immediately is checked against the store rather than against an
/// empty placeholder.
pub fn watch(mut stopping: tokio::sync::watch::Receiver<bool>) -> Watching {
    let first = Permits::load().unwrap_or_else(|e| {
        eprintln!("  · could not read the device list ({e}); permissions will not be watched");
        Permits::default()
    });
    let (tx, rx) = tokio::sync::watch::channel(Arc::new(first));
    let tell = Arc::new(tx);
    let polling = Arc::clone(&tell);

    tokio::spawn(async move {
        let mut complained = false;
        loop {
            tokio::select! {
                _ = stopping.changed() => return,
                () = tokio::time::sleep(INTERVAL) => {}
            }
            match Permits::load() {
                Ok(now) => {
                    complained = false;
                    publish(&polling, now);
                }
                Err(e) => {
                    // Said once, and the last good reading is kept. A file
                    // being edited by hand is momentarily whatever the editor
                    // has written so far.
                    if !complained {
                        complained = true;
                        eprintln!("  · could not read the device list ({e}); keeping the last one");
                    }
                }
            }
            // A receiver going away means no session is open and none will
            // be: the host has stopped.
            if polling.is_closed() {
                return;
            }
        }
    });

    Watching { now: rx, tell }
}

/// What one session's device may do, read from the store every time it is
/// asked.
///
/// **The intersection of what the session was granted and what the device
/// holds now**, so a change in the store can only ever shrink it. Widening a
/// live session from the *store* would hand a controller something it never
/// asked for and never rendered a control for.
///
/// # The one way it widens
///
/// Somebody at the machine answering a question about this session, and
/// nothing else. That set is kept apart from the store's — see
/// [`Live::allow_here`] — because the two answer different questions and
/// mixing them would break both: a permission allowed for one session is not
/// in the store, so comparing the whole set against the store would read as a
/// withdrawal and end the session a moment after it was widened.
#[derive(Debug, Clone)]
pub struct Live {
    permits: Watching,
    device: [u8; KEY_LEN],
    granted: Vec<Permission>,
    /// Allowed by a person at this machine, for this session and no other.
    ///
    /// A channel rather than a field, because every clone of a `Live` is a
    /// different task's view of the same session — the one that injects input
    /// is not the one that asked the question — and an answer that reached
    /// only the task that heard it would be an answer that changed nothing.
    allowed_here: Arc<tokio::sync::watch::Sender<Vec<Permission>>>,
}

impl Live {
    /// A live view of one session's permissions.
    #[must_use]
    pub fn new(permits: Watching, device: [u8; KEY_LEN], granted: Vec<Permission>) -> Self {
        Self {
            permits,
            device,
            granted,
            allowed_here: Arc::new(tokio::sync::watch::Sender::new(Vec::new())),
        }
    }

    /// Allow one more thing, because somebody at this machine said so.
    ///
    /// **For this session only, and deliberately not written to the store.**
    /// The question a person answered was about the session in front of them;
    /// a permission that outlived it would be one they never agreed to, and
    /// the way to grant one of those is `esper grant`, which is a decision
    /// somebody makes deliberately and can review afterwards.
    ///
    /// Idempotent: allowing what is already allowed changes nothing and wakes
    /// nobody.
    ///
    /// # The limit, stated rather than papered over
    ///
    /// It cannot be taken back on its own. `revoke` works on the store and
    /// this is not in the store, so somebody who changes their mind ends the
    /// session — the panic key, or taking away the `view` that the session is
    /// made of. That is the same answer the product gives for "stop this now"
    /// everywhere else, and the prompt says the allowance lasts for the
    /// session, so it is what somebody agreed to.
    pub fn allow_here(&self, p: Permission) {
        self.allowed_here.send_if_modified(|allowed| {
            if allowed.contains(&p) {
                false
            } else {
                allowed.push(p);
                true
            }
        });
    }

    /// Wait until the store changes.
    ///
    /// The wake-up, so a session can act on a revocation rather than
    /// discovering it the next time it happens to look.
    pub async fn changed(&mut self) {
        // An error means the watcher has stopped, which happens when the host
        // is shutting down — and then the session is ending anyway. Waiting
        // for ever is the right answer: the branch that owns the shutdown is
        // the one that should end this session.
        if self.permits.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }

    /// How this session stands against the store as it is now.
    #[must_use]
    pub fn standing(&self) -> Standing {
        self.permits.borrow().standing(&self.device, &self.granted)
    }
}

impl esper_transport::Grants for Live {
    fn granted(&self) -> Vec<Permission> {
        let permits = self.permits.borrow();
        let Some(now) = permits.of(&self.device) else {
            // Not paired any more, so nothing at all — not "what it used to
            // have", and not what somebody allowed for this session either.
            // Unpairing a device takes back every answer anybody gave about
            // it. This is also the answer that stops a revoked device's shell
            // being parked for it to come back to.
            return Vec::new();
        };
        let mut held: Vec<Permission> = self
            .granted
            .iter()
            .copied()
            .filter(|p| now.contains(p))
            .collect();
        // **And what a person allowed for this session.** Added rather than
        // intersected: it is not in the store and never will be, so an
        // intersection would drop it.
        for p in self.allowed_here.borrow().iter() {
            if !held.contains(p) {
                held.push(*p);
            }
        }
        held
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn permits(devices: &[([u8; KEY_LEN], &[Permission])]) -> Permits {
        Permits {
            devices: devices
                .iter()
                .map(|(k, p)| Entry {
                    key: *k,
                    permissions: p.to_vec(),
                    pairing: esper_crypto::Pairing::Kept,
                })
                .collect(),
        }
    }

    /// A reading taken outside the poll reaches a session that is already open.
    ///
    /// **The narrowing is meant to be a change made *since* the session
    /// opened.** It was in fact a comparison against whatever the poll last
    /// happened to see, which can predate the session — and then a permission
    /// granted moments before the session was agreed reads as one that was
    /// taken away, on a session that was told it holds it. Half a second of
    /// the host refusing the device the thing it had just granted it.
    ///
    /// The filesystem half of this lives in the smoke suite, because
    /// `ESPER_HOME` is the only way to point the store somewhere and setting
    /// an environment variable is `unsafe` in this edition. What is here is
    /// the part that decides.
    #[test]
    fn a_session_is_not_narrowed_by_a_reading_older_than_its_grant() {
        let key = [1u8; KEY_LEN];
        // The store as the poll last saw it: this device holds nothing.
        let watching = watching(permits(&[(key, &[])]));
        // The session was agreed against a newer store, in which it holds one.
        let live = Live::new(watching.clone(), key, vec![Permission::Forward]);
        assert!(
            esper_transport::Grants::granted(&live).is_empty(),
            "the stale reading is the situation this is about; without it the \
             rest of this test proves nothing"
        );

        publish(&watching.tell, permits(&[(key, &[Permission::Forward])]));

        assert_eq!(
            esper_transport::Grants::granted(&live),
            vec![Permission::Forward],
            "a reading taken when the session opened has to reach the session"
        );
    }

    /// A `Watching` holding one reading and nothing polling it.
    fn watching(now: Permits) -> Watching {
        let (tx, rx) = tokio::sync::watch::channel(Arc::new(now));
        Watching {
            now: rx,
            tell: Arc::new(tx),
        }
    }

    fn invited(key: [u8; KEY_LEN], used: bool, expires_at: u64) -> Permits {
        Permits {
            devices: vec![Entry {
                key,
                permissions: vec![Permission::View],
                pairing: esper_crypto::Pairing::Once { used, expires_at },
            }],
        }
    }

    #[test]
    fn a_spent_invitation_admits_nothing_and_still_describes_its_session() {
        // The two questions this type answers, and why they are separate: the
        // handshake asks whether a *new* connection is allowed, and a session
        // that is already running asks what it may do. A pairing spent by the
        // session it is running must answer no to the first and still answer
        // the second — otherwise spending it would end it.
        let key = [4u8; KEY_LEN];
        let p = invited(key, true, u64::MAX);
        assert!(!p.admits(&key, 0), "a spent invitation let another in");
        assert_eq!(p.of(&key), Some(&[Permission::View][..]));
        assert_eq!(p.standing(&key, &[Permission::View]), Standing::Unchanged);
    }

    #[test]
    fn an_unused_invitation_admits_one_and_lapses_on_time() {
        let key = [5u8; KEY_LEN];
        let p = invited(key, false, 100);
        assert!(p.admits(&key, 99));
        assert!(!p.admits(&key, 100), "it must lapse at its deadline");
    }

    #[test]
    fn a_device_that_is_not_there_admits_nothing() {
        assert!(!permits(&[]).admits(&[7u8; KEY_LEN], 0));
    }

    #[test]
    fn a_session_whose_device_still_holds_everything_is_unchanged() {
        let p = permits(&[([1u8; 32], &[Permission::View, Permission::Control])]);
        assert_eq!(
            p.standing(&[1u8; 32], &[Permission::View, Permission::Control]),
            Standing::Unchanged
        );
    }

    #[test]
    fn a_withdrawn_permission_is_reported_by_name() {
        let p = permits(&[([1u8; 32], &[Permission::View])]);
        assert_eq!(
            p.standing(&[1u8; 32], &[Permission::View, Permission::Control]),
            Standing::Narrowed(vec![Permission::Control])
        );
    }

    #[test]
    fn a_device_that_is_no_longer_paired_is_forgotten_not_narrowed() {
        // The distinction matters to the message somebody reads afterwards:
        // one is "you may do less", the other is "we have never met".
        let p = permits(&[([2u8; 32], &[Permission::View])]);
        assert_eq!(
            p.standing(&[1u8; 32], &[Permission::View]),
            Standing::Forgotten
        );
        assert_eq!(p.standing(&[1u8; 32], &[]), Standing::Forgotten);
    }

    #[test]
    fn an_empty_store_forgets_everybody() {
        // Deleting the device list is unpairing every device, and a host that
        // read it as "no change" would keep serving the sessions it had.
        let p = Permits::default();
        assert_eq!(
            p.standing(&[1u8; 32], &[Permission::View]),
            Standing::Forgotten
        );
    }

    /// **A grant at the machine does not reach a session already running.**
    /// The one thing that widens one is somebody answering a question about
    /// it — see `allow_here` and the test below — and that is deliberately a
    /// different path: this one nobody asked for.
    #[test]
    fn a_grant_in_the_store_does_not_widen_a_session_already_open() {
        let p = permits(&[(
            [1u8; 32],
            &[Permission::View, Permission::Control, Permission::Terminal],
        )]);
        assert_eq!(
            p.standing(&[1u8; 32], &[Permission::View]),
            Standing::Unchanged,
            "a live session must not be widened behind the controller's back"
        );
    }

    #[test]
    fn losing_a_permission_and_being_forgotten_both_count_as_losing_it() {
        let before = permits(&[
            ([1u8; 32], &[Permission::View, Permission::Terminal]),
            ([2u8; 32], &[Permission::Terminal]),
            ([3u8; 32], &[Permission::View]),
        ]);
        let after = permits(&[
            // 1 kept it, 2 was forgotten, 3 never had it.
            ([1u8; 32], &[Permission::View, Permission::Terminal]),
            ([3u8; 32], &[Permission::View]),
        ]);
        assert_eq!(
            before.lost_since(&after, Permission::Terminal),
            vec![[2u8; 32]]
        );

        // And a narrowing, not only a forgetting.
        let narrowed = permits(&[
            ([1u8; 32], &[Permission::View]),
            ([2u8; 32], &[Permission::Terminal]),
            ([3u8; 32], &[Permission::View]),
        ]);
        assert_eq!(
            before.lost_since(&narrowed, Permission::Terminal),
            vec![[1u8; 32]]
        );

        // Nothing changed, nothing lost.
        assert!(before.lost_since(&before, Permission::Terminal).is_empty());
    }

    /// **The one thing that widens a live session, and the trap in it.**
    ///
    /// A permission somebody allowed for one session is not in the store — and
    /// `standing` compares the session against the store. So if the two sets
    /// were kept together, a session widened at the machine would report
    /// itself *narrowed* a moment later and end. They are kept apart, and this
    /// holds them apart.
    #[test]
    fn what_a_person_allows_for_one_session_widens_it_and_is_not_a_narrowing() {
        use esper_transport::Grants as _;

        let live = Live::new(
            watching(permits(&[([1u8; 32], &[Permission::View])])),
            [1u8; 32],
            vec![Permission::View],
        );
        assert_eq!(live.standing(), Standing::Unchanged);

        live.allow_here(Permission::Control);
        assert_eq!(live.granted(), vec![Permission::View, Permission::Control]);
        assert!(live.allows(Permission::Control));
        assert_eq!(
            live.standing(),
            Standing::Unchanged,
            "a session widened at the machine must not read as one that lost something"
        );

        // Twice is once: a controller that asked again gains nothing and the
        // set does not grow a duplicate.
        live.allow_here(Permission::Control);
        assert_eq!(live.granted(), vec![Permission::View, Permission::Control]);
    }

    /// **Every clone of a `Live` is the same session**, and the task that
    /// injects input is not the one that heard the answer.
    #[test]
    fn allowing_it_on_one_handle_allows_it_on_every_other() {
        use esper_transport::Grants as _;

        let live = Live::new(
            watching(permits(&[([1u8; 32], &[Permission::View])])),
            [1u8; 32],
            vec![Permission::View],
        );
        let injecting = live.clone();
        assert!(!injecting.allows(Permission::Control));
        live.allow_here(Permission::Control);
        assert!(
            injecting.allows(Permission::Control),
            "the task that injects would have gone on refusing keystrokes"
        );
    }

    /// Unpairing takes back every answer anybody gave about a device,
    /// including one that was only ever about this session.
    #[test]
    fn unpairing_takes_back_what_was_allowed_for_the_session_too() {
        use esper_transport::Grants as _;

        let store = watching(permits(&[([1u8; 32], &[Permission::View])]));
        let live = Live::new(store.clone(), [1u8; 32], vec![Permission::View]);
        live.allow_here(Permission::Control);
        assert!(live.allows(Permission::Control));

        publish(&store.tell, permits(&[]));
        assert!(live.granted().is_empty());
        assert_eq!(live.standing(), Standing::Forgotten);
    }

    #[test]
    fn a_live_view_shrinks_when_the_store_does_and_never_grows() {
        use esper_transport::Grants as _;

        let store = watching(permits(&[(
            [1u8; 32],
            &[Permission::View, Permission::Control],
        )]));
        let live = Live::new(
            store.clone(),
            [1u8; 32],
            vec![Permission::View, Permission::Control],
        );
        assert_eq!(live.granted(), vec![Permission::View, Permission::Control]);

        // Narrowed at the machine.
        publish(&store.tell, permits(&[([1u8; 32], &[Permission::View])]));
        assert_eq!(live.granted(), vec![Permission::View]);
        assert!(!live.allows(Permission::Control));

        // Widened beyond what this session asked for: still what it asked for.
        publish(
            &store.tell,
            permits(&[(
                [1u8; 32],
                &[Permission::View, Permission::Control, Permission::Terminal],
            )]),
        );
        assert_eq!(
            live.granted(),
            vec![Permission::View, Permission::Control],
            "a live session must not gain what it never requested"
        );

        // Forgotten entirely.
        publish(&store.tell, Permits::default());
        assert!(
            live.granted().is_empty(),
            "a device that is no longer paired may do nothing, not what it used to"
        );
        assert_eq!(live.standing(), Standing::Forgotten);
    }

    #[test]
    fn a_narrowing_names_every_permission_it_took() {
        let p = permits(&[([1u8; 32], &[Permission::View])]);
        match p.standing(
            &[1u8; 32],
            &[Permission::View, Permission::Control, Permission::Audio],
        ) {
            Standing::Narrowed(lost) => {
                assert!(lost.contains(&Permission::Control));
                assert!(lost.contains(&Permission::Audio));
                assert!(!lost.contains(&Permission::View));
            }
            other => panic!("expected a narrowing, got {other:?}"),
        }
    }
}
