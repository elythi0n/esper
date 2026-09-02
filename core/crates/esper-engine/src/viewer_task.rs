//! Handing a session's pictures to a window, encoded.
//!
//! # Why the window gets them encoded
//!
//! Because it has a decoder, and a better one than a copy across a process
//! boundary deserves. The webview's `VideoDecoder` was measured on this
//! encoder's own output before any of this was built — 1920x1080 at 151
//! frames a second with every frame drawn, software rendering, no GPU — and
//! decoding there costs no new crates, where a WebRTC stack to hand it the
//! same pictures costs 129.
//!
//! So this is the seam the viewer needed: the encoded frames the recorder
//! already taps, repackaged into the one form a webview accepts, written down
//! a socket the window is listening on.
//!
//! # Why the engine dials and does not listen
//!
//! The window owns the socket. It knows where its own home is, it creates the
//! file with the permissions everything else under that home has, and it
//! removes it when it closes — none of which an engine that outlives a window
//! can do for it. A dial also fails immediately and locally when nothing is
//! there, which is the honest outcome for a viewer nobody is watching.

use std::io;
use std::path::Path;
use std::time::Duration;

use esper_media::avcc::Repackager;
use esper_media::viewer_stream::Writer;
use esper_stream::Encoded;

/// The pictures of one session, on their way to a window — and what the
/// person did with them on the way back.
///
/// # Why one socket carries both
///
/// Because the two have exactly one lifetime between them. A window that has
/// stopped taking pictures is a window whose keystrokes nobody should be
/// sending, and a second socket would be a second thing to notice that about.
/// The window's own end of the back channel.
///
/// A unix socket where there is one. On Windows there is not, and the window
/// there would use a named pipe that does not exist yet — so `Empty` stands in,
/// which reads zero bytes for ever. That is exactly what "no window is talking
/// to us" means, so the reading loop above needs no `cfg` of its own and there
/// is no second copy of it to drift.
#[cfg(unix)]
type BackChannel = std::os::unix::net::UnixStream;
#[cfg(not(unix))]
type BackChannel = std::io::Empty;

pub struct Viewer {
    stream: Writer<Box<dyn io::Write + Send>>,
    repack: Repackager,
    size: (u32, u32),
    /// Whether the far end has stopped taking pictures. Said once.
    gone: bool,
    frames: u64,
    waited: u64,
    /// The window's own end of the socket, read without blocking.
    ///
    /// `None` for a viewer that is only a sink — the loopback diagnostic, and
    /// every test that hands it a buffer.
    listening: Option<BackChannel>,
    /// What has arrived from the window and is not yet a whole event.
    events: esper_media::WindowEvents,
    /// Said once: a window whose input this session is not carrying.
    refused: bool,
}

/// Written by hand because the sink is a trait object and a socket has
/// nothing worth printing. What is worth printing is how it is going.
impl std::fmt::Debug for Viewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewer")
            .field("size", &self.size)
            .field("frames", &self.frames)
            .field("waited", &self.waited)
            .field("gone", &self.gone)
            .finish()
    }
}

impl Viewer {
    /// Dial the window's socket and prepare to write to it.
    ///
    /// # Errors
    ///
    /// When nothing is listening, which is a window that closed between asking
    /// for a session and the session opening.
    #[cfg(unix)]
    pub fn connect(path: &Path, size: (u32, u32)) -> io::Result<Self> {
        let socket = std::os::unix::net::UnixStream::connect(path)?;
        // The reading half is its own handle on the same connection, and it
        // never blocks: the session's loop has pictures to move and cannot
        // wait on a person who is not typing.
        let reading = socket.try_clone()?;
        reading.set_nonblocking(true)?;
        let mut viewer = Self::to(Box::new(socket), size);
        viewer.listening = Some(reading);
        Ok(viewer)
    }

