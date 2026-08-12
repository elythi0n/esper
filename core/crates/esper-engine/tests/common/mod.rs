#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    dead_code
)]

//! A real host, a home with a device in it, and a peer that speaks the
//! protocol by hand.
//!
//! Shared by the test files that need a *running product* rather than a
//! function: the red-team pass, which attacks it, and the backdrop keeper,
//! which kills it. `dead_code` is allowed because each file uses the part of
//! this it needs and no file uses all of it.

use std::io::{BufRead, BufReader};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use esper_crypto::{Device, DeviceStore, Identity};
use esper_proto::{Hello, MediaMessage, Permission, Role, SessionRequest, StreamPurpose};
use esper_transport::{Control, Endpoint, NoPeers, OnlyPeer, Session};

/// How long to wait for the host to say where it is listening.
const STARTUP: Duration = Duration::from_secs(20);

/// So two homes in one process cannot pick the same directory.
static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A home directory with an identity and one paired device in it.
///
/// Its own directory rather than a crate for making one: nothing in this
/// workspace has a temporary-directory dependency, and a test is not a reason
/// to audit one.
pub struct Home {
    dir: PathBuf,
    id: Identity,
}

impl Home {
    /// A fresh home, with an identity of its own.
    pub fn new(what: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "esper-red-team-{}-{what}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a directory of its own");
        let id = Identity::load_or_create(&dir.join("identity.key")).expect("an identity");
        Self { dir, id }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Write the other device into this home's list, with these permissions.
    ///
    /// **The pairing itself is not what is under test here** and has tests of
    /// its own, so the record is written directly: it is what pairing
    /// produces, and writing it is how a test gets exactly the permissions it
    /// wants to attack from.
    pub fn pairs_with(&self, other: &Identity, permissions: &[Permission]) {
        let mut store = DeviceStore::new();
        let mut device = Device::new(other.public_key(), "red team".to_owned(), 0);
        device.permissions = permissions.to_vec();
        store.insert(device).expect("one device");
        store
            .save(&self.path().join("devices.json"))
            .expect("a device list");
    }
}

