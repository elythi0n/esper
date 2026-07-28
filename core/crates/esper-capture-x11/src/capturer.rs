//! The X11 capturer.
//!
//! # The surface is kept, the frames are copies
//!
//! Only what changed is fetched from the server, so something has to remember
//! the rest. That cannot be a pooled buffer — a recycled buffer holds whatever
//! the last frame that used it held, and the parts nothing damaged have to be
//! the *previous frame's* pixels. So this owns one surface for the selected
//! monitor, updates the damaged bands in it, and hands out a copy.
//!
//! The copy looks like waste and is not. The pipeline reads a whole frame more
//! often than it looks — the first frame, after any resize, and whenever the
//! gate reports that most of the screen changed — so a frame with stale gaps
//! in it would be correct almost always, which is the worst kind of correct.
//! And the copy is one sequential memcpy of a few megabytes against an encode
//! measured in milliseconds.
//!
//! # One round trip, however many bands
//!
//! `ShmGetImage` takes an offset into the segment, so every band of a frame
//! can be requested before any reply is read: the requests go out together and
//! the replies come back together. Thirty-two bands cost one round trip rather
//! than thirty-two, which matters because a round trip to the X server is tens
//! of microseconds and the frame budget is sixteen milliseconds.

use std::time::{Duration, Instant};

use esper_capture::capturer::{Capture, CaptureError, Capturer, MonitorId, MonitorInfo};
use esper_capture::damage::{Damage, Regions};
use esper_capture::frame::{Frame, PixelFormat};
use esper_capture::geometry::Rect;
use esper_capture::pool::BufferPool;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::shm;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, Window};
use x11rb::rust_connection::RustConnection;

use crate::damage::Tracker;
use crate::monitors::{self, Monitors};
use crate::shm::Segment;

/// The backend name reported in logs and the session HUD.
pub const BACKEND: &str = "x11";

/// Bytes per pixel, for every layout this backend accepts.
const BPP: usize = 4;

/// How the pixels are being moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// MIT-SHM: the server blits into memory both processes can see.
    SharedMemory,
    /// The pixels come back in the reply, over the socket.
    ///
    /// Correct and slower. The case that needs it is a *remote* X display,
    /// where shared memory is not shared with anything.
    Socket,
}

impl Transport {
    /// A phrase for the startup log.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::SharedMemory => "MIT-SHM",
            Self::Socket => "the X socket (no MIT-SHM: expect a remote display to be slow)",
        }
    }
}

/// Screen capture through an X server.
pub struct X11Capturer {
    conn: RustConnection,
    root: Window,
    format: PixelFormat,
    monitors: Vec<MonitorInfo>,
    source: monitors::Source,
    /// Numbers that stay with a monitor for as long as this capturer runs.
    ///
    /// Owned here because this is the thing with the lifetime they are stable
    /// across. See [`esper_capture::Numbering`] for what goes wrong without it.
    numbering: esper_capture::Numbering,
    selected: MonitorId,
    /// `None` when the server has no DAMAGE, in which case every frame is
    /// reported as wholly changed and the idle ladder is the only thing
    /// keeping a static screen cheap.
    tracker: Option<Tracker>,
    segment: Option<Segment>,
    transport: Transport,
    /// The current picture of the selected monitor, tightly packed.
    surface: Vec<u8>,
    /// Somewhere to land a band's packed pixels before they are placed into
    /// the surface. Reused.
    scratch: Vec<u8>,
    pool: BufferPool,
    /// Whether a frame has been produced yet. The first one is whole: there is
    /// no previous picture for damage to be relative to.
    started: bool,
}

impl std::fmt::Debug for X11Capturer {
    /// The shape and the mechanism, never the pixels.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Capturer")
            .field("monitors", &self.monitors.len())
            .field("selected", &self.selected)
            .field("format", &self.format.name())
            .field("transport", &self.transport)
            .field("damage", &self.tracker.is_some())
            .finish_non_exhaustive()
    }
}

