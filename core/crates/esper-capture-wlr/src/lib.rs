//! Screen capture on a wlroots compositor, through `zwlr_screencopy_v1`.
//!
//! # Why not the portal
//!
//! `xdg-desktop-portal-wlr` does implement ScreenCast, so unlike input there
//! *is* a portal path here. It is not the one taken, for two reasons measured
//! on 2026-09-12: the portal hands back a **PipeWire** node, which is a second
//! large dependency this workspace does not have and which buys nothing on a
//! compositor that will copy a frame directly; and every portal session needs
//! a person at a consent dialog, which a host started by `systemd --user` does
//! not have at login.
//!
//! On GNOME and KDE there is no screencopy and the portal is the only way, so
//! that path is still owed. This is the half that can be had without either.
//!
//! # How a frame arrives
//!
//! The compositor does not hand over pixels; it copies them into a buffer this
//! process supplies. So each frame is a small conversation: ask for one, be
//! told what shape the buffer must be, make one, hand it over, wait for
//! `ready`, and read it.
//!
//! # The buffer is a file, deliberately
//!
//! Shared memory between a client and a compositor is a file descriptor, and
//! the usual way to read one is `mmap` — which needs `unsafe` or a crate that
//! hides it. This workspace forbids `unsafe` outside a declared FFI inventory,
//! and a mapping buys nothing here: the frame has to be copied into a `Vec`
//! for the encoder anyway.
//!
//! So the buffer is an ordinary file in the runtime directory, unlinked at
//! once so it exists only as a descriptor, and the frame is read back with
//! `std::fs`. The read comes from the page cache — the compositor wrote it a
//! moment ago — and costs one copy, which is the copy that was going to happen
//! regardless.

// Unix only, and empty elsewhere. The Wayland socket does not exist on Windows
// or in a macOS application, and the engine already takes this as a Linux-only
// dependency.
#![cfg(unix)]

use std::io::{Read as _, Seek as _};
use std::os::fd::AsFd as _;
use std::time::{Duration, Instant};

use esper_capture::{
    Capture, CaptureError, Capturer, Damage, Frame, MonitorId, MonitorInfo, PixelFormat, Pixels,
    Rect,
};
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

/// The name this backend reports, in the log and the session HUD.
pub const BACKEND: &str = "wlr-screencopy";

/// What the compositor said a buffer has to look like.
#[derive(Debug, Clone, Copy)]
struct Shape {
    format: wl_shm::Format,
    width: u32,
    height: u32,
    stride: u32,
}

/// How a frame ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    Waiting,
    Ready,
    /// The compositor gave up on this frame. Not fatal: an output being
    /// reconfigured says this and the next attempt succeeds.
    Failed,
}

/// One output, as the compositor describes it.
#[derive(Debug, Clone)]
struct Screen {
    output: wl_output::WlOutput,
    info: MonitorInfo,
}

