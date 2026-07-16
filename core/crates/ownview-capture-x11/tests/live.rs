#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Tests that need a real X server.
//!
//! # Why these skip instead of failing
//!
//! A headless machine has no X display, and that is the *normal* state for a
//! host this product is built for. A test that failed there would fail for
//! everyone running the suite on a server.
//!
//! So they skip — and CI runs them under `Xvfb`, which means the skip is the
//! exception rather than the path everything takes. A backend nothing
//! exercises is a backend that does not work, and the way to avoid that is to
//! give CI a display rather than to write tests that pass without one.

use std::time::Duration;

use ownview_capture::capturer::{Capture, CaptureError, Capturer, MonitorId};
use ownview_capture::damage::Damage;
use ownview_capture_x11::capturer::{Transport, X11Capturer};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{
    BackingStore, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, MapState, StackMode,
    Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

/// The display, to this suite alone.
///
/// **These read pixels back**, and three other suites in this workspace draw
/// over the screen, take the focus and move the pointer. `cargo test` runs
/// test binaries in parallel, so without a lock that spans processes one of
/// them happens in the middle of one of these — which is a failure nobody
/// reading either test could explain.
fn exclusive() -> ownview_display_lock::DisplayLock {
    ownview_display_lock::DisplayLock::take()
}

/// `Some(capturer)`, or `None` with a note, when there is no display.
fn open() -> Option<X11Capturer> {
    if std::env::var_os("DISPLAY").is_none() {
        println!("skipped: no DISPLAY. CI runs these under Xvfb.");
        return None;
    }
    match X11Capturer::open(None) {
        Ok(c) => Some(c),
        Err(e) => {
            println!("skipped: DISPLAY is set but unusable ({e})");
            None
        }
    }
}

/// Capture until a frame arrives, tolerating reconfiguration.
fn first_frame(cap: &mut X11Capturer) -> ownview_capture::frame::Frame {
    for _ in 0..20 {
        match cap.next_frame(Duration::from_secs(5)).expect("a capture") {
            Capture::Frame(f) => return f,
            Capture::Unchanged | Capture::Reconfigured(_) => {}
        }
    }
    panic!("no frame after twenty attempts");
}

/// Whether a frame's reported damage covers all of `want`.
///
/// The reported rectangles are non-overlapping bands, so summing the
/// intersected areas is exact rather than an approximation.
fn damage_covers(
    frame: &ownview_capture::frame::Frame,
    want: &ownview_capture::geometry::Rect,
) -> bool {
    let got: u64 = frame
        .damage()
        .rects(&frame.bounds())
        .iter()
        .filter_map(|r| r.intersect(want))
        .map(|r| u64::from(r.width) * u64::from(r.height))
        .sum();
    got >= u64::from(want.width) * u64::from(want.height)
}

/// A change on the screen that a real desktop cannot swallow: a small window
/// of a known colour, mapped at a known place and taken away again on drop.
///
/// Filling the root window directly is the obvious way to make a change, and
/// it only works where nothing covers the root. X11 keeps no backing store for
/// the parts of the root nobody can see, so a fill into an obscured root is
/// clipped away and draws nothing at all — no error, the request succeeds.
/// Under `Xvfb` the root is bare and that never shows; on a real desktop the
/// corner of a screen is somebody else's window, and a test drawing there is
/// testing an empty screen. The dangerous half of that is not the failure: a
/// test that draws nowhere can **pass**, by proving nothing happened when
/// nothing was made to happen.
///
/// A window of our own is what a screen change actually is, and damage on the
/// root covers what its children draw, which is why capturing the root works
/// at all.
struct Patch {
    conn: RustConnection,
    root: Window,
    window: Window,
}

impl Patch {
    /// `size` by `size` of solid `pixel` at desktop position `x`, `y`, on
    /// screen by the time this returns.
    fn map(x: i32, y: i32, size: u16, pixel: u32) -> Self {
        let (conn, screen_num) = x11rb::connect(None).expect("a second connection");
        let screen = conn.setup().roots.get(screen_num).expect("a screen");
        let (root, depth, visual) = (screen.root, screen.root_depth, screen.root_visual);
        let window = conn.generate_id().expect("an id");
        conn.create_window(
            depth,
            window,
            root,
            i16::try_from(x).expect("on-screen"),
            i16::try_from(y).expect("on-screen"),
            size,
            size,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            // Override-redirect so the window manager leaves it exactly where
            // it was put instead of decorating it, moving it or refusing it.
            &CreateWindowAux::new()
                .background_pixel(pixel)
                .override_redirect(1)
                .backing_store(BackingStore::ALWAYS),
        )
        .expect("create_window")
        .check()
        .expect("a window");
        conn.map_window(window)
            .expect("map_window")
            .check()
            .expect("mapped");
        conn.configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        )
        .expect("configure_window")
        .check()
        .expect("raised");

        let patch = Self { conn, root, window };
        patch.wait_until_on_screen();
        patch
    }

    fn conn(&self) -> &RustConnection {
        &self.conn
    }

    /// The root this was mapped on, for reading back what is on screen.
    fn root(&self) -> Window {
        self.root
    }

    /// Move it, which is a change at the old place and at the new one.
    fn nudge(&self, x: i32, y: i32) {
        self.conn
            .configure_window(self.window, &ConfigureWindowAux::new().x(x).y(y))
            .expect("configure_window")
            .check()
            .expect("moved");
        self.wait_until_on_screen();
    }

    /// The server has processed the request and the pixels are on screen.
    /// Without this a read can happen before they are anywhere.
    fn wait_until_on_screen(&self) {
        for _ in 0..40 {
            let attrs = self
                .conn
                .get_window_attributes(self.window)
                .expect("get_window_attributes")
                .reply()
                .expect("a reply");
            if attrs.map_state == MapState::VIEWABLE {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the window never became viewable");
    }
}

impl Drop for Patch {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}

#[test]
fn the_server_reports_at_least_one_monitor_with_a_size() {
    let Some(cap) = open() else { return };
    let monitors = cap.monitors();
    assert!(!monitors.is_empty(), "no monitors at all");
    println!("source: {}", cap.monitor_source().describe());
    for m in monitors {
        println!(
            "  {} {:>20}  {:>5}x{:<5} at {:>6},{:<6} scale {:.2}{}",
            m.id.0,
            m.name,
            m.geometry.width,
            m.geometry.height,
            m.geometry.x,
            m.geometry.y,
            m.scale,
            if m.primary { "  primary" } else { "" }
        );
        assert!(
            m.geometry.width > 0 && m.geometry.height > 0,
            "{m:?} is empty"
        );
        assert!(m.scale > 0.0, "{m:?} has a non-positive scale");
        assert!(!m.name.is_empty(), "{m:?} has no name");
    }
    // Ids are what `--monitor` selects, so they have to be the indices a
    // person would guess rather than whatever order RandR returned.
    for (i, m) in monitors.iter().enumerate() {
        assert_eq!(m.id, MonitorId(u32::try_from(i).unwrap()));
    }
    let primaries = monitors.iter().filter(|m| m.primary).count();
    assert!(primaries <= 1, "{primaries} monitors claim to be primary");
    if primaries == 1 {
        assert!(
            monitors.first().is_some_and(|m| m.primary),
            "a primary exists but does not sort first, so --monitor 0 is not it"
        );
    }
}

#[test]
fn the_first_frame_is_the_whole_monitor_and_the_right_size() {
    let Some(mut cap) = open() else { return };
    let want = cap
        .monitors()
        .iter()
        .find(|m| m.id == cap.selected())
        .expect("the selected monitor exists")
        .size();
    println!(
        "capturing {}x{} as {} over {}",
        want.0,
        want.1,
        cap.format().name(),
        cap.transport().describe()
    );

    let frame = first_frame(&mut cap);
    assert_eq!((frame.width(), frame.height()), want);
    assert_eq!(
        *frame.damage(),
        Damage::Everything,
        "the first frame has no previous picture to be a delta against"
    );
    // The last row is the one a stride mistake loses.
    let last = frame
        .row(frame.height().saturating_sub(1))
        .expect("last row");
    assert_eq!(last.len(), frame.stride());
}

#[test]
fn a_captured_frame_has_every_row_present() {
    let Some(mut cap) = open() else { return };
    let frame = first_frame(&mut cap);
    for y in 0..frame.height() {
        assert!(frame.row(y).is_some(), "row {y} is missing");
    }
    assert_eq!(
        frame.bytes().len(),
        frame.stride() * frame.height() as usize,
        "the buffer is not the size the frame claims"
    );
    println!(
        "{}x{} {} stride {}",
        frame.width(),
        frame.height(),
        frame.format().name(),
        frame.stride()
    );
}

#[test]
fn an_unchanged_screen_says_so_instead_of_spinning() {
    let Some(mut cap) = open() else { return };
    if !cap.reports_damage() {
        println!("skipped: no XDamage, so every frame is whole");
        return;
    }
    let _ = first_frame(&mut cap);

    // On a real desktop something is usually moving, so this cannot assert
    // that nothing changed. What it can assert is that when the answer *is*
    // Unchanged it took the timeout — returning immediately would turn the
    // capture thread into a busy loop.
    let mut unchanged = 0;
    for _ in 0..3 {
        let at = std::time::Instant::now();
        if matches!(
            cap.next_frame(Duration::from_millis(300))
                .expect("a capture"),
            Capture::Unchanged
        ) {
            let waited = at.elapsed();
            unchanged += 1;
            assert!(
                waited >= Duration::from_millis(250),
                "Unchanged came back after {waited:?}, which would spin the capture loop"
            );
        }
    }
    println!("{unchanged} of 3 polls found nothing changed");
}

/// **The multi-monitor bug this is here to prevent.** Damage arrives in
/// desktop coordinates; a monitor that is not at the origin needs it
/// translated. Forget that and the rectangles are correct on the desktop and
/// outside the frame, which reads as a stale patch or a refusal in the
/// converter.
///
/// Every monitor is tried, not just the selected one. Which screen has
/// something moving on it is a property of the machine, and on the four-monitor
/// desktop this was written against the busy one was not the one `--monitor 0`
/// picks — so a version of this test that only looked at the first monitor saw
/// exactly one whole frame and proved nothing about translation.
#[test]
fn damage_stays_inside_the_frame_on_every_monitor() {
    let Some(probe) = open() else { return };
    if !probe.reports_damage() {
        println!("skipped: no XDamage on this server");
        return;
    }
    let monitors: Vec<_> = probe
        .monitors()
        .iter()
        .map(|m| (m.id, m.geometry))
        .collect();
    drop(probe);

    let mut total_regions = 0;
    for (id, geometry) in monitors {
        let mut cap = X11Capturer::open(Some(id)).expect("the monitor exists");
        let mut regions = 0;
        let mut whole = 0;
        for _ in 0..24 {
            match cap
                .next_frame(Duration::from_millis(120))
                .expect("a capture")
            {
                Capture::Frame(f) => {
                    let bounds = f.bounds();
                    for r in f.damage().rects(&bounds) {
                        assert!(
                            bounds.contains(&r),
                            "{id} at {},{}: {r:?} is outside the {}x{} frame — damage was \
                             not translated out of desktop coordinates",
                            geometry.x,
                            geometry.y,
                            f.width(),
                            f.height()
                        );
                    }
                    match f.damage() {
                        Damage::Regions(_) => regions += 1,
                        Damage::Everything => whole += 1,
                        Damage::Nothing => panic!("a frame was produced with no damage"),
                    }
                }
                Capture::Unchanged => {}
                Capture::Reconfigured(_) => break,
            }
        }
        println!(
            "  {id} at {:>6},{:<6}  {regions} region frames, {whole} whole",
            geometry.x, geometry.y
        );
        total_regions += regions;
    }
    // Not an assertion that any particular monitor is busy — that depends on
    // the machine. But if *no* monitor produced region damage in the whole
    // sweep, either nothing on this desktop moved or the region path is dead,
    // and the two are worth telling apart by hand.
    println!("{total_regions} region frames across all monitors");
}

/// **The cursor is not in the picture, and that is a property of X11 rather
/// than something this backend does.**
///
/// `GetImage` returns the contents of a drawable. The pointer is a sprite the
/// server composites onto the display on its way to the screen, so it is not
/// drawable content and never appears in a capture. XFIXES exists precisely
/// because the cursor has to be asked for separately — which is what this
/// checks, and it is also the shape the design wants: the cursor travels out
/// of band and is drawn locally, because a remote cursor moving at the far
/// end's latency feels worse than one drawn at the near end's.
#[test]
fn the_cursor_is_available_separately_and_so_is_not_in_the_frame() {
    if std::env::var_os("DISPLAY").is_none() {
        println!("skipped: no DISPLAY");
        return;
    }
    let Ok((conn, _)) = x11rb::connect(None) else {
        println!("skipped: DISPLAY is set but unusable");
        return;
    };
    use x11rb::protocol::xfixes::ConnectionExt as _;
    if conn.xfixes_query_version(5, 0).map(|c| c.reply()).is_err() {
        println!("skipped: no XFIXES");
        return;
    }
    match conn.xfixes_get_cursor_image().map(|c| c.reply()) {
        Ok(Ok(img)) => {
            println!(
                "cursor {}x{} at {},{} — fetched out of band, so it is a sprite and \
                 not part of the root window",
                img.width, img.height, img.x, img.y
            );
            assert!(
                img.width > 0 && img.height > 0,
                "the server reports a cursor with no size"
            );
        }
        _ => println!("skipped: this server has no cursor image (Xvfb with no pointer)"),
    }
}

#[test]
fn asking_for_a_monitor_that_is_not_there_is_refused() {
    if std::env::var_os("DISPLAY").is_none() {
        println!("skipped: no DISPLAY");
        return;
    }
    match X11Capturer::open(Some(MonitorId(9999))) {
        Err(CaptureError::NoSuchMonitor { count, .. }) => {
            assert!(count > 0, "refused but claims there are no monitors");
        }
        other => panic!("expected NoSuchMonitor, got {other:?}"),
    }
}

#[test]
fn every_monitor_can_be_captured_at_its_own_size() {
    let Some(probe) = open() else { return };
    let ids: Vec<_> = probe.monitors().iter().map(|m| (m.id, m.size())).collect();
    drop(probe);

    for (id, want) in ids {
        let mut cap = X11Capturer::open(Some(id)).expect("the monitor exists");
        let frame = first_frame(&mut cap);
        assert_eq!(
            (frame.width(), frame.height()),
            want,
            "{id} produced the wrong size"
        );
        println!("  {id}: {}x{} ok", want.0, want.1);
    }
}

/// Both transports have to place rows identically. Getting the arithmetic
/// wrong in one produces a sheared picture on whichever machines take the
/// other path — which is the kind of bug that reaches a user rather than a
/// test, because the machine you develop on takes one path every time.
#[test]
fn the_socket_transport_agrees_with_shared_memory() {
    let _display = exclusive();
    let Some(mut shm) = open() else { return };
    if shm.transport() != Transport::SharedMemory {
        println!("skipped: no MIT-SHM here, so there is nothing to compare against");
        return;
    }
    let a = first_frame(&mut shm);

    let mut sock = X11Capturer::open(Some(shm.selected())).expect("open");
    sock.force_socket_transport();
    assert_eq!(sock.transport(), Transport::Socket);
    let b = first_frame(&mut sock);

    assert_eq!((a.width(), a.height()), (b.width(), b.height()));
    assert_eq!(a.stride(), b.stride());
    assert_eq!(a.bytes().len(), b.bytes().len());
    assert_eq!(a.format(), b.format());
    println!(
        "both transports produced {}x{} stride {}",
        a.width(),
        a.height(),
        a.stride()
    );
}

/// **The region path, tested deterministically.**
///
/// Ambient desktop activity is a bad witness: on a real desktop the busy
/// screen is whichever one happens to have something on it, and under `Xvfb`
/// — which is where CI runs these — *nothing* moves, so a test that waits for
/// damage to happen proves nothing exactly where it matters most.
///
/// So this makes the damage, with a [`Patch`] at a known position on the
/// captured monitor, and then asserts that what comes back covers it. That
/// exercises the whole chain at once: the event arriving, the region being
/// drained and re-armed, the clip to this monitor, and the translation out of
/// desktop coordinates.
///
/// On a real desktop this shows a 64x64 square in the corner of one screen for
/// a moment. That is the price of testing the path rather than hoping.
#[test]
fn a_known_change_is_reported_at_the_place_it_happened() {
    let _display = exclusive();
    let Some(mut cap) = open() else { return };
    if !cap.reports_damage() {
        println!("skipped: no XDamage on this server");
        return;
    }
    let selected = cap.selected();
    let monitor = cap
        .monitors()
        .iter()
        .find(|m| m.id == selected)
        .expect("selected")
        .geometry;

    // Get past the first frame, and drain whatever the desktop is doing.
    let _ = first_frame(&mut cap);
    for _ in 0..4 {
        let _ = cap.next_frame(Duration::from_millis(80));
    }

    // Monitor-local (16, 16), which is somewhere on the captured screen
    // whatever its offset on the desktop is.
    const SIZE: u16 = 64;
    let want = ownview_capture::geometry::Rect {
        x: 16,
        y: 16,
        width: u32::from(SIZE),
        height: u32::from(SIZE),
    };
    let _patch = Patch::map(monitor.x + want.x, monitor.y + want.y, SIZE, 0x00ff_00ff);

    // Now the damage must arrive, and it must cover the square.
    let mut covered = false;
    let mut saw_regions = false;
    for _ in 0..20 {
        match cap
            .next_frame(Duration::from_millis(250))
            .expect("a capture")
        {
            Capture::Frame(f) => {
                let bounds = f.bounds();
                let rects = f.damage().rects(&bounds);
                for r in &rects {
                    assert!(bounds.contains(r), "{r:?} is outside {bounds:?}");
                }
                if matches!(f.damage(), Damage::Regions(_)) {
                    saw_regions = true;
                }
                if damage_covers(&f, &want) {
                    covered = true;
                    println!(
                        "the square was reported by {} rect(s){}",
                        rects.len(),
                        if saw_regions {
                            " as regions"
                        } else {
                            " as the whole frame"
                        }
                    );
                    break;
                }
            }
            Capture::Unchanged => {}
            Capture::Reconfigured(_) => break,
        }
    }
    assert!(
        covered,
        "filling {want:?} on the captured monitor produced no damage covering it — \
         the event, the drain, the clip or the translation is wrong"
    );
}

/// **A monitor switch on a live server, on every monitor there is.**
///
/// The X11 backend captures the root either way, so a switch is a change of
/// which rectangle of it gets read — cheap, and easy to get wrong in a way
/// that still produces a picture: read the new monitor's geometry into a
/// surface sized for the old one and the result is a sheared or truncated
/// screen rather than an error.
#[test]
fn every_monitor_can_be_switched_to_on_a_live_session() {
    let Some(mut cap) = open() else { return };
    let all: Vec<_> = cap.monitors().to_vec();
    if all.len() < 2 {
        println!("skipped: one monitor, so there is nothing to switch to");
        return;
    }
    let _ = first_frame(&mut cap);

    for m in &all {
        cap.select(m.id).expect("the switch");
        assert_eq!(cap.selected(), m.id);
        let frame = first_frame(&mut cap);
        assert_eq!(
            (frame.width(), frame.height()),
            m.size(),
            "{} is {}x{}, so its frames must be",
            m.name,
            m.geometry.width,
            m.geometry.height
        );
        // Every row present at the new size: a surface still sized for the
        // previous monitor is exactly how this fails while still looking like
        // a picture.
        for y in 0..frame.height() {
            assert!(
                frame
                    .row(y)
                    .is_some_and(|r| r.len() >= frame.width() as usize * 4),
                "row {y} of {} is short",
                m.name
            );
        }
        println!(
            "{} captured at {}x{}",
            m.name,
            frame.width(),
            frame.height()
        );
    }
}

#[test]
fn switching_to_a_monitor_the_server_does_not_have_is_refused() {
    let Some(mut cap) = open() else { return };
    let was = cap.selected();
    let count = cap.monitors().len();
    let e = cap
        .select(MonitorId(9999))
        .expect_err("no server has monitor 9999");
    assert!(matches!(e, CaptureError::NoSuchMonitor { .. }), "{e:?}");
    assert_eq!(cap.selected(), was, "a refused switch changes nothing");
    assert_eq!(cap.monitors().len(), count);
    // And it still captures.
    let _ = first_frame(&mut cap);
}

/// **The pixels, not just the geometry.**
///
/// Everything above checks that damage lands in the right place. This checks
/// that the *bytes* do, which is a different set of mistakes: a stride
/// miscalculated in one of the two transports shears the picture, a byte order
/// derived wrongly swaps red and blue, and a band placed at the wrong offset
/// moves part of the screen somewhere else. All three produce something that
/// still looks like a picture.
///
/// A known colour is put on screen at a known place and then looked for. The
/// race with a live desktop is settled by reading the same rectangle back from
/// the server: if the colour is not there either, there is nothing to compare.
/// If it is there and the captured frame disagrees, that is this backend's
/// bug.
#[test]
fn a_known_colour_arrives_at_the_place_it_was_drawn() {
    let _display = exclusive();
    use x11rb::protocol::xproto::ImageFormat;

    let Some(mut cap) = open() else { return };
    let selected = cap.selected();
    let monitor = cap
        .monitors()
        .iter()
        .find(|m| m.id == selected)
        .expect("selected")
        .geometry;
    let format = cap.format();
    let _ = first_frame(&mut cap);
    for _ in 0..4 {
        let _ = cap.next_frame(Duration::from_millis(80));
    }

    // A colour with all three channels distinct, so a swap cannot pass.
    const R: u8 = 0x11;
    const G: u8 = 0x77;
    const B: u8 = 0xDD;
    const SIZE: u16 = 64;
    // Somewhere the other test's square cannot reach: these two run at the
    // same time, and a window over the colour would only ever be read as the
    // desktop having repainted.
    let want = ownview_capture::geometry::Rect {
        x: 16,
        y: 112,
        width: u32::from(SIZE),
        height: u32::from(SIZE),
    };

    let pixel = (u32::from(R) << 16) | (u32::from(G) << 8) | u32::from(B);
    let patch = Patch::map(monitor.x + want.x, monitor.y + want.y, SIZE, pixel);
    let conn = patch.conn();
    let root = patch.root();
    let (r_at, g_at, b_at) = format.rgb_offsets();

    // What the server itself says is at that place on screen, read
    // independently of the backend under test.
    let reads_as_the_colour = || {
        let direct = conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                root,
                i16::try_from(monitor.x + want.x).expect("on-screen"),
                i16::try_from(monitor.y + want.y).expect("on-screen"),
                SIZE,
                SIZE,
                !0,
            )
            .expect("get_image")
            .reply()
            .expect("reply");
        direct.data.as_chunks::<4>().0.iter().all(|px| {
            px.get(r_at) == Some(&R) && px.get(g_at) == Some(&G) && px.get(b_at) == Some(&B)
        })
    };

    // **A mapped window is not yet a painted one.** `map_state` says the
    // window is on screen; a compositing window manager paints it into the
    // root a frame or so later. A capture taken in between is a picture of the
    // desktop, and correctly so — so nothing can be asked of the backend until
    // the server agrees the colour is there.
    let mut on_screen = false;
    for _ in 0..40 {
        if reads_as_the_colour() {
            on_screen = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if !on_screen {
        println!("skipped: the server never showed the square at that place on the root");
        return;
    }

    // **The frame that carried the change, not merely the next one.** A frame
    // can arrive whose damage was accumulated before the window was painted,
    // and it updates only the bands that damage names — so the square's pixels
    // would still be the desktop's, and comparing that frame would be
    // comparing a picture of a moment before the thing being looked for
    // existed.
    let mut carried: Option<ownview_capture::frame::Frame> = None;
    for _ in 0..20 {
        match cap
            .next_frame(Duration::from_millis(250))
            .expect("a capture")
        {
            Capture::Frame(f) if damage_covers(&f, &want) => {
                carried = Some(f);
                break;
            }
            Capture::Frame(_) | Capture::Unchanged => {}
            Capture::Reconfigured(_) => break,
        }
    }
    let Some(frame) = carried else {
        panic!("no frame reported {want:?} as changed after the window was painted there");
    };

    // And it must still be there, or the desktop won the race after all and
    // there is nothing left to compare against.
    if !reads_as_the_colour() {
        println!("skipped: the desktop repainted over the square before it could be compared");
        return;
    }

    // The square is on the server. It must be in the frame, at the same place,
    // in the same channels.
    let mut checked = 0u32;
    for y in want.y..want.y + want.height as i32 {
        let row = frame
            .row(u32::try_from(y).expect("positive"))
            .expect("row present");
        for x in want.x..want.x + want.width as i32 {
            let at = usize::try_from(x).expect("positive") * 4;
            let px = row.get(at..at + 4).expect("pixel in row");
            assert_eq!(
                (px.get(r_at), px.get(g_at), px.get(b_at)),
                (Some(&R), Some(&G), Some(&B)),
                "at {x},{y} the frame has {px:?} where the server has \
                 r={R:#04x} g={G:#04x} b={B:#04x} as {}",
                format.name()
            );
            checked += 1;
        }
    }
    println!(
        "{checked} pixels matched the server exactly, as {}",
        format.name()
    );
}

/// **The spin this prevents.** Damage anywhere on the desktop wakes the
/// capturer, and on a multi-monitor host most of it belongs to a screen nobody
/// asked for. Reporting `Unchanged` the moment that happens is true and
/// useless: the caller polls again immediately, is woken again by the next
/// change on the neighbouring screen, and the capture thread burns a core for
/// as long as anything else on the desktop is busy.
///
/// Measured on the four-monitor desktop this was written against, that turned
/// a 300 ms wait into a 99 ms one, over and over.
///
/// Deterministic only where there is somewhere else to draw. A single-monitor
/// host — which is what CI has — has no "elsewhere", so this skips there and
/// the property is covered by `to_frame_coordinates`' unit tests plus this
/// test on a real desktop.
#[test]
fn damage_on_another_monitor_does_not_end_the_wait_early() {
    let _display = exclusive();
    let Some(mut cap) = open() else { return };
    if !cap.reports_damage() {
        println!("skipped: no XDamage on this server");
        return;
    }
    let selected = cap.selected();
    let mine = cap
        .monitors()
        .iter()
        .find(|m| m.id == selected)
        .expect("selected")
        .geometry;
    // Somewhere that is definitely not on the captured monitor.
    let Some(elsewhere) = cap
        .monitors()
        .iter()
        .map(|m| m.geometry)
        .find(|g| g.intersect(&mine).is_none())
    else {
        println!(
            "skipped: every monitor overlaps the captured one, so there is nowhere \
                 else to draw (a single-monitor host, which is what CI has)"
        );
        return;
    };

    let _ = first_frame(&mut cap);

    let patch = Patch::map(elsewhere.x + 8, elsewhere.y + 8, 32, 0x0033_6699);

    const TIMEOUT: Duration = Duration::from_millis(300);
    let mut judged = 0;
    for round in 1..=6 {
        // Move it, so there is definitely a change on the other screen for
        // this monitor to ignore.
        patch.nudge(elsewhere.x + 8 + round * 4, elsewhere.y + 8);

        let started = std::time::Instant::now();
        let got = cap.next_frame(TIMEOUT).expect("a capture");
        let waited = started.elapsed();
        match got {
            Capture::Unchanged => {
                judged += 1;
                assert!(
                    waited >= TIMEOUT.mul_f64(0.8),
                    "a change on the monitor at {},{} ended a {TIMEOUT:?} wait after \
                     {waited:?} with Unchanged — the capture loop would spin",
                    elsewhere.x,
                    elsewhere.y
                );
            }
            // Real damage on the captured monitor is a legitimate early
            // return; there is nothing to judge in that round.
            Capture::Frame(_) | Capture::Reconfigured(_) => {}
        }
    }
    println!("{judged} of 6 rounds returned Unchanged, all after the full wait");
}