impl X11Capturer {
    /// Connect to the X server named by `DISPLAY` and capture one monitor.
    ///
    /// `monitor` selects by id; `None` takes the first, which
    /// [`monitors::enumerate`] has ordered primary-first.
    pub fn open(monitor: Option<MonitorId>) -> Result<Self, CaptureError> {
        let (conn, screen_num) = x11rb::connect(None).map_err(|e| CaptureError::NoBackend {
            tried: format!("X11 via DISPLAY: {e}"),
        })?;
        let setup = conn.setup().clone();
        let screen = setup
            .roots
            .get(screen_num)
            .ok_or_else(|| CaptureError::Backend {
                backend: BACKEND,
                detail: format!("the server has no screen {screen_num}"),
            })?
            .clone();
        let root = screen.root;

        let format =
            crate::format::of(&setup, screen.root_depth, screen.root_visual).map_err(|e| {
                CaptureError::Backend {
                    backend: BACKEND,
                    detail: e.to_string(),
                }
            })?;

        let found = monitors::enumerate(&conn, root).map_err(|e| CaptureError::Backend {
            backend: BACKEND,
            detail: e.to_string(),
        })?;
        let Monitors { mut list, source } = found;
        let mut numbering = esper_capture::Numbering::new();
        numbering.number(&mut list);

        let selected = match monitor {
            None => list.first().map(|m| m.id).unwrap_or(MonitorId(0)),
            Some(id) => {
                if list.iter().any(|m| m.id == id) {
                    id
                } else {
                    return Err(CaptureError::NoSuchMonitor {
                        id,
                        count: list.len(),
                    });
                }
            }
        };

        // Ask to be told when the desktop is rearranged. Without this a
        // monitor being unplugged is discovered by capturing outside the
        // screen, which the server answers with an error rather than a hint.
        let _ = conn.randr_select_input(
            root,
            randr::NotifyMask::SCREEN_CHANGE
                | randr::NotifyMask::CRTC_CHANGE
                | randr::NotifyMask::OUTPUT_CHANGE,
        );

        let tracker = match Tracker::create(&conn, root) {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!("no XDamage ({e}); every frame will be reported as wholly changed");
                None
            }
        };

        let (segment, transport) = match crate::shm::probe(&conn) {
            Ok(_) => (None, Transport::SharedMemory),
            Err(e) => {
                tracing::warn!("no MIT-SHM ({e}); pixels will come over the socket");
                (None, Transport::Socket)
            }
        };

