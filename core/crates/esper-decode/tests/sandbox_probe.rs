#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(target_os = "linux")]

//! Prove the sandbox is in force, rather than reported.
//!
//! A [`SandboxReport`] saying the jail was applied is worth nothing on its
//! own — it is this crate's own claim about itself. What matters is whether a
//! forbidden thing is actually forbidden, and the only way to find out is to
//! try it.
//!
//! # How this works
//!
//! The sandbox cannot be applied to the test process: seccomp is
//! irreversible, so the first probe would take the test harness with it. So
//! each probe runs in a **fresh child**, which is this same test binary
//! re-executed with `ESPER_SANDBOX_PROBE` set — the standard trick for
//! testing process-level behaviour, and it needs no second binary to keep in
//! step.
//!
//! # Being killed is a pass
//!
//! The filter's action is `KillProcess`, so a probe that reaches a forbidden
//! syscall dies without printing anything. That is the *success* condition,
//! and the parent asserts on the exit status rather than on output. A probe
//! that exits cleanly having opened a file is the failure.

use std::process::{Command, Stdio};

/// The variable that turns this binary into a probe.
const PROBE: &str = "ESPER_SANDBOX_PROBE";

/// What a probe reported before exiting, if it got that far.
#[derive(Debug)]
struct Outcome {
    /// Exit code, if it exited normally. Read only through `Debug`, in a
    /// failure message — which is exactly what it is for.
    #[allow(dead_code)]
    code: Option<i32>,
    /// Signal, if it was killed. `SIGSYS` is 31 on Linux, and it is what
    /// seccomp's `KillProcess` produces.
    signal: Option<i32>,
    stdout: String,
}

impl Outcome {
    /// Whether the kernel killed it for a forbidden syscall.
    fn killed_by_seccomp(&self) -> bool {
        // SIGSYS. `KillProcess` reports as death by SIGSYS even though the
        // process cannot handle it.
        self.signal == Some(31)
    }

    fn said(&self, what: &str) -> bool {
        self.stdout.contains(what)
    }
}

