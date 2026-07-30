#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! **The encoder is checked against a decoder, not against itself.**
//!
//! A compressor tested by its own reader agrees with itself and can be wrong
//! in both directions at once. So this file contains an inflater — written
//! from RFC 1951 rather than from the encoder — and everything the encoder
//! produces has to come back out of it byte for byte.
//!
//! It is deliberately not shared with anything: a helper both sides used would
//! be one place for a misreading of the format to hide. `smoke.sh` runs the
//! same images through Python's `zlib`, which is a truly independent
//! implementation — so a shared misunderstanding would have to be shared by
//! three parties.

/// Bits out of a deflate stream, least-significant first.
struct Bits<'a> {
    data: &'a [u8],
    at: usize,
    bit: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            at: 0,
            bit: 0,
        }
    }

    fn one(&mut self) -> u32 {
        let byte = self.data.get(self.at).copied().unwrap_or(0);
        let value = (u32::from(byte) >> self.bit) & 1;
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.at += 1;
        }
        value
    }

    /// `count` bits, least-significant first: lengths, distances, sizes.
    fn low_first(&mut self, count: u32) -> u32 {
        let mut out = 0;
        for i in 0..count {
            out |= self.one() << i;
        }
        out
    }

    /// A Huffman code, which arrives most-significant bit first.
    fn code(&mut self, count: u32) -> u32 {
        let mut out = 0;
        for _ in 0..count {
            out = (out << 1) | self.one();
        }
        out
    }

    fn align(&mut self) {
        if self.bit > 0 {
            self.bit = 0;
            self.at += 1;
        }
    }
}

/// One fixed-Huffman symbol, decoded by its prefix — the only way to read a
/// Huffman code without a table, and the way that cannot disagree with the
/// encoder's table because it does not use one.
fn symbol(bits: &mut Bits<'_>) -> u16 {
    let seven = bits.code(7);
    if seven <= 0b001_0111 {
        return u16::try_from(256 + seven).unwrap();
    }
    let eight = (seven << 1) | bits.one();
    if (0b0011_0000..=0b1011_1111).contains(&eight) {
        return u16::try_from(eight - 0b0011_0000).unwrap();
    }
    if (0b1100_0000..=0b1100_0111).contains(&eight) {
        return u16::try_from(280 + (eight - 0b1100_0000)).unwrap();
    }
    let nine = (eight << 1) | bits.one();
    u16::try_from(144 + (nine - 0b1_1001_0000)).unwrap()
}

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

/// Distance codes 0-3 are distances 1-4 with no extra bits; past that they
/// carry extra bits. The encoder only ever writes 0, and this reads them all
/// so an encoder that started writing others would be caught rather than
/// silently mis-read.
fn distance(bits: &mut Bits<'_>) -> usize {
    let code = bits.code(5) as usize;
    if code < 4 {
        return code + 1;
    }
    let extra = u32::try_from((code - 2) / 2).unwrap();
    let base = (1usize << (extra + 1)) + 1 + ((code % 2) << extra);
    base + bits.low_first(extra) as usize
}

/// Inflate a deflate stream written by this crate.
fn inflate(data: &[u8]) -> Vec<u8> {
    let mut bits = Bits::new(data);
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = bits.one();
        let kind = bits.low_first(2);
        match kind {
            0 => {
                bits.align();
                let len = bits.low_first(16) as usize;
                let _nlen = bits.low_first(16);
                for _ in 0..len {
                    out.push(u8::try_from(bits.low_first(8)).unwrap());
                }
            }
            1 => loop {
                let s = symbol(&mut bits);
                if s == 256 {
                    break;
                }
                if s < 256 {
                    out.push(u8::try_from(s).unwrap());
                    continue;
                }
                let (_, extra, base) = LENGTHS
                    .iter()
                    .find(|(sym, _, _)| *sym == s)
                    .copied()
                    .expect("a length symbol this crate writes");
                let len = usize::from(base) + bits.low_first(extra) as usize;
                let back = distance(&mut bits);
                for _ in 0..len {
                    let from = out
                        .len()
                        .checked_sub(back)
                        .expect("a distance inside the output");
                    let byte = out[from];
                    out.push(byte);
                }
            },
            other => panic!("this crate does not write block type {other}"),
        }
        if last == 1 {
            return out;
        }
    }
}