        let mut me = Self {
            conn,
            root,
            format,
            monitors: list,
            source,
            numbering,
            selected,
            tracker,
            segment,
            transport,
            surface: Vec::new(),
            scratch: Vec::new(),
            pool: BufferPool::default(),
            started: false,
        };
        me.resize_for_selected()?;
        Ok(me)
    }

    /// Where the monitor being captured sits on the desktop.
    fn geometry(&self) -> Rect {
        self.monitors
            .iter()
            .find(|m| m.id == self.selected)
            .map_or(Rect::sized(0, 0), |m| m.geometry)
    }

    /// How the monitor list was obtained, for the log.
    #[must_use]
    pub const fn monitor_source(&self) -> monitors::Source {
        self.source
    }

    /// How the pixels are being moved.
    #[must_use]
    pub const fn transport(&self) -> Transport {
        self.transport
    }

    /// Stop using shared memory and read the pixels over the socket instead.
    ///
    /// Two reasons this is public. It is how the two transports get compared —
    /// they have to place rows identically, and a shear in one of them would
    /// otherwise only appear on machines that take the other path. And it is
    /// what an operator needs when shared memory is the suspect: a session
    /// that works this way and not otherwise says where to look.
    pub fn force_socket_transport(&mut self) {
        if let Some(segment) = self.segment.take() {
            segment.detach(&self.conn);
        }
        self.transport = Transport::Socket;
        // The surface still holds the last picture, so damage stays valid.
    }

    /// Size the surface and the shared segment for the selected monitor.
    fn resize_for_selected(&mut self) -> Result<(), CaptureError> {
        let g = self.geometry();
        let bytes = (g.width as usize)
            .saturating_mul(g.height as usize)
            .saturating_mul(BPP);
        if bytes == 0 {
            return Err(CaptureError::Backend {
                backend: BACKEND,
                detail: format!("{} has no size", self.selected),
            });
        }
        self.surface.clear();
        self.surface.resize(bytes, 0);
        self.scratch.clear();
        // The surface no longer holds a picture, so the next frame is whole.
        self.started = false;

        if self.transport == Transport::SharedMemory {
            if let Some(old) = self.segment.take() {
                old.detach(&self.conn);
            }
            match Segment::create(&self.conn, bytes) {
                Ok(s) => self.segment = Some(s),
                Err(e) => {
                    tracing::warn!("MIT-SHM segment failed ({e}); falling back to the socket");
                    self.transport = Transport::Socket;
                }
            }
        }
        Ok(())
    }

    /// Re-read the monitor list; `true` when the desktop is now different.
    fn refresh_monitors(&mut self) -> Result<bool, CaptureError> {
        let mut found =
            monitors::enumerate(&self.conn, self.root).map_err(|e| CaptureError::Backend {
                backend: BACKEND,
                detail: e.to_string(),
            })?;
        // **Numbered before it is compared.** Fresh out of `enumerate` every
        // monitor carries a placeholder, so a comparison against the numbered
        // list this holds would say the layout changed every single time.
        self.numbering.number(&mut found.list);
        if monitors::same_layout(&self.monitors, &found.list) {
            return Ok(false);
        }
        let was = self.geometry();
        self.monitors = found.list;
        self.source = found.source;
        // The selected monitor may be gone. Falling back to the first is
        // better than failing: a session watching a screen that was unplugged
        // should show something.
        if !self.monitors.iter().any(|m| m.id == self.selected) {
            self.selected = self.monitors.first().map(|m| m.id).unwrap_or(MonitorId(0));
        }
        if self.geometry() != was {
            self.resize_for_selected()?;
        }
        Ok(true)
    }

    /// Wait for something to happen, or for the deadline.
    ///
    /// Returns what arrived. Events are drained fully each time round: XDamage
    /// at `NON_EMPTY` sends one event per accumulation, but a burst of RandR
    /// changes can queue several and reacting to the first while three more
    /// wait produces a reconfiguration per event.
    fn wait(&mut self, timeout: Duration) -> Result<Woke, CaptureError> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            self.conn.flush().map_err(|e| CaptureError::Backend {
                backend: BACKEND,
                detail: e.to_string(),
            })?;

            let mut woke = Woke::Nothing;
            loop {
                match self.conn.poll_for_event() {
                    Ok(Some(ev)) => match ev {
                        Event::DamageNotify(_) => {
                            if woke != Woke::Layout {
                                woke = Woke::Damage;
                            }
                        }
                        Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => {
                            woke = Woke::Layout;
                        }
                        _ => {}
                    },
                    Ok(None) => break,
                    Err(e) => {
                        return Err(CaptureError::Backend {
                            backend: BACKEND,
                            detail: e.to_string(),
                        });
                    }
                }
            }
            if woke != Woke::Nothing || !self.started {
                return Ok(if self.started { woke } else { Woke::Damage });
            }

            let left = deadline
                .map(|d| d.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::ZERO);
            if left.is_zero() {
                return Ok(Woke::Nothing);
            }
            self.poll_socket(left)?;
        }
    }

    /// Block on the X connection for at most `left`.
    fn poll_socket(&self, left: Duration) -> Result<(), CaptureError> {
        let millis = i32::try_from(left.as_millis().max(1)).unwrap_or(i32::MAX);
        let fd = self.conn.stream();
        let mut fds = [rustix::event::PollFd::new(fd, rustix::event::PollFlags::IN)];
        match rustix::event::poll(
            &mut fds,
            Some(&rustix::event::Timespec {
                tv_sec: i64::from(millis) / 1000,
                tv_nsec: (i64::from(millis) % 1000) * 1_000_000,
            }),
        ) {
            Ok(_) => Ok(()),
            // A signal arriving is not a capture failure; the caller's
            // deadline is still the deadline, so the loop simply goes round.
            Err(rustix::io::Errno::INTR) => Ok(()),
            Err(e) => Err(CaptureError::Backend {
                backend: BACKEND,
                detail: format!("waiting on the X connection: {e}"),
            }),
        }
    }
}

