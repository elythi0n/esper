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

/// One screen inside the picture.
///
/// Which screen it is lives in `served`, at the same position: the two are
/// built together by `retile` and are never written anywhere else, so a tile
/// carries where it sits and what to draw and nothing about identity.
#[derive(Debug)]
struct Tile {
    /// Where this screen sits in the picture, in picture coordinates.
    at: Rect,
    pattern: Pattern,
}

/// A synthetic screen, rendered on demand.
#[derive(Debug)]
pub struct SyntheticCapturer {
    /// The screens being rendered, in the order they were asked for.
    ///
    /// Never empty: `serve` refuses an empty set and the constructor starts
    /// with one. Laid out by [`crate::geometry::tiles`], so this is the same
    /// arrangement a real backend produces from the same monitor list.
    tiles: Vec<Tile>,
    /// The ids in `tiles`, kept only because [`Capturer::serving`] hands back
    /// a slice. Built in the one place that builds `tiles`.
    served: Vec<MonitorId>,
    /// The size of the whole picture, which is the bounding box of the tiles
    /// rather than any one screen's size.
    picture: (u32, u32),
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
    /// Monitors appearing and disappearing, each delivered once.
    ///
    /// **The one event that is nearly untestable on real hardware.** Unplugging
    /// a monitor mid-session breaks the announced layout, the selected monitor,
    /// the pointer's frame of reference and every buffer size downstream — and
    /// arranging it means somebody standing next to a machine with a cable in
    /// their hand. X11's own RandR 1.5 can add a *logical* monitor without one,
    /// but an `Xvfb` will not, so CI cannot do it either.
    replugs: Vec<(u64, Replug)>,
    /// The same sticky numbering a real backend uses, so a fixture cannot be
    /// kinder about monitor identity than the thing it stands in for.
    numbering: crate::capturer::Numbering,
}

/// A change to which monitors exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Replug {
    /// This one is unplugged.
    Gone(MonitorId),
    /// One appears, to the right of everything before it.
    New { width: u32, height: u32 },
    /// The desktop is rearranged and this one moves.
    Moved { id: MonitorId, x: i32, y: i32 },
}

impl SyntheticCapturer {
    /// A synthetic capturer of this size, at this frame rate.
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, PatternError> {
        let pattern = Pattern::new(width, height)?;
        let mut numbering = crate::capturer::Numbering::new();
        let mut monitors = vec![MonitorInfo {
            id: MonitorId(0),
            name: "synthetic-0".to_owned(),
            geometry: Rect::sized(width, height),
            scale: 1.0,
            primary: true,
        }];
        numbering.number(&mut monitors);
        let first = monitors.first().map_or(MonitorId(0), |m| m.id);
        Ok(Self {
            tiles: vec![Tile {
                at: Rect::sized(width, height),
                pattern,
            }],
            served: vec![first],
            picture: (width, height),
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
            replugs: Vec::new(),
            numbering,
        })
    }

    /// How much of the picture changes from frame to frame.
    ///
    /// A screen and a video are two different things to ask an encoder for,
    /// and the project's targets are stated for both. See
    /// [`crate::pattern::Motion`].
    #[must_use]
    pub fn with_motion(mut self, motion: crate::pattern::Motion) -> Self {
        for tile in &mut self.tiles {
            tile.pattern = tile.pattern.with_motion(motion);
        }
        self
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
        self.add_monitor(width, height)?;
        Ok(self)
    }