/// Everything the event handlers collect.
#[derive(Default)]
struct State {
    manager: Option<ZwlrScreencopyManagerV1>,
    shm: Option<wl_shm::WlShm>,
    screens: Vec<Screen>,
    /// The shape of the frame being asked for, when one is in flight.
    shape: Option<Shape>,
    ending: Option<Ending>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "zwlr_screencopy_manager_v1" => {
                state.manager = Some(registry.bind(name, version.min(3), queue, ()));
            }
            "wl_shm" => state.shm = Some(registry.bind(name, version.min(1), queue, ())),
            "wl_output" => {
                // Version 4 is where an output tells you its name, which is
                // what a person calls a monitor.
                let output: wl_output::WlOutput =
                    registry.bind(name, version.min(4), queue, state.screens.len());
                state.screens.push(Screen {
                    output,
                    info: MonitorInfo {
                        id: MonitorId(u32::try_from(state.screens.len()).unwrap_or(0)),
                        name: String::new(),
                        geometry: Rect {
                            x: 0,
                            y: 0,
                            width: 0,
                            height: 0,
                        },
                        scale: 1.0,
                        // The compositor does not say which output is
                        // primary, so nothing here claims one is. The first
                        // is served by default, which is a different thing
                        // from calling it primary.
                        primary: false,
                    },
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, usize> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        which: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(screen) = state.screens.get_mut(*which) else {
            return;
        };
        match event {
            wl_output::Event::Geometry { x, y, .. } => {
                screen.info.geometry.x = x;
                screen.info.geometry.y = y;
            }
            wl_output::Event::Mode {
                flags,
                width,
                height,
                ..
            } => {
                // The current mode only. An output lists every mode it can do,
                // and the others are sizes it is not showing.
                if matches!(flags, WEnum::Value(f) if f.contains(wl_output::Mode::Current)) {
                    screen.info.geometry.width = u32::try_from(width).unwrap_or(0);
                    screen.info.geometry.height = u32::try_from(height).unwrap_or(0);
                }
            }
            wl_output::Event::Scale { factor } => {
                screen.info.scale = factor as f32;
            }
            wl_output::Event::Name { name } => screen.info.name = name,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // The compositor's answer to "what must the buffer look like".
            // There may be several — one per format it can copy into — and the
            // first supported one is taken.
            zwlr_screencopy_frame_v1::Event::Buffer {
                format: WEnum::Value(format),
                width,
                height,
                stride,
            } => {
                if state.shape.is_none() && pixel_format(format).is_some() {
                    state.shape = Some(Shape {
                        format,
                        width,
                        height,
                        stride,
                    });
                }
            }
            zwlr_screencopy_frame_v1::Event::Ready { .. } => state.ending = Some(Ending::Ready),
            zwlr_screencopy_frame_v1::Event::Failed => state.ending = Some(Ending::Failed),
            _ => {}
        }
    }
}

delegate_noop!(State: ignore ZwlrScreencopyManagerV1);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);

/// What this project calls a compositor's pixel layout.
///
/// `Xrgb8888` names the *word*, and these are little-endian machines, so the
/// bytes in memory run blue, green, red, unused — which this project calls
/// `Bgrx8`. Reading the name as a byte order is the mistake that produces a
/// picture with the red and blue channels swapped, and it looks enough like a
/// colour-management problem to waste an afternoon.
const fn pixel_format(format: wl_shm::Format) -> Option<PixelFormat> {
    match format {
        wl_shm::Format::Xrgb8888 => Some(PixelFormat::Bgrx8),
        wl_shm::Format::Argb8888 => Some(PixelFormat::Bgra8),
        wl_shm::Format::Xbgr8888 => Some(PixelFormat::Rgbx8),
        wl_shm::Format::Abgr8888 => Some(PixelFormat::Rgba8),
        _ => None,
    }
}

/// Capture through the compositor's own copy.
pub struct WlrCapturer {
    connection: Connection,
    queue: wayland_client::EventQueue<State>,
    state: State,
    manager: ZwlrScreencopyManagerV1,
    shm: wl_shm::WlShm,
    serving: Vec<MonitorId>,
    monitors: Vec<MonitorInfo>,
    format: PixelFormat,
    /// Whether the cursor is drawn into the frame.
    ///
    /// It is not. This product sends the cursor as its own message so the far
    /// end can draw it without waiting for a frame, and a cursor baked into
    /// the picture as well would be drawn twice.
    overlay_cursor: bool,
}

impl std::fmt::Debug for WlrCapturer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WlrCapturer")
            .field("monitors", &self.monitors.len())
            .field("serving", &self.serving)
            .finish_non_exhaustive()
    }
}