    /// Dialling a window on a platform with no socket to dial.
    ///
    /// Present so that every caller compiles everywhere and none of them has to
    /// carry a `cfg`. It refuses, naming the reason: a host on Windows can
    /// serve a session perfectly well, it just has no local window to show it
    /// in yet.
    #[cfg(not(unix))]
    pub fn connect(_path: &Path, _size: (u32, u32)) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the window's socket is a unix socket, and this platform has none",
        ))
    }

    /// The same, over any sink. The tests write to a buffer.
    ///
    /// Where there is a socket, or in a test. On Windows the only caller would
    /// be a `connect` that refuses before it builds anything, and an
    /// unreachable constructor in a binary is dead code the compiler is right
    /// about.
    #[cfg(any(unix, test))]
    pub fn to(sink: Box<dyn io::Write + Send>, size: (u32, u32)) -> Self {
        Self {
            stream: Writer::new(sink),
            repack: Repackager::new(),
            size,
            gone: false,
            frames: 0,
            waited: 0,
            listening: None,
            events: esper_media::WindowEvents::new(),
            refused: false,
        }
    }

    /// What the person did in the window since the last time this was asked.
    ///
    /// Empty for a viewer nobody can type into, and empty when nothing has
    /// arrived — which is most turns, because a session's loop runs far more
    /// often than anybody types.
    ///
    /// A stream that is not a window's input, or is a version this build does
    /// not read, stops the reading and says so **once**: a wrong stream will
    /// not become a right one, and a line per turn would bury the session's
    /// own output.
    pub fn take_events(&mut self) -> Vec<esper_media::WindowEvent> {
        let Some(socket) = self.listening.as_mut() else {
            return Vec::new();
        };
        let mut arrived = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            match io::Read::read(socket, &mut buffer) {
                Ok(0) => break,
                Ok(n) => match self.events.feed(buffer.get(..n).unwrap_or_default()) {
                    Ok(events) => arrived.extend(events),
                    Err(e) => {
                        println!("  · the window's input was refused: {e}");
                        self.listening = None;
                        break;
                    }
                },
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    // The window closed its end. Pictures stopping is said
                    // where pictures are written; this half just stops.
                    self.listening = None;
                    break;
                }
            }
        }
        arrived
    }

    /// Say once that a window is typing into a session that does not carry it.
    ///
    /// Not an error and not a refusal to run: a person who opens a window on a
    /// session they only asked to *watch* has done nothing wrong, and the
    /// keystroke going nowhere is the whole of what happens. Saying it once
    /// is the difference between that and a keyboard that seems broken.
    pub fn note_input_not_carried(&mut self) {
        if !self.refused {
            self.refused = true;
            println!(
                "  · this session does not carry input, so what is typed in the window stays there"
            );
        }
    }

    /// How many pictures have gone to the window.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// How many arrived before a decoder could have used them.
    ///
    /// A viewer that joins mid-stream sees delta frames first, and a decoder
    /// handed one with no reference shows garbage. They are counted rather
    /// than silently dropped, because a viewer that waits a long time for its
    /// first picture is a keyframe interval nobody measured.
    #[must_use]
    pub const fn waited(&self) -> u64 {
        self.waited
    }

    /// Take the frames this turn produced.
    ///
    /// `since` is the session's own clock, which is what the window's decoder
    /// orders pictures by — not the wall clock, which a session that started
    /// at a minute boundary would make jump.
    pub fn take(&mut self, frames: &[Encoded], since: Duration) {
        for frame in frames {
            self.picture(&frame.bitstream, since);
        }
    }

    /// One access unit, from wherever it came from.
    ///
    /// The session has them as `Encoded`; the loopback diagnostic has them
    /// straight out of the pipeline and would have to build an `Encoded` to
    /// hand them over — a clone of every frame to satisfy a type. So the
    /// bitstream is the argument, and `take` is the convenience over it.
    pub fn picture(&mut self, bitstream: &[u8], since: Duration) {
        if self.gone {
            return;
        }
        let micros = u64::try_from(since.as_micros()).unwrap_or(u64::MAX);
        let Some(sample) = self.repack.push(bitstream) else {
            self.waited = self.waited.saturating_add(1);
            return;
        };
        if self.repack.take_description_change() {
            let (w, h) = self.size;
            if let Err(e) = self.stream.configuration(w, h, self.repack.sets()) {
                self.stop(&e);
                return;
            }
        }
        if let Err(e) = self.stream.picture(&sample, micros) {
            self.stop(&e);
            return;
        }
        self.frames = self.frames.saturating_add(1);
    }

    /// Tell the window what the session is of, and what else it could be of.
    ///
    /// Sent when the host first says what it has and again whenever that
    /// changes — a screen plugged in, a screen switched to, a quality the
    /// person asked for and got. A window told once would go on offering a
    /// monitor somebody unplugged and showing a setting somebody changed.
    ///
    /// Written straight through rather than remembered here: the session owns
    /// what it is of, and a second copy in this struct would be a second
    /// thing to keep in step with the host.
    pub fn session(
        &mut self,
        profile: u8,
        sharpness: u8,
        showing: &[u32],
        screens: &[esper_media::viewer_stream::Screen],
        asked: (u16, u32),
    ) {
        if self.gone {
            return;
        }
        if let Err(e) = self
            .stream
            .session(profile, sharpness, showing, screens, asked)
        {
            self.stop(&e);
        }
    }

    /// Where the host's pointer is, and whether the window has to draw it.
    ///
    /// The decision is passed on rather than the evidence: whether a pointer
    /// is worn or drawn turns on whether this session is driving and whether
    /// the host is following, and that has been worked out once already. A
    /// window deciding it again would be a second implementation of a rule
    /// that took a finding to get right.
    pub fn pointer(&mut self, in_picture: bool, draw: bool, x: i32, y: i32) {
        if self.gone {
            return;
        }
        if let Err(e) = self.stream.pointer(in_picture, draw, x, y) {
            self.stop(&e);
        }
    }

    /// What the host's pointer looks like. `None` is hidden.
    pub fn pointer_shape(&mut self, image: Option<&esper_capture::cursor::CursorImage>) {
        if self.gone {
            return;
        }
        let said = image.map(|i| {
            (
                i.width,
                i.height,
                i.hotspot_x,
                i.hotspot_y,
                i.rgba.as_slice(),
            )
        });
        if let Err(e) = self.stream.pointer_shape(said) {
            self.stop(&e);
        }
    }

    /// The window went away. Said once, and never again.
    ///
    /// Not an error the session ends on: a viewer closing its window is an
    /// ordinary thing to do, and a session that also carries a terminal or a
    /// file transfer has no business ending because nobody is watching.
    fn stop(&mut self, why: &io::Error) {
        self.gone = true;
        println!("  · the window stopped taking pictures: {why}");
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "a test that indexes past its own fixture is a failing test, which is the point"
)]
mod tests {
    use super::*;
    use esper_media::viewer_stream::{MAGIC, record};

