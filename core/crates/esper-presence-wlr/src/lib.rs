//! Presence on a wlroots compositor: a frame that takes no clicks.
//!
//! # Why this is what unblocks a Wayland host
//!
//! Esper does not serve a screen it cannot tell somebody is being watched. On
//! sway that refusal was total: input had a Wayland path and the indicator did
//! not, so a wlroots host either refused to start or ran with
//! `host.serve_without_indicator` — which is the setting nobody should need.
//!
//! # The frame
//!
//! Four surfaces on the **overlay** layer, one along each edge, anchored so the
//! compositor sizes them rather than this code guessing. Overlay because a
//! fullscreen window must not cover the one thing telling somebody they are
//! being watched.
//!
//! **They are visible and take no clicks.** An empty input region means the
//! compositor routes pointer events straight through to whatever is
//! underneath — without it the frame would swallow every click on the edge of
//! the screen, where the panel and the window borders are. That is the same
//! property the X11 backend gets from an empty SHAPE input region, arrived at
//! through a different mechanism because the two display servers have nothing
//! in common here.
//!
//! **The exclusive zone is -1**, not 0. Zero means "do not reserve space but
//! respect what others reserved", which on a desktop with a panel puts the
//! frame inside the panel rather than at the screen's edge. Minus one means the
//! surface is laid out against the raw output, which is where an edge is.
//!
//! # No pixels are pushed
//!
//! `wp_single_pixel_buffer` makes a one-pixel buffer of a named colour, and
//! `wp_viewporter` stretches it over the surface. So a frame is four one-pixel
//! buffers and no shared memory at all — no `mmap`, no `unsafe`, and nothing to
//! resize when an output changes.
//!
//! # What it cannot do
//!
//! **The panic key.** Wayland has no global hotkey for an ordinary client, by
//! design, so `poll` returns nothing and this backend says so. That is the same
//! wall the tracker's one open presence item is against, and it is not this
//! crate's to knock down: it needs a device below the compositor.

// Unix only, and empty elsewhere.
//
// A session bus is a unix socket, the layer shell and the virtual-device
// protocols are Wayland, and none of that exists on Windows or in a macOS
// application. The engine already takes these as Linux-only dependencies; this
// is what lets the *workspace* type-check for another target without a stack
// of `cfg` inside every function.
#![cfg(unix)]
use std::collections::HashMap;

use esper_presence::{
    Blanking, HostEvent, Missing, Presence, PresenceError, THICKNESS, Verdict, Watching, colour_of,
};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_output, wl_region, wl_registry, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::wp::single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1;
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

/// The name this backend reports, in the log and the session HUD.
pub const BACKEND: &str = "wayland-wlr";

/// Which edge a strip runs along.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    const ALL: [Self; 4] = [Self::Top, Self::Bottom, Self::Left, Self::Right];

    /// Anchored to three sides, so the compositor stretches it along one.
    const fn anchor(self) -> Anchor {
        match self {
            Self::Top => Anchor::Top.union(Anchor::Left).union(Anchor::Right),
            Self::Bottom => Anchor::Bottom.union(Anchor::Left).union(Anchor::Right),
            Self::Left => Anchor::Left.union(Anchor::Top).union(Anchor::Bottom),
            Self::Right => Anchor::Right.union(Anchor::Top).union(Anchor::Bottom),
        }
    }

    /// Zero on the stretched axis: the compositor decides that one.
    const fn size(self) -> (u32, u32) {
        match self {
            Self::Top | Self::Bottom => (0, THICKNESS as u32),
            Self::Left | Self::Right => (THICKNESS as u32, 0),
        }
    }
}

/// One edge's surface and the things that keep it alive.
#[derive(Debug)]
struct Strip {
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    viewport: WpViewport,
    buffer: Option<wl_buffer::WlBuffer>,
    /// The size the compositor told us to be, which is the only size it is.
    configured: Option<(u32, u32)>,
}

