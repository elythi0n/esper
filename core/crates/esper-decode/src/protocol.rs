//! The messages between the parent and the sandboxed decoder.
//!
//! # Both directions are trust boundaries
//!
//! The parent sending bitstream to the child is the obvious one. The other
//! direction is the one people forget: **a sandbox contains a compromise, it
//! does not make the compromised thing's output trustworthy.** If the decoder
//! is taken over, it controls every byte the parent reads here.
//!
//! So the parent's parser is written to the same standard as the network's.
//! Every length is checked against a bound before it is used to allocate or to
//! index, and the bound is the same one the decoder's own input has — because
//! a message claiming a four-gigabyte picture must cost a rejected header, not
//! four gigabytes, whichever side it came from.
//!
//! # Why the framing is hand-rolled
//!
//! The project has CBOR, and this does not use it. A picture is 1.5 bytes per
//! pixel — three megabytes at 1080p — and putting that through a serialiser
//! means a copy into an intermediate representation and a copy out. The
//! framing here is a tag and three lengths, which is small enough to audit by
//! reading and cheap enough to run every frame.

/// Largest message either side will accept.
///
/// An 8K picture is 50 MB of planes, and this has to hold one. Deliberately a
/// single bound shared by both directions: two bounds is two things to get
/// right, and the asymmetry buys nothing.
pub const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// What the parent sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToDecoder {
    /// A packet of Annex-B bitstream.
    Decode(Vec<u8>),
    /// Finish and exit.
    Shutdown,
}

/// What the child sends back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromDecoder {
    /// The sandbox is on and the decoder is ready.
    ///
    /// **Sent only after the jail is applied**, and the parent refuses to send
    /// anything before receiving it. That ordering is the security property:
    /// setup needs syscalls the sandbox forbids, so the sandbox goes on after
    /// setup and before the first byte from a peer.
    Ready {
        /// What actually took effect, so the parent can refuse to proceed if
        /// the child is not really sandboxed.
        report: crate::sandbox::SandboxReport,
    },
    /// A decoded picture.
    Picture {
        width: u32,
        height: u32,
        /// Y, then U, then V, each tightly packed.
        planes: Vec<u8>,
    },
    /// A frame of sound: interleaved 16-bit samples.
    ///
    /// **The audio worker's answer, and the video worker never sends it.**
    /// One worker process decodes one kind of thing — see
    /// [`crate::SandboxedAudio`] for why sound gets its own jail rather than
    /// sharing the picture's.
    Samples { values: Vec<i16> },
    /// The packet was consumed and completed no picture. Ordinary.
    NoPicture,
    /// The decoder refused or failed. Fatal to the stream.
    Failed(String),
}

/// Message tags. Explicit numbers, because the wire is a contract even between
/// two halves of the same binary — a mismatched build must fail to parse
/// rather than misinterpret.
mod tag {
    pub const DECODE: u8 = 1;
    pub const SHUTDOWN: u8 = 2;
    pub const READY: u8 = 3;
    pub const PICTURE: u8 = 4;
    pub const NO_PICTURE: u8 = 5;
    pub const FAILED: u8 = 6;
    pub const SAMPLES: u8 = 7;
}

/// The most samples one message may carry.
///
/// **The largest legal Opus frame, and not a byte more.** 60 ms at 48 kHz in
/// stereo is 5760 values; a message claiming more than that is not a frame of
/// sound, whatever it says. Bounded here because this is a length from a
/// process that is assumed to be compromised.
pub const MAX_SAMPLE_VALUES: usize = 5760;

/// What went wrong on the wire.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    /// The peer is gone. Not an error in itself — it is how a shutdown looks.
    #[error("the other end closed")]
    Closed,
    /// A length past [`MAX_MESSAGE`]. Rejected before anything is allocated.
    #[error("a message claiming {len} bytes exceeds the {MAX_MESSAGE} byte limit")]
    TooLarge { len: u64 },
    /// The message ended mid-field.
    #[error("a message ended after {got} of {wanted} bytes")]
    Truncated { wanted: usize, got: usize },
    /// An unknown tag, which means the two halves disagree about the wire.
    #[error("unknown message tag {tag}")]
    UnknownTag { tag: u8 },
    /// The declared planes do not match the declared dimensions.
    ///
    /// Its own variant because a consumer that trusted the dimensions and
    /// indexed by stride would read past the end of the planes — which is the
    /// specific thing a compromised child would try.
    #[error("a {width}x{height} picture needs {needed} plane bytes, got {got}")]
    PlaneMismatch {
        width: u32,
        height: u32,
        needed: usize,
        got: usize,
    },
    /// The transport failed.
    #[error("io: {0}")]
    Io(String),
}

