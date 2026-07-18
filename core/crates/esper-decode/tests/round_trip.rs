#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! The synthetic pattern, all the way through and back.
//!
//! Capture → convert → encode → decode, and then check the picture against
//! what the pattern says it should be. This is what every band of that
//! pattern was built for, and it is the first test in the project that can
//! catch a mistake anywhere in the media pipeline rather than in one stage of
//! it.
//!
//! What each check is for:
//!
//! | Check | Catches |
//! |---|---|
//! | The frame counter reads back | a dropped, repeated or reordered picture — and it is how latency gets measured later with no second clock |
//! | The corners are the right colours | a vertical flip, a horizontal flip, a rotation, a crop |
//! | The colour bars are in order | red and blue swapped anywhere in the chain, or a colour matrix disagreement between the two ends |
//! | Luma PSNR | the whole thing being subtly wrong in a way no single pixel reveals |
//! | Dimensions survive | a resolution the decoder invented |

use esper_capture::damage::Damage;
use esper_capture::frame::{Frame, PixelFormat};
use esper_capture::pattern::Pattern;
use esper_capture::pool::Pixels;
use esper_color::convert::{reference_yuv, whole_frame};
use esper_color::{ColorSpace, I420};
use esper_decode::decoder::{DecoderConfig, VideoDecoder};
use esper_decode::openh264::OpenH264Decoder;
use esper_encode::encoder::{EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;

/// Render the pattern into a frame, the way a capture backend would.
fn captured(pattern: &Pattern, index: u64) -> Frame {
    let (w, h) = (pattern.width(), pattern.height());
    let format = PixelFormat::Bgrx8;
    let bpp = format.bytes_per_pixel();
    // A padded stride, because no real backend is tightly packed.
    let stride = (w as usize) * bpp + 64;
    let mut buf = vec![0u8; stride * h as usize];
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = pattern.pixel(x, y, index);
            let at = y as usize * stride + x as usize * bpp;
            buf[at] = b;
            buf[at + 1] = g;
            buf[at + 2] = r;
        }
    }
    Frame::new(w, h, stride, format, Damage::Everything, Pixels::owned(buf)).expect("a valid frame")
}

/// One end-to-end pass.
struct Pipeline {
    pattern: Pattern,
    space: ColorSpace,
    encoder: OpenH264,
    decoder: OpenH264Decoder,
    staging: I420,
}

impl Pipeline {
    fn new(w: u32, h: u32) -> Self {
        let pattern = Pattern::new(w, h).expect("a valid size");
        let space = ColorSpace::default();
        // **The encoder is configured from the buffer, not from the screen.**
        // A 321x241 window becomes a 322x242 I420, because 4:2:0 has no odd
        // dimension, and the encoder refuses an odd size outright. Passing it
        // the screen's dimensions is a real mistake with an obvious symptom —
        // the encoder simply will not start — and it is the sort of thing that
        // only shows up on the one user whose compositor hands out odd sizes.
        let staging = I420::new(w, h).expect("valid");
        let encoder = OpenH264::new(&EncoderConfig {
            fps: 30,
            ..EncoderConfig::for_size(staging.width(), staging.height())
        })
        .expect("encoder");
        let decoder = OpenH264Decoder::new(&DecoderConfig::default()).expect("decoder");
        Self {
            pattern,
            space,
            encoder,
            decoder,
            staging,
        }
    }

    /// Push one frame through and get the decoded picture back.
    fn pass(&mut self, index: u64, keyframe: bool) -> I420 {
        let frame = captured(&self.pattern, index);
        let _ = whole_frame(&frame, self.space, &mut self.staging).expect("converted");
        let encoded = self
            .encoder
            .encode(&self.staging, keyframe)
            .expect("encoded");
        assert!(!encoded.is_empty(), "frame {index} encoded to nothing");

        // Annex-B: one call with the whole access unit, which is what the
        // encoder hands over and what a packetiser will later split up.
        match self.decoder.decode(&encoded.bitstream) {
            Ok(Some(picture)) => picture,
            Ok(None) => panic!("frame {index} decoded to no picture at all"),
            Err(e) => panic!("frame {index} failed to decode: {e}"),
        }
    }
}

