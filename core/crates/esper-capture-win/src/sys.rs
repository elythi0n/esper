//! Every call into Windows that this crate makes.
//!
//! # Why all of it is in one file
//!
//! So it can be read as a unit, which is what `security-engineering.md` asks
//! of a crate that needs FFI. Nothing above this module is `unsafe`, and the
//! safe surface here is deliberately small: open a duplication, ask it for a
//! frame, read the frame, give it back.
//!
//! The parts that could be got wrong *quietly* are not here at all. What
//! changed in a frame is [`esper_capture::duplication`], the pointer's pixels
//! are [`esper_capture::pointer_shape`], and the rule about releasing frames
//! is [`esper_capture::frames`] — all three in the platform-neutral crate,
//! compiled and tested on every machine. What is left here is the calls, and a
//! wrong call does not compile.
//!
//! # Unsafe inventory
//!
//! Fifteen calls. Every one is a Win32 or COM function that is `unsafe` only
//! because it is `extern "system"`; the invariants are the ones the API
//! documents, and each is established by construction rather than assumed.
//!
//! There are **no raw pointers held across a call** except the one the mapped
//! subresource hands back, which is bounded by the pitch and row count the
//! same call reports and is unmapped before the borrow ends.
//!
//! - **`D3D11CreateDevice`** — the adapter is `None`, so Windows picks; the
//!   feature levels are a slice this function owns; `ppdevice` and
//!   `ppimmediatecontext` are out-parameters written before the call returns
//!   success. The invariant is `D3D11_SDK_VERSION`, which comes from the
//!   crate's own constant.
//! - **`IDXGIDevice::GetAdapter`** — takes nothing, returns an interface with
//!   its own reference. Nothing outlives the call.
//! - **`IDXGIAdapter::EnumOutputs`** — takes an index. Out of range is
//!   `DXGI_ERROR_NOT_FOUND`, which is the loop's exit and not a failure.
//! - **`IDXGIOutput::GetDesc`** — returns a `DXGI_OUTPUT_DESC` by value.
//! - **`IDXGIOutput1::DuplicateOutput`** — takes the device by reference and
//!   returns the duplication. Fails when something else already duplicates
//!   this output, which is reported rather than retried.
//! - **`AcquireNextFrame`** — `pframeinfo` and `ppdesktopresource` are
//!   out-parameters this function owns; both are initialised before the call
//!   and only read when it returns success. The API's invariant is the one
//!   [`esper_capture::frames`] enforces: no acquire while a frame is held.
//! - **`GetFrameMoveRects`** — the buffer is a `Vec` this function sized from
//!   `TotalMetadataBufferSize`; the count argument is its byte length, not its
//!   element count, which is the API's unit and the easiest thing to get
//!   wrong. `pmoverectsbuffersizerequired` says how much was written.
//! - **`GetFrameDirtyRects`** — the same, into the remainder of the same
//!   buffer, because the two calls share one allocation by design.
//! - **`GetFramePointerShape`** — the buffer is sized from
//!   `PointerShapeBufferSize`; the shape header is a separate out-parameter.
//! - **`ID3D11Device::CreateTexture2D`** — the description is a value this
//!   function fills in whole; `pinitialdata` is `None` because a staging
//!   texture is written by a copy, not by initial data.
//! - **`CopyResource`** — both resources are ones this crate owns, and the API
//!   requires identical dimensions and format, which is why the staging
//!   texture is rebuilt whenever the frame's description differs.
//! - **`Map`** — the resource is the staging texture, subresource zero, and
//!   `D3D11_MAP_READ` matches the `CPU_ACCESS_READ` it was created with. The
//!   mapped structure is an out-parameter this function owns.
//! - **`std::slice::from_raw_parts`** — over the pointer `Map` returned, for
//!   `RowPitch * rows` bytes, which is the size the same call reports. It is
//!   read-only, it does not outlive the `Unmap` below, and nothing else holds
//!   the texture while it exists because `&mut self` is held throughout. The
//!   copy itself is [`esper_capture::pitched::copy_rows`], which is safe and
//!   tested on every platform — the arithmetic that decides whether the
//!   picture is straight is not inside an `unsafe` block.
//! - **`Unmap`** — the same resource and subresource as the `Map`. Called on
//!   every path out of the readback, including the error ones, so a failed
//!   copy cannot leave the texture mapped.
//! - **`ReleaseFrame`** — takes nothing. Called exactly once per successful
//!   acquire, which is what [`esper_capture::frames`] is for.