/// Bytes a tightly packed I420 picture of this size needs.
///
/// Returns `None` rather than wrapping, so an absurd size from a compromised
/// child is a refusal instead of a small number that passes a length check.
#[must_use]
pub fn plane_bytes(width: u32, height: u32) -> Option<usize> {
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return None;
    }
    let luma = (width as usize).checked_mul(height as usize)?;
    // Two chroma planes at a quarter each: luma * 3 / 2, exact because both
    // dimensions are even.
    luma.checked_add(luma / 2)
}

/// Write one message.
pub fn write<W: std::io::Write>(to: &mut W, message: &ToDecoder) -> Result<(), ProtocolError> {
    let mut body = Vec::new();
    match message {
        ToDecoder::Decode(packet) => {
            body.push(tag::DECODE);
            body.extend_from_slice(&(packet.len() as u64).to_be_bytes());
            body.extend_from_slice(packet);
        }
        ToDecoder::Shutdown => body.push(tag::SHUTDOWN),
    }
    frame(to, &body)
}

/// Write one reply.
pub fn write_reply<W: std::io::Write>(
    to: &mut W,
    message: &FromDecoder,
) -> Result<(), ProtocolError> {
    let mut body = Vec::new();
    match message {
        FromDecoder::Ready { report } => {
            body.push(tag::READY);
            body.push(u8::from(report.address_space));
            body.push(u8::from(report.open_files));
            body.push(u8::from(report.no_new_privs));
            body.push(u8::from(report.landlock));
            body.push(u8::from(report.seccomp));
            body.push(u8::from(report.enforcing));
        }
        FromDecoder::Picture {
            width,
            height,
            planes,
        } => {
            body.push(tag::PICTURE);
            body.extend_from_slice(&width.to_be_bytes());
            body.extend_from_slice(&height.to_be_bytes());
            body.extend_from_slice(&(planes.len() as u64).to_be_bytes());
            body.extend_from_slice(planes);
        }
        FromDecoder::Samples { values } => {
            body.push(tag::SAMPLES);
            body.extend_from_slice(&(values.len() as u64).to_be_bytes());
            for v in values {
                body.extend_from_slice(&v.to_le_bytes());
            }
        }
        FromDecoder::NoPicture => body.push(tag::NO_PICTURE),
        FromDecoder::Failed(detail) => {
            body.push(tag::FAILED);
            let text = detail.as_bytes();
            // Bounded, because a compromised child could otherwise send an
            // error message the size of memory.
            let text = text.get(..text.len().min(4096)).unwrap_or(&[]);
            body.extend_from_slice(&(text.len() as u64).to_be_bytes());
            body.extend_from_slice(text);
        }
    }
    frame(to, &body)
}

/// Length-prefix and write.
fn frame<W: std::io::Write>(to: &mut W, body: &[u8]) -> Result<(), ProtocolError> {
    if body.len() > MAX_MESSAGE {
        return Err(ProtocolError::TooLarge {
            len: body.len() as u64,
        });
    }
    let len = (body.len() as u64).to_be_bytes();
    to.write_all(&len).map_err(io)?;
    to.write_all(body).map_err(io)?;
    to.flush().map_err(io)
}

fn io(e: std::io::Error) -> ProtocolError {
    ProtocolError::Io(e.to_string())
}

/// Read one framed message body.
///
/// The length is checked against [`MAX_MESSAGE`] **before** the buffer is
/// allocated. That is the same rule the network's frame reader follows and for
/// the same reason: a claim must not be able to cost memory.
pub fn read_body<R: std::io::Read>(from: &mut R) -> Result<Vec<u8>, ProtocolError> {
    let mut header = [0u8; 8];
    match read_exact(from, &mut header)? {
        // Nothing at all: a clean close.
        0 => return Err(ProtocolError::Closed),
        8 => {}
        got => {
            return Err(ProtocolError::Truncated { wanted: 8, got });
        }
    }
    let len = u64::from_be_bytes(header);
    if len > MAX_MESSAGE as u64 {
        return Err(ProtocolError::TooLarge { len });
    }
    let len = len as usize;
    let mut body = vec![0u8; len];
    let got = read_exact(from, &mut body)?;
    if got != len {
        return Err(ProtocolError::Truncated { wanted: len, got });
    }
    Ok(body)
}

