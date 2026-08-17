//! Whether text at a given rung can still be read.
//!
//! # The gap this closes
//!
//! The degradation ladder is stated as "never below a legibility floor", and
//! the floor is **a rung** — 320 pixels on the long edge. Nothing measured
//! whether text at that rung can be read, which is the only thing the phrase
//! is actually about. A number of pixels is not a promise about reading; it is
//! a number somebody chose.
//!
//! # What legibility is, as something a machine can count
//!
//! Reading is telling one character from another. So the measurement is
//! **discriminability**: take a page of known text through the rung, and for
//! every glyph cell ask which of the ninety-five printable characters it now
//! most resembles. The fraction that still resemble themselves is the
//! recognition rate.
//!
//! The templates are put through **the same downscale** as the page, and that
//! choice is the whole design. A reader at a low rung is not comparing what
//! they see against crisp print — they are asking which character could have
//! produced this blur. If `e` and `c` collapse into the same handful of grey
//! pixels then no reader can tell them apart, and matching against downscaled
//! templates is what makes that show up as a confusion rather than as a
//! uniformly poor score against everything.
//!
//! Correlation rather than squared error, and each cell normalised first, so
//! the metric is not measuring contrast or brightness — both of which the
//! codec moves and neither of which stops somebody reading.
//!
//! # Two numbers, because there are two causes
//!
//! **Resampling** and **quantisation** both destroy text, and the fix for each
//! is a different rung of the ladder. So every measurement is taken twice: once
//! through the scaler alone, and once through the scaler and the codec. The
//! difference says which one the floor is actually set by, which is the thing
//! somebody tuning the ladder needs to know.

use esper_capture::Damage;
use esper_capture::frame::{Frame, PixelFormat};
use esper_capture::pool::Pixels;
use esper_color::I420;
use esper_dialog::font;
use esper_encode::encoder::{EncoderConfig, VideoEncoder};
use esper_encode::openh264::OpenH264;
use esper_scale::scaler::Scaler;

use crate::scenario::BenchError;

/// How many pixels one glyph is drawn at, before the rung.
///
/// Five by nine at scale two is a ten-by-eighteen cell, which is about what a
/// terminal on a 1080p screen actually uses.
const SCALE: usize = 2;

/// Margin around the page, so no glyph sits against an edge where the scaler's
/// span arithmetic is a special case.
const MARGIN: usize = 16;

/// Ink and paper, as a terminal has them: light text on a dark ground.
const PAPER: u8 = 24;
const INK: u8 = 220;

/// Where one character was drawn, and which one it was.
#[derive(Debug, Clone, Copy)]
struct Cell {
    x: usize,
    y: usize,
    ch: char,
}

/// What one rung did to a page of text.
#[derive(Debug, Clone, Copy)]
pub struct Legibility {
    /// The long edge this was measured at.
    pub long_edge: u32,
    /// Characters correctly identified after the scaler alone, 0.0 to 1.0.
    pub scaled: f64,
    /// And after the scaler and the codec.
    pub coded: f64,
}

/// The text the page is made of.
///
/// Every printable character appears, because a recognition rate over a page
/// of lowercase prose would say nothing about the characters that are hardest
/// to tell apart — and `1 l I`, `0 O` and `. ,` are exactly the ones a rung
/// destroys first and a person most needs.
fn corpus() -> Vec<String> {
    let mut lines = vec![
        "the quick brown fox jumps over the lazy dog".to_owned(),
        "THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG".to_owned(),
        "0123456789 il1I| oO0 .,;: '\"` -_= ()[]{}".to_owned(),
        "$ git commit -m \"fix: the rung was not measured\"".to_owned(),
        "!#%&*+/<>?@\\^~ mnrn cleo adgq bdpq vwy".to_owned(),
    ];
    // Every printable character, so nothing is scored on prose alone.
    let mut all = String::new();
    for c in (font::FIRST as u8)..=(font::LAST as u8) {
        all.push(char::from(c));
        if all.chars().count() == 48 {
            lines.push(std::mem::take(&mut all));
        }
    }
    if !all.is_empty() {
        lines.push(all);
    }
    lines
}