/// What ended a wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Woke {
    Nothing,
    Damage,
    Layout,
}

impl Drop for X11Capturer {
    fn drop(&mut self) {
        if let Some(s) = self.segment.take() {
            s.detach(&self.conn);
        }
        if let Some(t) = self.tracker.take() {
            t.destroy(&self.conn);
        }
        let _ = self.conn.flush();
    }
}

impl Capturer for X11Capturer {
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
        self.tracker.is_some()
    }

    /// Cheap here, because the capture is of the root either way: what changes
    /// is the rectangle of it that gets read. No new connection, no new
    /// permission, and the accumulated damage stays valid because the surface
    /// it described has been discarded and the next frame is whole.
    fn select(&mut self, monitor: MonitorId) -> Result<(), CaptureError> {
        if monitor == self.selected {
            return Ok(());
        }
        if !self.monitors.iter().any(|m| m.id == monitor) {
            return Err(CaptureError::NoSuchMonitor {
                id: monitor,
                count: self.monitors.len(),
            });
        }
        let was = self.selected;
        self.selected = monitor;
        // A segment that could not be made for the new size leaves the
        // capturer sized for a monitor it is no longer pointed at, so the old
        // selection goes back.
        if let Err(e) = self.resize_for_selected() {
            self.selected = was;
            let _ = self.resize_for_selected();
            return Err(e);
        }
        Ok(())
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Capture, CaptureError> {
        let deadline = Instant::now().checked_add(timeout);
        let remaining = move || {
            deadline.map_or(Duration::ZERO, |d| {
                d.saturating_duration_since(Instant::now())
            })
        };

        // A loop rather than a single pass, because being woken is not the
        // same as having something to send. Damage anywhere on the desktop
        // wakes this, and on a multi-monitor host most of it belongs to a
        // screen nobody asked for.
        loop {
            match self.wait(remaining())? {
                Woke::Nothing => return Ok(Capture::Unchanged),
                Woke::Layout => {
                    if self.refresh_monitors()? {
                        return Ok(Capture::Reconfigured(self.monitors.clone()));
                    }
                }
                Woke::Damage => {}
            }

            let g = self.geometry();
            let whole = !self.started || self.tracker.is_none();

            // **Drain the damage region first, always — even when the answer
            // is going to be `Everything` anyway.**
            //
            // `DamageSubtract` is what empties the server's accumulated
            // region, and at `NON_EMPTY` the event only fires when that
            // region goes from empty to not. Report a whole frame without
            // draining and the region stays non-empty for the rest of the
            // session, so the transition never happens again and no further
            // event is ever sent. The backend produces exactly one frame and
            // then blocks until its timeout, forever, with no error anywhere.
            let raw = match self.tracker.as_ref() {
                Some(t) => t.take(&self.conn).map_err(|e| CaptureError::Backend {
                    backend: BACKEND,
                    detail: e.to_string(),
                })?,
                None => Vec::new(),
            };

            let damage = if whole {
                Damage::Everything
            } else {
                to_frame_coordinates(&raw, &g)
            };

            if damage.is_empty() {
                // Something changed, and none of it was on this monitor.
                //
                // Returning `Unchanged` here would be true and useless: the
                // caller polls again immediately, gets woken by the next
                // change on the neighbouring screen, and the capture thread
                // spins for as long as anything else on the desktop is busy.
                // On the four-monitor desktop this was written against that
                // turned a 300 ms wait into a 99 ms one, over and over.
                if remaining().is_zero() {
                    return Ok(Capture::Unchanged);
                }
                continue;
            }

            let bands = damage.rects(&Rect::sized(g.width, g.height));
            self.fetch(&g, &bands)?;
            self.started = true;

            let stride = (g.width as usize).saturating_mul(BPP);
            let mut buffer = self.pool.take(self.surface.len());
            if let Some(dst) = buffer.get_mut(..self.surface.len()) {
                dst.copy_from_slice(&self.surface);
            }
            let frame = Frame::new(g.width, g.height, stride, self.format, damage, buffer)?;
            return Ok(Capture::Frame(frame));
        }
    }
}