impl WlrCapturer {
    /// Connect and find the outputs.
    ///
    /// # Errors
    ///
    /// If there is no Wayland display, or the compositor offers no
    /// `zwlr_screencopy_manager_v1` — which is GNOME and KDE, where the
    /// ScreenCast portal is the only way and this is a refusal rather than a
    /// fault.
    pub fn open() -> Result<Self, CaptureError> {
        let connection = Connection::connect_to_env().map_err(|e| CaptureError::NoBackend {
            tried: format!("the Wayland display: {e}"),
        })?;
        let mut queue = connection.new_event_queue();
        let handle = queue.handle();
        let _registry = connection.display().get_registry(&handle, ());

        let mut state = State::default();
        // Twice: the first pass finds the globals and binds the outputs, the
        // second collects what each output says about itself.
        for _ in 0..2 {
            queue
                .roundtrip(&mut state)
                .map_err(|e| CaptureError::NoBackend {
                    tried: format!("asking the compositor what it offers: {e}"),
                })?;
        }

        let manager = state
            .manager
            .clone()
            .ok_or_else(|| CaptureError::NoBackend {
                tried: "this compositor offers no zwlr_screencopy_manager_v1, so it is not a \
                    wlroots one — GNOME and KDE capture through the ScreenCast portal"
                    .to_owned(),
            })?;
        let shm = state.shm.clone().ok_or_else(|| CaptureError::NoBackend {
            tried: "this compositor offers no wl_shm to put a frame in".to_owned(),
        })?;
        if state.screens.is_empty() {
            return Err(CaptureError::NoBackend {
                tried: "this compositor has no output to capture".to_owned(),
            });
        }

        let monitors: Vec<MonitorInfo> = state.screens.iter().map(|s| s.info.clone()).collect();
        let serving = monitors.first().map(|m| vec![m.id]).unwrap_or_default();

        Ok(Self {
            connection,
            queue,
            state,
            manager,
            shm,
            serving,
            monitors,
            // Replaced by what the compositor asks for on the first frame.
            format: PixelFormat::Bgrx8,
            overlay_cursor: false,
        })
    }

    /// A file nobody else can see, sized for a frame.
    ///
    /// Unlinked at once, so it exists only as an open descriptor: a frame
    /// buffer left on disk is a small thing, and one per frame is a pile.
    fn scratch(&self, bytes: usize) -> Result<std::fs::File, CaptureError> {
        let directory = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_owned());
        let path =
            std::path::Path::new(&directory).join(format!("esper-frame-{}", std::process::id()));
        let file = std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| CaptureError::Backend {
                backend: BACKEND,
                detail: format!("making a frame buffer in {directory}: {e}"),
            })?;
        let _ = std::fs::remove_file(&path);
        file.set_len(bytes as u64)
            .map_err(|e| CaptureError::Backend {
                backend: BACKEND,
                detail: format!("sizing the frame buffer: {e}"),
            })?;
        Ok(file)
    }
}

impl Capturer for WlrCapturer {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn monitors(&self) -> &[MonitorInfo] {
        &self.monitors
    }

    fn serving(&self) -> &[MonitorId] {
        &self.serving
    }

    fn format(&self) -> PixelFormat {
        self.format
    }