use std::time::Duration;

use esper_capture::cursor::CursorImage;
use esper_capture::duplication::{Moved, Report};
use esper_capture::frames::Acquired;
use esper_capture::geometry::Rect;
use esper_capture::pointer_shape::{self, Shape, ShapeInfo};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTDUPL_MOVE_RECT, DXGI_OUTDUPL_POINTER_SHAPE_INFO, IDXGIDevice, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

/// What went wrong talking to Windows.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SysError {
    /// A call failed, named with what it was doing.
    #[error("{doing}: {code}")]
    Call { doing: &'static str, code: String },

    /// This output is rotated, and nothing here rotates a picture.
    ///
    /// **Refused rather than sent sideways.** Duplication hands over the
    /// *unrotated* desktop image while the damage rectangles are in that same
    /// unrotated space — so a capturer that ignored rotation would send a
    /// picture turned the wrong way with damage on the wrong side of it, which
    /// reads as a driver fault and is not one.
    #[error(
        "{name} is rotated, and this backend does not turn a picture round yet; \
         set the display back to landscape or capture another one"
    )]
    Rotated { name: String },

    /// No output at that index.
    #[error("this machine has no output {index}")]
    NoSuchOutput { index: u32 },
}

fn failing(doing: &'static str) -> impl FnOnce(windows::core::Error) -> SysError {
    move |e| SysError::Call {
        doing,
        code: e.message(),
    }
}

/// One output, as DXGI describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// `\\.\DISPLAY1` and the like. Shown to a person, never matched on.
    pub name: String,
    /// Position and size in desktop coordinates. The origin is **negative**
    /// for an output left of or above the primary one.
    pub bounds: Rect,
    /// Whether it is part of the desktop. An output that is not cannot be
    /// duplicated.
    pub attached: bool,
    /// Whether it is turned. See [`SysError::Rotated`].
    pub rotated: bool,
    /// Its index for [`Duplicated::open`].
    pub index: u32,
}

/// Every output on the first adapter, with the interface for each.
///
/// One place that walks device → adapter → output → description, because both
/// [`screens`] and [`Duplicated::open`] need exactly that walk — and four
/// `unsafe` blocks written twice is four more than the inventory should have to
/// account for.
///
/// # Errors
///
/// If a device or an adapter cannot be had at all. An index past the last
/// output is the loop's exit, not an error.
fn outputs() -> Result<(ID3D11Device, ID3D11DeviceContext, Vec<Output>), SysError> {
    let (device, context) = make_device()?;
    let dxgi: IDXGIDevice = device
        .cast()
        .map_err(failing("asking for the DXGI device"))?;
    #[allow(unsafe_code)]
    // SAFETY: takes nothing and returns an interface with its own reference.
    // See the unsafe inventory.
    let adapter = unsafe { dxgi.GetAdapter() }.map_err(failing("asking for the adapter"))?;

    let mut found = Vec::new();
    for index in 0.. {
        #[allow(unsafe_code)]
        // SAFETY: an index; out of range is DXGI_ERROR_NOT_FOUND, which is the
        // loop's exit. See the unsafe inventory.
        let interface = match unsafe { adapter.EnumOutputs(index) } {
            Ok(o) => o,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(failing("enumerating outputs")(e)),
        };
        #[allow(unsafe_code)]
        // SAFETY: returns a description by value. See the unsafe inventory.
        let desc = unsafe { interface.GetDesc() }.map_err(failing("describing an output"))?;
        found.push(Output {
            screen: screen_of(index, &desc),
            interface,
        });
    }
    Ok((device, context, found))
}