impl X11Capturer {
    /// Bring `bands` of the monitor into the surface.
    fn fetch(&mut self, g: &Rect, bands: &[Rect]) -> Result<(), CaptureError> {
        match self.transport {
            Transport::SharedMemory => self.fetch_shm(g, bands),
            Transport::Socket => self.fetch_socket(g, bands),
        }
    }

    fn fetch_shm(&mut self, g: &Rect, bands: &[Rect]) -> Result<(), CaptureError> {
        let Some(segment) = self.segment.as_ref() else {
            return self.fetch_socket(g, bands);
        };
        let fail = |detail: String| CaptureError::Backend {
            backend: BACKEND,
            detail,
        };

        // Send every band before reading any reply: the requests pipeline and
        // the whole frame costs one round trip.
        let mut placed: Vec<(Rect, usize)> = Vec::with_capacity(bands.len());
        let mut offset = 0usize;
        let mut cookies = Vec::with_capacity(bands.len());
        for band in bands {
            let (Ok(x), Ok(y), Ok(w), Ok(h)) = (
                i16::try_from(g.x.saturating_add(band.x)),
                i16::try_from(g.y.saturating_add(band.y)),
                u16::try_from(band.width),
                u16::try_from(band.height),
            ) else {
                return Err(fail(format!("{band:?} does not fit an X11 request")));
            };
            let bytes = (band.width as usize)
                .saturating_mul(band.height as usize)
                .saturating_mul(BPP);
            if offset.saturating_add(bytes) > segment.size() {
                return Err(fail(format!(
                    "{} bands need more than the {}-byte segment",
                    bands.len(),
                    segment.size()
                )));
            }
            let at = u32::try_from(offset).map_err(|_| fail("segment offset overflowed".into()))?;
            let cookie = shm::get_image(
                &self.conn,
                self.root,
                x,
                y,
                w,
                h,
                !0,
                ImageFormat::Z_PIXMAP.into(),
                segment.id(),
                at,
            )
            .map_err(|e| fail(e.to_string()))?;
            cookies.push(cookie);
            placed.push((*band, offset));
            offset = offset.saturating_add(bytes);
        }
        for cookie in cookies {
            let _ = cookie.reply().map_err(|e| fail(e.to_string()))?;
        }

        // Then read them out and put them where they belong.
        let stride = (g.width as usize).saturating_mul(BPP);
        for (band, at) in placed {
            let row_bytes = (band.width as usize).saturating_mul(BPP);
            let bytes = row_bytes.saturating_mul(band.height as usize);
            // The whole monitor lands straight in the surface, no shuffling.
            if band.x == 0 && band.width == g.width && row_bytes == stride {
                let start = (band.y as usize).saturating_mul(stride);
                let Some(dst) = self.surface.get_mut(start..start.saturating_add(bytes)) else {
                    return Err(fail(format!("{band:?} does not fit the surface")));
                };
                segment
                    .read_into(at, dst)
                    .map_err(|e| fail(e.to_string()))?;
                continue;
            }
            self.scratch.clear();
            self.scratch.resize(bytes, 0);
            segment
                .read_into(at, &mut self.scratch)
                .map_err(|e| fail(e.to_string()))?;
            place(&self.scratch, &mut self.surface, band, stride, row_bytes).map_err(&fail)?;
        }
        Ok(())
    }

    fn fetch_socket(&mut self, g: &Rect, bands: &[Rect]) -> Result<(), CaptureError> {
        let fail = |detail: String| CaptureError::Backend {
            backend: BACKEND,
            detail,
        };
        let stride = (g.width as usize).saturating_mul(BPP);
        let mut cookies = Vec::with_capacity(bands.len());
        for band in bands {
            let (Ok(x), Ok(y), Ok(w), Ok(h)) = (
                i16::try_from(g.x.saturating_add(band.x)),
                i16::try_from(g.y.saturating_add(band.y)),
                u16::try_from(band.width),
                u16::try_from(band.height),
            ) else {
                return Err(fail(format!("{band:?} does not fit an X11 request")));
            };
            let cookie = self
                .conn
                .get_image(ImageFormat::Z_PIXMAP, self.root, x, y, w, h, !0)
                .map_err(|e| fail(e.to_string()))?;
            cookies.push((*band, cookie));
        }
        for (band, cookie) in cookies {
            let reply = cookie.reply().map_err(|e| fail(e.to_string()))?;
            let row_bytes = (band.width as usize).saturating_mul(BPP);
            place(&reply.data, &mut self.surface, band, stride, row_bytes).map_err(&fail)?;
        }
        Ok(())
    }
}

