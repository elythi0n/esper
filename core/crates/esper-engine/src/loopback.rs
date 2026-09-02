//! `esper-engine loopback` — the whole media path, measured.
//!
//! A diagnostic, not a product surface. It exists because every number in this
//! project's findings should be reproducible with one command on the machine
//! in question, rather than trusted from a document written on somebody else's.
//!
//! # It runs with no display
//!
//! Against the synthetic capturer, which is why it works in CI and on a
//! headless server. When a platform capture backend exists, `--monitor` will
//! select a real screen; until then it reports that there is only one source
//! and gets on with it, rather than pretending to enumerate monitors it cannot
//! see.
//!
//! # The size it encodes is not always the size it captures
//!
//! A resolution ceiling caps the longer edge before any load is measured,
//! defaulting to 1920, and the CPU budget's ladder can take a fraction of
//! what is left. So the header reports both the display and the encode size,
//! because a session quietly encoding a quarter of the pixels and a session
//! encoding all of them are very different things to be measuring.
//! `--max-resolution none` turns the cap off.
//!
//! # Why it decodes in-process
//!
//! The bitstream is this machine's own encoder's output, not a peer's, so the
//! large C parser is not pointed at anything hostile. A controller decoding a
//! peer's stream must use the sandboxed decoder, and the pipeline reports which
//! it has so the difference is never a guess.

use std::time::Duration;

use esper_capture::gate::Policy;
use esper_capture::pattern::Pattern;
use esper_color::RgbCoefficients;
use esper_color::convert::reference_yuv;
use esper_color::{ColorSpace, I420};
use esper_decode::decoder::DecoderConfig;
use esper_decode::openh264::OpenH264Decoder;
use esper_encode::budget::Budget;
use esper_encode::encoder::{EncodeError, EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;
use esper_pipeline::pipeline::{Pipeline, Step};
use esper_scale::Ceiling;

use crate::backend::{self, Choice};

/// What the loopback was asked to do.
#[derive(Debug, Clone)]
pub struct Options {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub seconds: u64,
    /// Share of one core the pipeline may use.
    pub budget: f64,
    /// Decode and compare, which costs time but measures the whole path.
    pub verify: bool,
    /// Cap on the longer edge, or `None` for no cap.
    pub max_resolution: Option<u32>,
    /// Which capture backend to use.
    pub backend: Choice,
    /// Which monitor, for a backend that has more than one.
    pub monitor: Option<u32>,
    /// Open a window and draw the decoded pictures in it.
    pub present: bool,
    /// A socket to write the encoded pictures to, for a window that decodes
    /// them itself — the viewer's path, exercised with no peer and no display.
    pub viewer: Option<std::path::PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 30,
            seconds: 5,
            budget: Budget::default().cores,
            verify: true,
            max_resolution: Ceiling::default().long_edge(),
            backend: Choice::default(),
            monitor: None,
            present: false,
            viewer: None,
        }
    }
}

/// Parse the flags, returning `Err` with a message a person can act on.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut i = 0usize;
    while let Some(arg) = args.get(i) {
        let value = |name: &str| -> Result<String, String> {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--size" => {
                let v = value("--size")?;
                let (w, h) = v
                    .split_once('x')
                    .ok_or_else(|| format!("--size wants WIDTHxHEIGHT, got {v}"))?;
                o.width = w.parse().map_err(|_| format!("bad width {w}"))?;
                o.height = h.parse().map_err(|_| format!("bad height {h}"))?;
                i += 2;
            }
            "--fps" => {
                o.fps = value("--fps")?
                    .parse()
                    .map_err(|_| "--fps wants a number".to_owned())?;
                i += 2;
            }
            "--seconds" => {
                o.seconds = value("--seconds")?
                    .parse()
                    .map_err(|_| "--seconds wants a number".to_owned())?;
                i += 2;
            }
            "--budget" => {
                o.budget = value("--budget")?
                    .parse()
                    .map_err(|_| "--budget wants a number of cores".to_owned())?;
                i += 2;
            }
            "--monitor" => {
                o.monitor = Some(
                    value("--monitor")?
                        .parse()
                        .map_err(|_| "--monitor wants a monitor number".to_owned())?,
                );
                i += 2;
            }
            "--backend" => {
                o.backend = Choice::parse(&value("--backend")?)?;
                i += 2;
            }
            "--max-resolution" => {
                let raw = value("--max-resolution")?;
                o.max_resolution = if raw == "none" {
                    None
                } else {
                    Some(raw.parse().map_err(|_| {
                        "--max-resolution wants pixels on the long edge, or `none`".to_owned()
                    })?)
                };
                i += 2;
            }
            "--present" => {
                o.present = true;
                i += 1;
            }
            "--viewer" => {
                let Some(path) = args.get(i + 1) else {
                    return Err("--viewer wants the socket a window is listening on".to_owned());
                };
                o.viewer = Some(std::path::PathBuf::from(path));
                i += 2;
            }
            "--no-verify" => {
                o.verify = false;
                i += 1;
            }
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    if o.fps == 0 {
        return Err("--fps must be at least 1".to_owned());
    }
    Ok(o)
}

