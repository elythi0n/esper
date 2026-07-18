//! A capturer that needs no display.
//!
//! The first backend, deliberately. It is what lets the rest of the pipeline —
//! damage filtering, colour conversion, encode, decode, present — be built and
//! tested before any platform capture code exists, and it is what lets CI run
//! the whole thing on a machine with no screen.
//!
//! It is not a stub. It renders [`crate::pattern::Pattern`], reports damage
//! that is exactly right rather than approximately right, honours a frame rate,
//! and can be told to produce padded strides and any of the pixel formats — so
//! a consumer that only works on tightly-packed BGRA fails here rather than on
//! someone's machine.

use std::time::{Duration, Instant};

use crate::capturer::{Capture, CaptureError, Capturer, MonitorId, MonitorInfo};
use crate::damage::{Damage, Regions};
use crate::frame::{Frame, PixelFormat};
use crate::geometry::Rect;
use crate::pattern::{Pattern, PatternError};
use crate::pool::BufferPool;

/// The backend name reported in logs and the session HUD.
pub const BACKEND: &str = "synthetic";

/// A synthetic screen, rendered on demand.
#[derive(Debug)]
pub struct SyntheticCapturer {
    pattern: Pattern,
    monitors: Vec<MonitorInfo>,
    format: PixelFormat,
    /// Extra bytes per row beyond the pixels.
    ///
    /// Configurable because no real backend hands back a tightly-packed
    /// buffer, and a consumer that assumes one works here and nowhere else.
    row_padding: usize,
    interval: Duration,
    pool: BufferPool,
    frame: u64,
    /// When the next frame is due, so a caller polling faster than the frame
    /// rate is told nothing changed instead of being handed the same picture
    /// again.
    next_due: Option<Instant>,
    /// Whether a frame has been produced yet.
    started: bool,
    /// Frames to produce before the stream ends, if limited.
    limit: Option<u64>,
    /// A pending geometry change, delivered once.
    reconfigure_at: Option<u64>,
    selected: MonitorId,
}

impl SyntheticCapturer {
    /// A synthetic capturer of this size, at this frame rate.
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, PatternError> {
        let pattern = Pattern::new(width, height)?;
        let monitors = vec![MonitorInfo {
            id: MonitorId(0),
            name: "synthetic-0".to_owned(),
            geometry: Rect::sized(width, height),
            scale: 1.0,
            primary: true,
        }];
        Ok(Self {
            pattern,
            monitors,
            format: PixelFormat::Bgrx8,
            row_padding: 0,
            interval: Duration::from_nanos(1_000_000_000 / u64::from(fps.max(1))),
            pool: BufferPool::default(),
            frame: 0,
            next_due: None,
            started: false,
            limit: None,
            reconfigure_at: None,
            selected: MonitorId(0),
        })
    }

    /// Produce frames in this pixel layout.
    #[must_use]
    pub const fn with_format(mut self, format: PixelFormat) -> Self {
        self.format = format;
        self
    }

    /// Pad every row by this many bytes.
    ///
    /// The default is zero, which is the *unrealistic* case. A test that means
    /// to exercise a real backend's layout should set it.
    #[must_use]
    pub const fn with_row_padding(mut self, bytes: usize) -> Self {
        self.row_padding = bytes;
        self
    }

    /// Stop after this many frames.
    ///
    /// For tests that want a stream to end rather than run for ever.
    #[must_use]
    pub const fn with_frame_limit(mut self, frames: u64) -> Self {
        self.limit = Some(frames);
        self
    }