    fn reports_damage(&self) -> bool {
        // `zwlr_screencopy` version 3 has a `damage` event, and this asks for
        // whole frames rather than tracking it. Saying so honestly means the
        // pipeline compares frames itself instead of trusting a damage report
        // that is not being collected.
        false
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Capture, CaptureError> {
        let Some(id) = self.serving.first().copied() else {
            return Err(CaptureError::Backend {
                backend: BACKEND,
                detail: "nothing is being served, so there is no screen to copy".to_owned(),
            });
        };
        let Some(screen) = self.state.screens.iter().find(|s| s.info.id == id).cloned() else {
            return Err(CaptureError::Backend {
                backend: BACKEND,
                detail: format!("monitor {id:?} is not one this compositor has"),
            });
        };

        let handle = self.queue.handle();
        self.state.shape = None;
        self.state.ending = None;

        let frame = self.manager.capture_output(
            i32::from(self.overlay_cursor),
            &screen.output,
            &handle,
            (),
        );

        // The compositor answers with the shape a buffer must have.
        self.queue
            .roundtrip(&mut self.state)
            .map_err(|e| CaptureError::Backend {
                backend: BACKEND,
                detail: format!("asking for a frame: {e}"),
            })?;

        let Some(shape) = self.state.shape else {
            frame.destroy();
            return Err(CaptureError::Backend {
                backend: BACKEND,
                detail: "the compositor offered no pixel format this understands".to_owned(),
            });
        };
        let Some(format) = pixel_format(shape.format) else {
            frame.destroy();
            return Err(CaptureError::Backend {
                backend: BACKEND,
                detail: format!(
                    "the compositor offered {:?}, which is not a layout this reads",
                    shape.format
                ),
            });
        };
        self.format = format;

        let bytes = (shape.stride as usize).saturating_mul(shape.height as usize);
        let mut file = self.scratch(bytes)?;
        let pool = self
            .shm
            .create_pool(file.as_fd(), bytes as i32, &handle, ());
        let buffer = pool.create_buffer(
            0,
            shape.width as i32,
            shape.height as i32,
            shape.stride as i32,
            shape.format,
            &handle,
            (),
        );

        frame.copy(&buffer);

        // Wait for the compositor to say it has finished with the buffer.
        // Reading it before `ready` is reading a frame that is still being
        // written, and the tearing looks like a codec fault.
        let deadline = Instant::now() + timeout;
        let ending = loop {
            match self.state.ending {
                Some(end) => break end,
                None if Instant::now() >= deadline => break Ending::Waiting,
                None => {}
            }
            self.connection.flush().map_err(|e| CaptureError::Backend {
                backend: BACKEND,
                detail: format!("flushing a capture request: {e}"),
            })?;
            self.queue
                .blocking_dispatch(&mut self.state)
                .map_err(|e| CaptureError::Backend {
                    backend: BACKEND,
                    detail: format!("waiting for a frame: {e}"),
                })?;
        };

        buffer.destroy();
        pool.destroy();
        frame.destroy();

        match ending {
            Ending::Waiting => return Ok(Capture::Unchanged),
            Ending::Failed => {
                return Err(CaptureError::Backend {
                    backend: BACKEND,
                    detail: "the compositor refused to copy this frame".to_owned(),
                });
            }
            Ending::Ready => {}
        }

        let mut pixels = Vec::with_capacity(bytes);
        file.rewind()
            .and_then(|()| file.read_to_end(&mut pixels))
            .map_err(|e| CaptureError::Backend {
                backend: BACKEND,
                detail: format!("reading the frame back: {e}"),
            })?;

        // **Everything**, because no damage is being collected. Saying
        // `Nothing` changed would be a lie that stops the screen updating.
        let frame = Frame::new(
            shape.width,
            shape.height,
            shape.stride as usize,
            format,
            Damage::Everything,
            Pixels::owned(pixels),
        )
        .map_err(|e| CaptureError::Backend {
            backend: BACKEND,
            detail: format!("the frame the compositor copied is not one this can carry: {e}"),
        })?;
        Ok(Capture::Frame(frame))
    }

    fn serve(&mut self, monitors: &[MonitorId]) -> Result<(), CaptureError> {
        if monitors.is_empty() {
            return Err(CaptureError::Backend {
                backend: BACKEND,
                detail: "a capturer serving nothing has no picture to give".to_owned(),
            });
        }
        for id in monitors {
            if !self.monitors.iter().any(|m| m.id == *id) {
                return Err(CaptureError::NoSuchMonitor {
                    id: *id,
                    count: self.monitors.len(),
                });
            }
        }
        // One at a time: `zwlr_screencopy` copies one output per request, and
        // stitching several into one picture is the multi-monitor slice rather
        // than something to half-do here.
        self.serving = vec![monitors.first().copied().unwrap_or(MonitorId(0))];
        Ok(())
    }
}