/// **Everything the compressor writes must come back byte for byte.**
///
/// The cases are the shapes it treats differently: nothing, one byte (which
/// cannot start a run), a long run (the whole point), a run longer than one
/// length symbol can describe, high bytes that take the nine-bit codes, and
/// data with no runs at all.
#[test]
fn every_input_survives_the_round_trip() {
    let mut long_run = vec![7u8];
    long_run.extend(std::iter::repeat_n(0u8, 5000));
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("nothing", vec![]),
        ("one byte", vec![0x42]),
        ("two of a kind", vec![9, 9]),
        ("a short run", vec![1, 1, 1, 1]),
        ("a long run past one symbol", long_run),
        ("the nine-bit codes", (144u8..=255).collect()),
        ("the eight-bit codes", (0u8..=143).collect()),
        ("every byte", (0u8..=255).collect()),
        ("no runs at all", (0u8..=255).rev().step_by(3).collect()),
        (
            "runs and literals interleaved",
            (0u8..40)
                .flat_map(|n| std::iter::repeat_n(n, (n % 7) as usize + 1))
                .collect(),
        ),
    ];
    for (what, data) in cases {
        let squeezed = esper_png::compress(&data);
        let back = inflate(&squeezed);
        assert_eq!(back, data, "{what}: {} bytes did not survive", data.len());
    }
}

/// A run of zeros — what a filtered screenshot is mostly made of — has to
/// actually compress, or the reason for the run matcher is gone.
#[test]
fn a_screenful_of_nothing_compresses_to_almost_nothing() {
    let flat = vec![0u8; 1920 * 3 * 1080];
    let squeezed = esper_png::compress(&flat);
    assert_eq!(inflate(&squeezed), flat, "and it still round-trips");
    let ratio = flat.len() as f64 / squeezed.len() as f64;
    assert!(
        ratio > 100.0,
        "a flat frame compressed only {ratio:.0}x, which means the run matcher \
         is not finding runs"
    );
}

/// The bytes a PNG must start and end with, and the size it says it is.
#[test]
fn the_container_is_a_png() {
    let png = esper_png::write_rgb8(2, 2, &[0; 12]).expect("written");
    assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    assert_eq!(&png[12..16], b"IHDR");
    assert_eq!(&png[16..20], &2u32.to_be_bytes());
    assert_eq!(&png[20..24], &2u32.to_be_bytes());
    assert_eq!(&png[24..29], &[8, 2, 0, 0, 0]);
    assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
}

/// A buffer that does not match the size it was given is refused rather than
/// written short — a truncated PNG is a file that opens and lies.
#[test]
fn a_mismatched_buffer_is_refused() {
    assert!(esper_png::write_rgb8(4, 4, &[0; 47]).is_err());
    assert!(esper_png::write_rgb8(4, 4, &[0; 49]).is_err());
    assert!(esper_png::write_rgb8(4, 4, &[0; 48]).is_ok());
    assert!(esper_png::write_rgb8(0, 4, &[]).is_err());
    assert!(esper_png::write_rgb8(4, 0, &[]).is_err());
}

/// The CRC table is the polynomial, derived here so the shipped constants are
/// asserted rather than trusted.
#[test]
fn the_table_is_the_polynomial() {
    let mut wanted = [0u32; 16];
    for (i, slot) in wanted.iter_mut().enumerate() {
        let mut c = u32::try_from(i).unwrap();
        for _ in 0..4 {
            c = if c & 1 == 1 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    // A PNG this crate wrote must carry a CRC computed from those constants,
    // which is what checking the whole file against an independent CRC does.
    let png = esper_png::write_rgb8(1, 1, &[1, 2, 3]).expect("written");
    let ihdr_crc = u32::from_be_bytes(png[29..33].try_into().unwrap());
    let mut check = 0xFFFF_FFFFu32;
    for byte in &png[12..29] {
        for nibble in [u32::from(*byte) & 0xF, u32::from(*byte) >> 4] {
            check = (check >> 4) ^ wanted[((check ^ nibble) & 0xF) as usize];
        }
    }
    assert_eq!(ihdr_crc, check ^ 0xFFFF_FFFF, "the IHDR CRC is not the CRC");
}
