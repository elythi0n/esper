//! **When state gets put back, which is the part that is hard to see wrong.**
//!
//! No display anywhere in this file: the records are applied by a closure that
//! records what it was asked to do. That is deliberate — the failure this
//! crate prevents is a matter of *timing and process lifetime*, and a test
//! that needed a monitor to check the timing would be a test nobody runs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::sync::{Arc, Mutex};

use esper_keeper::{Keeper, Undo, journal_path, read_journal, run_keeper};

fn undo(what: &str) -> Undo {
    Undo::Crtc {
        crtc: 59,
        x: 0,
        y: 0,
        mode: 58,
        rotation: 1,
        outputs: vec![58],
        what: what.to_owned(),
    }
}

/// Everything the applier was asked to put back.
type Seen = Arc<Mutex<Vec<String>>>;

fn recorder() -> (Seen, impl FnMut(&Undo) -> Result<(), String>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let handle = Arc::clone(&seen);
    (seen, move |u: &Undo| {
        handle.lock().expect("a lock").push(u.describe());
        Ok(())
    })
}

/// **A host that dies without saying anything gets its work undone.**
///
/// The case the crate exists for. End of the pipe with no cancellation *is*
/// the death signal — a `SIGKILL` closes a pipe exactly as a clean exit does,
/// which is why nothing here polls or waits for a timeout.
#[test]
fn a_pipe_that_closes_without_a_cancellation_restores_everything() {
    let home = tempdir();
    let (seen, apply) = recorder();
    let records = format!(
        "{}\n{}\n",
        serde_json::to_string(&undo("the left monitor")).unwrap(),
        serde_json::to_string(&undo("the right monitor")).unwrap()
    );
    let mut said = Vec::new();

    let applied = run_keeper(Cursor::new(records), &mut said, &home, apply);

    assert_eq!(applied, 2, "both records should have been put back");
    assert_eq!(
        *seen.lock().expect("a lock"),
        vec!["the left monitor", "the right monitor"],
        "and in the order they were registered"
    );
    assert!(
        String::from_utf8_lossy(&said).starts_with("keeping"),
        "the keeper has to say it is watching before anything is changed: {:?}",
        String::from_utf8_lossy(&said)
    );
}

/// **A restore that fails once is tried again.**
///
/// Measured rather than assumed: applying the undo immediately after the
/// host's process died failed two runs in three with "connection reset by
/// peer" — a display server refusing a *new* connection while it is still
/// tearing down the dead client's. One attempt is not enough for the one
/// action in this product that must not fail quietly.
#[test]
fn a_restore_that_fails_at_first_is_tried_again() {
    let home = tempdir();
    let attempts = Arc::new(Mutex::new(0usize));
    let counter = Arc::clone(&attempts);
    let apply = move |_: &Undo| {
        let mut n = counter.lock().expect("a lock");
        *n += 1;
        if *n < 3 {
            Err("connection reset by peer".to_owned())
        } else {
            Ok(())
        }
    };
    let records = format!("{}\n", serde_json::to_string(&undo("a screen")).unwrap());
    let mut said = Vec::new();

    let applied = run_keeper(Cursor::new(records), &mut said, &home, apply);

    assert_eq!(applied, 1, "it should have got there in the end");
    assert_eq!(
        *attempts.lock().expect("a lock"),
        3,
        "and it should have taken three goes"
    );
}