/// Draw one glyph into a luma buffer.
fn draw(luma: &mut [u8], stride: usize, height: usize, at: (usize, usize), ch: char) {
    let g = font::glyph(ch);
    for (row, bits) in g.iter().enumerate() {
        for col in 0..font::GLYPH_WIDTH {
            // Bit four is the leftmost pixel, so a row reads as it is written.
            let lit = bits & (1 << (font::GLYPH_WIDTH - 1 - col)) != 0;
            if !lit {
                continue;
            }
            for sy in 0..SCALE {
                for sx in 0..SCALE {
                    let x = at.0 + col * SCALE + sx;
                    let y = at.1 + row * SCALE + sy;
                    if x >= stride || y >= height {
                        continue;
                    }
                    if let Some(p) = luma.get_mut(y * stride + x) {
                        *p = INK;
                    }
                }
            }
        }
    }
}

/// Blank one glyph cell back to paper.
fn clear(luma: &mut [u8], stride: usize, height: usize, at: (usize, usize)) {
    for row in 0..font::GLYPH_HEIGHT * SCALE {
        for col in 0..font::ADVANCE * SCALE {
            let (x, y) = (at.0 + col, at.1 + row);
            if x >= stride || y >= height {
                continue;
            }
            if let Some(p) = luma.get_mut(y * stride + x) {
                *p = PAPER;
            }
        }
    }
}

/// A page of text, and where every character on it went.
fn page(width: u32, height: u32) -> (Vec<u8>, Vec<Cell>) {
    let (w, h) = (width as usize, height as usize);
    let mut luma = vec![PAPER; w * h];
    let mut cells = Vec::new();

    let advance = font::ADVANCE * SCALE;
    let line_height = (font::GLYPH_HEIGHT + 3) * SCALE;
    let lines = corpus();

    let mut y = MARGIN;
    let mut which = 0usize;
    // Fill the page, repeating the corpus, so the sample is the whole screen
    // rather than five lines at the top of one.
    while y + line_height + MARGIN <= h {
        let Some(line) = lines.get(which % lines.len()) else {
            break;
        };
        which += 1;
        let mut x = MARGIN;
        for ch in line.chars() {
            if x + advance + MARGIN > w {
                break;
            }
            if ch != ' ' {
                draw(&mut luma, w, h, (x, y), ch);
                cells.push(Cell { x, y, ch });
            }
            x += advance;
        }
        y += line_height;
    }
    (luma, cells)
}

/// A greyscale BGRA frame, which is what the product's scaler takes.
fn frame_from(luma: &[u8], width: u32, height: u32) -> Result<Frame, BenchError> {
    let stride = width as usize * 4;
    let mut f = Frame::new(
        width,
        height,
        stride,
        PixelFormat::Bgrx8,
        Damage::Everything,
        Pixels::zeroed(stride * height as usize),
    )
    .map_err(|e| BenchError::Failed(e.to_string()))?;
    for y in 0..height as usize {
        let Some(row) = f.row_mut(y as u32) else {
            continue;
        };
        for x in 0..width as usize {
            let v = luma.get(y * width as usize + x).copied().unwrap_or(PAPER);
            // Grey, so the luma out of the converter is the value put in and
            // the measurement is not also measuring a colour matrix.
            if let Some(px) = row.get_mut(x * 4..x * 4 + 4) {
                px.fill(v);
                if let Some(a) = px.get_mut(3) {
                    *a = 255;
                }
            }
        }
    }
    Ok(f)
}

/// Put a page through the scaler, and return the scaled luma.
fn scale_luma(luma: &[u8], from: (u32, u32), to: (u32, u32)) -> Result<Vec<u8>, BenchError> {
    if from == to {
        return Ok(luma.to_vec());
    }
    let src = frame_from(luma, from.0, from.1)?;
    let mut scaler = Scaler::new(from.0, from.1, to.0, to.1, PixelFormat::Bgrx8)
        .map_err(|e| BenchError::Failed(e.to_string()))?;
    let out = scaler
        .scale(&src, &Damage::Everything)
        .map_err(|e| BenchError::Failed(e.to_string()))?;
    let mut got = vec![PAPER; to.0 as usize * to.1 as usize];
    let bpp = out.format().bytes_per_pixel();
    for y in 0..to.1 as usize {
        let Some(row) = out.row(y as u32) else {
            continue;
        };
        for x in 0..to.0 as usize {
            if let (Some(dst), Some(px)) = (
                got.get_mut(y * to.0 as usize + x),
                row.get(x * bpp..x * bpp + bpp),
            ) {
                // Grey in, grey out: any channel is the value.
                *dst = px.first().copied().unwrap_or(PAPER);
            }
        }
    }
    Ok(got)
}

