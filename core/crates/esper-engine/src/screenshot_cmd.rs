//! One picture of somebody's screen, and nothing else.
//!
//! # Why this is not `connect` with a flag
//!
//! A `view` session opens a capture backend, an encoder, a presenter, an input
//! stream, a clipboard and a file service, and keeps them for as long as
//! somebody is watching. A screenshot wants one frame. Threading a
//! "screenshot" flag through all of that would leave every one of those paths
//! carrying a case it never exercises — and the case that never runs is the
//! one that breaks.
//!
//! So this asks for `view`, waits for one keyframe, writes it, and closes. It
//! is the shortest session this product can have, which is also what makes it
//! useful: it works over ssh, in a script, and on a machine with no display
//! and nothing to show a window with.
//!
//! # Why stdout is a real destination
//!
//! `esper screenshot elythion | wl-copy`, or into an issue tracker, or over a
//! pipe. A tool that could only write files would make the caller invent a
//! temporary one and clean it up — so `-` is stdout, and everything this
//! prints on the way goes to stderr so it cannot corrupt the image.

use std::io::Write as _;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use esper_color::space::ColorSpace;

use crate::session_cmd::{Reached, Reaching, reach};

/// How long to wait for a picture before giving up.
///
/// A keyframe is asked for the moment the session opens, and one arrives
/// inside a round trip on any link this runs over. Ten seconds is long enough
/// for a host that has to start a capture backend and short enough that a
/// script does not hang on a machine that is not going to answer.
const PATIENCE: Duration = Duration::from_secs(10);

/// What to take a picture of, and where to put it.
#[derive(Debug)]
pub struct ShotOptions {
    pub addr: Option<SocketAddr>,
    pub named: Option<String>,
    pub via: Option<esper_rendezvous::Via>,
    pub mdns: bool,
    /// Which monitor, when the host has more than one.
    pub monitor: Option<u32>,
    /// Every screen, laid out the way the host has them.
    pub all: bool,
    /// Where the PNG goes. `None` or `-` is stdout.
    pub out: Option<String>,
}