    /// A sink the test can read back.
    #[derive(Debug, Clone, Default)]
    struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl io::Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A sink that refuses everything, like a window that has closed.
    #[derive(Debug)]
    struct Closed;
    impl io::Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the window closed",
            ))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn annexb(units: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for u in units {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(u);
        }
        out
    }
    fn keyframe() -> Encoded {
        Encoded {
            bitstream: annexb(&[
                &[0x67, 0x4D, 0x40, 0x28, 0x1F],
                &[0x68, 0xEE, 0x3C],
                &[0x65, 0x88, 0x84],
            ]),
            keyframe: true,
        }
    }
    fn delta() -> Encoded {
        Encoded {
            bitstream: annexb(&[&[0x41, 0x9A, 0x00]]),
            keyframe: false,
        }
    }

    #[test]
    fn the_window_is_configured_before_it_is_fed() {
        let sink = Shared::default();
        let mut viewer = Viewer::to(Box::new(sink.clone()), (1280, 720));
        viewer.take(&[keyframe(), delta()], Duration::from_millis(0));
        let out = sink.0.lock().unwrap().clone();

        assert_eq!(&out[..8], MAGIC);
        assert_eq!(out[9], record::CONFIGURATION, "configuration first, always");
        assert_eq!(viewer.frames(), 2);
        assert_eq!(viewer.waited(), 0);
    }

    #[test]
    fn a_viewer_that_joins_mid_stream_waits_rather_than_sending_garbage() {
        let sink = Shared::default();
        let mut viewer = Viewer::to(Box::new(sink.clone()), (64, 48));
        viewer.take(&[delta(), delta()], Duration::from_millis(0));
        assert_eq!(viewer.frames(), 0, "nothing a decoder could have used");
        assert_eq!(viewer.waited(), 2);
        assert!(
            sink.0.lock().unwrap().is_empty(),
            "not even a magic number yet"
        );

        viewer.take(&[keyframe()], Duration::from_millis(33));
        assert_eq!(
            viewer.frames(),
            1,
            "and then the picture it was waiting for"
        );
    }

    #[test]
    fn a_window_that_closed_does_not_end_the_session() {
        let mut viewer = Viewer::to(Box::new(Closed), (64, 48));
        viewer.take(&[keyframe()], Duration::from_millis(0));
        assert_eq!(viewer.frames(), 0);
        // And it stops trying rather than printing on every frame for an hour.
        viewer.take(&[keyframe(), delta(), delta()], Duration::from_millis(33));
        assert_eq!(viewer.frames(), 0);
    }

    /// A monitor switch, or a rung that moves the size, brings new parameter
    /// sets — and a decoder configured for the old ones produces nothing and
    /// says nothing about why. The format has two record kinds for exactly
    /// this, and this is the test that they are both used.
    #[test]
    fn a_new_parameter_set_reconfigures_the_window() {
        let sink = Shared::default();
        let mut viewer = Viewer::to(Box::new(sink.clone()), (1280, 720));
        viewer.take(&[keyframe(), delta()], Duration::from_millis(0));

        // The same sets again: the encoder repeats them before every keyframe,
        // and repeating is not changing.
        viewer.take(&[keyframe()], Duration::from_millis(33));
        assert_eq!(
            configurations(&sink),
            1,
            "a repeated parameter set is not a new one"
        );

        // A different SPS, as a different picture size produces.
        let other = Encoded {
            bitstream: annexb(&[
                &[0x67, 0x4D, 0x40, 0x1F, 0x2A],
                &[0x68, 0xEE, 0x3C],
                &[0x65, 0x88, 0x84],
            ]),
            keyframe: true,
        };
        viewer.take(&[other], Duration::from_millis(66));
        assert_eq!(
            configurations(&sink),
            2,
            "a changed parameter set has to reconfigure the window's decoder"
        );
        assert_eq!(viewer.frames(), 4);
    }

    /// How many configuration records the stream holds, by walking it the way
    /// a reader that knows only the framing would.
    fn configurations(sink: &Shared) -> usize {
        let out = sink.0.lock().unwrap().clone();
        let mut at = 9;
        let mut found = 0;
        while at + 5 <= out.len() {
            let kind = out[at];
            let len = u32::from_be_bytes(out[at + 1..at + 5].try_into().unwrap()) as usize;
            if kind == record::CONFIGURATION {
                found += 1;
            }
            at += 5 + len;
        }
        found
    }

    /// The socket's own half: bytes in one end, events out of the other.
    ///
    /// A `UnixStream::pair` is the same kind of socket the window creates, so
    /// what is exercised here is the reading code as it runs — including the
    /// part that must never block, because a session's loop has pictures to
    /// move and cannot wait on somebody who is not typing.
    #[test]
    #[cfg(unix)]
    fn what_a_window_writes_is_what_the_session_reads() {
        use esper_media::WindowEvent;
        use esper_media::window_input::Writer as InputWriter;

        let (theirs, ours) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        ours.set_nonblocking(true).expect("non-blocking");
        let mut viewer = Viewer::to(Box::new(Vec::new()), (1920, 1080));
        viewer.listening = Some(ours);

        // Nothing typed yet: the read must return at once rather than wait.
        assert!(viewer.take_events().is_empty());

        let mut writing = InputWriter::new(theirs);
        for event in [
            WindowEvent::Pointer { x: 100, y: 50 },
            WindowEvent::Button {
                button: 0,
                pressed: true,
            },
            WindowEvent::Key {
                usage: 0x04,
                pressed: true,
            },
            WindowEvent::LetGo,
        ] {
            writing.event(event).expect("write");
        }

        let mut heard = Vec::new();
        for _ in 0..50 {
            heard.extend(viewer.take_events());
            if heard.len() >= 4 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            heard,
            vec![
                WindowEvent::Pointer { x: 100, y: 50 },
                WindowEvent::Button {
                    button: 0,
                    pressed: true
                },
                WindowEvent::Key {
                    usage: 0x04,
                    pressed: true
                },
                WindowEvent::LetGo,
            ]
        );
    }

    /// The picture stream's own bytes, sent up the input half by something
    /// confused about which direction it is talking in.
    #[test]
    #[cfg(unix)]
    fn the_wrong_direction_of_the_socket_is_refused_rather_than_decoded() {
        let (theirs, ours) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        ours.set_nonblocking(true).expect("non-blocking");
        let mut viewer = Viewer::to(Box::new(Vec::new()), (64, 48));
        viewer.listening = Some(ours);

        let mut writing = theirs;
        io::Write::write_all(&mut writing, b"ESPRVID\x00\x02\x01\x00\x00\x00\x04abcd")
            .expect("write");

        let mut waited = 0;
        while viewer.listening.is_some() && waited < 50 {
            assert!(
                viewer.take_events().is_empty(),
                "nothing may be decoded out of it"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
            waited += 1;
        }
        assert!(
            viewer.listening.is_none(),
            "a stream that is not a window's input stops being read"
        );
    }

    #[test]
    fn the_clock_is_the_session_s_own() {
        let sink = Shared::default();
        let mut viewer = Viewer::to(Box::new(sink.clone()), (64, 48));
        viewer.take(&[keyframe()], Duration::from_millis(0));
        viewer.take(&[delta()], Duration::from_micros(33_367));
        let out = sink.0.lock().unwrap().clone();
        // Walked rather than counted backwards: a record carries its own
        // length, so a reader never has to know how big a sample was.
        let mut at = 9;
        let mut last = None;
        while at + 5 <= out.len() {
            let kind = out[at];
            let len = u32::from_be_bytes(out[at + 1..at + 5].try_into().unwrap()) as usize;
            if kind == record::PICTURE {
                last = Some(at + 5);
            }
            at += 5 + len;
        }
        let body = last.expect("a picture");
        assert_eq!(
            u64::from_be_bytes(out[body + 1..body + 9].try_into().unwrap()),
            33_367,
            "the session's own clock, as the caller gave it"
        );
    }
}