/// Normalised, so a comparison is about shape and not about contrast.
fn normalise(v: &[f64]) -> Option<Vec<f64>> {
    let n = v.len() as f64;
    if n == 0.0 {
        return None;
    }
    let mean = v.iter().sum::<f64>() / n;
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
    // A cell with no variation at all — a blank, or one the rung flattened
    // completely — correlates with nothing. Reported as unrecognised rather
    // than divided by zero, which is the honest answer: there is nothing there
    // to read.
    if var <= f64::EPSILON {
        return None;
    }
    let sd = var.sqrt();
    Some(v.iter().map(|x| (x - mean) / sd).collect())
}

/// Lift one glyph cell out of a luma plane, at the rung's scale.
fn patch(
    luma: &[u8],
    stride: usize,
    height: usize,
    at: (f64, f64),
    size: (usize, usize),
) -> Vec<f64> {
    let mut out = Vec::with_capacity(size.0 * size.1);
    for row in 0..size.1 {
        for col in 0..size.0 {
            let x = (at.0 + col as f64).round() as isize;
            let y = (at.1 + row as f64).round() as isize;
            let v = if x < 0 || y < 0 || x as usize >= stride || y as usize >= height {
                f64::from(PAPER)
            } else {
                f64::from(
                    luma.get(y as usize * stride + x as usize)
                        .copied()
                        .unwrap_or(PAPER),
                )
            };
            out.push(v);
        }
    }
    out
}

/// What fraction of the page's characters can still be told apart.
fn recognise(
    got: &[u8],
    at: (u32, u32),
    from: (u32, u32),
    cells: &[Cell],
    templates: &[(char, Vec<f64>)],
    cell: (usize, usize),
) -> f64 {
    let fx = f64::from(at.0) / f64::from(from.0);
    let fy = f64::from(at.1) / f64::from(from.1);
    let (w, h) = (at.0 as usize, at.1 as usize);

    let mut right = 0usize;
    let mut counted = 0usize;
    for c in cells {
        counted += 1;
        // **Nine offsets, and the best of them.** A glyph's position scaled by
        // a rung lands on a fractional pixel, and the fraction differs from
        // character to character — at 1920 to 720 the advance is four and a
        // half pixels, so every second glyph sits half a pixel off the one
        // before it. Matching at one fixed offset scores that as illegible,
        // which it is not: a reader is not troubled by where a glyph sits
        // relative to the pixel grid, and neither should this be. Without the
        // search, 720 measured *worse* than 480 — a result about the sampling
        // and not about the rung.
        let mut best: Option<(f64, char)> = None;
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                let at = (
                    c.x as f64 * fx + f64::from(dx),
                    c.y as f64 * fy + f64::from(dy),
                );
                let Some(here) = normalise(&patch(got, w, h, at, cell)) else {
                    continue;
                };
                for (ch, t) in templates {
                    let score: f64 = here.iter().zip(t).map(|(a, b)| a * b).sum();
                    if best.is_none_or(|(s, _)| score > s) {
                        best = Some((score, *ch));
                    }
                }
            }
        }
        if best.is_some_and(|(_, ch)| ch == c.ch) {
            right += 1;
        }
    }
    if counted == 0 {
        return 0.0;
    }
    right as f64 / counted as f64
}

/// The alphabet, drawn once each and put through the same rung as the page.
fn templates(
    from: (u32, u32),
    to: (u32, u32),
    cell: (usize, usize),
) -> Result<Vec<(char, Vec<f64>)>, BenchError> {
    // One glyph per tile, laid out on a page of the same size so the scaler's
    // span arithmetic falls the same way it does for the real page.
    let advance = font::ADVANCE * SCALE;
    let line_height = (font::GLYPH_HEIGHT + 3) * SCALE;
    let (w, h) = (from.0 as usize, from.1 as usize);
    let mut luma = vec![PAPER; w * h];
    let mut where_ = Vec::new();
    let (mut x, mut y) = (MARGIN, MARGIN);
    for c in (font::FIRST as u8)..=(font::LAST as u8) {
        let ch = char::from(c);
        if x + advance + MARGIN > w {
            x = MARGIN;
            y += line_height;
        }
        if y + line_height + MARGIN > h {
            break;
        }
        draw(&mut luma, w, h, (x, y), ch);
        where_.push(Cell { x, y, ch });
        x += advance;
    }

    let scaled = scale_luma(&luma, from, to)?;
    let fx = f64::from(to.0) / f64::from(from.0);
    let fy = f64::from(to.1) / f64::from(from.1);
    let mut out = Vec::new();
    for c in &where_ {
        let p = patch(
            &scaled,
            to.0 as usize,
            to.1 as usize,
            (c.x as f64 * fx, c.y as f64 * fy),
            cell,
        );
        // A character the rung flattened to nothing is not a template anybody
        // can match against, and leaving it in would let every flattened cell
        // match the first flat template and score as correct.
        if let Some(n) = normalise(&p) {
            out.push((c.ch, n));
        }
    }
    Ok(out)
}