/// Take one picture.
///
/// # Errors
///
/// If the host cannot be reached, refuses, or sends nothing decodable in
/// [`PATIENCE`].
pub fn run(o: &ShotOptions) -> Result<(), String> {
    crate::net::install_crypto();
    let identity = crate::paths::identity()?;
    let to_stdout = o.out.as_deref().is_none_or(|p| p == "-");

    crate::net::runtime()?.block_on(async {
        let reached = reach(
            &identity,
            &Reaching {
                label: "📸 esper screenshot",
                addr: o.addr,
                named: o.named.as_deref(),
                via: o.via,
                mdns: o.mdns,
                // **On stderr, always.** The image may be going to stdout, and
                // a banner printed there is not untidy — it is a corrupt PNG.
                aside: true,
            },
        )
        .await?;
        let Reached {
            endpoint,
            mut session,
            ..
        } = reached;

        let (media, mut control) = esper_link::parts(&mut session).map_err(|e| e.to_string())?;
        let grant = esper_transport::request_session(
            &mut control,
            &esper_proto::SessionRequest {
                kind: esper_proto::SessionKind::Screen,
                // **`view` and nothing else.** A screenshot has no use for
                // control, and asking for a permission it will not use is
                // asking somebody to agree to more than is happening.
                want: vec![esper_proto::Permission::View],
                codecs: vec![esper_proto::Codec::H264.name().to_owned()],
                reason: "one picture".to_owned(),
            },
        )
        .await
        .map_err(|e| match e {
            esper_transport::AuthorizeError::Refused(d) => {
                format!("the host refused: {} ({})", d.detail, d.reason.0)
            }
            other => format!("agreeing what the session may do: {other}"),
        })?;
        if !grant.allows(esper_proto::Permission::View) {
            return Err("this device may not view that host's screen".to_owned());
        }

        let link = esper_link::ControllerLink::open(media, control)
            .await
            .map_err(|e| format!("opening the stream: {e}"))?;
        let config = link.config().clone();
        eprintln!(
            "  stream     {}x{} at up to {} fps",
            config.width, config.height, config.max_fps
        );

        // Sandboxed, for the reason `connect` gives: this is a peer's
        // bitstream reaching a large C parser.
        let worker = esper_decode::SandboxedDecoder::spawn_self(
            &esper_decode::decoder::DecoderConfig::default(),
        )
        .map_err(|e| format!("{e}"))?;
        let mut link =
            link.with_decoder(Box::new(worker) as Box<dyn esper_decode::decoder::VideoDecoder>);

        if let Some(wanted) = o.monitor
            && wanted != config.selected
        {
            link.select_monitor(wanted);
            link.flush_outbound()
                .await
                .map_err(|e| format!("asking for monitor {wanted}: {e}"))?;
            eprintln!("  monitor    asking for {wanted}");
        }

        // **The same space the viewer opens with**, which is the default the
        // host encodes in. It is not carried in `StreamConfig`: the host
        // signals it in the bitstream's VUI, where a decoder would have to
        // read it back out. So this assumes the default exactly as
        // `open_window` does, and the two being the same assumption is the
        // point — a screenshot in one space and a window in another would
        // differ by a couple of levels with nothing to say which was right.
        // Reading the VUI is the fix for both at once, and neither does it.
        let space = ColorSpace::default();
        let png = if o.all {
            every_screen(&mut link, space).await?
        } else {
            let picture = wait_for_a_picture(&mut link).await?;
            to_png(&picture, space)?
        };
        link.close(esper_proto::Reason::SESSION_COMPLETE);
        // The close is queued, not sent; a process that exits immediately
        // leaves the host waiting out its idle timeout for a session that
        // ended cleanly. The same wait `connect` does, for the same reason.
        tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle())
            .await
            .ok();

        match (&o.out, to_stdout) {
            (_, true) => {
                std::io::stdout()
                    .write_all(&png)
                    .map_err(|e| format!("writing the image: {e}"))?;
                eprintln!("  wrote      {} bytes to stdout", png.len());
            }
            (Some(path), false) => {
                std::fs::write(path, &png).map_err(|e| format!("{path}: {e}"))?;
                eprintln!("  wrote      {path} — {} bytes", png.len());
            }
            (None, false) => unreachable!("no path and not stdout"),
        }
        Ok(())
    })
}