/// Read `[r, g, b]` back out of a decoded picture, inverting the conversion.
fn rgb_at(picture: &I420, x: u32, y: u32, space: ColorSpace) -> [u8; 3] {
    let y_sample = picture.luma_at(x, y).unwrap_or(0);
    let (u, v) = picture.chroma_at(x, y).unwrap_or((128, 128));
    let (r, g, b) = esper_color::convert::reference_rgb(y_sample, u, v, space);
    [r, g, b]
}

/// Luma peak signal-to-noise ratio between what was sent and what came back.
///
/// The single number that catches "subtly wrong everywhere" — a slightly wrong
/// matrix, an off-by-one in a stride, a rounding bias — none of which any
/// individual pixel check would notice.
fn luma_psnr(pattern: &Pattern, index: u64, picture: &I420, space: ColorSpace) -> f64 {
    let (w, h) = (pattern.width(), pattern.height());
    let mut sum_sq = 0f64;
    let mut n = 0u64;
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = pattern.pixel(x, y, index);
            let (want, _, _) = reference_yuv(r, g, b, space);
            let got = picture.luma_at(x, y).unwrap_or(0);
            let d = f64::from(i32::from(got) - i32::from(want));
            sum_sq += d * d;
            n += 1;
        }
    }
    if n == 0 {
        return 0.0;
    }
    let mse = sum_sq / n as f64;
    if mse <= 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

/// **The Phase 3 round trip.** The pattern survives capture, conversion,
/// encode and decode.
#[test]
fn the_synthetic_pattern_survives_the_round_trip() {
    let mut p = Pipeline::new(640, 480);
    let space = p.space;

    // A keyframe first, then deltas — the shape a real stream has.
    for index in 0..8u64 {
        let picture = p.pass(index, index == 0);

        assert_eq!(
            (picture.width(), picture.height()),
            (640, 480),
            "frame {index}: the decoder invented a resolution"
        );

        // **Which frame is this?** Read out of the pixels, which is how
        // glass-to-glass latency gets measured later without a second clock.
        let read = p.pattern.read_counter(|x, y| rgb_at(&picture, x, y, space));
        assert_eq!(
            read, index,
            "frame {index} came back saying it was frame {read} — a dropped, \
             repeated or reordered picture"
        );

        // **Which way up is it?** Four different colours, so a vertical flip,
        // a horizontal flip and a rotation each give a distinguishable wrong
        // answer.
        let corners = [
            ((0u32, 0u32), [255u8, 0, 0], "top left, red"),
            ((639, 0), [0, 255, 0], "top right, green"),
            ((639, 479), [0, 0, 255], "bottom right, blue"),
            ((0, 479), [255, 255, 0], "bottom left, yellow"),
        ];
        for ((x, y), want, which) in corners {
            let got = rgb_at(&picture, x, y, space);
            let off: i32 = (0..3)
                .map(|c| (i32::from(got[c]) - i32::from(want[c])).abs())
                .max()
                .unwrap_or(0);
            assert!(
                off < 90,
                "frame {index}: the {which} corner came back {got:?} instead \
                 of {want:?} — the picture is flipped, rotated or cropped"
            );
        }

        // **Are red and blue the right way round?** The bars are every
        // combination of full channels, so a swap exchanges yellow with cyan
        // and red with blue, side by side.
        let bar_y = 3;
        for (bar, want) in [
            (1u32, [255u8, 255, 0]), // yellow
            (2, [0, 255, 255]),      // cyan
            (5, [255, 0, 0]),        // red
            (6, [0, 0, 255]),        // blue
        ] {
            let x = bar * 80 + 40;
            let got = rgb_at(&picture, x, bar_y, space);
            // Which channels are strong, rather than exact values: a lossy
            // codec moves the numbers, but it does not swap which channel is
            // bright.
            let strong = |v: u8| v > 110;
            let want_strong: Vec<bool> = want.iter().map(|v| strong(*v)).collect();
            let got_strong: Vec<bool> = got.iter().map(|v| strong(*v)).collect();
            assert_eq!(
                got_strong, want_strong,
                "frame {index}: bar {bar} came back {got:?}, expected roughly \
                 {want:?} — red and blue are swapped somewhere in the chain"
            );
        }

        // **Is it subtly wrong everywhere?** The number no single pixel check
        // would notice.
        let psnr = luma_psnr(&p.pattern, index, &picture, space);
        let floor = if index == 0 { 26.0 } else { 24.0 };
        assert!(
            psnr > floor,
            "frame {index}: luma PSNR is {psnr:.1} dB, under {floor} — the \
             picture is wrong in a way no single pixel reveals. The pattern is \
             deliberately hostile to a codec (one-pixel stripes, hard edges), \
             so this floor is low; falling under it means something is broken, \
             not merely lossy"
        );
    }
}

