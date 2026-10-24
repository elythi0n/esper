//! Proving that what arrived is what was sent.
//!
//! # Why the whole file, and why at both ends
//!
//! A transfer can go wrong in ways the transport cannot see. A file that
//! changed while it was being read, a resumed transfer that picked up from the
//! wrong place, a partial left over from a different version of the same
//! path — none of these are corruption on the wire, and QUIC's own integrity
//! checks say nothing about any of them.
//!
//! So the sender hashes what it read and the receiver hashes what it wrote,
//! and a file that does not match is not a file. **There is no flag to skip
//! it.** A verification that can be turned off is one that is off in the
//! situation where it mattered.
//!
//! SHA-256 because it is already the hash in this tree. A second one, to be
//! marginally faster, is how a dependency list becomes a liability.

use std::io::Read;

use sha2::{Digest as _, Sha256};

/// How much is read at a time when hashing a file that is already on a disk.
///
/// Large enough that the syscall is not the cost, small enough to stay in
/// cache.
const AT_A_TIME: usize = 128 * 1024;

/// The hash of a file's whole contents.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sum([u8; 32]);

impl Sum {
    /// The bytes, as the wire carries them.
    #[must_use]
    pub const fn bytes(&self) -> [u8; 32] {
        self.0
    }

    /// A hash a peer sent.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The first few characters, which is what a person needs to tell two
    /// files apart and all a log line should carry.
    #[must_use]
    pub fn short(&self) -> String {
        self.0
            .iter()
            .take(6)
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    }

    /// Read something to its end and hash it.
    ///
    /// # Errors
    ///
    /// If the reading fails.
    pub fn of(reader: &mut impl Read) -> std::io::Result<Self> {
        let mut running = Running::new();
        let mut buffer = vec![0u8; AT_A_TIME];
        loop {
            let got = reader.read(&mut buffer)?;
            if got == 0 {
                return Ok(running.finish());
            }
            running.add(buffer.get(..got).unwrap_or_default());
        }
    }
}

/// **Printed as itself and nothing else.** A hash in a log line is how two
/// transfers of the same path are told apart, so it has to be the same text
/// every time.
impl std::fmt::Debug for Sum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.short())
    }
}

impl std::fmt::Display for Sum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// A hash being built as bytes go past.
///
/// The receiving side never has the whole file in memory, so it hashes what it
/// writes as it writes it.
pub struct Running(Sha256);

impl Running {
    /// Start one.
    #[must_use]
    pub fn new() -> Self {
        Self(Sha256::new())
    }

    /// Take in some more.
    pub fn add(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// What it comes to.
    #[must_use]
    pub fn finish(self) -> Sum {
        Sum(self.0.finalize().into())
    }
}

impl Default for Running {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Running {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Running(a hash in progress)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against the value everything else in the world agrees on, so this is
    /// checked against SHA-256 rather than against itself.
    #[test]
    fn it_is_sha_256_and_not_something_that_resembles_it() {
        let sum = Sum::of(&mut b"abc".as_slice()).expect("hash");
        assert_eq!(
            sum.to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sum.short(), "ba7816bf8f01");
        assert_eq!(format!("{sum:?}"), "ba7816bf8f01");

        let empty = Sum::of(&mut b"".as_slice()).expect("hash");
        assert_eq!(
            empty.to_string(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// **Whether the bytes arrive in one piece or a thousand cannot matter**,
    /// because the receiver hashes datagram-sized pieces and the sender hashes
    /// a file in chunks.
    #[test]
    fn a_file_hashed_in_pieces_is_the_same_file() {
        let whole: Vec<u8> = (0..300_000u32)
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect();
        let at_once = Sum::of(&mut whole.as_slice()).expect("hash");

        for piece in [1usize, 7, 4096, 128 * 1024, 299_999] {
            let mut running = Running::new();
            for chunk in whole.chunks(piece) {
                running.add(chunk);
            }
            assert_eq!(running.finish(), at_once, "in pieces of {piece}");
        }
    }

    #[test]
    fn a_hash_survives_the_wire_and_two_files_are_told_apart() {
        let one = Sum::of(&mut b"one".as_slice()).expect("hash");
        let other = Sum::of(&mut b"two".as_slice()).expect("hash");
        assert_ne!(one, other);
        assert_eq!(Sum::from_bytes(one.bytes()), one);
        assert_ne!(Sum::from_bytes(one.bytes()), other);
        assert_ne!(one.short(), other.short());
        // A running hash of nothing is the hash of nothing, not a panic.
        assert_eq!(
            Running::default().finish(),
            Sum::of(&mut b"".as_slice()).expect("hash")
        );
    }
}