/// Move damage from desktop coordinates into frame coordinates.
///
/// # The multi-monitor bug this exists to prevent
///
/// XDamage reports in the coordinates of the drawable it watches, which is the
/// root window — the whole desktop. A monitor that is not at the desktop origin
/// therefore gets rectangles whose numbers are correct and whose *meaning* is
/// wrong: damage at desktop (1920, 521) is at frame (0, 0) on the monitor
/// starting there. Passed through untranslated it either lands outside the
/// frame, where the converter refuses it, or worse lands inside it and repaints
/// the wrong part of the screen.
///
/// Rectangles from other monitors are dropped rather than clamped to an edge.
/// A neighbouring screen scrolling is not a change to this one, and clamping
/// would report a strip of this monitor's edge as damaged every time anything
/// moved anywhere on the desktop.
///
/// The clip here is deliberately **redundant**: `Regions::coalesce` clamps to
/// the frame as well, and clipping before translation is the same operation as
/// clipping after it, because translation is a bijection. Removing this line
/// changes no result — which is only visible if you try it and nothing fails.
/// It stays because it makes this function's contract its own rather than a
/// property of what `coalesce` happens to do internally, and
/// `clipping_before_translation_matches_clipping_after` holds the two to each
/// other so that a change to either is a test failure rather than a leak.
///
/// Lifted out of the capture path because it is arithmetic, and arithmetic can
/// be tested on a machine with no display. The live tests can only check this
/// on a host that *has* an off-origin monitor, and CI does not have one.
fn to_frame_coordinates(raw: &[Rect], monitor: &Rect) -> Damage {
    let bounds = Rect::sized(monitor.width, monitor.height);
    let local: Vec<Rect> = raw
        .iter()
        .filter_map(|r| r.clamp_to(monitor))
        .map(|r| r.translate(-monitor.x, -monitor.y))
        .collect();
    if local.is_empty() {
        return Damage::Nothing;
    }
    Regions::coalesce(&local, &bounds)
}