/// A running `esper-engine host`, and what it has said.
pub struct Host {
    child: Child,
    pub addr: SocketAddr,
    said: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Host {
    /// Start one, and wait until it says where it is listening.
    pub fn start(home: &Home) -> Self {
        Self::started(home, false)
    }

    /// The same, keeping the display this process can see.
    ///
    /// For the one thing that cannot be measured without a screen: what a
    /// host does to the desktop it is serving, and whether it puts it back.
    pub fn start_on_this_display(home: &Home) -> Self {
        Self::started(home, true)
    }

    fn started(home: &Home, with_a_display: bool) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_esper-engine"));
        command
            .arg("host")
            .arg("--listen")
            .arg("127.0.0.1:0")
            .arg("--no-mdns")
            .env("ESPER_HOME", home.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !with_a_display {
            // **Headless on purpose.** See the note at the top of this file.
            command
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY")
                .env_remove("XAUTHORITY");
        }
        let mut child = command.spawn().expect("the engine binary");

        let said = Arc::new(std::sync::Mutex::new(Vec::new()));
        for stream in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        ] {
            let Some(stream) = stream else { continue };
            let kept = Arc::clone(&said);
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    if let Ok(mut kept) = kept.lock() {
                        kept.push(line);
                    }
                }
            });
        }

        let deadline = std::time::Instant::now() + STARTUP;
        let found = loop {
            if let Some(addr) = Self::listening(&said) {
                break Some(addr);
            }
            if std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        match found {
            Some(addr) => Self { child, addr, said },
            // **Reaped before panicking.** A test that gave up here used to
            // drop the handle, and dropping a `Child` neither kills nor waits:
            // one slow start would leave a host running on somebody's machine
            // for as long as the machine was up.
            None => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "the host never said where it was listening: {:?}",
                    said.lock().map(|l| l.clone()).unwrap_or_default()
                );
            }
        }
    }

    pub fn listening(said: &Arc<std::sync::Mutex<Vec<String>>>) -> Option<SocketAddr> {
        let lines = said.lock().ok()?;
        lines.iter().find_map(|line| {
            let rest = line.trim().strip_prefix("listening ")?;
            rest.split_whitespace().next()?.parse().ok()
        })
    }

    /// Wait until the host has said something matching, or give up.
    pub fn waits_to_say(&self, what: &str) -> bool {
        self.waits_up_to(what, Duration::from_secs(10))
    }

    /// The same, where what is being waited for is a timeout of the host's own
    /// and the wait therefore has to outlast it.
    pub fn waits_up_to(&self, what: &str, within: Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            if self.has_said(what) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    pub fn has_said(&self, what: &str) -> bool {
        self.said
            .lock()
            .map(|lines| lines.iter().any(|l| l.contains(what)))
            .unwrap_or(false)
    }

    pub fn everything(&self) -> Vec<String> {
        self.said.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

impl Host {
    /// Kill the engine outright, the way a crash does.
    ///
    /// **Not the process this rig spawned politely.** A `SIGTERM` gives the
    /// host time to put its own state back and cancel its keeper, which reads
    /// as a clean shutdown — and the case worth testing is the other one.
    pub fn killed_outright(&mut self) {
        let _ = Command::new("kill")
            .arg("-9")
            .arg(self.child.id().to_string())
            .status();
        let _ = self.child.wait();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // Best effort: a test that failed is more useful with its home left
        // behind than with the removal's error on top of the failure.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The far end, speaking the protocol by hand.
pub struct Peer {
    /// Kept because dropping it closes the connection under the session.
    _endpoint: Endpoint,
    pub session: Session,
    control: Control,
    pub granted: Vec<Permission>,
}

impl Peer {
    /// Connect to a running host and open a session, asking for `want`.
    pub async fn open(home: &Home, host: &Host, host_key: [u8; 32], want: &[Permission]) -> Self {
        let endpoint = Endpoint::bind(
            &home.id,
            (Ipv4Addr::LOCALHOST, 0).into(),
            Arc::new(NoPeers),
            Arc::new(OnlyPeer::new(host_key)),
        )
        .expect("an endpoint");
        let conn = endpoint
            .connect(host.addr)
            .await
            .expect("the host accepted the connection");
        let mut session = Session::open(
            conn,
            Hello::new(Role::Controller, home.id.public_key(), "red team"),
        )
        .await
        .map_err(|(e, _)| e)
        .expect("a session");
        let mut control = session.take_control().expect("the first control stream");
        let granted = esper_transport::request_session(
            &mut control,
            &SessionRequest {
                kind: esper_proto::SessionKind::Screen,
                want: want.to_vec(),
                codecs: vec!["h264".to_owned()],
                reason: "a red-team pass".to_owned(),
            },
        )
        .await
        .expect("a grant")
        .granted;
        Self {
            _endpoint: endpoint,
            session,
            control,
            granted,
        }
    }

    /// Send anything at all on the session's control stream.
    pub async fn sends(&mut self, message: &MediaMessage) {
        self.control
            .send(message)
            .await
            .expect("the host took the bytes");
    }

    /// Open a stream and say it is for `purpose`, the way a controller does.
    pub async fn opens(&self, purpose: StreamPurpose) -> Control {
        let mut control = self
            .session
            .open_control()
            .await
            .expect("a stream to the host");
        control.send(&purpose).await.expect("said what it was for");
        control
    }

    /// The next answer to a question, ignoring everything else the host says.
    pub async fn next_answer(&mut self, within: Duration) -> Option<esper_proto::PermissionAnswer> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            match tokio::time::timeout(left, self.control.recv::<MediaMessage>()).await {
                Ok(Ok(MediaMessage::Answer(answer))) => return Some(answer),
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => return None,
            }
        }
    }
}

/// A host paired with one device holding `permissions`, and the red team's own
/// home — the shape every test here starts from.
pub fn facing(what: &str, permissions: &[Permission]) -> (Home, Host, [u8; 32], Home) {
    faced(what, permissions, false)
}

/// The same, with a host that keeps the display this process can see.
pub fn facing_on_this_display(
    what: &str,
    permissions: &[Permission],
) -> (Home, Host, [u8; 32], Home) {
    faced(what, permissions, true)
}

/// Two homes, paired, and no host yet.
///
/// For a test that has to write a config before the host reads it — and
/// starting one only to throw it away would leave a process on somebody's
/// machine for as long as it took to notice.
pub fn homes(what: &str, permissions: &[Permission]) -> (Home, [u8; 32], Home) {
    let host_home = Home::new(&format!("{what}-host"));
    let red_home = Home::new(&format!("{what}-red"));
    let mut holding = permissions.to_vec();
    holding.push(Permission::Unattended);
    host_home.pairs_with(&red_home.id, &holding);
    let host_key = host_home.id.public_key();
    (host_home, host_key, red_home)
}

fn faced(
    what: &str,
    permissions: &[Permission],
    with_a_display: bool,
) -> (Home, Host, [u8; 32], Home) {
    let host_home = Home::new(&format!("{what}-host"));
    let red_home = Home::new(&format!("{what}-red"));
    // **`unattended` as well as whatever is being attacked from.** A headless
    // host has nobody to ask, so a device without it cannot open a session at
    // all — the refusal every one of these tests would get instead of the one
    // it is about. It grants nothing that matters here: what a session may do
    // is still only what it asked for and holds.
    let mut holding = permissions.to_vec();
    holding.push(Permission::Unattended);
    host_home.pairs_with(&red_home.id, &holding);
    let host_key = host_home.id.public_key();
    let host = if with_a_display {
        Host::start_on_this_display(&host_home)
    } else {
        Host::start(&host_home)
    };
    (host_home, host, host_key, red_home)
}