/// What the registry handed us.
#[derive(Default)]
struct Globals {
    compositor: Option<wl_compositor::WlCompositor>,
    layer_shell: Option<ZwlrLayerShellV1>,
    viewporter: Option<WpViewporter>,
    single_pixel: Option<WpSinglePixelBufferManagerV1>,
    /// Configure events, keyed by the layer surface that got them.
    configures: HashMap<u32, (u32, u32)>,
    /// A layer surface the compositor closed under us.
    closed: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
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
            "wl_compositor" => {
                state.compositor = Some(registry.bind(name, version.min(4), queue, ()))
            }
            "zwlr_layer_shell_v1" => {
                state.layer_shell = Some(registry.bind(name, version.min(4), queue, ()));
            }
            "wp_viewporter" => {
                state.viewporter = Some(registry.bind(name, version.min(1), queue, ()))
            }
            "wp_single_pixel_buffer_manager_v1" => {
                state.single_pixel = Some(registry.bind(name, version.min(1), queue, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, u32> for Globals {
    fn event(
        state: &mut Self,
        surface: &ZwlrLayerSurfaceV1,
        event: wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Event,
        which: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Event;
        match event {
            Event::Configure {
                serial,
                width,
                height,
            } => {
                // Acknowledged first. A surface that draws before it
                // acknowledges is a protocol error, and the compositor's
                // answer to one is to disconnect.
                surface.ack_configure(serial);
                state.configures.insert(*which, (width, height));
            }
            // The compositor took the surface away — an output went, most
            // often. Recorded rather than ignored, because an indicator that
            // has been closed is an indicator that is not on screen, and this
            // process would otherwise keep saying it is.
            Event::Closed => state.closed = true,
            _ => {}
        }
    }
}

delegate_noop!(Globals: ignore wl_compositor::WlCompositor);
delegate_noop!(Globals: ignore wl_surface::WlSurface);
delegate_noop!(Globals: ignore wl_region::WlRegion);
delegate_noop!(Globals: ignore wl_output::WlOutput);
delegate_noop!(Globals: ignore wl_buffer::WlBuffer);
delegate_noop!(Globals: ignore ZwlrLayerShellV1);
delegate_noop!(Globals: ignore WpViewporter);
delegate_noop!(Globals: ignore WpViewport);
delegate_noop!(Globals: ignore WpSinglePixelBufferManagerV1);

/// One channel of an 8-bit colour, over the range the protocol wants.
///
/// `wp_single_pixel_buffer` takes each channel as a `u32` covering the whole
/// range, so an eight-bit value is *spread* rather than shifted: `0xFF` is
/// `u32::MAX`, not `0xFF00_0000`. Shifting makes every colour almost black,
/// which on a four-pixel frame looks like no frame at all.
const fn spread(byte: u32) -> u32 {
    (byte & 0xFF) * 0x0101_0101
}

/// The indicator, on a wlroots compositor.
pub struct WlrPresence {
    queue: wayland_client::EventQueue<Globals>,
    globals: Globals,
    compositor: wl_compositor::WlCompositor,
    layer_shell: ZwlrLayerShellV1,
    viewporter: WpViewporter,
    single_pixel: WpSinglePixelBufferManagerV1,
    strips: Vec<Strip>,
    showing: bool,
}

impl std::fmt::Debug for WlrPresence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WlrPresence")
            .field("showing", &self.showing)
            .field("strips", &self.strips.len())
            .finish_non_exhaustive()
    }
}

impl WlrPresence {
    /// Connect, and find what a frame needs.
    ///
    /// # Errors
    ///
    /// If there is no Wayland display, or the compositor offers no layer shell
    /// — which is GNOME and KDE, where this is a refusal rather than a fault
    /// and the ladder above tries something else.
    pub fn open() -> Result<Self, PresenceError> {
        let connection = Connection::connect_to_env().map_err(|e| PresenceError::NoBackend {
            tried: format!("the Wayland display: {e}"),
        })?;
        let mut queue = connection.new_event_queue();
        let handle = queue.handle();
        let _registry = connection.display().get_registry(&handle, ());

        let mut globals = Globals::default();
        queue
            .roundtrip(&mut globals)
            .map_err(|e| PresenceError::NoBackend {
                tried: format!("asking the compositor what it offers: {e}"),
            })?;

        let missing = |what: &str| PresenceError::NoBackend {
            tried: format!(
                "this compositor offers no {what}, so it is not a wlroots one — \
                 GNOME and KDE decline the layer shell on purpose"
            ),
        };
        let compositor = globals
            .compositor
            .clone()
            .ok_or_else(|| missing("wl_compositor"))?;
        let layer_shell = globals
            .layer_shell
            .clone()
            .ok_or_else(|| missing("zwlr_layer_shell_v1"))?;
        let viewporter = globals
            .viewporter
            .clone()
            .ok_or_else(|| missing("wp_viewporter"))?;
        let single_pixel = globals
            .single_pixel
            .clone()
            .ok_or_else(|| missing("wp_single_pixel_buffer_manager_v1"))?;

        Ok(Self {
            queue,
            globals,
            compositor,
            layer_shell,
            viewporter,
            single_pixel,
            strips: Vec::new(),
            showing: false,
        })
    }

    fn tear_down(&mut self) {
        for strip in self.strips.drain(..) {
            if let Some(buffer) = strip.buffer {
                buffer.destroy();
            }
            strip.viewport.destroy();
            strip.layer.destroy();
            strip.surface.destroy();
        }
        self.globals.configures.clear();
        let _ = self.queue.flush();
    }
}

impl Presence for WlrPresence {
    fn backend(&self) -> &'static str {
        BACKEND
    }

    fn show(&mut self, watching: &Watching) -> Result<(), PresenceError> {
        let colour = colour_of(watching);

        // Rebuilt rather than recoloured. Four one-pixel buffers cost nothing
        // to make, and a strip whose buffer is swapped while the compositor is
        // mid-configure is a race for no gain.
        self.tear_down();
        let handle = self.queue.handle();

        for (index, edge) in Edge::ALL.into_iter().enumerate() {
            let surface = self.compositor.create_surface(&handle, ());

            // **Empty, not absent.** An absent input region means the whole
            // surface takes input; an empty one means none of it does, and
            // this frame must not swallow a click at the edge of the screen.
            let region = self.compositor.create_region(&handle, ());
            surface.set_input_region(Some(&region));
            region.destroy();

            let index = u32::try_from(index).unwrap_or(0);
            let layer = self.layer_shell.get_layer_surface(
                &surface,
                // No output named: the compositor puts it on the one the
                // person is looking at, which is the one to tell them about.
                None,
                Layer::Overlay,
                "esper-session-indicator".to_owned(),
                &handle,
                index,
            );
            let (width, height) = edge.size();
            layer.set_size(width, height);
            layer.set_anchor(edge.anchor());
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            // -1, not 0. See the module comment: zero respects other surfaces'
            // exclusive zones and puts the frame inside a panel.
            layer.set_exclusive_zone(-1);

            let viewport = self.viewporter.get_viewport(&surface, &handle, ());
            surface.commit();

            self.strips.push(Strip {
                surface,
                layer,
                viewport,
                buffer: None,
                configured: None,
            });
        }

        // The compositor answers with the size each strip is to be, and
        // nothing may be drawn before that arrives.
        self.queue
            .roundtrip(&mut self.globals)
            .map_err(|e| PresenceError::Backend {
                backend: BACKEND,
                detail: format!("waiting for the compositor to size the frame: {e}"),
            })?;

        let single_pixel = self.single_pixel.clone();
        let handle = self.queue.handle();
        for (index, strip) in self.strips.iter_mut().enumerate() {
            let index = u32::try_from(index).unwrap_or(0);
            let Some(&(width, height)) = self.globals.configures.get(&index) else {
                return Err(PresenceError::Backend {
                    backend: BACKEND,
                    detail: "the compositor never said how big a strip should be".to_owned(),
                });
            };
            strip.configured = Some((width, height));

            let buffer = single_pixel.create_u32_rgba_buffer(
                spread(colour >> 16),
                spread(colour >> 8),
                spread(colour),
                u32::MAX,
                &handle,
                (),
            );
            // One pixel, stretched over the whole strip. Without the viewport
            // the buffer would cover exactly one pixel of it.
            strip.viewport.set_destination(
                i32::try_from(width.max(1)).unwrap_or(1),
                i32::try_from(height.max(1)).unwrap_or(1),
            );
            strip.surface.attach(Some(&buffer), 0, 0);
            strip.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
            strip.surface.commit();
            strip.buffer = Some(buffer);
        }

        self.queue.flush().map_err(|e| PresenceError::Backend {
            backend: BACKEND,
            detail: format!("showing the frame: {e}"),
        })?;
        self.showing = true;
        Ok(())
    }

    fn hide(&mut self) -> Result<(), PresenceError> {
        self.tear_down();
        self.showing = false;
        Ok(())
    }

    fn poll(&mut self) -> Result<Vec<HostEvent>, PresenceError> {
        // Anything the compositor has to say — a `Closed`, most usefully.
        let _ = self.queue.dispatch_pending(&mut self.globals);

        // **No panic key, and no pretending.** Wayland gives an ordinary
        // client no global hotkey, deliberately, so there is nothing to poll
        // for. The key needs a device below the compositor, which is the open
        // item this cannot close.
        Ok(Vec::new())
    }

    fn why_no_panic_key(&self) -> Option<&'static str> {
        Some(
            "Wayland gives an ordinary client no global hotkey, so there is none to hold — \
             this is the platform's design and not something on this machine",
        )
    }