/// Copy a band's tightly packed rows into the surface.
///
/// Split out so both transports share it: getting the row arithmetic wrong in
/// one of them and not the other is a shear that only appears on whichever
/// machine takes the other path.
fn place(
    packed: &[u8],
    surface: &mut [u8],
    band: Rect,
    stride: usize,
    row_bytes: usize,
) -> Result<(), String> {
    let left = (band.x.max(0) as usize).saturating_mul(BPP);
    for row in 0..band.height as usize {
        let from = row.saturating_mul(row_bytes);
        let Some(src) = packed.get(from..from.saturating_add(row_bytes)) else {
            return Err(format!(
                "{band:?}: the server returned {} bytes, short of {}",
                packed.len(),
                row_bytes.saturating_mul(band.height as usize)
            ));
        };
        let start = (band.y.max(0) as usize)
            .saturating_add(row)
            .saturating_mul(stride)
            .saturating_add(left);
        let Some(dst) = surface.get_mut(start..start.saturating_add(row_bytes)) else {
            return Err(format!("{band:?} does not fit the surface"));
        };
        dst.copy_from_slice(src);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    /// The monitor this project's own development machine has third from the
    /// left: 2560x1440 at 1920,521. Nothing about it is at the origin, which
    /// is why it makes a better fixture than a 1920x1080 at 0,0.
    const OFF_ORIGIN: Rect = Rect {
        x: 1920,
        y: 521,
        width: 2560,
        height: 1440,
    };

    #[test]
    fn damage_is_moved_into_frame_coordinates() {
        // The top-left pixel of this monitor, as the desktop names it.
        let d = to_frame_coordinates(&[rect(1920, 521, 10, 10)], &OFF_ORIGIN);
        assert_eq!(
            d.rects(&Rect::sized(2560, 1440)),
            vec![rect(0, 0, 10, 10)],
            "the monitor's own origin must become the frame's origin"
        );
    }

    #[test]
    fn every_reported_rectangle_lands_inside_the_frame() {
        let bounds = Rect::sized(OFF_ORIGIN.width, OFF_ORIGIN.height);
        // A sweep across and past both edges, so anything that forgot to
        // translate or forgot to clip shows up.
        for x in (0..6000).step_by(97) {
            for y in (0..3000).step_by(89) {
                let d = to_frame_coordinates(&[rect(x, y, 200, 150)], &OFF_ORIGIN);
                for r in d.rects(&bounds) {
                    assert!(
                        bounds.contains(&r),
                        "desktop damage at {x},{y} produced {r:?}, outside {bounds:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn damage_on_another_monitor_is_dropped_not_clamped() {
        // The laptop panel on the same desktop, well clear of OFF_ORIGIN.
        let elsewhere = rect(0, 1777, 1920, 1200);
        let d = to_frame_coordinates(&[elsewhere], &OFF_ORIGIN);
        assert_eq!(
            d,
            Damage::Nothing,
            "a neighbouring screen changing is not a change to this one; clamping it \
             would report an edge strip as damaged whenever anything moved anywhere"
        );
    }

    #[test]
    fn a_rectangle_straddling_the_edge_keeps_only_the_part_inside() {
        // Half on this monitor, half to the left of it.
        let d = to_frame_coordinates(&[rect(1820, 521, 200, 100)], &OFF_ORIGIN);
        assert_eq!(
            d.rects(&Rect::sized(2560, 1440)),
            vec![rect(0, 0, 100, 100)],
            "the overlapping half must survive at the frame's origin"
        );
    }

    #[test]
    fn nothing_in_means_nothing_out() {
        assert_eq!(to_frame_coordinates(&[], &OFF_ORIGIN), Damage::Nothing);
        assert_eq!(
            to_frame_coordinates(&[rect(0, 0, 0, 0)], &OFF_ORIGIN),
            Damage::Nothing
        );
    }

    /// A monitor at the origin is the case that hides a missing translation,
    /// because subtracting zero is the same as not subtracting.
    #[test]
    fn a_monitor_at_the_origin_is_unchanged() {
        let at_origin = Rect::sized(1600, 900);
        let d = to_frame_coordinates(&[rect(32, 32, 64, 64)], &at_origin);
        assert_eq!(d.rects(&Rect::sized(1600, 900)), vec![rect(32, 32, 64, 64)]);
    }

    /// Damage covering most of the monitor is reported as the whole frame,
    /// which is the region tracker's decision and has to survive translation.
    #[test]
    fn most_of_the_screen_becomes_the_whole_screen() {
        let d = to_frame_coordinates(&[OFF_ORIGIN], &OFF_ORIGIN);
        assert_eq!(d, Damage::Everything);
    }

    /// The clip in `to_frame_coordinates` is redundant with the one inside
    /// `Regions::coalesce`, and this is what says so out loud. If either side
    /// stops clipping, the two stop agreeing and this fails — rather than the
    /// capturer quietly starting to report damage from a neighbouring screen.
    #[test]
    fn clipping_before_translation_matches_clipping_after() {
        let bounds = Rect::sized(OFF_ORIGIN.width, OFF_ORIGIN.height);
        for x in (-500..6200).step_by(131) {
            for y in (-500..3200).step_by(113) {
                let raw = rect(x, y, 300, 220);
                let with_clip = to_frame_coordinates(&[raw], &OFF_ORIGIN);
                // The same thing with the clip left out: translate, then let
                // coalesce do the clamping.
                let translated = raw.translate(-OFF_ORIGIN.x, -OFF_ORIGIN.y);
                let without_clip = if translated.is_empty() {
                    Damage::Nothing
                } else {
                    let d = Regions::coalesce(&[translated], &bounds);
                    if d.is_empty() { Damage::Nothing } else { d }
                };
                assert_eq!(
                    with_clip, without_clip,
                    "at {x},{y} clipping before translation and after disagree"
                );
            }
        }
    }

    /// Two thin strips crossing at right angles: the case that must not be
    /// merged into its bounding box, checked here because translation is
    /// where the rectangles are rebuilt.
    #[test]
    fn crossing_strips_survive_translation_without_merging() {
        let vertical = rect(2000, 521, 8, 1440);
        let horizontal = rect(1920, 1000, 2560, 8);
        let d = to_frame_coordinates(&[vertical, horizontal], &OFF_ORIGIN);
        let bounds = Rect::sized(2560, 1440);
        let area: u64 = d.rects(&bounds).iter().map(Rect::area).sum();
        let bounding_box = 2560u64 * 1440;
        assert!(
            area < bounding_box / 20,
            "the two strips cover {area} px but were reported as {}, near the \
             {bounding_box} px bounding box",
            area
        );
        for r in d.rects(&bounds) {
            assert!(bounds.contains(&r), "{r:?} escaped the frame");
        }
    }

    /// **A band narrower than the frame, at a non-zero offset.**
    ///
    /// The live tests capture whatever the desktop happens to damage, and on
    /// both machines this was written against that turned out to be full-width
    /// bands every time — so `left`, the horizontal half of this arithmetic,
    /// was exercised by nothing that could fail. Shifting it by a pixel broke
    /// no test at all, which is the only reason this exists: a band is placed
    /// here at a known offset, and a shift in either direction shows up.
    #[test]
    fn a_band_lands_at_its_own_offset_in_the_surface() {
        const W: usize = 8;
        const H: usize = 6;
        let stride = W * BPP;
        let mut surface = vec![0u8; stride * H];

        // Two rows of three pixels, starting at (2, 3). Each pixel is
        // numbered so a row placed at the wrong offset is visible rather than
        // merely different.
        let band = rect(2, 3, 3, 2);
        let row_bytes = 3 * BPP;
        let packed: Vec<u8> = (0..(row_bytes * 2) as u8).collect();

        place(&packed, &mut surface, band, stride, row_bytes).expect("placed");

        for (y, row) in surface.chunks_exact(stride).enumerate() {
            for (x, px) in row.as_chunks::<BPP>().0.iter().enumerate() {
                let inside = (2..5).contains(&x) && (3..5).contains(&y);
                if !inside {
                    assert_eq!(
                        px, &[0; BPP],
                        "({x},{y}) is outside {band:?} and was written"
                    );
                    continue;
                }
                // The pixel this band held for that position.
                let at = (y - 3) * row_bytes + (x - 2) * BPP;
                let want = packed.get(at..at + BPP).expect("in the band");
                assert_eq!(
                    px.as_slice(),
                    want,
                    "({x},{y}) holds the wrong pixel of {band:?}"
                );
            }
        }
    }

    /// A band that does not fit is refused rather than clipped.
    #[test]
    fn a_band_past_the_surface_is_refused() {
        const W: usize = 8;
        let stride = W * BPP;
        let mut surface = vec![0u8; stride * 4];
        let row_bytes = 3 * BPP;
        let packed = vec![0u8; row_bytes * 2];
        // Starts on the last row, so its second row is past the end.
        let band = rect(2, 3, 3, 2);
        assert!(place(&packed, &mut surface, band, stride, row_bytes).is_err());
    }

    /// The server returning fewer bytes than the band needs is an error, not a
    /// partial picture: a short reply placed as far as it goes leaves a band
    /// half of this frame and half of the last one.
    #[test]
    fn a_short_reply_is_refused() {
        const W: usize = 8;
        let stride = W * BPP;
        let mut surface = vec![0u8; stride * 8];
        let row_bytes = 3 * BPP;
        let band = rect(2, 3, 3, 2);
        let packed = vec![0u8; row_bytes * 2 - 1];
        let e = place(&packed, &mut surface, band, stride, row_bytes).expect_err("refused");
        assert!(e.contains("short of"), "{e}");
    }
}