/// A keyframe alone must be decodable, because that is what a late-joining
/// viewer and a recovering decoder get.
#[test]
fn a_keyframe_alone_decodes() {
    let mut p = Pipeline::new(320, 240);
    let picture = p.pass(0, true);
    assert_eq!((picture.width(), picture.height()), (320, 240));
    let read = p
        .pattern
        .read_counter(|x, y| rgb_at(&picture, x, y, p.space));
    assert_eq!(read, 0);
}

/// Odd dimensions all the way through. A compositor will hand out a 1919x1079
/// window, the converter rounds up, and the decoder has to give back something
/// the viewer can use.
#[test]
fn an_odd_sized_screen_survives_the_round_trip() {
    for (w, h) in [(321u32, 241u32), (639, 479)] {
        let mut p = Pipeline::new(w, h);
        let picture = p.pass(0, true);
        // The encoder works in even dimensions, so that is what comes back.
        assert_eq!(
            (picture.width(), picture.height()),
            ((w + 1) & !1, (h + 1) & !1),
            "{w}x{h}"
        );
        let read = p
            .pattern
            .read_counter(|x, y| rgb_at(&picture, x, y, p.space));
        assert_eq!(read, 0, "{w}x{h}: the counter did not survive");
    }
}

/// Several sizes, because a stride mistake shows at some widths and not others.
#[test]
fn the_round_trip_works_at_several_sizes() {
    for (w, h) in [(64u32, 48u32), (320, 240), (854, 480), (1280, 720)] {
        let mut p = Pipeline::new(w, h);
        let picture = p.pass(0, true);
        assert_eq!((picture.width(), picture.height()), (w, h), "{w}x{h}");

        let psnr = luma_psnr(&p.pattern, 0, &picture, p.space);
        assert!(psnr > 20.0, "{w}x{h}: luma PSNR is only {psnr:.1} dB");
        // The corners are the cheapest orientation check and they must hold
        // at every size.
        let tl = rgb_at(&picture, 0, 0, p.space);
        assert!(
            tl[0] > tl[2],
            "{w}x{h}: the top-left corner is {tl:?}, and red should dominate \
             blue there — the picture is flipped or the channels are swapped"
        );
    }
}

/// A resolution change mid-stream. Every reference the decoder holds becomes
/// the wrong size, which is why the encoder forces a keyframe — and this is
/// the test that the decoder actually follows.
#[test]
fn a_resolution_change_mid_stream_is_followed() {
    let mut p = Pipeline::new(320, 240);
    let first = p.pass(0, true);
    assert_eq!((first.width(), first.height()), (320, 240));

    // Reconfigure both the pattern and the encoder, as a monitor change would.
    p.pattern = Pattern::new(640, 480).expect("valid");
    p.staging = I420::new(640, 480).expect("valid");
    p.encoder
        .reconfigure(&EncoderConfig {
            fps: 30,
            ..EncoderConfig::for_size(p.staging.width(), p.staging.height())
        })
        .expect("reconfigured");

    let after = p.pass(1, false);
    assert_eq!(
        (after.width(), after.height()),
        (640, 480),
        "the decoder did not follow the resolution change"
    );
    let read = p.pattern.read_counter(|x, y| rgb_at(&after, x, y, p.space));
    assert_eq!(read, 1, "the counter did not survive the change");
}

/// The statistics have to describe what happened, because they are what the
/// session HUD shows.
#[test]
fn the_decoder_reports_what_it_did() {
    let mut p = Pipeline::new(320, 240);
    for i in 0..5u64 {
        let _ = p.pass(i, i == 0);
    }
    let s = p.decoder.stats();
    assert_eq!(s.pictures, 5);
    assert_eq!(s.packets, 5);
    assert!(s.bytes > 0);
    assert_eq!(s.refused, 0, "nothing legitimate should have been refused");
    assert!(s.mean_ms() > 0.0);
}
