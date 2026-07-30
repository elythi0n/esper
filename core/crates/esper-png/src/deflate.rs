//! Just enough deflate: fixed Huffman, literals and runs.
//!
//! # The format, in the part that is used
//!
//! A deflate stream is a sequence of blocks. This writes one, with the
//! **fixed** Huffman codes — the ones RFC 1951 defines outright, so there is
//! no tree to build and none to transmit:
//!
//! | symbol | bits | code |
//! |---|---|---|
//! | 0–143 | 8 | `00110000`–`10111111` |
//! | 144–255 | 9 | `110010000`–`111111111` |
//! | 256–279 | 7 | `0000000`–`0010111` |
//! | 280–287 | 8 | `11000000`–`11000111` |
//!
//! Symbols under 256 are literal bytes, 256 ends the block, and 257–285 are
//! *lengths* — each followed by a distance code, which together say "repeat
//! the bytes that were this far back, this many times".
//!
//! Huffman codes go out **most-significant bit first**, and everything else in
//! deflate goes out least-significant bit first. Getting that backwards
//! produces a stream that looks plausible and inflates to rubbish, so the two
//! are separate functions here and neither is used for the other's job.

/// The bit sink. Deflate packs least-significant bit first within a byte.
struct Bits {
    out: Vec<u8>,
    /// Bits not yet flushed, in the low end of `hold`.
    hold: u32,
    used: u32,
}

impl Bits {
    fn new(capacity: usize) -> Self {
        Self {
            out: Vec::with_capacity(capacity),
            hold: 0,
            used: 0,
        }
    }

    /// Write `count` bits, least-significant first. For everything that is not
    /// a Huffman code.
    fn low_first(&mut self, value: u32, count: u32) {
        self.hold |= (value & ((1u32 << count) - 1)) << self.used;
        self.used = self.used.saturating_add(count);
        while self.used >= 8 {
            self.out.push((self.hold & 0xFF) as u8);
            self.hold >>= 8;
            self.used -= 8;
        }
    }

    /// Write a Huffman code, which is defined most-significant bit first.
    fn code(&mut self, value: u32, count: u32) {
        for i in (0..count).rev() {
            self.low_first((value >> i) & 1, 1);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.used > 0 {
            self.out.push((self.hold & 0xFF) as u8);
        }
        self.out
    }
}

/// The fixed-Huffman code for a literal or length symbol.
const fn fixed(symbol: u16) -> (u32, u32) {
    match symbol {
        0..=143 => (0x30 + symbol as u32, 8),
        144..=255 => (0x190 + (symbol as u32 - 144), 9),
        256..=279 => (symbol as u32 - 256, 7),
        _ => (0xC0 + (symbol as u32 - 280), 8),
    }
}

/// The length codes, as `(symbol, extra bits, smallest length)`.
///
/// From RFC 1951's table. Written out rather than computed: the table is the
/// specification, and arithmetic that reproduced it would be a second place
/// for it to be wrong.
const LENGTHS: [(u16, u32, u16); 29] = [
    (257, 0, 3),
    (258, 0, 4),
    (259, 0, 5),
    (260, 0, 6),
    (261, 0, 7),
    (262, 0, 8),
    (263, 0, 9),
    (264, 0, 10),
    (265, 1, 11),
    (266, 1, 13),
    (267, 1, 15),
    (268, 1, 17),
    (269, 2, 19),
    (270, 2, 23),
    (271, 2, 27),
    (272, 2, 31),
    (273, 3, 35),
    (274, 3, 43),
    (275, 3, 51),
    (276, 3, 59),
    (277, 4, 67),
    (278, 4, 83),
    (279, 4, 99),
    (280, 4, 115),
    (281, 5, 131),
    (282, 5, 163),
    (283, 5, 195),
    (284, 5, 227),
    (285, 0, 258),
];

/// The longest run one length symbol can describe.
const LONGEST: usize = 258;

/// The shortest run worth encoding as one.
///
/// Deflate cannot express a match shorter than three, and at three a match
/// costs about as much as three literals — so this is the format's floor
/// rather than a tuned number.
const SHORTEST: usize = 3;

/// Compress with one fixed-Huffman block.
///
/// Finds runs — a match at distance one — and emits everything else as
/// literals. See the module documentation for why that is the right shape for
/// what this compresses.
#[must_use]
pub fn compress(data: &[u8]) -> Vec<u8> {
    let mut bits = Bits::new(data.len() / 4 + 16);
    // One block, and it is the last: `1` for final, `01` for fixed Huffman.
    bits.low_first(1, 1);
    bits.low_first(1, 2);

    let mut at = 0usize;
    while at < data.len() {
        let run = run_at(data, at);
        if run >= SHORTEST {
            emit_run(&mut bits, run);
            at = at.saturating_add(run);
        } else {
            let byte = data.get(at).copied().unwrap_or(0);
            let (code, len) = fixed(u16::from(byte));
            bits.code(code, len);
            at = at.saturating_add(1);
        }
    }

    // End of block.
    let (code, len) = fixed(256);
    bits.code(code, len);
    bits.finish()
}

/// How many bytes from `at` repeat the byte before them.
///
/// The first byte of a stream has nothing before it, so a run cannot start
/// there — which is also why the count begins at `at` and not `at - 1`.
fn run_at(data: &[u8], at: usize) -> usize {
    if at == 0 {
        return 0;
    }
    let Some(previous) = data.get(at.saturating_sub(1)).copied() else {
        return 0;
    };
    let mut n = 0usize;
    while n < LONGEST {
        match data.get(at.saturating_add(n)) {
            Some(b) if *b == previous => n = n.saturating_add(1),
            _ => break,
        }
    }
    n
}

/// Emit a run of `len` bytes as a length symbol and a distance of one.
fn emit_run(bits: &mut Bits, len: usize) {
    let len = len.min(LONGEST);
    // The last entry whose smallest length still fits.
    let mut chosen = (257u16, 0u32, 3u16);
    for entry in LENGTHS {
        if (entry.2 as usize) <= len {
            chosen = entry;
        } else {
            break;
        }
    }
    let (symbol, extra, base) = chosen;
    let (code, bit_len) = fixed(symbol);
    bits.code(code, bit_len);
    if extra > 0 {
        bits.low_first((len as u32).saturating_sub(u32::from(base)), extra);
    }
    // Distance code 0 is a distance of one, with no extra bits. Five bits,
    // because the distance alphabet's fixed code is a flat five-bit code
    // rather than a Huffman one.
    bits.code(0, 5);
}
