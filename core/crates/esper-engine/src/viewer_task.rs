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

/// The pictures of one session, on their way to a window.
pub struct Viewer {
    stream: Writer<Box<dyn io::Write + Send>>,
    repack: Repackager,
    size: (u32, u32),
    /// Whether the far end has stopped taking pictures. Said once.
    gone: bool,
    frames: u64,
    waited: u64,
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
        Ok(Self::to(Box::new(socket), size))
    }

    /// The same, over any sink. The tests write to a buffer.
    pub fn to(sink: Box<dyn io::Write + Send>, size: (u32, u32)) -> Self {
        Self {
            stream: Writer::new(sink),
            repack: Repackager::new(),
            size,
            gone: false,
            frames: 0,
            waited: 0,
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