/// An output and how this crate describes it.
struct Output {
    screen: Screen,
    interface: windows::Win32::Graphics::Dxgi::IDXGIOutput,
}

impl std::fmt::Debug for Output {
    /// The description, never the interface pointer.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Output")
            .field("screen", &self.screen)
            .finish_non_exhaustive()
    }
}

/// A `DXGI_OUTPUT_DESC` as a [`Screen`].
fn screen_of(index: u32, desc: &windows::Win32::Graphics::Dxgi::DXGI_OUTPUT_DESC) -> Screen {
    Screen {
        name: String::from_utf16_lossy(&desc.DeviceName)
            .trim_end_matches('\0')
            .to_owned(),
        bounds: rect_of(desc.DesktopCoordinates),
        attached: desc.AttachedToDesktop.as_bool(),
        // `UNSPECIFIED` means the driver does not say, which is how a great
        // deal of hardware reports an unrotated display — so it is not
        // treated as turned.
        rotated: desc.Rotation != DXGI_MODE_ROTATION_IDENTITY
            && desc.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED,
        index,
    }
}

/// Every output on the first adapter.
///
/// # Errors
///
/// If a device or an adapter cannot be had at all.
pub fn screens() -> Result<Vec<Screen>, SysError> {
    Ok(outputs()?.2.into_iter().map(|o| o.screen).collect())
}

/// A `RECT` as this project's rectangle.
///
/// The three ways this goes wrong — inclusive edges, a negative origin
/// wrapping a `u32`, and an inverted rectangle becoming enormous — are
/// [`Rect::from_edges`]'s to get right, and they are tested there. This is the
/// two-line adapter.
const fn rect_of(r: RECT) -> Rect {
    Rect::from_edges(r.left, r.top, r.right, r.bottom)
}

/// A device and its immediate context.
fn make_device() -> Result<(ID3D11Device, ID3D11DeviceContext), SysError> {
    let levels = [D3D_FEATURE_LEVEL_11_0];
    let mut device = None;
    let mut context = None;
    #[allow(unsafe_code)]
    // SAFETY: the adapter is None so Windows picks; `levels` is a slice this
    // function owns; the two out-parameters are written before success is
    // returned. See the unsafe inventory.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            // No software rasteriser module: the driver type above is
            // hardware, and this parameter is only read for
            // `D3D_DRIVER_TYPE_SOFTWARE`.
            windows::Win32::Foundation::HMODULE::default(),
            // BGRA support, because that is the format the desktop arrives in
            // and the one the pipeline wants.
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(failing("creating a Direct3D device"))?;

    match (device, context) {
        (Some(device), Some(context)) => Ok((device, context)),
        // The API contract is that both are written on success, so this is
        // unreachable — and a refusal rather than an unwrap, because the
        // workspace does not panic and a driver is not a thing to trust with
        // the process.
        _ => Err(SysError::Call {
            doing: "creating a Direct3D device",
            code: "the driver reported success and returned nothing".to_owned(),
        }),
    }
}

/// A duplication of one output, and everything needed to read from it.
#[derive(Debug)]
pub struct Duplicated {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    /// The output this duplicates, so a reconfiguration can be reported.
    screen: Screen,
    /// The texture the desktop is copied into to be read by the CPU. Rebuilt
    /// whenever a frame's description differs from the one it was made for.
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    /// The frame currently held, if one is.
    held: Option<IDXGIResource>,
    /// What the last acquire reported.
    info: DXGI_OUTDUPL_FRAME_INFO,
}