/// Every screen the host has, laid out the way the host has them.
///
/// # Why one at a time rather than all at once
///
/// A host serving several screens at once means a pipeline, an encoder and a
/// packetizer for each, several streams reassembled on the controller, and a
/// CPU budget divided between them. That is what a *live* tiled session needs
/// and it is a large piece of work.
///
/// **A screenshot is a still, so it does not need any of it.** Each screen is
/// selected in turn on the one stream there already is, given a moment to
/// arrive, and kept. The screens end up a few hundred milliseconds apart,
/// which is invisible in a photograph and would be quite wrong in a video —
/// which is exactly why this is a screenshot and not the tiled session.
///
/// A screen the host refuses is skipped with a line saying so rather than
/// abandoning the picture: two screens out of three is a useful image, and a
/// host is entitled to decline one.
async fn every_screen(
    link: &mut esper_link::ControllerLink,
    space: ColorSpace,
) -> Result<Vec<u8>, String> {
    let advertised = link.config().monitors.clone();
    let mut taken: Vec<(esper_color::mosaic::At, esper_color::I420)> = Vec::new();
    for screen in &advertised {
        if link.config().selected != screen.id {
            link.select_monitor(screen.id);
            link.flush_outbound()
                .await
                .map_err(|e| format!("asking for screen {}: {e}", screen.id))?;
            match wait_for_the_switch(link, screen.id).await {
                Ok(()) => {}
                Err(why) => {
                    eprintln!("  screen {} skipped: {why}", screen.id);
                    continue;
                }
            }
        }
        let picture = wait_for_a_picture(link).await?;
        eprintln!(
            "  screen {}  {} — {}x{} at {},{}",
            screen.id,
            screen.name,
            picture.width(),
            picture.height(),
            screen.x,
            screen.y
        );
        taken.push((
            esper_color::mosaic::At {
                x: screen.x,
                y: screen.y,
            },
            picture,
        ));
    }
    if taken.is_empty() {
        return Err("no screen could be captured".to_owned());
    }

    // The layout comes from the geometry the host advertised, against the
    // *picture* sizes rather than the advertised ones: a host whose governor
    // has lowered the resolution sends a smaller picture of the same screen,
    // and a mosaic sized from what was advertised would leave a gap.
    let boxes: Vec<(esper_color::mosaic::At, u32, u32)> = taken
        .iter()
        .map(|(at, p)| (*at, p.width(), p.height()))
        .collect();
    let bounds = esper_color::mosaic::Bounds::around(&boxes)
        .ok_or_else(|| "no screen to lay out".to_owned())?;
    let mut mosaic = esper_color::I420::new(bounds.width, bounds.height)
        .map_err(|e| format!("a mosaic of {}x{}: {e}", bounds.width, bounds.height))?;
    esper_color::mosaic::clear(&mut mosaic);
    for (at, picture) in &taken {
        esper_color::mosaic::place(&mut mosaic, bounds, *at, picture);
    }
    eprintln!(
        "  laid out   {} screen(s) as {}x{}",
        taken.len(),
        bounds.width,
        bounds.height
    );
    to_png(&mosaic, space)
}

/// Wait for the host to say it has switched, or to say it has not.
///
/// **A host's answer to a selection is never silence** — it sends either the
/// new config or the one it is keeping — so this waits for a config and reads
/// which of the two arrived rather than assuming the switch happened.
async fn wait_for_the_switch(
    link: &mut esper_link::ControllerLink,
    wanted: u32,
) -> Result<(), String> {
    let deadline = Instant::now() + PATIENCE;
    let mut out = Vec::new();
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, link.pump(&mut out)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("the session ended: {e}")),
            Err(_) => break,
        }
        out.clear();
        if let Some(config) = link.take_announcement() {
            if config.selected == wanted {
                return Ok(());
            }
            return Err(format!(
                "the host is keeping screen {} instead",
                config.selected
            ));
        }
    }
    Err("the host did not answer".to_owned())
}

/// Wait for the first decoded picture, or give up saying what did arrive.
async fn wait_for_a_picture(
    link: &mut esper_link::ControllerLink,
) -> Result<esper_color::I420, String> {
    let deadline = Instant::now() + PATIENCE;
    let mut lost = 0u64;
    let mut skipped = 0u64;
    let mut out = Vec::new();
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        // The jitter buffer's own deadline, or the time left, whichever is
        // sooner — the same protocol `connect` follows with `due_in`.
        let wait = link
            .due_in(Instant::now())
            .map_or(left, |due| due.min(left))
            .max(Duration::from_millis(1));
        match tokio::time::timeout(wait, link.receive(&mut out)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("the session ended: {e}")),
            Err(_) => link.tick(Instant::now(), &mut out),
        }
        for decoded in out.drain(..) {
            match decoded {
                esper_stream::Decoded::Picture { picture, .. } => return Ok(picture),
                esper_stream::Decoded::Lost { .. } => lost = lost.saturating_add(1),
                esper_stream::Decoded::Skipped { .. } => skipped = skipped.saturating_add(1),
            }
        }
    }
    // **Say what happened rather than "timed out".** A link that lost every
    // frame and one where the host never sent anything are different problems,
    // and the number is the only thing that tells them apart.
    Err(format!(
        "no picture arrived in {}s ({lost} frame(s) lost, {skipped} skipped)",
        PATIENCE.as_secs()
    ))
}