    /// Add another monitor of this size, to the right of the ones before it.
    ///
    /// Give them **different sizes**. A switch that changed `selected` while
    /// still capturing the old monitor would be invisible between two monitors
    /// of the same size, and the frame's size is what a test can see.
    pub fn with_extra_monitor(mut self, width: u32, height: u32) -> Result<Self, PatternError> {
        // Rejected here rather than at select time: an unusable monitor in the
        // list is a fixture that lies about the host it is standing in for.
        let _ = Pattern::new(width, height)?;
        let x = self
            .monitors
            .iter()
            .map(|m| {
                m.geometry
                    .x
                    .saturating_add(i32::try_from(m.geometry.width).unwrap_or(i32::MAX))
            })
            .max()
            .unwrap_or(0);
        let id = MonitorId(u32::try_from(self.monitors.len()).unwrap_or(0));
        self.monitors.push(MonitorInfo {
            name: format!("synthetic-{}", id.0),
            id,
            geometry: Rect {
                x,
                y: 0,
                width,
                height,
            },
            scale: 1.0,
            primary: false,
        });
        Ok(self)
    }

    /// Report a geometry change before the given frame.
    ///
    /// A resolution change mid-session is a real event that breaks every
    /// buffer size downstream, and it is far easier to test here than by
    /// unplugging a monitor.
    #[must_use]
    pub const fn with_reconfigure_at(mut self, frame: u64) -> Self {
        self.reconfigure_at = Some(frame);
        self
    }

    /// The frame number the next capture will render.
    #[must_use]
    pub const fn frame_number(&self) -> u64 {
        self.frame
    }

    /// The pattern being rendered, for a test that wants to compute expected
    /// pixels or expected damage.
    #[must_use]
    pub const fn pattern(&self) -> &Pattern {
        &self.pattern
    }

    /// Bytes per row, pixels plus padding.
    fn stride(&self) -> usize {
        (self.pattern.width() as usize)
            .saturating_mul(self.format.bytes_per_pixel())
            .saturating_add(self.row_padding)
    }

    /// Render the current frame into a pooled buffer.
    fn render(&mut self, damage: Damage) -> Result<Frame, CaptureError> {
        let width = self.pattern.width();
        let height = self.pattern.height();
        let stride = self.stride();
        let bpp = self.format.bytes_per_pixel();
        let (r_at, g_at, b_at) = self.format.rgb_offsets();
        let opaque = self.format.has_alpha();
        let frame_no = self.frame;

        let mut buffer = self.pool.take(stride.saturating_mul(height as usize));

        for y in 0..height {
            let start = stride.saturating_mul(y as usize);
            let row_bytes = (width as usize).saturating_mul(bpp);
            let Some(row) = buffer.get_mut(start..start.saturating_add(row_bytes)) else {
                continue;
            };
            for (x, px) in row.chunks_exact_mut(bpp).enumerate() {
                let [r, g, b] = self
                    .pattern
                    .pixel(u32::try_from(x).unwrap_or(0), y, frame_no);
                if let Some(c) = px.get_mut(r_at) {
                    *c = r;
                }
                if let Some(c) = px.get_mut(g_at) {
                    *c = g;
                }
                if let Some(c) = px.get_mut(b_at) {
                    *c = b;
                }
                // The fourth byte is set only when the format says it means
                // something. Leaving it alone in an X format is the honest
                // thing: a consumer that reads it is reading padding, and this
                // is where that should be caught.
                if opaque && let Some(a) = px.get_mut(3) {
                    *a = 255;
                }
            }
        }

        Frame::new(width, height, stride, self.format, damage, buffer).map_err(CaptureError::from)
    }
}

impl Capturer for SyntheticCapturer {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn monitors(&self) -> &[MonitorInfo] {
        &self.monitors
    }

    fn selected(&self) -> MonitorId {
        self.selected
    }

    fn format(&self) -> PixelFormat {
        self.format
    }

    fn reports_damage(&self) -> bool {
        true
    }