/// Run one probe in a fresh child.
fn probe(name: &str) -> Outcome {
    use std::os::unix::process::ExitStatusExt as _;

    let exe = std::env::current_exe().expect("the test binary's own path");
    let out = Command::new(exe)
        .args(["--exact", "the_probe_entry_point", "--nocapture"])
        .env(PROBE, name)
        // Inherited, so a panic or a sandbox complaint is visible when a probe
        // fails for an unexpected reason.
        .stderr(Stdio::inherit())
        .stdout(Stdio::piped())
        .output()
        .expect("spawn the probe");

    Outcome {
        code: out.status.code(),
        signal: out.status.signal(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

/// The child's side. Does one thing and exits.
///
/// Named as a test so the harness will run it when asked for it by name, and
/// a no-op when the environment variable is absent — which is what happens
/// during an ordinary `cargo test`.
#[test]
fn the_probe_entry_point() {
    let Some(which) = std::env::var_os(PROBE) else {
        // Not a probe run. Nothing to do.
        return;
    };
    let which = which.to_string_lossy().into_owned();
    run_probe(&which);
}

/// A marker separating the test harness's own output from the protocol.
///
/// The child here is a **test binary**, so libtest prints "running 1 test" to
/// stdout before anything else — straight into the protocol stream, where the
/// parent read it as an eight-byte length and got 752793204883549799.
///
/// The product's worker will not have this problem: it is the engine binary
/// with a subcommand, and its stdout carries nothing but the protocol. This
/// marker is a test-only affordance, and it is a marker rather than "skip the
/// first two lines" because the harness's preamble is not something to encode
/// an assumption about.
const PROTOCOL_MARKER: &[u8] = b"ESPER-PROTOCOL-BEGINS\n";

/// The child loop probe. Runs the **real** `child::run`, not a copy of it, so
/// the filter is tested against the code that will actually be jailed —
/// including reading from a pipe and decoding repeatedly, neither of which a
/// single inline decode exercised.
fn run_child_loop() -> ! {
    use esper_decode::child;
    use esper_decode::decoder::DecoderConfig;
    use esper_decode::sandbox::Sandbox;

    use std::io::Write as _;
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    // Everything the harness wrote is behind us; the protocol starts here.
    let _ = output.write_all(PROTOCOL_MARKER);
    let _ = output.flush();
    let result = child::run(
        &DecoderConfig::default(),
        &Sandbox::default(),
        &mut input,
        &mut output,
    );
    // Deliberately not returning into the test harness. See the note in
    // `run_probe`.
    let _ = std::io::stdout().flush();
    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("CHILD FAILED: {e}");
            std::process::exit(70)
        }
    }
}

fn run_probe(which: &str) {
    use esper_decode::sandbox::Sandbox;

    if which == "child_loop" {
        run_child_loop();
    }

    let report = Sandbox::default().apply().expect("apply the sandbox");
    println!("SANDBOX: {}", report.describe());
    // Not asserted here. Audit mode exists so a missing syscall can be found
    // on an unfamiliar system, and a probe that refused to run in audit mode
    // would make the diagnostic useless — which it did, the first time.
    //
    // The parent asserts that the report says "enforcing", so a probe that
    // proved something under a non-enforcing filter cannot be mistaken for a
    // passing test.

    // From here on, only what the filter allows.
    match which {
        "open_a_file" => {
            // Landlock should deny this, and if the kernel has no Landlock
            // the filter kills the process for calling `openat`. Either is a
            // pass; both are checked by the parent.
            match std::fs::read("/etc/hostname") {
                Ok(bytes) => println!("PROBE: READ {} BYTES — NOT SANDBOXED", bytes.len()),
                Err(e) => println!("PROBE: DENIED {}", e.kind() as u8),
            }
        }
        "reach_the_network" => match std::net::TcpStream::connect("127.0.0.1:9") {
            Ok(_) => println!("PROBE: CONNECTED — NOT SANDBOXED"),
            Err(e) => println!("PROBE: DENIED {}", e.kind() as u8),
        },
        "exceed_the_address_space" => {
            // Well past the cap. A successful allocation means the limit is
            // not in force, which is the hole openh264's own allocations go
            // through.
            let huge = 2usize * 1024 * 1024 * 1024;
            let mut v: Vec<u8> = Vec::new();
            match v.try_reserve_exact(huge) {
                Ok(()) => println!("PROBE: ALLOCATED {huge} — NOT CAPPED"),
                Err(_) => println!("PROBE: ALLOCATION REFUSED"),
            }
        }
        "write_a_lot" => {
            // Isolating one difference between the probe that worked and the
            // child loop that did not: the loop writes a 115 KB picture.
            use std::io::Write as _;
            let big = vec![b'x'; 200 * 1024];
            let mut out = std::io::stdout().lock();
            match out.write_all(&big).and_then(|()| out.flush()) {
                Ok(()) => println!("PROBE: WROTE {}", big.len()),
                Err(e) => println!("PROBE: WRITE FAILED {e}"),
            }
        }
        "allocate_a_lot" => {
            // The other difference: a large allocation.
            let mut kept: Vec<Vec<u8>> = Vec::new();
            for _ in 0..64 {
                kept.push(vec![7u8; 512 * 1024]);
            }
            println!("PROBE: ALLOCATED {} BLOCKS", kept.len());
            drop(kept);
            println!("PROBE: FREED");
        }
        other => panic!("unknown probe {other}"),
    }

    // **Exit deliberately, rather than returning into the test harness.**
    //
    // libtest runs each test on a thread and tears that thread down
    // afterwards, which touches `sigaltstack` and friends. Those are the test
    // harness's needs, not the decoder's, and widening the product's filter to
    // accommodate them would make the filter describe something other than
    // what it protects.
    //
    // The first version of this probe returned normally and died of SIGSYS in
    // teardown, after the decode had already succeeded — which looked exactly
    // like a decoder that could not run sandboxed.
    //
    // Flushed first: stdout is a pipe here and therefore block-buffered, so
    // exiting without a flush loses the very line the parent is asserting on.
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    std::process::exit(0);
}

/// **The functional test, and the most important one here.** A sandbox that
/// breaks decoding is not a sandbox, it is an outage — and the failure mode is
/// a syscall missing from the filter, which shows up on somebody's unfamiliar
/// libc rather than here.
///
/// Drives the **real** child loop over real pipes, with **thirty** frames
/// rather than one. Both of those matter, and the first version of this test
/// had neither:
///
/// - The real loop reads from a pipe, so `read` is exercised. An inline decode
///   never touched it, and a bisection against that probe declared `read`
///   removable — which would have produced a filter that killed the decoder on
///   its first packet.
/// - Thirty frames exercise allocation churn, so the allocator's `brk` and
///   `madvise` are reached. One frame did not.
#[test]
fn a_decoder_still_works_inside_the_sandbox() {
    use std::io::Write as _;

    let frames = encode_frames(30);
    let exe = std::env::current_exe().expect("this binary");
    let mut child = Command::new(exe)
        .args(["--exact", "the_probe_entry_point", "--nocapture"])
        .env(PROBE, "child_loop")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn the child");

    let mut stdin = child.stdin.take().expect("stdin");
    let writer = std::thread::spawn(move || {
        for frame in &frames {
            if esper_decode::protocol::write(
                &mut stdin,
                &esper_decode::protocol::ToDecoder::Decode(frame.clone()),
            )
            .is_err()
            {
                return;
            }
        }
        let _ =
            esper_decode::protocol::write(&mut stdin, &esper_decode::protocol::ToDecoder::Shutdown);
        let _ = stdin.flush();
    });

    let out = child.wait_with_output().expect("wait");
    let _ = writer.join();

    use std::os::unix::process::ExitStatusExt as _;
    assert_ne!(
        out.status.signal(),
        Some(31),
        "the child was killed by the syscall filter, which means the allow \
         list is missing something the decoder needs on its real path — \
         reading a pipe, or allocating across many frames"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "the child did not finish cleanly: signal {:?}",
        out.status.signal()
    );

    // Now read what it sent back, from the marker onwards.
    let at = find(&out.stdout, PROTOCOL_MARKER).unwrap_or_else(|| {
        panic!(
            "the child never reached the protocol; its output was {:?}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    let mut cursor = out
        .stdout
        .get(at + PROTOCOL_MARKER.len()..)
        .expect("past the marker");
    let ready = esper_decode::protocol::parse_reply(
        &esper_decode::protocol::read_body(&mut cursor).expect("a ready message"),
    )
    .expect("parse ready");
    match ready {
        esper_decode::protocol::FromDecoder::Ready { report } => assert!(
            report.is_enforcing(),
            "the child was not enforcing, so this proves nothing: {}",
            report.describe()
        ),
        other => panic!("expected Ready first, got {other:?}"),
    }

    let mut pictures = 0usize;
    while let Ok(body) = esper_decode::protocol::read_body(&mut cursor) {
        match esper_decode::protocol::parse_reply(&body).expect("parse reply") {
            esper_decode::protocol::FromDecoder::Picture { width, height, .. } => {
                assert_eq!((width, height), (320, 240));
                pictures += 1;
            }
            esper_decode::protocol::FromDecoder::NoPicture => {}
            other => panic!("the sandboxed child reported {other:?}"),
        }
    }
    assert_eq!(
        pictures, 30,
        "thirty frames in, {pictures} pictures out — the sandbox is breaking \
         decoding partway through"
    );
}

/// Find a byte sequence.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Encode `count` frames of the synthetic pattern, before any sandbox exists.
fn encode_frames(count: u64) -> Vec<Vec<u8>> {
    use esper_capture::damage::Damage;
    use esper_capture::frame::{Frame, PixelFormat};
    use esper_capture::pattern::Pattern;
    use esper_capture::pool::Pixels;
    use esper_color::convert::whole_frame;
    use esper_color::{ColorSpace, I420};
    use esper_encode::encoder::{EncoderConfig, VideoEncoder};
    use esper_encode::openh264::OpenH264;

    let (w, h) = (320u32, 240u32);
    let pattern = Pattern::new(w, h).expect("valid");
    let mut staging = I420::new(w, h).expect("valid");
    let mut enc = OpenH264::new(&EncoderConfig {
        fps: 30,
        ..EncoderConfig::for_size(w, h)
    })
    .expect("encoder");

    let bpp = PixelFormat::Bgrx8.bytes_per_pixel();
    let stride = (w as usize) * bpp;
    (0..count)
        .map(|index| {
            let mut buf = vec![0u8; stride * h as usize];
            for y in 0..h {
                for x in 0..w {
                    let [r, g, b] = pattern.pixel(x, y, index);
                    let at = y as usize * stride + x as usize * bpp;
                    if let Some(px) = buf.get_mut(at..at + bpp) {
                        if let Some(c) = px.first_mut() {
                            *c = b;
                        }
                        if let Some(c) = px.get_mut(1) {
                            *c = g;
                        }
                        if let Some(c) = px.get_mut(2) {
                            *c = r;
                        }
                    }
                }
            }
            let frame = Frame::new(
                w,
                h,
                stride,
                PixelFormat::Bgrx8,
                Damage::Everything,
                Pixels::owned(buf),
            )
            .expect("valid");
            let _ = whole_frame(&frame, ColorSpace::default(), &mut staging).expect("converted");
            enc.encode(&staging, index == 0).expect("encoded").bitstream
        })
        .collect()
}

/// A compromised decoder must not be able to read a file. Landlock denies the
/// path; a kernel without Landlock gets a filter that kills the process for
/// calling `openat`. Both are a pass, and neither is "it worked".
#[test]
fn the_sandboxed_decoder_cannot_read_a_file() {
    let out = probe("open_a_file");
    assert!(
        !out.said("NOT SANDBOXED"),
        "the sandboxed decoder read a file: {}",
        out.stdout
    );
    assert!(
        out.killed_by_seccomp() || out.said("PROBE: DENIED"),
        "expected the file access to be denied or the process killed, got \
         {out:?}"
    );
}

/// **The one that turns a parsing bug into an exfiltration channel if it
/// fails.** A compromised decoder with a socket can send the user's screen
/// anywhere.
#[test]
fn the_sandboxed_decoder_cannot_reach_the_network() {
    let out = probe("reach_the_network");
    assert!(
        !out.said("NOT SANDBOXED"),
        "the sandboxed decoder opened a socket: {}",
        out.stdout
    );
    assert!(
        out.killed_by_seccomp() || out.said("PROBE: DENIED"),
        "expected the socket to be refused or the process killed, got {out:?}"
    );
}

/// **The bound the in-process decoder cannot have.** openh264 allocates for
/// whatever picture size a peer declares, inside `decode`, before this code
/// sees anything. The address space limit is what stops that from being a
/// memory exhaustion attack.
#[test]
fn the_sandboxed_decoder_cannot_allocate_past_its_limit() {
    let out = probe("exceed_the_address_space");
    assert!(
        !out.said("NOT CAPPED"),
        "a two gigabyte allocation succeeded against a 768 MB cap, which means \
         a peer declaring a 16384x16384 picture can still exhaust memory: {}",
        out.stdout
    );
    assert!(
        out.said("PROBE: ALLOCATION REFUSED") || out.killed_by_seccomp(),
        "expected the allocation to be refused, got {out:?}"
    );
}