    /// The same, without consuming this.
    ///
    /// The builder form loses what it was given when it refuses, which makes a
    /// loop that adds monitors until one is too small impossible to write. This
    /// is what the loop uses and what the builder is written on top of.
    ///
    /// # Errors
    ///
    /// If no pattern can be rendered at that size.
    pub fn add_monitor(&mut self, width: u32, height: u32) -> Result<(), PatternError> {
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
        let name = format!("synthetic-{}", self.numbering.handed_out());
        let id = self.numbering.number_of(&name);
        self.monitors.push(MonitorInfo {
            name,
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
        Ok(())
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

    /// Unplug a monitor before the given frame.
    ///
    /// The list shrinks and, if the one that went was the one being captured,
    /// the selection moves — which is what a real backend does, and what
    /// everything above it has to cope with.
    #[must_use]
    pub fn with_unplug_at(mut self, frame: u64, monitor: MonitorId) -> Self {
        self.replugs.push((frame, Replug::Gone(monitor)));
        self
    }

    /// Plug a monitor in before the given frame.
    #[must_use]
    pub fn with_plug_at(mut self, frame: u64, width: u32, height: u32) -> Self {
        self.replugs.push((frame, Replug::New { width, height }));
        self
    }

    /// Move a monitor before the given frame, as rearranging a desktop does.
    ///
    /// Nothing about the picture changes, which is the point: a layout change
    /// that does not change the frame's size is the one a host announcing only
    /// on a size change will miss.
    #[must_use]
    pub fn with_move_at(mut self, frame: u64, monitor: MonitorId, x: i32, y: i32) -> Self {
        self.replugs
            .push((frame, Replug::Moved { id: monitor, x, y }));
        self
    }

    /// Apply whatever is due at this frame. Says whether anything was.
    fn replug(&mut self) -> bool {
        let Some(at) = self
            .replugs
            .iter()
            .position(|(frame, _)| *frame == self.frame)
        else {
            return false;
        };
        let Some((_, what)) = self.replugs.get(at).copied() else {
            return false;
        };
        self.replugs.remove(at);
        match what {
            Replug::Gone(id) => self.monitors.retain(|m| m.id != id),
            Replug::New { width, height } => {
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
                // **Never a number already in use**, and the same numbering
                // a real backend has: a monitor plugged in after one was
                // unplugged must not inherit its number, or a controller
                // holding the old list would think it already knew this screen.
                let name = format!("synthetic-{}", self.numbering.handed_out());
                let id = self.numbering.number_of(&name);
                self.monitors.push(MonitorInfo {
                    name,
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
            }
            Replug::Moved { id, x, y } => {
                if let Some(m) = self.monitors.iter_mut().find(|m| m.id == id) {
                    m.geometry.x = x;
                    m.geometry.y = y;
                }
            }
        }
        // A served screen may be the one that went, and a served screen may
        // have moved — which changes where it sits in a picture of several.
        // Re-tiling covers both: it drops what is gone, falls back to the
        // first monitor if that leaves nothing, and re-reads the geometry of
        // whatever is left.
        //
        // The picture's size is what everything downstream sizes itself from,
        // so rendering the old size into a stream announced at the new one
        // would be a fixture that lies about the host.
        let still_there: Vec<MonitorId> = self
            .served
            .iter()
            .copied()
            .filter(|id| self.monitors.iter().any(|m| m.id == *id))
            .collect();
        let want = if still_there.is_empty() {
            self.monitors.first().map_or(Vec::new(), |m| vec![m.id])
        } else {
            still_there
        };
        let before = self.picture;
        if self.retile(&want).is_ok() && self.picture != before {
            self.started = false;
        }
        true
    }

    /// The frame number the next capture will render.
    #[must_use]
    pub const fn frame_number(&self) -> u64 {
        self.frame
    }

    /// The pattern being rendered, for a test that wants to compute expected
    /// pixels or expected damage.
    ///
    /// The first screen's, which is the only one while one is served — and the
    /// frame of reference for the others, which sit beside it.
    ///
    /// `None` only if a backend broke the "never empty" contract, which is a
    /// bug rather than a state to render around.
    #[must_use]
    pub fn pattern(&self) -> Option<Pattern> {
        self.tiles.first().map(|t| t.pattern)
    }

    /// Lay out these screens and make a pattern for each.
    ///
    /// The one place `tiles`, `served` and `picture` are written, because
    /// three fields describing one arrangement is three chances to describe
    /// different arrangements.
    fn retile(&mut self, want: &[MonitorId]) -> Result<(), CaptureError> {
        if want.is_empty() {
            return Err(CaptureError::NothingToServe);
        }
        let mut geometry = Vec::with_capacity(want.len());
        for id in want {
            let Some(info) = self.monitors.iter().find(|m| m.id == *id) else {
                return Err(CaptureError::NoSuchMonitor {
                    id: *id,
                    count: self.monitors.len(),
                });
            };
            geometry.push(info.geometry);
        }
        if let Some((a, b)) = crate::geometry::first_overlap(&geometry) {
            return Err(CaptureError::ScreensOverlap {
                first: want.get(a).copied().unwrap_or(MonitorId(u32::MAX)),
                second: want.get(b).copied().unwrap_or(MonitorId(u32::MAX)),
            });
        }
        let (bounds, placed) = crate::geometry::tiles(&geometry);
        let mut tiles = Vec::with_capacity(want.len());
        for at in placed {
            // A pattern per screen rather than one stretched across the
            // picture: a test that cannot tell two screens apart cannot tell
            // whether either arrived.
            let pattern = Pattern::new(at.width, at.height).map_err(|e| CaptureError::Backend {
                backend: BACKEND,
                detail: e.to_string(),
            })?;
            tiles.push(Tile { at, pattern });
        }
        self.tiles = tiles;
        self.served = want.to_vec();
        self.picture = (bounds.width, bounds.height);
        Ok(())
    }

    /// Bytes per row, pixels plus padding.
    fn stride(&self) -> usize {
        (self.picture.0 as usize)
            .saturating_mul(self.format.bytes_per_pixel())
            .saturating_add(self.row_padding)
    }

    /// Render the current frame into a pooled buffer.
    ///
    /// **Blanked first when there is more than one screen.** A pooled buffer
    /// arrives with whatever was in it — poison in a debug build, a previous
    /// frame in a release one — so a gap between two screens would show
    /// recycled pixels of somewhere else. With one screen the tile is the
    /// whole picture and there is nothing to blank.
    fn render(&mut self, damage: Damage) -> Result<Frame, CaptureError> {
        let (width, height) = self.picture;
        let stride = self.stride();
        let bpp = self.format.bytes_per_pixel();
        let (r_at, g_at, b_at) = self.format.rgb_offsets();
        let opaque = self.format.has_alpha();
        let frame_no = self.frame;

        let mut buffer = self.pool.take(stride.saturating_mul(height as usize));
        if self.tiles.len() > 1 {
            buffer.fill(0);
        }

        for tile in &self.tiles {
            // Tile coordinates are the picture's own, so they are never
            // negative and the casts below cannot lose anything.
            let left = usize::try_from(tile.at.x).unwrap_or(0);
            let top = usize::try_from(tile.at.y).unwrap_or(0);
            for local_y in 0..tile.at.height {
                let row_at = stride
                    .saturating_mul(top.saturating_add(local_y as usize))
                    .saturating_add(left.saturating_mul(bpp));
                let row_bytes = (tile.at.width as usize).saturating_mul(bpp);
                let Some(row) = buffer.get_mut(row_at..row_at.saturating_add(row_bytes)) else {
                    continue;
                };
                for (x, px) in row.chunks_exact_mut(bpp).enumerate() {
                    let [r, g, b] =
                        tile.pattern
                            .pixel(u32::try_from(x).unwrap_or(0), local_y, frame_no);
                    if let Some(c) = px.get_mut(r_at) {
                        *c = r;
                    }
                    if let Some(c) = px.get_mut(g_at) {
                        *c = g;
                    }
                    if let Some(c) = px.get_mut(b_at) {
                        *c = b;
                    }
                    // The fourth byte is set only when the format says it
                    // means something. Leaving it alone in an X format is the
                    // honest thing: a consumer that reads it is reading
                    // padding, and this is where that should be caught.
                    if opaque && let Some(a) = px.get_mut(3) {
                        *a = 255;
                    }
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

    fn serving(&self) -> &[MonitorId] {
        &self.served
    }

    fn format(&self) -> PixelFormat {
        self.format
    }

    fn reports_damage(&self) -> bool {
        true
    }

    fn serve(&mut self, monitors: &[MonitorId]) -> Result<(), CaptureError> {
        if monitors == self.served.as_slice() {
            return Ok(());
        }
        self.retile(monitors)?;
        // What is being rendered has never been rendered, so the next frame is
        // whole and due immediately.
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
        if self.replug() {
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
            // Each screen's own damage, moved to where that screen sits. A
            // picture of two screens has two things moving in it, and a
            // consumer told only about the first would leave the second
            // stale.
            let mut raw = Vec::new();
            for tile in &self.tiles {
                raw.extend(
                    tile.pattern
                        .damage_between(self.frame.saturating_sub(1), self.frame)
                        .into_iter()
                        .map(|r| r.translate(tile.at.x, tile.at.y)),
                );
            }
            Regions::coalesce(&raw, &Rect::sized(self.picture.0, self.picture.1))
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

    /// **Two screens arrive as one picture, in the arrangement of the desk.**
    ///
    /// The picture is as wide as both and as tall as the taller, each screen
    /// carries its own pattern, and a consumer that only ever received the
    /// first screen's pixels would fail here.
    #[test]
    fn two_screens_are_one_picture_with_both_in_it() {
        let mut cap = SyntheticCapturer::new(320, 240, 1000)
            .expect("valid")
            .with_extra_monitor(160, 120)
            .expect("a second screen");
        let ids: Vec<MonitorId> = cap.monitors().iter().map(|m| m.id).collect();
        cap.serve(&ids).expect("both screens");
        assert_eq!(cap.serving(), ids.as_slice());
        assert_eq!(
            cap.selected(),
            ids.first().copied().expect("an id"),
            "the first served screen is the one everything per-stream keys on"
        );

        let f = grab(&mut cap);
        assert_eq!(
            (f.width(), f.height()),
            (480, 240),
            "as wide as both and as tall as the taller"
        );

        // The second screen's own pattern, at the offset it sits at.
        let second = Pattern::new(160, 120).expect("valid");
        let want = second.pixel(3, 5, 0);
        let got = f.pixel(320 + 3, 5).expect("in bounds");
        assert_eq!(
            [
                got.get(2).copied().unwrap_or(0),
                got.get(1).copied().unwrap_or(0),
                got.first().copied().unwrap_or(0)
            ],
            want,
            "the second screen's pixels are not where the layout says they are"
        );
    }

    /// **The part of the picture that belongs to no screen is black.**
    ///
    /// The 160x120 screen sits beside a 320x240 one, so the picture has 120
    /// rows below it that belong to nothing. A pooled buffer arrives with
    /// whatever was in it last — in a release build, the pixels of a previous
    /// frame — so this is the check that a gap is blanked rather than
    /// inherited.
    #[test]
    fn the_gap_beside_a_shorter_screen_is_black() {
        let mut cap = SyntheticCapturer::new(320, 240, 1000)
            .expect("valid")
            .with_extra_monitor(160, 120)
            .expect("a second screen");
        let ids: Vec<MonitorId> = cap.monitors().iter().map(|m| m.id).collect();
        cap.serve(&ids).expect("both screens");
        // Twice, because the second frame is the one that gets a recycled
        // buffer: the first may well have had a fresh one.
        let _ = grab(&mut cap);
        let f = grab(&mut cap);
        for (x, y) in [(320u32, 200u32), (479, 239), (400, 130)] {
            let px = f.pixel(x, y).expect("in bounds");
            assert_eq!(
                (px.first().copied(), px.get(1).copied(), px.get(2).copied()),
                (Some(0), Some(0), Some(0)),
                "({x},{y}) belongs to no screen and is not black"
            );
        }
    }

    /// A picture of no screens is refused, rather than becoming a zero-sized
    /// frame that something downstream divides by.
    #[test]
    fn serving_nothing_is_refused() {
        let mut cap = SyntheticCapturer::new(320, 240, 1000).expect("valid");
        assert!(matches!(cap.serve(&[]), Err(CaptureError::NothingToServe)));
        assert_eq!(cap.serving().len(), 1, "and it keeps what it had");
    }

    /// A screen that is not there is refused **by name**, and the capturer
    /// goes on sending what it was sending: a controller asking for a monitor
    /// somebody unplugged should see a refusal, not a stopped session.
    #[test]
    fn a_screen_that_is_not_there_is_refused_and_changes_nothing() {
        let mut cap = SyntheticCapturer::new(320, 240, 1000).expect("valid");
        let before = cap.serving().to_vec();
        let missing = MonitorId(77);
        assert!(matches!(
            cap.serve(&[missing]),
            Err(CaptureError::NoSuchMonitor { id, .. }) if id == missing
        ));
        assert_eq!(cap.serving(), before.as_slice());
        let f = grab(&mut cap);
        assert_eq!((f.width(), f.height()), (320, 240));
    }

    /// **Asking for what is already being sent costs nothing.** A controller
    /// clicking the screen it is watching should not cost a keyframe, and the
    /// evidence is that the next frame is still a damage report rather than a
    /// whole picture.
    #[test]
    fn serving_the_same_screens_again_is_not_a_reset() {
        let mut cap = SyntheticCapturer::new(320, 240, 1000).expect("valid");
        let ids = cap.serving().to_vec();
        let _ = grab(&mut cap);
        cap.serve(&ids).expect("the same screens");
        let next = grab(&mut cap);
        assert!(
            matches!(next.damage(), Damage::Regions(_)),
            "re-serving the same screens threw away the reference picture"
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
                    let want = cap.pattern().expect("a tile").pixel(x, y, 0);
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
                    let want = cap.pattern().expect("a tile").pixel(x, y, 0);
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
            let read = cap.pattern().expect("a tile").read_counter(|x, y| {
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