/// Fill `buf`, returning how much arrived.
fn read_exact<R: std::io::Read>(from: &mut R, buf: &mut [u8]) -> Result<usize, ProtocolError> {
    let mut got = 0usize;
    while got < buf.len() {
        let Some(rest) = buf.get_mut(got..) else {
            break;
        };
        match from.read(rest) {
            Ok(0) => break,
            Ok(n) => got = got.saturating_add(n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io(e)),
        }
    }
    Ok(got)
}

/// Parse a message from the parent.
pub fn parse_request(body: &[u8]) -> Result<ToDecoder, ProtocolError> {
    let tag = body
        .first()
        .copied()
        .ok_or(ProtocolError::Truncated { wanted: 1, got: 0 })?;
    match tag {
        tag::DECODE => {
            let len = read_u64(body, 1)? as usize;
            let from = 9usize;
            let packet = body
                .get(
                    from..from
                        .checked_add(len)
                        .ok_or(ProtocolError::TooLarge { len: len as u64 })?,
                )
                .ok_or(ProtocolError::Truncated {
                    wanted: from.saturating_add(len),
                    got: body.len(),
                })?;
            Ok(ToDecoder::Decode(packet.to_vec()))
        }
        tag::SHUTDOWN => Ok(ToDecoder::Shutdown),
        other => Err(ProtocolError::UnknownTag { tag: other }),
    }
}

/// Parse a reply from the child.
///
/// **This is the parser that a compromised decoder attacks.** Every field is
/// checked, and the planes are checked against the dimensions, so a consumer
/// indexing by stride cannot be walked off the end.
pub fn parse_reply(body: &[u8]) -> Result<FromDecoder, ProtocolError> {
    let tag = body
        .first()
        .copied()
        .ok_or(ProtocolError::Truncated { wanted: 1, got: 0 })?;
    match tag {
        tag::READY => {
            let flag = |at: usize| body.get(at).copied().unwrap_or(0) != 0;
            if body.len() < 7 {
                return Err(ProtocolError::Truncated {
                    wanted: 7,
                    got: body.len(),
                });
            }
            Ok(FromDecoder::Ready {
                report: crate::sandbox::SandboxReport {
                    address_space: flag(1),
                    open_files: flag(2),
                    no_new_privs: flag(3),
                    landlock: flag(4),
                    seccomp: flag(5),
                    enforcing: flag(6),
                },
            })
        }
        tag::PICTURE => {
            let width = read_u32(body, 1)?;
            let height = read_u32(body, 5)?;
            let len = read_u64(body, 9)? as usize;
            let from = 17usize;
            let planes = body
                .get(
                    from..from
                        .checked_add(len)
                        .ok_or(ProtocolError::TooLarge { len: len as u64 })?,
                )
                .ok_or(ProtocolError::Truncated {
                    wanted: from.saturating_add(len),
                    got: body.len(),
                })?;

            // The check that stops a compromised child walking a consumer off
            // the end of the planes.
            let needed = plane_bytes(width, height).ok_or(ProtocolError::PlaneMismatch {
                width,
                height,
                needed: 0,
                got: planes.len(),
            })?;
            if planes.len() != needed {
                return Err(ProtocolError::PlaneMismatch {
                    width,
                    height,
                    needed,
                    got: planes.len(),
                });
            }
            Ok(FromDecoder::Picture {
                width,
                height,
                planes: planes.to_vec(),
            })
        }
        tag::SAMPLES => {
            let count = read_u64(body, 1)? as usize;
            if count > MAX_SAMPLE_VALUES {
                return Err(ProtocolError::TooLarge { len: count as u64 });
            }
            let from = 9usize;
            let bytes = count
                .checked_mul(2)
                .ok_or(ProtocolError::TooLarge { len: count as u64 })?;
            let raw = body
                .get(
                    from..from
                        .checked_add(bytes)
                        .ok_or(ProtocolError::TooLarge { len: bytes as u64 })?,
                )
                .ok_or(ProtocolError::Truncated {
                    wanted: from.saturating_add(bytes),
                    got: body.len(),
                })?;
            let values = raw
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| i16::from_le_bytes(*pair))
                .collect();
            Ok(FromDecoder::Samples { values })
        }
        tag::NO_PICTURE => Ok(FromDecoder::NoPicture),
        tag::FAILED => {
            let len = read_u64(body, 1)? as usize;
            let from = 9usize;
            let text =
                body.get(from..from.saturating_add(len))
                    .ok_or(ProtocolError::Truncated {
                        wanted: from.saturating_add(len),
                        got: body.len(),
                    })?;
            Ok(FromDecoder::Failed(
                String::from_utf8_lossy(text).into_owned(),
            ))
        }
        other => Err(ProtocolError::UnknownTag { tag: other }),
    }
}