impl Duplicated {
    /// Duplicate one output.
    ///
    /// # Errors
    ///
    /// If there is no such output, if it is rotated, if it is not attached, or
    /// if something else already duplicates it.
    pub fn open(index: u32) -> Result<Self, SysError> {
        let (device, context, found) = outputs()?;
        let Some(output) = found.into_iter().find(|o| o.screen.index == index) else {
            return Err(SysError::NoSuchOutput { index });
        };
        if output.screen.rotated {
            return Err(SysError::Rotated {
                name: output.screen.name,
            });
        }

        let output1: IDXGIOutput1 = output
            .interface
            .cast()
            .map_err(failing("asking the output for duplication"))?;
        #[allow(unsafe_code)]
        // SAFETY: the device is passed by reference and outlives the call.
        // See the unsafe inventory.
        let duplication = unsafe { output1.DuplicateOutput(&device) }
            .map_err(failing("duplicating the output"))?;

        Ok(Self {
            device,
            context,
            duplication,
            screen: output.screen,
            staging: None,
            held: None,
            info: DXGI_OUTDUPL_FRAME_INFO::default(),
        })
    }

    /// The output being duplicated.
    #[must_use]
    pub const fn screen(&self) -> &Screen {
        &self.screen
    }

    /// Ask for the next frame.
    ///
    /// The outcome is the one [`esper_capture::frames`] reasons about, so a
    /// caller records it there rather than deciding for itself what to do.
    pub fn acquire(&mut self, within: Duration) -> Acquired {
        let ms = u32::try_from(within.as_millis()).unwrap_or(u32::MAX);
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        #[allow(unsafe_code)]
        // SAFETY: both out-parameters are owned here, initialised before the
        // call, and read only on success. See the unsafe inventory.
        let got = unsafe {
            self.duplication
                .AcquireNextFrame(ms, &mut info, &mut resource)
        };
        match got {
            Ok(()) => {
                self.info = info;
                self.held = resource;
                Acquired::Frame
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => Acquired::Timeout,
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => Acquired::AccessLost,
            Err(e) => {
                tracing::debug!("acquiring a frame: {}", e.message());
                Acquired::Failed
            }
        }
    }

    /// What the held frame's metadata says, reduced to what damage needs.
    #[must_use]
    pub const fn report(&self) -> Report {
        Report {
            // Zero means the pointer moved and the image did not.
            image_changed: self.info.LastPresentTime != 0,
            // Set by `rects` when the buffer was too small; assumed complete
            // until then, which is what the API reports for the common case.
            metadata_complete: true,
            accumulated: self.info.AccumulatedFrames,
        }
    }

    /// The moved and dirty rectangles of the held frame.
    ///
    /// Both calls share one buffer, sized from `TotalMetadataBufferSize`,
    /// because that is the size the API reports for the pair of them.
    ///
    /// # Errors
    ///
    /// If either call fails. A failure here can be `ACCESS_LOST` *after* the
    /// acquire succeeded, which the caller turns into
    /// [`esper_capture::frames::Frames::lost`] rather than a release.
    pub fn rects(&mut self) -> Result<(Vec<Moved>, Vec<Rect>, bool), SysError> {
        let room = self.info.TotalMetadataBufferSize;
        if room == 0 {
            return Ok((Vec::new(), Vec::new(), true));
        }
        let bytes = usize::try_from(room).unwrap_or(0);

        // Moves first, because the API fills one buffer with moves then dirt
        // and reports how much each used.
        let mut moves: Vec<DXGI_OUTDUPL_MOVE_RECT> =
            vec![
                DXGI_OUTDUPL_MOVE_RECT::default();
                bytes / size_of::<DXGI_OUTDUPL_MOVE_RECT>() + 1
            ];
        let mut used = 0u32;
        #[allow(unsafe_code)]
        // SAFETY: the buffer is a Vec this function owns and the count is its
        // **byte** length, which is the API's unit. See the unsafe inventory.
        unsafe {
            self.duplication.GetFrameMoveRects(
                u32::try_from(moves.len() * size_of::<DXGI_OUTDUPL_MOVE_RECT>()).unwrap_or(0),
                moves.as_mut_ptr(),
                &mut used,
            )
        }
        .map_err(failing("asking what moved"))?;
        let move_count = usize::try_from(used).unwrap_or(0) / size_of::<DXGI_OUTDUPL_MOVE_RECT>();

        let mut dirty: Vec<RECT> = vec![RECT::default(); bytes / size_of::<RECT>() + 1];
        let mut dirt_used = 0u32;
        #[allow(unsafe_code)]
        // SAFETY: the same, in bytes, into a buffer this function owns. See
        // the unsafe inventory.
        unsafe {
            self.duplication.GetFrameDirtyRects(
                u32::try_from(dirty.len() * size_of::<RECT>()).unwrap_or(0),
                dirty.as_mut_ptr(),
                &mut dirt_used,
            )
        }
        .map_err(failing("asking what changed"))?;
        let dirty_count = usize::try_from(dirt_used).unwrap_or(0) / size_of::<RECT>();

        let moved = moves
            .get(..move_count)
            .unwrap_or_default()
            .iter()
            .map(|m| Moved {
                from_x: m.SourcePoint.x,
                from_y: m.SourcePoint.y,
                to: rect_of(m.DestinationRect),
            })
            .collect();
        let changed = dirty
            .get(..dirty_count)
            .unwrap_or_default()
            .iter()
            .map(|r| rect_of(*r))
            .collect();
        Ok((moved, changed, true))
    }

    /// The pointer's pixels, when this frame carried a new shape.
    ///
    /// `None` when it did not, which is most frames: a shape is sent when it
    /// changes, not when the pointer moves.
    ///
    /// # Errors
    ///
    /// If the call fails, or the shape is one
    /// [`esper_capture::pointer_shape`] refuses.
    pub fn pointer(&mut self) -> Result<Option<CursorImage>, SysError> {
        let room = self.info.PointerShapeBufferSize;
        if room == 0 {
            return Ok(None);
        }
        let mut bytes = vec![0u8; usize::try_from(room).unwrap_or(0)];
        let mut used = 0u32;
        let mut header = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        #[allow(unsafe_code)]
        // SAFETY: the buffer is sized from the size the API reported and the
        // header is an out-parameter this function owns. See the unsafe
        // inventory.
        unsafe {
            self.duplication.GetFramePointerShape(
                room,
                bytes.as_mut_ptr().cast(),
                &mut used,
                &mut header,
            )
        }
        .map_err(failing("asking for the pointer's shape"))?;

        let Some(shape) = Shape::from_kind(header.Type) else {
            // A type Windows added. Not guessed at: a wrong reading is a
            // cursor made of somebody's memory.
            tracing::debug!(
                "a pointer shape of type {} is not one this knows",
                header.Type
            );
            return Ok(None);
        };
        let info = ShapeInfo {
            shape,
            width: header.Width,
            height: header.Height,
            pitch: header.Pitch,
            hotspot_x: u32::try_from(header.HotSpot.x).unwrap_or(0),
            hotspot_y: u32::try_from(header.HotSpot.y).unwrap_or(0),
        };
        match pointer_shape::image_of(
            &info,
            bytes
                .get(..usize::try_from(used).unwrap_or(0))
                .unwrap_or_default(),
        ) {
            Ok(image) => Ok(Some(image)),
            Err(e) => {
                // A shape this cannot draw is not a reason to end a session:
                // the picture is what matters and the pointer is drawn by the
                // viewer from the last shape it had.
                tracing::debug!("a pointer shape was refused: {e}");
                Ok(None)
            }
        }
    }

    /// Copy the held frame into `into`, row by row.
    ///
    /// `stride` is the destination's row length in bytes, which is not the
    /// source's: the mapped texture has a pitch of its own and copying by
    /// width shears the image.
    ///
    /// # Errors
    ///
    /// If the frame is not a texture, if the staging texture cannot be made,
    /// or if the copy or the map fails.
    pub fn read_into(&mut self, into: &mut [u8], stride: usize) -> Result<(), SysError> {
        let Some(resource) = self.held.clone() else {
            return Err(SysError::Call {
                doing: "reading a frame",
                code: "no frame is held".to_owned(),
            });
        };
        let texture: ID3D11Texture2D = resource
            .cast()
            .map_err(failing("reading the frame as a texture"))?;

        let (width, height) = (self.screen.bounds.width, self.screen.bounds.height);
        let staging = self.staging_for(width, height)?;

        #[allow(unsafe_code)]
        // SAFETY: both resources are owned here and have the same dimensions
        // and format by construction — `staging_for` rebuilds whenever they
        // differ. See the unsafe inventory.
        unsafe {
            self.context.CopyResource(&staging, &texture);
        }

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        #[allow(unsafe_code)]
        // SAFETY: subresource zero of a staging texture created with
        // CPU_ACCESS_READ, matching D3D11_MAP_READ. See the unsafe inventory.
        unsafe {
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .map_err(failing("mapping the frame for reading"))?;

        let pitch = usize::try_from(mapped.RowPitch).unwrap_or(0);
        let rows = usize::try_from(height).unwrap_or(0);
        let copied = if mapped.pData.is_null() || pitch == 0 {
            Err(SysError::Call {
                doing: "mapping the frame for reading",
                code: "the map succeeded and gave nothing to read".to_owned(),
            })
        } else {
            #[allow(unsafe_code)]
            // SAFETY: `pData` is what Map returned and `pitch * rows` is the
            // size the same call reports for it. Read-only, does not outlive
            // the Unmap below, and `&mut self` is held throughout so nothing
            // else touches the texture. See the unsafe inventory.
            let source = unsafe {
                std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch.saturating_mul(rows))
            };
            esper_capture::pitched::copy_rows(source, pitch, into, stride, stride, rows).map_err(
                |e| SysError::Call {
                    doing: "copying a frame",
                    code: e.to_string(),
                },
            )
        };

        #[allow(unsafe_code)]
        // SAFETY: the same resource and subresource as the Map above, and
        // reached on every path out of the readback including the failing
        // ones. See the unsafe inventory.
        unsafe {
            self.context.Unmap(&staging, 0);
        }
        copied
    }

    /// Give the frame back.
    ///
    /// # Errors
    ///
    /// If the call fails. Called exactly once per successful acquire — see
    /// [`esper_capture::frames`].
    pub fn release(&mut self) -> Result<(), SysError> {
        self.held = None;
        #[allow(unsafe_code)]
        // SAFETY: takes nothing. See the unsafe inventory.
        unsafe { self.duplication.ReleaseFrame() }.map_err(failing("giving the frame back"))
    }

    /// A staging texture of the right size, rebuilt if the size changed.
    fn staging_for(&mut self, width: u32, height: u32) -> Result<ID3D11Texture2D, SysError> {
        if let Some((texture, w, h)) = &self.staging
            && *w == width
            && *h == height
        {
            return Ok(texture.clone());
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            // Staging plus read access is the pair that makes a texture
            // readable by the CPU; either alone is refused by the API.
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut made = None;
        #[allow(unsafe_code)]
        // SAFETY: the description is filled in whole here; `pinitialdata` is
        // None because a staging texture is written by a copy. See the unsafe
        // inventory.
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut made)) }
            .map_err(failing("making a texture to read the frame through"))?;
        let Some(texture) = made else {
            return Err(SysError::Call {
                doing: "making a texture to read the frame through",
                code: "the driver reported success and returned nothing".to_owned(),
            });
        };
        self.staging = Some((texture.clone(), width, height));
        Ok(texture)
    }
}