/// A decoded picture as a PNG.
///
/// **The same conversion the viewer uses**, through `esper_color::rgb`, so a
/// screenshot is what somebody watching would have seen rather than a second
/// opinion about the same pixels. The reference conversion in
/// `esper_color::convert` is floating-point and exists to check that one; using
/// it here would mean two answers in the product, differing by a level or two,
/// with nothing to say which was right.
///
/// It converts into `Bgrx8` because that is what `rgb` writes and what a
/// window's back buffer wants, then drops to three bytes for PNG — which has
/// no padding byte and no alpha in colour type 2. Going straight to three
/// would mean a second destination format in that module for one caller.
fn to_png(picture: &esper_color::I420, space: ColorSpace) -> Result<Vec<u8>, String> {
    let (w, h) = (picture.width(), picture.height());
    let stride = (w as usize)
        .checked_mul(4)
        .ok_or_else(|| format!("{w}x{h} is too large to convert"))?;
    let mut wide = vec![0u8; stride.saturating_mul(h as usize)];
    esper_color::rgb::whole_frame(
        picture,
        space,
        &mut wide,
        stride,
        esper_capture::frame::PixelFormat::Bgrx8,
    )
    .map_err(|e| format!("converting the picture: {e}"))?;

    let mut rgb = Vec::with_capacity((w as usize).saturating_mul(h as usize).saturating_mul(3));
    for row in wide.chunks_exact(stride) {
        // `as_chunks` rather than `chunks_exact`: it yields `&[u8; 4]`, so the
        // three channel reads below are provably in bounds and the row's
        // remainder is a value rather than something to remember to check.
        let (pixels, rest) = row.as_chunks::<4>();
        if !rest.is_empty() {
            return Err("a row that is not a whole number of pixels".to_owned());
        }
        for px in pixels {
            // Bgrx8: blue, green, red, and a fourth byte PNG has no room for.
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    esper_png::write_rgb8(w, h, &rgb).map_err(|e| format!("encoding the image: {e}"))
}

/// What to say when somebody asks, or gets it wrong.
#[must_use]
pub fn usage() -> String {
    let me = crate::net::me();
    format!(
        "usage: {me} screenshot [<address>|<name>] [options]\n\
         \n\
         One picture of a paired host's screen, as a PNG. Everything it says on\n\
         the way goes to stderr, so the image can go to stdout.\n\
         \n\
         options:\n  \
         \x20 -o, --out <path>     where to write it; `-` is stdout, and so is\n  \
         \x20                      leaving this out\n  \
         \x20 --monitor <id>       which screen, when the host has more than one\n  \
         \x20 --all                every screen, laid out the way the host has\n  \
         \x20                      them\n  \
         \x20 --via <addr>         a broker to be introduced through\n  \
         \x20 --broker-key <key>   the broker's key\n  \
         \x20 --no-mdns            do not ask the segment where the host is\n\
         \n\
         for example: {me} screenshot elythion -o desk.png\n\
         \x20            {me} screenshot elythion | wl-copy"
    )
}

/// Read the arguments.
///
/// # Errors
///
/// If a flag is unknown, or one that takes a value does not get one.
pub fn parse(args: &[String]) -> Result<ShotOptions, String> {
    let mut addr: Option<SocketAddr> = None;
    let mut named: Option<String> = None;
    let mut via_addr: Option<SocketAddr> = None;
    let mut via_key: Option<[u8; 32]> = None;
    let mut mdns = crate::settings::flag("discovery.mdns", true);
    let mut monitor: Option<u32> = None;
    let mut all = false;
    let mut out: Option<String> = None;
    let mut i = 0usize;
    // **No `--help` arm, deliberately.** `main` answers it before any parser
    // runs, on stdout with a zero status, because asking is a question rather
    // than an attempt — and no other parser here has one either. A second
    // place that answered would be a second answer to keep in step.
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--via" => {
                via_addr = Some(crate::net::parse_addr(
                    args.get(i + 1)
                        .ok_or_else(|| "--via needs the broker's address".to_owned())?,
                )?);
                i += 2;
            }
            "--broker-key" => {
                via_key = Some(crate::broker_cmd::parse_key(
                    args.get(i + 1)
                        .ok_or_else(|| "--broker-key needs the broker's key".to_owned())?,
                )?);
                i += 2;
            }
            "--no-mdns" => {
                mdns = false;
                i += 1;
            }
            "--all" => {
                all = true;
                i += 1;
            }
            "--monitor" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--monitor needs a screen id".to_owned())?;
                monitor = Some(
                    raw.parse::<u32>()
                        .map_err(|_| format!("--monitor wants a number, got {raw:?}"))?,
                );
                i += 2;
            }
            "--out" | "-o" => {
                out = Some(
                    args.get(i + 1)
                        .ok_or_else(|| "--out needs a path, or `-` for stdout".to_owned())?
                        .clone(),
                );
                i += 2;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other}\n\n{}", usage()));
            }
            other => {
                // An address if it parses as one, otherwise a paired host's
                // name — the same rule `connect` follows, so a person does not
                // have to remember which command wants which.
                match crate::net::parse_addr(other) {
                    Ok(a) => addr = Some(a),
                    Err(_) => named = Some(other.to_owned()),
                }
                i += 1;
            }
        }
    }
    // **Both is a confused intent, and refusing beats picking one.** A person
    // who asked for every screen and also for one of them wants something
    // this cannot do, and quietly honouring whichever the code happened to
    // check first is how somebody gets a picture of the wrong thing and does
    // not find out.
    if all && monitor.is_some() {
        return Err(format!(
            "--all and --monitor ask for different pictures: --all is every \
             screen laid out together, --monitor is one of them\n\n{}",
            usage()
        ));
    }
    // A broker on the command line replaces the configured one; without one,
    // whatever the settings say. The same rule `connect` follows.
    let via = match crate::session_cmd::broker_from(via_addr, via_key)? {
        Some(via) => Some(via),
        None => crate::settings::broker()?,
    };
    Ok(ShotOptions {
        addr,
        named,
        via,
        mdns,
        monitor,
        all,
        out,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn args(of: &[&str]) -> Vec<String> {
        of.iter().map(|a| (*a).to_owned()).collect()
    }

    /// **Asking for everything and for one thing is refused, not guessed at.**
    ///
    /// Honouring whichever the code checked first is how somebody gets a
    /// picture of the wrong screen and does not find out.
    #[test]
    fn every_screen_and_one_screen_are_not_asked_for_together() {
        let both = parse(&args(&["--all", "--monitor", "1"]));
        let message = both.expect_err("both must be refused");
        assert!(message.contains("--all and --monitor"), "{message}");
        // And either alone is fine.
        assert!(parse(&args(&["--all"])).is_ok());
        assert!(parse(&args(&["--monitor", "1"])).is_ok());
    }

    /// A screen id has to be a number, and a typo is named rather than
    /// silently becoming a host's name.
    #[test]
    fn a_screen_that_is_not_a_number_is_refused_by_name() {
        let e = parse(&args(&["--monitor", "left"])).expect_err("not a number");
        assert!(e.contains("left"), "{e}");
    }

    /// The output path is taken wherever it appears, and `-` means stdout —
    /// which is also what leaving it out means.
    #[test]
    fn the_output_path_is_read_from_either_form() {
        for form in [
            args(&["-o", "shot.png"]),
            args(&["--out", "shot.png"]),
            args(&["--all", "-o", "shot.png"]),
        ] {
            let o = parse(&form).expect("parsed");
            assert_eq!(o.out.as_deref(), Some("shot.png"), "{form:?}");
        }
        assert_eq!(parse(&args(&[])).expect("parsed").out, None);
        assert_eq!(
            parse(&args(&["-o", "-"])).expect("parsed").out.as_deref(),
            Some("-")
        );
    }
}