/// Measure one rung.
///
/// # Errors
///
/// If the scaler, encoder or decoder will not start.
pub fn at_rung(from: (u32, u32), long_edge: u32, qp: (u8, u8)) -> Result<Legibility, BenchError> {
    let (fw, fh) = from;
    let to = if long_edge >= fw {
        (fw, fh)
    } else {
        let h = (u64::from(long_edge) * u64::from(fh) / u64::from(fw)) as u32;
        (
            esper_scale::ceiling::even(long_edge),
            esper_scale::ceiling::even(h.max(2)),
        )
    };

    let (luma, cells) = page(fw, fh);
    // **The whole advance, not the glyph's own width.** A cell narrower than
    // the advance clips the right-hand side of every glyph, and it clips a
    // different fraction at each rung — which showed up as 640 scoring worse
    // than 480, a result about the sampling window rather than about the rung.
    // Rounded up for the same reason.
    let cell = {
        let fx = f64::from(to.0) / f64::from(fw);
        let fy = f64::from(to.1) / f64::from(fh);
        (
            ((font::ADVANCE * SCALE) as f64 * fx).ceil().max(1.0) as usize,
            ((font::GLYPH_HEIGHT * SCALE) as f64 * fy).ceil().max(1.0) as usize,
        )
    };

    let refs = templates(from, to, cell)?;
    let scaled = scale_luma(&luma, from, to)?;
    let scaled_rate = recognise(&scaled, to, from, &cells, &refs, cell);

    let coded = through_codec(&scaled, to, qp)?;
    let coded_rate = recognise(&coded, to, from, &cells, &refs, cell);

    Ok(Legibility {
        long_edge: to.0,
        scaled: scaled_rate,
        coded: coded_rate,
    })
}

/// Encode a luma plane and decode it again, returning what came back.
fn through_codec(luma: &[u8], size: (u32, u32), qp: (u8, u8)) -> Result<Vec<u8>, BenchError> {
    let (w, h) = size;
    let mut picture = I420::new(w, h).map_err(|e| BenchError::Failed(e.to_string()))?;
    {
        let (y, u, v) = picture.planes_mut();
        y.copy_from_slice(luma);
        u.fill(128);
        v.fill(128);
    }

    let config = EncoderConfig {
        width: w,
        height: h,
        fps: 30,
        bitrate: 2_000_000,
        keyframe_interval: std::time::Duration::from_secs(5),
        qp: Some(qp),
        ..EncoderConfig::default()
    };
    let mut encoder = OpenH264::new(&config)?;
    use esper_decode::decoder::VideoDecoder as _;
    let mut decoder = esper_decode::openh264::OpenH264Decoder::new(
        &esper_decode::decoder::DecoderConfig::default(),
    )
    .map_err(|e| BenchError::Failed(e.to_string()))?;

    // **Two frames, and the second is the one that is read.** The first is a
    // keyframe of a page nobody has seen, which is the most expensive and
    // least representative frame of a reading session. What somebody actually
    // looks at is a page that has been on the screen for a while.
    let mut last: Option<Vec<u8>> = None;
    for i in 0..2u32 {
        let coded = encoder
            .encode(&picture, i == 0)
            .map_err(|e| BenchError::Failed(e.to_string()))?;
        if let Some(out) = decoder
            .decode(&coded.bitstream)
            .map_err(|e| BenchError::Failed(e.to_string()))?
        {
            last = Some(out.y().to_vec());
        }
    }
    last.ok_or_else(|| BenchError::Failed("the decoder returned no picture".to_owned()))
}

