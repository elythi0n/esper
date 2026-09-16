#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
#![cfg(target_os = "linux")]

//! **A copy that survives the link going, without anybody typing it again.**
//!
//! The bytes that arrived before a link dropped are not lost: they are in a
//! partial on the receiving end, and the protocol already knows how to pick up
//! from there — the offer is answered with the byte to start at. What was
//! missing was somebody to notice and run the command again, and that somebody
//! was a person.
//!
//! This kills the host outright in the middle of a transfer and starts it
//! again, which is what a laptop changing networks or a host rebooting looks
//! like from the client's side. Nothing touches the `cp` process: if it
//! finishes, it reconnected and resumed on its own.
//!
//! # Why the host is started on a port somebody chose
//!
//! An ephemeral port moves when a process restarts, so a client reconnecting
//! to where the host *was* would be reconnecting to nothing — and a test built
//! that way proves the client gives up, which it already did.

mod common;

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{Home, Host};
use esper_proto::Permission;

/// Big enough that the transfer cannot finish before the host is killed, on a
/// loopback link that moves hundreds of megabytes a second.
const SIZE: u64 = 192 * 1024 * 1024;

/// Content that does not compress to nothing, so the link carries the size it
/// says it does.
fn fill(path: &std::path::Path, bytes: u64) {
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).expect("a file"));
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    let mut block = vec![0u8; 1 << 20];
    let mut left = bytes;
    while left > 0 {
        for chunk in block.chunks_mut(8) {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let word = seed.to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
        let take = left.min(block.len() as u64) as usize;
        file.write_all(&block[..take]).expect("write");
        left -= take as u64;
    }
    file.flush().expect("flush");
}

fn enable_files(home: &Home, root: &std::path::Path) {
    // Whole keys, not sections: this config refuses `[files]` and says so.
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "files.enabled = true\nfiles.root = {:?}\n",
            root.to_string_lossy()
        ),
    )
    .expect("a config");
}

/// The partial a fetch writes while it is going, whatever it is called.
fn partial_size(dir: &std::path::Path) -> Option<u64> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let name = e.file_name();
        let name = name.to_string_lossy();
        (name.ends_with(".part") || name.starts_with('.'))
            .then(|| e.metadata().ok().map(|m| m.len()))
            .flatten()
    })
}

#[test]
fn a_fetch_reconnects_by_itself_when_the_host_goes_away() {
    let host_home = Home::new("cp-reconnect-host");
    let mine = Home::new("cp-reconnect-mine");
    // Both ways: the host has to allow the copy, and the machine running
    // `cp` has to know the host exists at all.
    // `Unattended` because the host is headless: there is nobody at it to
    // agree to a session, which is the normal state for the fleet this is for.
    host_home.pairs_with(
        mine.identity(),
        &[Permission::FilesOut, Permission::Unattended],
    );
    mine.pairs_with(host_home.identity(), &[Permission::FilesOut]);

    // The file to fetch, in the host's own file directory.
    let served = host_home.path().join("served");
    std::fs::create_dir_all(&served).expect("a directory to serve");
    fill(&served.join("big.bin"), SIZE);
    enable_files(&host_home, &served);

    let port = Host::free_port();
    let mut host = Host::start_at(&host_home, port);

    let landing = mine.path().join("landing");
    std::fs::create_dir_all(&landing).expect("somewhere to put it");

    // Long enough to ride out one restart, short enough that a broken build
    // fails this test rather than hanging the suite.
    let mut copying = Command::new(env!("CARGO_BIN_EXE_esper-engine"))
        .args([
            "cp",
            &format!("127.0.0.1:{port}:big.bin"),
            &landing.to_string_lossy(),
            "--no-mdns",
            "--patience",
            "90",
            "--progress",
        ])
        .env("ESPER_HOME", mine.path())
        .env_remove("DISPLAY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the engine binary");

    // **Killed mid-transfer, and only mid-transfer.** A host shot after the
    // copy finished would leave this test passing while proving nothing, so
    // how far it had got is asserted rather than assumed.
    let deadline = Instant::now() + Duration::from_secs(60);
    let caught = loop {
        match partial_size(&landing) {
            Some(got) if got > 0 && got < SIZE / 2 => break Some(got),
            _ => {}
        }
        if landing.join("big.bin").exists() {
            break None;
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let caught = caught.unwrap_or_else(|| {
        let listing: Vec<String> = std::fs::read_dir(&landing)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| {
                format!(
                    "{} ({} bytes)",
                    e.file_name().to_string_lossy(),
                    e.metadata().map(|m| m.len()).unwrap_or(0)
                )
            })
            .collect();
        let mut out = String::new();
        if let Some(mut e) = copying.stderr.take() {
            use std::io::Read as _;
            let _ = e.read_to_string(&mut out);
        }
        panic!("the copy was never caught part way. Landing holds {listing:?}. It said:\n{out}");
    });
    host.killed_outright();

    // A beat with nothing listening, so the client really does fail to reach
    // it rather than racing the restart.
    std::thread::sleep(Duration::from_millis(400));
    let mut again = Host::start_at(&host_home, port);

    let finished = copying.wait().expect("the copy ended");
    let said: String = {
        use std::io::Read as _;
        let mut out = String::new();
        if let Some(mut e) = copying.stderr.take() {
            let _ = e.read_to_string(&mut out);
        }
        out
    };

    assert!(
        finished.success(),
        "the copy did not survive the host going away (caught at {caught} bytes):\n{said}"
    );
    assert!(
        said.contains("reconnecting in"),
        "the copy finished without ever saying it reconnected, so it is not what was \
         measured:\n{said}"
    );
    assert!(
        said.contains("resuming   from"),
        "the copy reconnected and started from the beginning; the bytes that had arrived \
         were thrown away:\n{said}"
    );

    let landed = landing.join("big.bin");
    let got = std::fs::metadata(&landed).map(|m| m.len()).unwrap_or(0);
    assert_eq!(got, SIZE, "the file that landed is the wrong size");
    assert_eq!(
        std::fs::read(&landed).expect("read it back"),
        std::fs::read(served.join("big.bin")).expect("read the original"),
        "the file that landed is not the file that was sent"
    );

    again.killed_outright();
}

/// **A refusal is not retried.** A host that will not serve a path will not
/// serve it in ninety seconds either, and a command that kept asking would
/// turn a clear answer into a hang.
#[test]
fn a_refusal_is_answered_once_and_not_retried() {
    let host_home = Home::new("cp-refusal-host");
    let mine = Home::new("cp-refusal-mine");
    // Both ways: the host has to allow the copy, and the machine running
    // `cp` has to know the host exists at all.
    // `Unattended` because the host is headless: there is nobody at it to
    // agree to a session, which is the normal state for the fleet this is for.
    host_home.pairs_with(
        mine.identity(),
        &[Permission::FilesOut, Permission::Unattended],
    );
    mine.pairs_with(host_home.identity(), &[Permission::FilesOut]);
    let served = host_home.path().join("served");
    std::fs::create_dir_all(&served).expect("a directory to serve");
    enable_files(&host_home, &served);

    let port = Host::free_port();
    let mut host = Host::start_at(&host_home, port);
    let landing = mine.path().join("landing");
    std::fs::create_dir_all(&landing).expect("somewhere to put it");

    let began = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_esper-engine"))
        .args([
            "cp",
            &format!("127.0.0.1:{port}:not-there.bin"),
            &landing.to_string_lossy(),
            "--no-mdns",
            "--patience",
            "90",
        ])
        .env("ESPER_HOME", mine.path())
        .env_remove("DISPLAY")
        .output()
        .expect("the engine binary");
    let took = began.elapsed();

    assert!(!out.status.success(), "a file that is not there was copied");
    // **Which refusal, not merely that it failed.** A test happy with any
    // failure passes when the host never started, when the port was wrong,
    // when the binary is missing — and then reports that retrying works.
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("no such file"),
        "it failed for some other reason, so this measured nothing:\n{said}"
    );
    assert!(
        took < Duration::from_secs(30),
        "a refusal took {took:?} with ninety seconds of patience available — it was \
         retried:\n{said}"
    );
    assert!(
        !said.contains("reconnecting in"),
        "a refusal was retried:\n{said}"
    );

    host.killed_outright();
}

