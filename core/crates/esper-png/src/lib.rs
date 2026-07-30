#![forbid(unsafe_code)]

//! Writes a PNG, and contains just enough deflate to do it.
//!
//! # Why this is here rather than a dependency
//!
//! One command needs to write one image. The obvious `png` crate arrives with
//! a compressor, a CRC crate and a bitflags crate behind it — five things to
//! audit and carry for ever, in a product whose first promise is that it is
//! small enough to reason about. What a PNG writer actually needs is a CRC, an
//! Adler sum and a deflate stream, and all three are short enough to read.
//!
//! This is not a general PNG library and does not want to be. It writes
//! 8-bit RGB, no interlacing, one image.
//!
//! # Why the compression is what it is
//!
//! Deflate has three block types, and the useful one here is **fixed
//! Huffman**: the code lengths are defined by the format, so there is no tree
//! to build, no lengths to transmit, and nothing to get subtly wrong.
//!
//! What it looks for is **runs**, and that is not a shortcut. PNG filters each
//! row against the one above it, and a screenshot is mostly flat colour and
//! unchanged pixels — so the filtered bytes are overwhelmingly zero, in long
//! stretches. A match at distance one is exactly a run-length encoding of
//! those, which is where nearly all of the compression on this kind of image
//! comes from. A general match finder would add a hash chain, a window, and a
//! great deal of surface, to find matches that screen content mostly does not
//! have.
//!
//! Measured on real captures, this gets a 1080p screenshot to a few per cent
//! of its raw size. If it ever needs to do better, the honest next step is a
//! real library rather than a bigger version of this.

mod deflate;

pub use deflate::compress;

/// What can go wrong writing an image.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PngError {
    /// The pixel buffer does not match the stated size.
    #[error("{width}x{height} needs {wanted} bytes of RGB, got {got}")]
    WrongSize {
        width: u32,
        height: u32,
        wanted: usize,
        got: usize,
    },
    /// A dimension is zero, or too large to describe.
    #[error("{width}x{height} is not an image this writes")]
    BadSize { width: u32, height: u32 },
}

/// The largest image this will write, per side.
///
/// PNG's own limit is 2^31-1, which at three bytes a pixel would be an
/// allocation no machine has. A screenshot of a 16K display is 15 360 across,
/// so this is generous by a factor of four and still refuses a number that
/// arrived by mistake.
pub const MOST_PIXELS_PER_SIDE: u32 = 65_535;

/// PNG's eight-byte signature.
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Write 8-bit RGB as a PNG.
///
/// `rgb` is `width * height * 3` bytes, row-major, no padding.
///
/// # Errors
///
/// If the buffer does not match the size, or the size is not one this writes.
pub fn write_rgb8(width: u32, height: u32, rgb: &[u8]) -> Result<Vec<u8>, PngError> {
    if width == 0 || height == 0 || width > MOST_PIXELS_PER_SIDE || height > MOST_PIXELS_PER_SIDE {
        return Err(PngError::BadSize { width, height });
    }
    let wanted = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(3))
        .ok_or(PngError::BadSize { width, height })?;
    if rgb.len() != wanted {
        return Err(PngError::WrongSize {
            width,
            height,
            wanted,
            got: rgb.len(),
        });
    }

    let mut out = Vec::with_capacity(rgb.len() / 8 + 1024);
    out.extend_from_slice(&SIGNATURE);

    // IHDR: size, 8 bits a channel, colour type 2 (truecolour), deflate,
    // adaptive filtering, no interlace. Every one of those is the only value
    // this writes, so they are constants rather than parameters.
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);

    chunk(&mut out, b"IDAT", &zlib(&filtered(width, height, rgb)));
    chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