fn encoder_for(config: &EncoderConfig) -> Result<Box<dyn VideoEncoder>, EncodeError> {
    Ok(Box::new(OpenH264::new(config)?))
}

/// A pointer to stand in for a host's, so the window's half can be exercised.
///
/// An arrow, drawn rather than loaded: the alternative is a file to ship and
/// a path to resolve, for sixteen pixels of shape whose only job is to be
/// something a window can wear.
fn pointer_shape() -> esper_capture::cursor::CursorImage {
    const N: u16 = 16;
    let mut rgba = vec![0u8; usize::from(N) * usize::from(N) * 4];
    for row in 0..usize::from(N) {
        for col in 0..=row.min(usize::from(N) - 1 - row / 3) {
            let at = (row * usize::from(N) + col) * 4;
            if let Some(px) = rgba.get_mut(at..at + 4) {
                px.copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    esper_capture::cursor::CursorImage {
        width: N,
        height: N,
        hotspot_x: 0,
        hotspot_y: 0,
        rgba,
    }
}

/// Run the loopback and print what it cost.
pub fn run(o: &Options) -> Result<(), String> {
    let pattern = Pattern::new(o.width, o.height).map_err(|e| e.to_string())?;
    // A frame rate far above the loop's own speed, so the measurement is not
    // coupled to how fast this loop happens to run — a mistake made once
    // already, where making the encoder faster silently changed how many
    // frames a measurement saw.
    let chosen = backend::open(
        o.backend,
        o.monitor.map(esper_capture::capturer::MonitorId),
        (o.width, o.height),
    )
    .map_err(|e| e.to_string())?;
    let monitors: Vec<_> = chosen.capturer.monitors().to_vec();
    let selected = chosen.capturer.selected();
    let reports_damage = chosen.capturer.reports_damage();
    let attempts = chosen.attempts.clone();
    let capturer = chosen.capturer;

    let mut pipeline = Pipeline::new(
        capturer,
        &encoder_for,
        Budget::of(o.budget),
        Policy {
            max_fps: o.fps,
            ..Policy::default()
        },
    )
    .map_err(|e| e.to_string())?;

    let ceiling = o.max_resolution.map_or_else(Ceiling::none, Ceiling::of);
    pipeline = pipeline.with_ceiling(ceiling);

    if o.verify {
        let decoder = OpenH264Decoder::new(&DecoderConfig::default()).map_err(|e| e.to_string())?;
        pipeline = pipeline.with_decoder(Box::new(decoder));
    }

    let (nw, nh) = pipeline.native_size();
    println!("🎞  esper loopback  {nw}x{nh} at {} fps", o.fps);
    println!();
    let (encoder, fast) = pipeline.encoder_backend();
    println!(
        "  capture   {}{}",
        pipeline.capture_backend(),
        if reports_damage {
            ""
        } else {
            "  (no damage reporting: every frame counts as wholly changed)"
        }
    );
    for line in &attempts {
        println!("              {line}");
    }
    if monitors.len() > 1 {
        for m in &monitors {
            println!(
                "              {} {}  {}x{} at {},{}{}",
                if m.id == selected { "→" } else { " " },
                m.name,
                m.geometry.width,
                m.geometry.height,
                m.geometry.x,
                m.geometry.y,
                if m.primary { "  primary" } else { "" }
            );
        }
    }
    println!(
        "  encode    {encoder}{}",
        if fast {
            ""
        } else {
            "  (no assembly: about 2.5% to 6% slower)"
        }
    );
    match pipeline.decoder_backend() {
        Some((name, sandboxed)) => println!(
            "  decode    {name}  ({})",
            if sandboxed {
                "sandboxed"
            } else {
                "in process — the bitstream is ours, not a peer's"
            }
        ),
        None => println!("  decode    not attached (--no-verify)"),
    }
    println!("  budget    {:.2} cores", o.budget);
    println!("  ceiling   {}", ceiling.describe());
    let (nw, nh) = pipeline.native_size();
    let (ew, eh) = pipeline.encode_size();
    if (ew, eh) == (nw, nh) {
        println!("  encoding  {ew}x{eh}, the whole display");
    } else {
        println!("  encoding  {ew}x{eh} from {nw}x{nh}, area averaged");
    }
    println!();

    // The frame budget: how long each turn is allowed, so the loop paces
    // itself rather than spinning.
    let interval = Duration::from_nanos(1_000_000_000 / u64::from(o.fps.max(1)));
    let deadline = std::time::Instant::now() + Duration::from_secs(o.seconds);

    // The window is the *encode* size, because that is what a decoded picture
    // is — a session under the resolution ceiling shows fewer pixels than the
    // display has, and a window sized to the display would letterbox them.
    let mut presenter: Option<Box<dyn esper_present::Presenter>> = if o.present {
        let (ew, eh) = pipeline.encode_size();
        match open_window(ew, eh) {
            Ok(p) => {
                println!("  present   {} at {ew}x{eh}", p.backend());
                println!();
                Some(p)
            }
            Err(e) => {
                // Not fatal: a loopback that measures the pipeline is still
                // useful without a window, and saying so beats refusing.
                println!("  present   unavailable ({e})");
                println!();
                None
            }
        }
    } else {
        None
    };

    // The viewer's path, with no peer and no display: the same repackaging and
    // the same stream a session writes, from this machine's own encoder.
    let mut viewer = match o.viewer.as_deref() {
        Some(path) => {
            let (ew, eh) = pipeline.encode_size();
            match crate::viewer_task::Viewer::connect(path, (ew, eh)) {
                Ok(mut window) => {
                    println!("  viewer    {} at {ew}x{eh}", path.display());
                    println!();
                    /*
                        What a window would be told by a session, told by this.

                        The loopback path has no peer, so there is no host to
                        say what it has — but it *is* capturing something, and
                        the screens it can see are the honest answer to the
                        same question. A window given nothing here would show
                        a switcher with no screens in it, which is the state
                        that used to be permanent.

                        The profile is the balanced one with nothing stated:
                        there is nobody to state it to.
                    */
                    window.session(
                        0,
                        0,
                        &[selected.0],
                        &monitors
                            .iter()
                            .map(|m| esper_media::viewer_stream::Screen {
                                id: m.id.0,
                                name: m.name.clone(),
                                x: m.geometry.x,
                                y: m.geometry.y,
                                width: m.geometry.width,
                                height: m.geometry.height,
                                primary: m.primary,
                            })
                            .collect::<Vec<_>>(),
                        // Nothing is asking for less: there is no link here
                        // to be short of anything.
                        (0, 0),
                    );
                    Some(window)
                }
                Err(e) => {
                    println!("  viewer    unavailable ({e})");
                    println!();
                    None
                }
            }
        }
        None => None,
    };
    let began = std::time::Instant::now();
    let mut pointer_said = false;

    let space = ColorSpace::default();
    let to_rgb = RgbCoefficients::for_space(space);
    let mut psnr_sum = 0f64;
    let mut psnr_count = 0u64;
    let mut counter_ok = 0u64;
    let mut counter_wrong = 0u64;
    // The counter and the PSNR are read out of the *known* pattern, so they
    // only mean anything against the synthetic source. Against a real screen
    // there is nothing to compare a decoded picture to — the reference is the
    // screen, and it has already moved on. Running them anyway reported every
    // real frame as "a dropped or reordered picture", which is a warning about
    // the harness rather than the pipeline.
    let verifiable = matches!(o.backend, Choice::Synthetic)
        || pipeline.capture_backend() == esper_capture::synthetic::BACKEND;

    while std::time::Instant::now() < deadline {
        match pipeline.step(interval).map_err(|e| e.to_string())? {
            Step::Encoded {
                decoded: Some(picture),
                damage,
                ref bitstream,
                ..
            } => {
                if let Some(window) = viewer.as_mut() {
                    window.picture(bitstream, began.elapsed());
                    /*
                        A pointer, so the whole of what a session sends is
                        exercised without a peer.

                        This path has no host to read a cursor from, and the
                        window's half of the pointer — wearing a shape,
                        drawing it where the picture is drawn — cannot be
                        reached at all without one. So the loopback stands in
                        for that too: one shape, once, and a position that
                        moves, which is exactly the split the format has.

                        `draw` is set because there is nothing here for a
                        pointer to follow: no session is being driven, so a
                        window that wore it would be showing somebody a
                        pointer at their own hand for a machine that does not
                        have one.
                    */
                    let (pw, ph) = pipeline.encode_size();
                    if !pointer_said {
                        pointer_said = true;
                        window.pointer_shape(Some(&pointer_shape()));
                    }
                    let round = began.elapsed().as_millis() as f64 / 900.0;
                    let x = f64::from(pw) / 2.0 * (1.0 + 0.6 * round.cos());
                    let y = f64::from(ph) / 2.0 * (1.0 + 0.6 * round.sin());
                    window.pointer(true, true, x as i32, y as i32);
                }
                if let Some(p) = presenter.as_mut() {
                    // The pipeline's damage is about the *capture*; what
                    // changed in the decoded picture is the same regions, so
                    // the window repaints only those. A decoder that lost sync
                    // would show it as a stale patch, which is the point.
                    let shown = p.present(&picture, &damage);
                    if let Err(e) = shown {
                        println!("  · the window stopped taking pictures: {e}");
                        presenter = None;
                    } else if p
                        .poll()
                        .is_ok_and(|evs| evs.contains(&esper_present::PresentEvent::CloseRequested))
                    {
                        println!("  · the window was closed");
                        presenter = None;
                    }
                }
                // The counter says which frame this is, read out of the
                // pixels. It is how latency will be measured later with no
                // second clock, and here it catches a dropped or repeated
                // picture.
                if !verifiable {
                    continue;
                }
                let read = pattern.read_counter(|x, y| rgb_at(&to_rgb, &picture, x, y));
                if read < u64::MAX {
                    // Any value is plausible; what matters is that it decodes
                    // to *a* frame index rather than noise.
                    counter_ok = counter_ok.saturating_add(1);
                }
                if let Some(q) = psnr(&pattern, read, &picture, space) {
                    psnr_sum += q;
                    psnr_count = psnr_count.saturating_add(1);
                } else {
                    counter_wrong = counter_wrong.saturating_add(1);
                }
            }
            Step::Encoded { .. } | Step::Skipped | Step::Unchanged => {}
            Step::Reconfigured { width, height } => {
                println!("  · reconfigured to {width}x{height}");
            }
            Step::Finished => break,
        }
    }

    report(&pipeline, psnr_sum, psnr_count, counter_wrong);
    Ok(())
}

/// One decoded pixel as RGB.
///
/// Through the fixed-point conversion rather than the `f64` reference. The
/// reference is what decides whether that conversion is right — checked
/// against it over all sixty-seven million triples — and using it here as well
/// meant reading a counter out of a picture cost a floating-point matrix per
/// pixel.
fn rgb_at(coeff: &RgbCoefficients, picture: &I420, x: u32, y: u32) -> [u8; 3] {
    let luma = picture.luma_at(x, y).unwrap_or(0);
    let (u, v) = picture.chroma_at(x, y).unwrap_or((128, 128));
    let (r, g, b) = coeff.pixel(luma, u, v);
    [r, g, b]
}

/// Luma PSNR of a decoded picture against the frame the counter says it is.
///
/// `None` when the counter did not read back as a frame this run produced,
/// because comparing against the wrong source frame would report a quality
/// collapse that is really a synchronisation problem.
fn psnr(pattern: &Pattern, frame: u64, got: &I420, space: ColorSpace) -> Option<f64> {
    let (w, h) = (pattern.width(), pattern.height());
    let mut sum = 0f64;
    let mut n = 0u64;
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = pattern.pixel(x, y, frame);
            let (want, _, _) = reference_yuv(r, g, b, space);
            let d = f64::from(i32::from(got.luma_at(x, y).unwrap_or(0)) - i32::from(want));
            sum += d * d;
            n = n.saturating_add(1);
        }
    }
    if n == 0 {
        return None;
    }
    let mse = sum / n as f64;
    if mse <= 0.0 {
        return Some(f64::INFINITY);
    }
    let value = 10.0 * (255.0f64 * 255.0 / mse).log10();
    // Below about 10 dB the pictures are unrelated, which means the counter
    // and the source disagree rather than that the codec failed.
    (value > 10.0).then_some(value)
}

fn report(pipeline: &Pipeline, psnr_sum: f64, psnr_count: u64, counter_wrong: u64) {
    // A real backend waits for the screen to change, so its wall clock is
    // mostly the wait and its CPU time is nearly nothing. The synthetic one
    // draws every pixel, which makes it the most expensive stage in the
    // pipeline. Both numbers mislead if the reader does not know which.
    let capture_note = if pipeline.capture_backend() == esper_capture::synthetic::BACKEND {
        "(synthetic: renders the pattern)"
    } else {
        "(mostly waiting for the screen to change)"
    };
    let s = pipeline.stats();
    let elapsed = pipeline.elapsed();

    println!("  per stage, per call:");
    println!(
        "    {:>9}  {:>8}  {:>8}  {:>8}  {:>8}",
        "stage", "calls", "wall ms", "cpu ms", "worst ms"
    );
    for (name, stage, note) in [
        // The synthetic source *renders* its picture, which a real capture
        // backend does not — it copies one the compositor already made. So
        // this row is the test pattern's cost, and labelling it matters:
        // unlabelled it reads as capture being the most expensive stage in the
        // pipeline, which is exactly the wrong conclusion to draw.
        ("capture", &s.capture, capture_note),
        ("convert", &s.convert, ""),
        ("encode", &s.encode, ""),
        ("decode", &s.decode, ""),
    ] {
        if stage.calls == 0 {
            continue;
        }
        let cpu = stage
            .mean_cpu_ms()
            .map_or_else(|| "     n/a".to_owned(), |v| format!("{v:>8.3}"));
        println!(
            "    {name:>9}  {:>8}  {:>8.3}  {cpu}  {:>8.2}  {note}",
            stage.calls,
            stage.mean_wall_ms(),
            stage.worst.as_secs_f64() * 1000.0
        );
    }

    println!();
    println!(
        "  frames    {} captured, {} encoded, {} skipped by the damage gate",
        s.frames_captured, s.frames_encoded, s.frames_skipped
    );
    println!(
        "  converted {:.2}% of the pixels a whole-frame pipeline would have",
        s.conversion_share() * 100.0
    );
    println!("  bitrate   {} kbit/s", s.bitrate(elapsed) / 1000);

    match s.core_share(elapsed) {
        Some(share) => {
            let cap = pipeline.governor().budget().cores;
            println!(
                "  host cpu  {:.1}% of one core, against a {:.0}% budget{}",
                share * 100.0,
                cap * 100.0,
                if share > cap { "  ← OVER" } else { "" }
            );
        }
        None => println!("  host cpu  not measurable on this platform"),
    }

    // The level **in force**, not the governor's own. They differ whenever
    // the caller asked for less than the ladder's top rung, and the first
    // version of this report said "60 fps" for a session running at 30.
    println!(
        "  quality   {}  (governor: {})",
        pipeline.level().describe(),
        pipeline.governor().describe()
    );
    if psnr_count > 0 {
        println!(
            "  picture   {:.2} dB luma PSNR over {psnr_count} frames",
            psnr_sum / psnr_count as f64
        );
    }
    if counter_wrong > 0 {
        println!(
            "  ⚠ {counter_wrong} frames whose counter did not match a source \
             frame — a dropped or reordered picture"
        );
    }

    let unapplied = pipeline.unapplied_levers();
    if !unapplied.is_empty() {
        println!();
        for lever in unapplied {
            println!("  ⚠ {:?} is not applied: {}", lever, lever.reason());
        }
    }
}

/// Open a window for pictures of this size, if this platform has one.
#[cfg(target_os = "linux")]
pub fn open_window(
    width: u32,
    height: u32,
) -> Result<Box<dyn esper_present::Presenter>, esper_present::PresentError> {
    let p = esper_present_x11::X11Presenter::open(width, height, ColorSpace::default())?;
    Ok(Box::new(p))
}

#[cfg(not(target_os = "linux"))]
pub fn open_window(
    _width: u32,
    _height: u32,
) -> Result<Box<dyn esper_present::Presenter>, esper_present::PresentError> {
    Err(esper_present::PresentError::Unavailable {
        tried: "no native presenter is compiled in for this platform".to_owned(),
    })
}