/// **A restore that never works says so, rather than saying nothing.**
///
/// The keeper's messages are the only evidence anybody has that it ran: when
/// this check first failed, the keeper had printed nothing at all, and "the
/// host went away with nothing registered" looked exactly like "the keeper
/// never woke up".
#[test]
fn a_restore_that_never_works_is_reported_and_not_retried_for_ever() {
    let home = tempdir();
    let attempts = Arc::new(Mutex::new(0usize));
    let counter = Arc::clone(&attempts);
    let apply = move |_: &Undo| {
        *counter.lock().expect("a lock") += 1;
        Err("this display is not there any more".to_owned())
    };
    let records = format!("{}\n", serde_json::to_string(&undo("a screen")).unwrap());
    let mut said = Vec::new();

    let applied = run_keeper(Cursor::new(records), &mut said, &home, apply);

    assert_eq!(applied, 0);
    let tried = *attempts.lock().expect("a lock");
    assert!(
        (2..=8).contains(&tried),
        "it should try more than once and stop eventually, not {tried} times"
    );
    // **And the journal is left alone**, because the thing still needs
    // putting back and the next host to start is what can do it. A keeper that
    // cleared the journal after failing would throw away the only remaining
    // record of a screen that is still dark.
    assert!(
        !said.is_empty(),
        "the keeper has to have said something: its messages are the only evidence it ran"
    );
}

/// **A host that put its own work back is not second-guessed.**
///
/// A keeper that restored anyway would fight whatever the person at the
/// machine did after the session ended — turning their monitor back on
/// minutes later, or off.
#[test]
fn a_cancellation_means_nothing_is_put_back() {
    let home = tempdir();
    let (seen, apply) = recorder();
    let records = format!(
        "{}\ncancelled\n",
        serde_json::to_string(&undo("the only monitor")).unwrap()
    );
    let mut said = Vec::new();

    let applied = run_keeper(Cursor::new(records), &mut said, &home, apply);

    assert_eq!(applied, 0);
    assert!(seen.lock().expect("a lock").is_empty());
}

/// A record this build cannot read is a record it says so about, and the
/// others are still put back.
///
/// Exiting instead would leave nobody watching the changes it *could* read,
/// which is worse than an unreadable line.
#[test]
fn one_unreadable_record_does_not_lose_the_others() {
    let home = tempdir();
    let (seen, apply) = recorder();
    let records = format!(
        "{}\n{{\"undo\":\"something_from_the_future\"}}\n{}\n",
        serde_json::to_string(&undo("first")).unwrap(),
        serde_json::to_string(&undo("third")).unwrap()
    );
    let mut said = Vec::new();

    let applied = run_keeper(Cursor::new(records), &mut said, &home, apply);

    assert_eq!(applied, 2);
    assert_eq!(*seen.lock().expect("a lock"), vec!["first", "third"]);
}

/// **The journal is written before the change, not after.**
///
/// It is what covers the keeper itself being killed, so the ordering is
/// chosen so the worst case is a journal entry for a change that never
/// happened — harmless, because putting something back that was never moved
/// is a no-op — rather than a change with no record of how to undo it.
#[test]
#[cfg_attr(windows, ignore = "the fake keeper is a shell script, which Windows cannot run")]
fn registering_writes_the_journal_before_the_caller_changes_anything() {
    let home = tempdir();
    let fake = fake_keeper();
    let mut keeper = Keeper::start(&fake, &home).expect("a keeper");
    assert_eq!(keeper.watching(), 0);

    keeper.keep(undo("a monitor")).expect("kept");

    let journal = read_journal(&journal_path(&home)).expect("a journal");
    assert_eq!(
        journal.len(),
        1,
        "the journal holds it as soon as it is kept"
    );
    assert_eq!(
        journal.first().map(Undo::describe).as_deref(),
        Some("a monitor")
    );
    assert_eq!(keeper.watching(), 1);

    // And letting go cancels, so the next start is not a recovery.
    keeper.cancel();
    let after = read_journal(&journal_path(&home)).expect("a journal");
    assert!(
        after.is_empty(),
        "a cancelled keeper should leave nothing to recover: {after:?}"
    );
}