    fn is_showing(&self) -> bool {
        self.showing && !self.globals.closed
    }

    fn blanking_plan(&mut self) -> Result<Blanking, PresenceError> {
        Err(PresenceError::Backend {
            backend: BACKEND,
            detail: "blanking this machine's own screen needs the compositor's output power \
                     state, which no Wayland protocol offers a client"
                .to_owned(),
        })
    }

    fn blank(&mut self, _plan: &Blanking) -> Result<(), PresenceError> {
        Err(PresenceError::Backend {
            backend: BACKEND,
            detail: "this backend cannot blank a screen, so it must not be asked to".to_owned(),
        })
    }

    fn unblank(&mut self) -> Result<(), PresenceError> {
        // Nothing was ever blanked, so putting it back is done.
        Ok(())
    }

    fn verify(&mut self) -> Result<Verdict, PresenceError> {
        // What this process believes, plus the one thing the compositor tells
        // it: whether a surface was closed underneath it.
        //
        // **This is weaker than the X11 backend's**, which reads the screen
        // back. Reading a wlroots screen back means `zwlr_screencopy`, and
        // doing that here would make the indicator depend on the capture path
        // — so it is honest about being a report rather than an observation,
        // and says how many surfaces it is speaking for.
        let _ = self.queue.dispatch_pending(&mut self.globals);
        if self.globals.closed {
            return Ok(Verdict::Missing(Missing::Gone));
        }
        if !self.showing || self.strips.is_empty() {
            return Ok(Verdict::Missing(Missing::Gone));
        }
        Ok(Verdict::OnScreen {
            checked: self.strips.len(),
        })
    }
}