fn read_u32(body: &[u8], at: usize) -> Result<u32, ProtocolError> {
    let slice = body
        .get(at..at.saturating_add(4))
        .ok_or(ProtocolError::Truncated {
            wanted: at.saturating_add(4),
            got: body.len(),
        })?;
    <[u8; 4]>::try_from(slice)
        .map(u32::from_be_bytes)
        .map_err(|_| ProtocolError::Truncated {
            wanted: at.saturating_add(4),
            got: body.len(),
        })
}

fn read_u64(body: &[u8], at: usize) -> Result<u64, ProtocolError> {
    let slice = body
        .get(at..at.saturating_add(8))
        .ok_or(ProtocolError::Truncated {
            wanted: at.saturating_add(8),
            got: body.len(),
        })?;
    <[u8; 8]>::try_from(slice)
        .map(u64::from_be_bytes)
        .map_err(|_| ProtocolError::Truncated {
            wanted: at.saturating_add(8),
            got: body.len(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_request(m: &ToDecoder) -> ToDecoder {
        let mut buf = Vec::new();
        write(&mut buf, m).expect("write");
        let body = read_body(&mut buf.as_slice()).expect("read");
        parse_request(&body).expect("parse")
    }

    fn round_trip_reply(m: &FromDecoder) -> FromDecoder {
        let mut buf = Vec::new();
        write_reply(&mut buf, m).expect("write");
        let body = read_body(&mut buf.as_slice()).expect("read");
        parse_reply(&body).expect("parse")
    }

    #[test]
    fn requests_round_trip() {
        let decode = ToDecoder::Decode(vec![0, 0, 0, 1, 0x67, 0x42]);
        assert_eq!(round_trip_request(&decode), decode);
        assert_eq!(
            round_trip_request(&ToDecoder::Shutdown),
            ToDecoder::Shutdown
        );
        // An empty packet is legitimate and must survive.
        let empty = ToDecoder::Decode(Vec::new());
        assert_eq!(round_trip_request(&empty), empty);
    }

    #[test]
    fn replies_round_trip() {
        let report = crate::sandbox::SandboxReport {
            address_space: true,
            open_files: true,
            no_new_privs: true,
            landlock: false,
            seccomp: true,
            enforcing: true,
        };
        assert_eq!(
            round_trip_reply(&FromDecoder::Ready { report }),
            FromDecoder::Ready { report },
            "every flag must survive, or the parent cannot tell whether the \
             child is really sandboxed"
        );
        assert_eq!(
            round_trip_reply(&FromDecoder::NoPicture),
            FromDecoder::NoPicture
        );

        let picture = FromDecoder::Picture {
            width: 4,
            height: 4,
            planes: vec![7u8; 4 * 4 * 3 / 2],
        };
        assert_eq!(round_trip_reply(&picture), picture);

        let failed = FromDecoder::Failed("something went wrong".to_owned());
        assert_eq!(round_trip_reply(&failed), failed);
    }

    /// **The attack a compromised decoder would try.** Claim a large picture
    /// and send short planes, so a consumer indexing by stride reads past the
    /// end.
    #[test]
    fn a_picture_whose_planes_do_not_match_its_dimensions_is_refused() {
        let mut buf = Vec::new();
        write_reply(
            &mut buf,
            &FromDecoder::Picture {
                width: 1920,
                height: 1080,
                // Nowhere near enough.
                planes: vec![0u8; 100],
            },
        )
        .expect("write");
        let body = read_body(&mut buf.as_slice()).expect("read");
        let err = parse_reply(&body).expect_err("mismatched planes");
        match err {
            ProtocolError::PlaneMismatch {
                width,
                height,
                needed,
                got,
            } => {
                assert_eq!((width, height), (1920, 1080));
                assert_eq!(needed, 1920 * 1080 * 3 / 2);
                assert_eq!(got, 100);
            }
            other => panic!("{other:?}"),
        }
    }

    /// And the other direction: planes longer than the dimensions need, which
    /// would leave a consumer with bytes it does not know about.
    #[test]
    fn a_picture_with_too_many_plane_bytes_is_refused() {
        let mut buf = Vec::new();
        write_reply(
            &mut buf,
            &FromDecoder::Picture {
                width: 4,
                height: 4,
                planes: vec![0u8; 4 * 4 * 3 / 2 + 1],
            },
        )
        .expect("write");
        let body = read_body(&mut buf.as_slice()).expect("read");
        assert!(matches!(
            parse_reply(&body),
            Err(ProtocolError::PlaneMismatch { .. })
        ));
    }

    /// An odd or zero dimension has no I420 representation, so it cannot be a
    /// real picture.
    #[test]
    fn an_impossible_picture_size_has_no_plane_count() {
        assert_eq!(plane_bytes(0, 4), None);
        assert_eq!(plane_bytes(4, 0), None);
        assert_eq!(plane_bytes(5, 4), None, "odd width");
        assert_eq!(plane_bytes(4, 5), None, "odd height");
        assert_eq!(plane_bytes(4, 4), Some(24));
        assert_eq!(plane_bytes(1920, 1080), Some(1920 * 1080 * 3 / 2));
        // And it must not wrap into a small number that passes a length check.
        assert_eq!(plane_bytes(u32::MAX - 1, u32::MAX - 1), None);
    }

    /// A claim must cost a rejected header, not memory — whichever side it
    /// came from.
    #[test]
    fn an_oversized_length_is_refused_before_anything_is_allocated() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&(u64::MAX).to_be_bytes());
        let err = read_body(&mut wire.as_slice()).expect_err("absurd length");
        assert!(matches!(err, ProtocolError::TooLarge { .. }), "{err:?}");

        let mut just_over = Vec::new();
        just_over.extend_from_slice(&((MAX_MESSAGE as u64) + 1).to_be_bytes());
        assert!(matches!(
            read_body(&mut just_over.as_slice()),
            Err(ProtocolError::TooLarge { .. })
        ));
    }

    #[test]
    fn a_closed_stream_is_not_an_error_shape() {
        let empty: &[u8] = &[];
        assert_eq!(
            read_body(&mut { empty }),
            Err(ProtocolError::Closed),
            "a clean close is how a shutdown looks, not a failure"
        );
    }

    #[test]
    fn a_truncated_message_is_refused() {
        // A header promising more than follows.
        let mut wire = Vec::new();
        wire.extend_from_slice(&100u64.to_be_bytes());
        wire.extend_from_slice(&[1, 2, 3]);
        let err = read_body(&mut wire.as_slice()).expect_err("truncated");
        assert!(matches!(err, ProtocolError::Truncated { .. }), "{err:?}");

        // A half-written header.
        let err = read_body(&mut [0u8, 0, 0].as_slice()).expect_err("short header");
        assert!(matches!(err, ProtocolError::Truncated { .. }), "{err:?}");
    }

    /// A mismatched build must fail to parse rather than misinterpret.
    #[test]
    fn an_unknown_tag_is_refused_in_both_directions() {
        assert!(matches!(
            parse_request(&[99]),
            Err(ProtocolError::UnknownTag { tag: 99 })
        ));
        assert!(matches!(
            parse_reply(&[99]),
            Err(ProtocolError::UnknownTag { tag: 99 })
        ));
        // And the tags do not overlap between directions in a way that would
        // let a reply be read as a request.
        assert!(parse_request(&[tag::PICTURE]).is_err());
        assert!(parse_reply(&[tag::SHUTDOWN]).is_err());
    }

    /// Arbitrary bytes must never panic, in either parser. Both are reachable
    /// from something that may be compromised.
    #[test]
    fn arbitrary_bytes_never_panic_in_either_parser() {
        let mut x = 0x1234_5678_9ABC_DEF0u64;
        for _ in 0..20_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let n = (x % 40) as usize;
            let body: Vec<u8> = (0..n).map(|i| ((x >> (i % 8)) & 0xFF) as u8).collect();
            let _ = parse_request(&body);
            let _ = parse_reply(&body);
            let _ = read_body(&mut body.as_slice());
        }
    }
}