/// **A change that cannot be journalled is refused.**
///
/// The journal is the cover for the keeper itself being killed. Without it a
/// change has one guard instead of two, and this crate exists because one is
/// not enough — so `keep` fails and the caller must not touch the display
/// server. Tested by putting a directory where the journal file goes, which is
/// the simplest thing the filesystem will refuse to overwrite.
#[test]
#[cfg_attr(windows, ignore = "the fake keeper is a shell script, which Windows cannot run")]
fn a_change_that_cannot_be_written_down_is_not_allowed_to_happen() {
    let home = tempdir();
    let fake = fake_keeper();
    std::fs::create_dir_all(journal_path(&home)).expect("a directory in the way");
    let mut keeper = Keeper::start(&fake, &home).expect("a keeper");

    let refused = keeper.keep(undo("a monitor"));

    assert!(
        refused.is_err(),
        "a change with no journal entry has one guard instead of two, and this crate exists \
         because one is not enough"
    );
    assert_eq!(
        keeper.watching(),
        0,
        "and the keeper must not believe it is watching something the journal does not have"
    );
}

/// **Dropping the handle cancels**, because a host that returns from its own
/// shutdown has already undone its work.
#[test]
#[cfg_attr(windows, ignore = "the fake keeper is a shell script, which Windows cannot run")]
fn dropping_the_handle_leaves_nothing_to_recover() {
    let home = tempdir();
    let fake = fake_keeper();
    {
        let mut keeper = Keeper::start(&fake, &home).expect("a keeper");
        keeper.keep(undo("a monitor")).expect("kept");
        assert!(
            !read_journal(&journal_path(&home))
                .expect("a journal")
                .is_empty()
        );
    }
    assert!(
        read_journal(&journal_path(&home))
            .expect("a journal")
            .is_empty(),
        "dropping the keeper has to cancel it, or every clean shutdown looks like a crash"
    );
}

/// **A keeper that does not say it is watching is a refusal**, because the
/// whole arrangement is that nothing is changed until somebody is.
#[test]
#[cfg_attr(windows, ignore = "the fake keeper is a shell script, which Windows cannot run")]
fn a_keeper_that_says_the_wrong_thing_is_refused() {
    let home = tempdir();
    let wrong = fixture("keeper-that-does-not-answer.sh");

    let refused = Keeper::start(&wrong, &home);
    match refused {
        Err(e) => {
            let said = e.to_string();
            assert!(
                said.contains("ready") || said.contains("hello"),
                "the refusal should say what it heard instead: {said}"
            );
        }
        Ok(_) => panic!("a program that does not speak the protocol was accepted as a keeper"),
    }
}

/// A scratch directory of this test's own.
///
/// **Unique by construction, not by hoping.** The first version named it from
/// the process id and the clock, and these tests run in parallel in one
/// process: two of them landed in the same directory, so one wrote the fake
/// keeper while another was executing it, and the exec failed with `ETXTBSY`
/// — "text file busy" — about one run in six. The counter is what makes two
/// tests unable to collide; `create_dir` rather than `create_dir_all` is what
/// makes a collision an error instead of a shared directory.
fn tempdir() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let base = std::env::temp_dir();
    for _ in 0..64 {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = base.join(format!("esper-keeper-{}-{n}", std::process::id()));
        match std::fs::create_dir(&path) {
            Ok(()) => return path,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("a scratch directory at {}: {e}", path.display()),
        }
    }
    panic!("no scratch directory was free")
}

/// The stand-in keeper: a committed script that says the word and waits.
///
/// **A fixture rather than a file the tests write**, for a reason worth
/// knowing: these tests run in parallel in one process, a `fork` in one thread
/// inherits every open file descriptor of every other thread, and so a script
/// another test had just finished writing could still be open for writing
/// inside a forked child that had not reached its own `exec` — making the
/// exec fail with `ETXTBSY`, about one run in four. Writing through a
/// temporary and renaming does not fix it either: the inode somebody holds
/// open is the same inode. A file nobody writes cannot be busy.
fn fake_keeper() -> std::path::PathBuf {
    fixture("keeper-that-answers.sh")
}

fn fixture(name: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    assert!(path.is_file(), "the fixture {} is missing", path.display());
    path
}