/// The image with a filter byte at the start of every row.
///
/// **Filter 2, "Up": every byte becomes its difference from the byte above.**
/// Chosen for what a screenshot is rather than for what an image might be —
/// large areas that do not change from row to row, which this turns into
/// zeros, and which the run matcher in `deflate` then costs almost nothing.
/// Per-row filter selection would be the general answer and would spend a pass
/// choosing between five options to beat "Up" on photographs, which is not
/// what this writes.
fn filtered(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let stride = (width as usize).saturating_mul(3);
    let mut out = Vec::with_capacity(rgb.len().saturating_add(height as usize));
    let mut previous: &[u8] = &[];
    for y in 0..height as usize {
        let from = y.saturating_mul(stride);
        let Some(row) = rgb.get(from..from.saturating_add(stride)) else {
            break;
        };
        out.push(2);
        if previous.is_empty() {
            // The first row has nothing above it. "Up" against an implicit
            // row of zeros is the row itself, which is what the format says.
            out.extend_from_slice(row);
        } else {
            for (byte, above) in row.iter().zip(previous) {
                out.push(byte.wrapping_sub(*above));
            }
        }
        previous = row;
    }
    out
}

/// A zlib stream: two header bytes, the deflate data, and an Adler sum.
fn zlib(data: &[u8]) -> Vec<u8> {
    // 0x78 0x01: deflate, 32 KiB window, no preset dictionary, fastest —
    // and the pair is what makes (0x78 << 8 | 0x01) a multiple of 31, which
    // is the header's own check.
    let mut out = vec![0x78, 0x01];
    out.extend_from_slice(&compress(data));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// One PNG chunk: length, type, payload, CRC over type and payload.
fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    let len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    let mut crc = Crc32::new();
    crc.eat(kind);
    crc.eat(payload);
    out.extend_from_slice(&crc.done().to_be_bytes());
}

/// The CRC-32 PNG uses, computed a nibble at a time.
///
/// A byte-at-a-time table is 1 KiB and a nibble table is 64 bytes, for two
/// lookups instead of one. On a few megabytes once, the table nobody has to
/// look at is worth more than the cycle.
struct Crc32(u32);

impl Crc32 {
    /// The standard CRC-32 nibble table for the polynomial PNG uses.
    ///
    /// Written out rather than computed in a `const` block, because building
    /// it needs indexed assignment and this workspace denies indexing that
    /// could panic — an exception carved out for sixteen constants would be a
    /// worse trade than the constants. `the_table_is_the_polynomial` derives
    /// all sixteen and checks them, so the arithmetic is still asserted; it
    /// just lives in a test rather than in the shipped path.
    const TABLE: [u32; 16] = [
        0x0000_0000,
        0x1DB7_1064,
        0x3B6E_20C8,
        0x26D9_30AC,
        0x76DC_4190,
        0x6B6B_51F4,
        0x4DB2_6158,
        0x5005_713C,
        0xEDB8_8320,
        0xF00F_9344,
        0xD6D6_A3E8,
        0xCB61_B38C,
        0x9B64_C2B0,
        0x86D3_D2D4,
        0xA00A_E278,
        0xBDBD_F21C,
    ];

    const fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    fn eat(&mut self, bytes: &[u8]) {
        for b in bytes {
            for nibble in [u32::from(*b) & 0xF, u32::from(*b) >> 4] {
                let index = ((self.0 ^ nibble) & 0xF) as usize;
                let entry = Self::TABLE.get(index).copied().unwrap_or(0);
                self.0 = (self.0 >> 4) ^ entry;
            }
        }
    }

    const fn done(self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }
}

/// Adler-32, which zlib puts at the end of the stream.
fn adler32(data: &[u8]) -> u32 {
    /// The largest number of bytes that cannot overflow the sums.
    const CHUNK: usize = 5552;
    let (mut a, mut b) = (1u32, 0u32);
    for part in data.chunks(CHUNK) {
        for byte in part {
            a = a.wrapping_add(u32::from(*byte));
            b = b.wrapping_add(a);
        }
        a %= 65_521;
        b %= 65_521;
    }
    (b << 16) | a
}