/// `--patience 0` is for a script that would rather decide for itself.
#[test]
fn no_patience_means_one_attempt() {
    let mine = Home::new("cp-impatient");
    let landing = mine.path().join("landing");
    std::fs::create_dir_all(&landing).expect("somewhere to put it");
    // A port with nothing on it: every attempt fails to reach anything.
    let port = Host::free_port();

    let began = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_esper-engine"))
        .args([
            "cp",
            &format!("127.0.0.1:{port}:whatever.bin"),
            &landing.to_string_lossy(),
            "--no-mdns",
            "--patience",
            "0",
        ])
        .env("ESPER_HOME", mine.path())
        .env_remove("DISPLAY")
        .output()
        .expect("the engine binary");
    let took = began.elapsed();

    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        !said.contains("reconnecting in"),
        "--patience 0 reconnected anyway:\n{said}"
    );
    assert!(
        took < Duration::from_secs(45),
        "--patience 0 took {took:?}, so it did more than one attempt"
    );
}

/// **A machine with nothing paired is told once, not nine times.**
///
/// The first version of the reconnecting loop treated every failure to reach a
/// host as a link problem, so `no devices are paired, so nobody could connect`
/// — a sentence about this machine's configuration that will read the same in
/// ninety seconds — was printed nine times over ninety seconds and then once
/// more at the end. A clear answer buried under eight copies of itself is a
/// hang with extra steps.
#[test]
fn a_machine_with_nothing_paired_is_told_at_once() {
    let mine = Home::new("cp-unpaired");
    let landing = mine.path().join("landing");
    std::fs::create_dir_all(&landing).expect("somewhere to put it");

    let began = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_esper-engine"))
        .args([
            "cp",
            "elythion:whatever.bin",
            &landing.to_string_lossy(),
            "--no-mdns",
            // Plenty of patience available, and none of it should be spent.
            "--patience",
            "90",
        ])
        .env("ESPER_HOME", mine.path())
        .env_remove("DISPLAY")
        .output()
        .expect("the engine binary");
    let took = began.elapsed();
    let said = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success());
    assert!(
        took < Duration::from_secs(10),
        "being told nothing is paired took {took:?} of the ninety available:\n{said}"
    );
    assert!(
        !said.contains("reconnecting in"),
        "a machine with nothing paired was asked again:\n{said}"
    );
    assert_eq!(
        said.matches("no devices are paired").count(),
        1,
        "said more than once:\n{said}"
    );
}