/// What the matcher scores on a page that is not text at all.
///
/// **The other half of the control.** A recognition rate near one at full
/// resolution shows the matcher can succeed; this shows it can fail. Without
/// it, a matcher with a bug that returned the true character regardless would
/// produce a table of perfect scores and a very convincing report.
///
/// Chance is one in ninety-five, so anything much above about 5% means the
/// matcher is finding structure in noise and the numbers above are not what
/// they claim.
///
/// # Errors
///
/// If the scaler will not start.
pub fn against_noise(from: (u32, u32)) -> Result<f64, BenchError> {
    let (fw, fh) = from;
    let (_, cells) = page(fw, fh);
    let cell = (font::ADVANCE * SCALE, font::GLYPH_HEIGHT * SCALE);
    let refs = templates(from, from, cell)?;

    // A deterministic pattern with no glyphs in it. Not `rand`: a benchmark
    // that reports a different number each run is a benchmark nobody can
    // compare, and this one is a guard whose value should never move.
    let mut noise = vec![PAPER; fw as usize * fh as usize];
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    for p in &mut noise {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *p = (state >> 32) as u8;
    }
    Ok(recognise(&noise, from, from, &cells, &refs, cell))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// A page this size keeps the test quick; the measurement itself runs at
    /// 1080p, where a rung means what it means on a real screen.
    const SMALL: (u32, u32) = (640, 360);

    /// **The metric can succeed.** Clean text at full resolution must be read
    /// almost perfectly, or every number this module produces is a number
    /// about the matcher rather than about the rung.
    #[test]
    fn clean_text_is_read_almost_perfectly() {
        let got = at_rung(
            SMALL,
            SMALL.0,
            esper_encode::budget::Fidelity::High.qp_range(),
        )
        .expect("the encoder and scaler start");
        assert!(
            got.scaled >= 0.95,
            "unscaled text scored {:.0}%, so the matcher cannot read text it has not damaged",
            got.scaled * 100.0
        );
    }

    /// **And it can fail.** Without this, a matcher that returned the true
    /// character regardless would produce a table of perfect scores and a very
    /// convincing report. Chance is one in ninety-five.
    #[test]
    fn noise_is_read_at_chance() {
        let rate = against_noise(SMALL).expect("the scaler starts");
        assert!(
            rate <= 0.05,
            "a page of noise scored {:.0}%, which is far above chance — the matcher is \
             finding structure that is not there",
            rate * 100.0
        );
    }

    /// **A rung that destroys the text has to show up as destroying it.** The
    /// two tests above bound the metric from either end and would both pass on
    /// a matcher that scored everything perfectly except pure noise — blurred
    /// text is a very different input from noise.
    ///
    /// An eighth, where a ten-pixel glyph becomes one pixel and a bit. A
    /// *quarter* is deliberately not the case tested: 1920 to 480 measures 97%,
    /// which was a surprise worth keeping — text survives a quarter far better
    /// than it sounds like it should, and an assertion written from intuition
    /// rather than from the measurement would have failed here for the wrong
    /// reason. It did.
    #[test]
    fn a_rung_far_below_the_text_is_not_read() {
        let got = at_rung(SMALL, 80, esper_encode::budget::Fidelity::High.qp_range())
            .expect("the encoder and scaler start");
        assert!(
            got.scaled < 0.5,
            "text at an eighth of its size scored {:.0}%, which is not credible",
            got.scaled * 100.0
        );
    }
}