    fn select(&mut self, monitor: MonitorId) -> Result<(), CaptureError> {
        if monitor == self.selected {
            return Ok(());
        }
        let Some(info) = self.monitors.iter().find(|m| m.id == monitor) else {
            return Err(CaptureError::NoSuchMonitor {
                id: monitor,
                count: self.monitors.len(),
            });
        };
        let (width, height) = (info.geometry.width, info.geometry.height);
        // `with_extra_monitor` refused a size no pattern can render, so this
        // is unreachable unless a monitor list was built some other way.
        self.pattern = Pattern::new(width, height).map_err(|e| CaptureError::Backend {
            backend: BACKEND,
            detail: e.to_string(),
        })?;
        self.selected = monitor;
        // The new monitor has never been rendered, so the next frame is whole
        // and due immediately.
        self.started = false;
        self.next_due = None;
        Ok(())
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Capture, CaptureError> {
        if self.limit.is_some_and(|n| self.frame >= n) {
            return Ok(Capture::Unchanged);
        }

        if self.reconfigure_at == Some(self.frame) {
            self.reconfigure_at = None;
            return Ok(Capture::Reconfigured(self.monitors.clone()));
        }

        let now = Instant::now();
        match self.next_due {
            // Asked again before the next frame is due. If the caller is
            // willing to wait that long, wait; otherwise say nothing changed,
            // which is the truth.
            Some(due) if due > now => {
                let wait = due.saturating_duration_since(now);
                if wait > timeout {
                    return Ok(Capture::Unchanged);
                }
                std::thread::sleep(wait);
            }
            _ => {}
        }
        // , because `Instant` has no saturating form and a
        // deadline that cannot be represented should mean "never due" rather
        // than panic.
        self.next_due = Instant::now().checked_add(self.interval);

        // The first frame is a whole frame however little the picture
        // changed: a decoder with no reference has nothing to build on.
        let damage = if self.started {
            let raw = self
                .pattern
                .damage_between(self.frame.saturating_sub(1), self.frame);
            Regions::coalesce(&raw, &self.pattern.bounds())
        } else {
            Damage::Everything
        };
        self.started = true;

        let frame = self.render(damage)?;
        self.frame = self.frame.saturating_add(1);
        Ok(Capture::Frame(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_of(c: Capture) -> Frame {
        match c {
            Capture::Frame(f) => f,
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    fn grab(cap: &mut SyntheticCapturer) -> Frame {
        frame_of(
            cap.next_frame(Duration::from_secs(1))
                .expect("capture should succeed"),
        )
    }

    #[test]
    fn it_produces_frames_with_no_display_anywhere() {
        let mut cap = SyntheticCapturer::new(320, 240, 1000).expect("valid");
        let f = grab(&mut cap);
        assert_eq!((f.width(), f.height()), (320, 240));
        assert_eq!(cap.backend(), "synthetic");
        assert_eq!(cap.monitors().len(), 1);
        assert!(cap.monitors().first().is_some_and(|m| m.primary));
    }

    /// The first frame must be a whole frame: a decoder with no reference has
    /// nothing to apply a partial update to.
    #[test]
    fn the_first_frame_is_a_whole_frame() {
        let mut cap = SyntheticCapturer::new(320, 240, 1000).expect("valid");
        let first = grab(&mut cap);
        assert_eq!(*first.damage(), Damage::Everything);

        let second = grab(&mut cap);
        assert!(
            matches!(second.damage(), Damage::Regions(_)),
            "the second frame should report what actually changed, got {:?}",
            second.damage()
        );
    }

    /// **The point of the synthetic source.** Every pixel is exactly what the
    /// pattern says it should be, in the format the capturer was asked for —
    /// so a consumer can be checked against the source rather than against a
    /// previous run.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn every_pixel_matches_the_pattern_in_every_format() {
        for format in [
            PixelFormat::Bgra8,
            PixelFormat::Bgrx8,
            PixelFormat::Rgba8,
            PixelFormat::Rgbx8,
        ] {
            let mut cap = SyntheticCapturer::new(64, 48, 1000)
                .expect("valid")
                .with_format(format);
            let f = grab(&mut cap);
            let (r_at, g_at, b_at) = format.rgb_offsets();

            for y in 0..48u32 {
                for x in 0..64u32 {
                    let want = cap.pattern().pixel(x, y, 0);
                    let got = f.pixel(x, y).expect("in bounds");
                    assert_eq!(
                        [
                            got.get(r_at).copied().unwrap_or(0),
                            got.get(g_at).copied().unwrap_or(0),
                            got.get(b_at).copied().unwrap_or(0)
                        ],
                        want,
                        "{}: ({x},{y})",
                        format.name()
                    );
                    if format.has_alpha() {
                        assert_eq!(
                            got.get(3).copied(),
                            Some(255),
                            "{}: alpha must be opaque where it means something",
                            format.name()
                        );
                    }
                }
            }
        }
    }

    /// No real backend hands back a tightly-packed buffer, so the padded case
    /// is the one that has to work.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn a_padded_stride_still_renders_correctly() {
        for padding in [0usize, 1, 7, 64, 4096] {
            let mut cap = SyntheticCapturer::new(64, 48, 1000)
                .expect("valid")
                .with_row_padding(padding);
            let f = grab(&mut cap);
            assert_eq!(f.stride(), 64 * 4 + padding);

            // The corners identify the orientation, and a stride error moves
            // every row but the first.
            assert_eq!(
                f.pixel(0, 0).map(|p| [p[2], p[1], p[0]]),
                Some([255, 0, 0]),
                "padding {padding}: top left should be red"
            );
            assert_eq!(
                f.pixel(0, 47).map(|p| [p[2], p[1], p[0]]),
                Some([255, 255, 0]),
                "padding {padding}: bottom left should be yellow — a wrong \
                 stride puts something else here"
            );
            for y in 0..48u32 {
                for x in 0..64u32 {
                    let want = cap.pattern().pixel(x, y, 0);
                    let got = f.pixel(x, y).expect("in bounds");
                    assert_eq!(
                        [got[2], got[1], got[0]],
                        want,
                        "padding {padding}: ({x},{y}) is wrong, which is what a \
                         stride mistake looks like"
                    );
                }
            }
        }
    }

    /// The frame counter is in the picture, so a consumer can say which frame
    /// it is looking at without a second clock.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn the_frame_number_is_readable_out_of_the_pixels() {
        let mut cap = SyntheticCapturer::new(320, 240, 10_000).expect("valid");
        for expected in 0..20u64 {
            let f = grab(&mut cap);
            let read = cap.pattern().read_counter(|x, y| {
                let p = f.pixel(x, y).unwrap_or([0; 4]);
                [p[2], p[1], p[0]]
            });
            assert_eq!(read, expected, "frame {expected} did not read back");
        }
    }

    /// Reported damage must cover every pixel that actually changed. Anything
    /// less leaves stale pixels on a viewer's screen.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn reported_damage_covers_every_changed_pixel() {
        let mut cap = SyntheticCapturer::new(64, 48, 10_000).expect("valid");
        let mut previous = grab(&mut cap);

        for round in 1..30u64 {
            let current = grab(&mut cap);
            let covered = current.damage().rects(&current.bounds());

            for y in 0..48u32 {
                for x in 0..64u32 {
                    if previous.pixel(x, y) == current.pixel(x, y) {
                        continue;
                    }
                    let px = Rect::new(x as i32, y as i32, 1, 1);
                    assert!(
                        covered.iter().any(|r| r.contains(&px)),
                        "frame {round}: ({x},{y}) changed but was not reported \
                         as damaged"
                    );
                }
            }
            previous = current;
        }
    }

    /// The capturer must actually plumb the pattern's damage through
    /// coalescing rather than reporting a whole frame every time.
    ///
    /// A modest frame on purpose: the strict ratio at 1080p lives in the
    /// pattern's own tests, where it needs no rendering. This one is about the
    /// wiring, and rendering twenty 1080p frames in a debug build to read a
    /// metadata field took longer than the rest of the crate's tests put
    /// together.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn a_steady_stream_reports_only_a_little_damage() {
        let mut cap = SyntheticCapturer::new(320, 240, 1_000_000).expect("valid");
        let first = grab(&mut cap);
        assert_eq!(*first.damage(), Damage::Everything);

        for round in 0..20 {
            let f = grab(&mut cap);
            let changed = f.damage().changed_pixels(&f.bounds());
            assert!(
                changed > 0,
                "round {round}: something moved, so something must be damaged"
            );
            assert!(
                changed * 33 < f.bounds().area(),
                "round {round}: one frame of change reported {changed} pixels \
                 of {} — a whole frame every time would defeat the design",
                f.bounds().area()
            );
        }
    }

    /// Polling faster than the frame rate must say "nothing changed" rather
    /// than handing back the same picture again, which would make a
    /// damage-driven consumer encode duplicates.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "depends on real elapsed time: MIRI interprets rather than \
                  executes, so the 100ms frame deadline has already passed by \
                  the time the poll happens and a frame is correctly returned"
    )]
    fn polling_faster_than_the_frame_rate_reports_no_change() {
        let mut cap = SyntheticCapturer::new(64, 48, 10).expect("valid");
        let _ = grab(&mut cap);
        // 10 fps means the next frame is 100 ms away; ask with no patience.
        assert!(matches!(
            cap.next_frame(Duration::ZERO).expect("no error"),
            Capture::Unchanged
        ));
        assert_eq!(cap.frame_number(), 1, "and no frame was consumed");
    }

    /// A caller willing to wait gets the frame.
    #[test]
    fn a_caller_that_waits_gets_the_next_frame() {
        let mut cap = SyntheticCapturer::new(64, 48, 200).expect("valid");
        let _ = grab(&mut cap);
        let f = frame_of(cap.next_frame(Duration::from_secs(1)).expect("no error"));
        assert_eq!(f.width(), 64);
        assert_eq!(cap.frame_number(), 2);
    }

    /// A resolution change mid-session breaks every buffer size downstream, so
    /// it arrives as its own answer rather than as a differently-sized frame.
    #[test]
    fn a_geometry_change_is_reported_separately() {
        let mut cap = SyntheticCapturer::new(64, 48, 10_000)
            .expect("valid")
            .with_reconfigure_at(2);
        let _ = grab(&mut cap);
        let _ = grab(&mut cap);
        match cap.next_frame(Duration::from_secs(1)).expect("no error") {
            Capture::Reconfigured(monitors) => assert_eq!(monitors.len(), 1),
            other => panic!("expected a reconfigure, got {other:?}"),
        }
        // And then the stream carries on.
        let f = grab(&mut cap);
        assert_eq!(f.width(), 64);
    }

    #[test]
    fn a_limited_stream_stops() {
        let mut cap = SyntheticCapturer::new(64, 48, 100_000)
            .expect("valid")
            .with_frame_limit(3);
        for _ in 0..3 {
            let _ = grab(&mut cap);
        }
        assert!(matches!(
            cap.next_frame(Duration::from_secs(1)).expect("no error"),
            Capture::Unchanged
        ));
    }

    /// A steady stream must not allocate a new buffer per frame. This is what
    /// the pool is for, and it is easy to lose by holding a frame too long.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn a_steady_stream_reuses_its_buffers() {
        let mut cap = SyntheticCapturer::new(320, 240, 1_000_000).expect("valid");
        let first = grab(&mut cap);
        let address = first.bytes().as_ptr() as usize;
        drop(first);

        for round in 0..30 {
            let f = grab(&mut cap);
            assert_eq!(
                f.bytes().as_ptr() as usize,
                address,
                "round {round} allocated a new buffer instead of reusing one"
            );
        }
    }

    /// Every size the pattern allows must produce a valid frame.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "an exhaustive sweep: MIRI interprets rather than executes, so this takes minutes. This crate forbids unsafe code and has no FFI, so what MIRI is here for is the buffer pool's Drop, which the cheaper tests exercise"
    )]
    fn every_supported_size_produces_a_valid_frame() {
        for (w, h) in [(32u32, 16u32), (33, 17), (64, 48), (1920, 1080)] {
            let mut cap = SyntheticCapturer::new(w, h, 1_000_000)
                .expect("valid")
                .with_row_padding(13);
            let f = grab(&mut cap);
            assert_eq!((f.width(), f.height()), (w, h));
            assert_eq!(f.stride(), (w as usize) * 4 + 13);
            assert!(f.row(h - 1).is_some(), "{w}x{h}: the last row must exist");
            assert!(f.row(h).is_none());
        }
    }

    #[test]
    fn a_screen_too_small_for_the_pattern_is_refused() {
        assert!(SyntheticCapturer::new(8, 8, 30).is_err());
    }

    /// **The frames follow the selection, not just the bookkeeping.** A
    /// backend that changed `selected` and went on capturing the old monitor
    /// would report a switch that never happened, and the size is what makes
    /// that visible.
    #[test]
    fn selecting_another_monitor_changes_the_frames() {
        let mut cap = SyntheticCapturer::new(320, 240, 1_000_000)
            .expect("a size")
            .with_extra_monitor(160, 128)
            .expect("a second size");
        assert_eq!(cap.selected(), MonitorId(0));
        let first = frame_of(cap.next_frame(Duration::from_millis(50)).expect("a frame"));
        assert_eq!((first.width(), first.height()), (320, 240));

        cap.select(MonitorId(1)).expect("the switch");
        assert_eq!(cap.selected(), MonitorId(1));
        let second = frame_of(cap.next_frame(Duration::from_millis(50)).expect("a frame"));
        assert_eq!(
            (second.width(), second.height()),
            (160, 128),
            "the frames are of the monitor now selected"
        );
        // The new monitor has never been seen, so its first frame is whole.
        assert!(
            matches!(second.damage(), Damage::Everything),
            "a monitor never captured before cannot have partial damage: {:?}",
            second.damage()
        );

        // And back, which is the same requirement in the other direction.
        cap.select(MonitorId(0)).expect("the switch back");
        let third = frame_of(cap.next_frame(Duration::from_millis(50)).expect("a frame"));
        assert_eq!((third.width(), third.height()), (320, 240));
    }

    #[test]
    fn selecting_a_monitor_that_is_not_there_is_refused_and_changes_nothing() {
        let mut cap = SyntheticCapturer::new(320, 240, 1_000_000)
            .expect("a size")
            .with_extra_monitor(160, 128)
            .expect("a second size");
        let e = cap.select(MonitorId(7)).expect_err("there is no monitor 7");
        assert!(
            matches!(e, CaptureError::NoSuchMonitor { .. }),
            "and it says which of the two failures it was: {e:?}"
        );
        assert_eq!(cap.selected(), MonitorId(0), "still on the monitor it had");
        let frame = frame_of(cap.next_frame(Duration::from_millis(50)).expect("a frame"));
        assert_eq!((frame.width(), frame.height()), (320, 240));
    }

    /// Selecting the current monitor must not reset anything: it is what a
    /// controller does when somebody clicks the screen they are watching, and
    /// a whole frame for that is a visible hitch for no change.
    #[test]
    fn selecting_the_current_monitor_does_not_reset_the_capture() {
        let mut cap = SyntheticCapturer::new(320, 240, 1_000_000).expect("a size");
        let first = frame_of(cap.next_frame(Duration::from_millis(50)).expect("a frame"));
        assert!(
            matches!(first.damage(), Damage::Everything),
            "the first is whole"
        );
        cap.select(MonitorId(0)).expect("a no-op");
        let second = frame_of(cap.next_frame(Duration::from_millis(50)).expect("a frame"));
        assert!(
            !matches!(second.damage(), Damage::Everything),
            "a no-op selection must not make the next frame whole again"
        );
    }

    /// A monitor too small for the pattern is refused when the fixture is
    /// built, not when a session tries to switch to it — a monitor list with
    /// an unusable screen in it lies about the host it stands in for.
    #[test]
    fn an_extra_monitor_too_small_to_render_is_refused_at_once() {
        assert!(
            SyntheticCapturer::new(320, 240, 30)
                .expect("a size")
                .with_extra_monitor(8, 8)
                .is_err()
        );
    }
}