/// What a reading session costs, and whether it is still readable.
///
/// # The claim being tested
///
/// "Terminal text is sharp at 1 Mbit/s." That is two claims — a *rate* and a
/// *quality* — and either alone is easy: any bitrate is achievable if the text
/// may be destroyed, and any quality is achievable if the rate may be
/// anything. So both are measured on the same run.
///
/// # What a reading session is
///
/// A full page of text that does not move, with somebody typing into it: one
/// glyph cell changes per frame. That is deliberately the *expensive* end of
/// reading — a person reading rather than typing changes nothing at all for
/// seconds at a time, and the damage gate then sends nothing. A number that
/// held only for a screen nobody was touching would not be a number about
/// using a terminal.
///
/// The keyframe is excluded from the rate and reported separately. It is a
/// one-off cost that a session pays when it opens, and folding it into a
/// per-second figure would make a thirty-second session look twice as
/// expensive as a five-minute one.
///
/// # Errors
///
/// If the encoder or decoder will not start.
pub fn reading_session(
    from: (u32, u32),
    qp: (u8, u8),
    fps: u32,
    seconds: u32,
) -> Result<Reading, BenchError> {
    let (w, h) = from;
    let (luma, cells) = page(w, h);
    let cell = (font::ADVANCE * SCALE, font::GLYPH_HEIGHT * SCALE);
    let refs = templates(from, from, cell)?;

    let mut picture = I420::new(w, h).map_err(|e| BenchError::Failed(e.to_string()))?;
    let config = EncoderConfig {
        width: w,
        height: h,
        fps,
        bitrate: 1_000_000,
        keyframe_interval: std::time::Duration::from_secs(5),
        qp: Some(qp),
        ..EncoderConfig::default()
    };
    let mut encoder = OpenH264::new(&config)?;
    use esper_decode::decoder::VideoDecoder as _;
    let mut decoder = esper_decode::openh264::OpenH264Decoder::new(
        &esper_decode::decoder::DecoderConfig::default(),
    )
    .map_err(|e| BenchError::Failed(e.to_string()))?;

    let frames = fps.saturating_mul(seconds).max(2);
    let mut working = luma.clone();
    let mut keyframe_bytes = 0u64;
    let mut bytes = 0u64;
    let mut last = None;

    // A cursor walking the text, and putting back what it stood on.
    //
    // **It has to clean up after itself**, and the first version did not: it
    // drew a block over a glyph and moved on, leaving a trail of cells with
    // two characters superimposed. Those are genuinely unreadable, so the
    // measurement scored 96% and the missing 4% was the instrument's own
    // graffiti rather than anything the codec did.
    let mut cursor_was: Option<Cell> = None;
    for i in 0..frames {
        if let Some(c) = cursor_was.take() {
            clear(&mut working, w as usize, h as usize, (c.x, c.y));
            draw(&mut working, w as usize, h as usize, (c.x, c.y), c.ch);
        }
        if let Some(c) = cells.get((i as usize).wrapping_mul(7) % cells.len().max(1)) {
            clear(&mut working, w as usize, h as usize, (c.x, c.y));
            draw(&mut working, w as usize, h as usize, (c.x, c.y), '#');
            cursor_was = Some(*c);
        }
        {
            let (y, u, v) = picture.planes_mut();
            y.copy_from_slice(&working);
            u.fill(128);
            v.fill(128);
        }
        let coded = encoder
            .encode(&picture, i == 0)
            .map_err(|e| BenchError::Failed(e.to_string()))?;
        if i == 0 {
            keyframe_bytes = coded.bitstream.len() as u64;
        } else {
            bytes = bytes.saturating_add(coded.bitstream.len() as u64);
        }
        if let Some(out) = decoder
            .decode(&coded.bitstream)
            .map_err(|e| BenchError::Failed(e.to_string()))?
        {
            last = Some(out.y().to_vec());
        }
    }

    // One final frame with the cursor lifted, so what is read back is the page
    // rather than the page with a block on it.
    if let Some(c) = cursor_was.take() {
        clear(&mut working, w as usize, h as usize, (c.x, c.y));
        draw(&mut working, w as usize, h as usize, (c.x, c.y), c.ch);
        {
            let (y, u, v) = picture.planes_mut();
            y.copy_from_slice(&working);
            u.fill(128);
            v.fill(128);
        }
        let coded = encoder
            .encode(&picture, false)
            .map_err(|e| BenchError::Failed(e.to_string()))?;
        if let Some(out) = decoder
            .decode(&coded.bitstream)
            .map_err(|e| BenchError::Failed(e.to_string()))?
        {
            last = Some(out.y().to_vec());
        }
    }

    let got =
        last.ok_or_else(|| BenchError::Failed("the decoder returned no picture".to_owned()))?;
    // Against the *original* page: what a reader is trying to read is the text
    // that was typed, not the last frame's idea of it.
    let read = recognise(&got, from, from, &cells, &refs, cell);

    let secs = f64::from(frames.saturating_sub(1)) / f64::from(fps.max(1));
    Ok(Reading {
        kbit: if secs > 0.0 {
            bytes as f64 * 8.0 / secs / 1000.0
        } else {
            0.0
        },
        keyframe_kbytes: keyframe_bytes as f64 / 1000.0,
        legible: read,
    })
}

/// What a reading session cost, and whether it stayed readable.
#[derive(Debug, Clone, Copy)]
pub struct Reading {
    /// Steady-state bitrate, excluding the opening keyframe.
    pub kbit: f64,
    /// What the opening keyframe cost, once.
    pub keyframe_kbytes: f64,
    /// Characters still correctly told apart, 0.0 to 1.0.
    pub legible: f64,
}
