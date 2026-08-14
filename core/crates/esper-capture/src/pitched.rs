//! Copying between buffers whose rows are not the same length.
//!
//! # Why this is its own module
//!
//! Every capture backend reads out of something with a **pitch**: a row length
//! the driver chose, which is at least the row's own size and usually more.
//! Copying by width instead of by pitch shears the image diagonally, and that
//! is the single most recognisable capture bug there is.
//!
//! It lives here rather than in a platform backend because it is arithmetic,
//! and arithmetic that decides whether the picture is straight should not be
//! the part nobody can test. The Windows backend is
//! `cfg(target_os = "windows")`, so its own tests run nowhere on the machine
//! this was written on.

/// Why a copy could not be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PitchError {
    /// A pitch cannot hold the row it claims to.
    #[error("a row of {row} bytes does not fit in a pitch of {pitch}")]
    RowTooWide { row: usize, pitch: usize },

    /// One of the buffers is shorter than the rows it is being asked for.
    #[error("{which} holds {have} bytes and {rows} rows at pitch {pitch} needs {want}")]
    TooShort {
        which: &'static str,
        have: usize,
        want: usize,
        rows: usize,
        pitch: usize,
    },
}

/// Copy `rows` rows of `row` bytes, from one pitch to another.
///
/// **The row length is given, not inferred from either pitch.** Inferring it
/// as the smaller of the two would silently copy a driver's padding into the
/// picture when the destination happened to be wider, and silently truncate
/// when it was narrower — and both look like a working capture until somebody
/// examines the right-hand edge.
///
/// # Errors
///
/// If either pitch cannot hold a row, or either buffer is too short for the
/// rows asked for. The last row need only hold its own bytes, not a whole
/// pitch: a driver supplies a full pitch, but demanding one would refuse a
/// buffer that is complete.
pub fn copy_rows(
    source: &[u8],
    source_pitch: usize,
    into: &mut [u8],
    into_pitch: usize,
    row: usize,
    rows: usize,
) -> Result<(), PitchError> {
    if source_pitch < row {
        return Err(PitchError::RowTooWide {
            row,
            pitch: source_pitch,
        });
    }
    if into_pitch < row {
        return Err(PitchError::RowTooWide {
            row,
            pitch: into_pitch,
        });
    }
    if rows == 0 || row == 0 {
        return Ok(());
    }

    let needed = |pitch: usize| {
        rows.saturating_sub(1)
            .saturating_mul(pitch)
            .saturating_add(row)
    };
    if source.len() < needed(source_pitch) {
        return Err(PitchError::TooShort {
            which: "the source",
            have: source.len(),
            want: needed(source_pitch),
            rows,
            pitch: source_pitch,
        });
    }
    if into.len() < needed(into_pitch) {
        return Err(PitchError::TooShort {
            which: "the destination",
            have: into.len(),
            want: needed(into_pitch),
            rows,
            pitch: into_pitch,
        });
    }

    for y in 0..rows {
        let from = y.saturating_mul(source_pitch);
        let to = y.saturating_mul(into_pitch);
        let (Some(src), Some(dst)) = (
            source.get(from..from.saturating_add(row)),
            into.get_mut(to..to.saturating_add(row)),
        ) else {
            // Unreachable given the checks above, and a refusal rather than a
            // panic because the workspace does not panic and a driver's
            // numbers are not a thing to trust with the process.
            return Err(PitchError::TooShort {
                which: "a row",
                have: source.len(),
                want: from.saturating_add(row),
                rows,
                pitch: source_pitch,
            });
        };
        dst.copy_from_slice(src);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// A source whose rows are recognisable: row `y` is filled with `y + 1`,
    /// and the padding past the row is `0xFF` so that copying it shows up.
    fn source(row: usize, pitch: usize, rows: usize) -> Vec<u8> {
        let mut out = vec![0xFFu8; pitch * rows];
        for y in 0..rows {
            for x in 0..row {
                if let Some(byte) = out.get_mut(y * pitch + x) {
                    *byte = u8::try_from(y + 1).unwrap_or(0);
                }
            }
        }
        out
    }

    #[test]
    fn a_wider_source_pitch_does_not_shear_the_picture() {
        // The bug this module exists for. Copying by the source's pitch into
        // a tight destination puts row 0's padding at the start of row 1, and
        // every row after slides further — the diagonal smear.
        let (row, rows) = (4, 5);
        let src = source(row, 16, rows);
        let mut dst = vec![0u8; row * rows];
        copy_rows(&src, 16, &mut dst, row, row, rows).expect("a copy");
        for y in 0..rows {
            for x in 0..row {
                assert_eq!(
                    dst.get(y * row + x).copied(),
                    Some(u8::try_from(y + 1).unwrap()),
                    "row {y} byte {x} came from the wrong place"
                );
            }
        }
    }

    #[test]
    fn the_padding_is_never_copied() {
        // 0xFF only exists past the end of a row. If any of it arrives, the
        // copy took the pitch for the row.
        let src = source(3, 8, 4);
        let mut dst = vec![0u8; 3 * 4];
        copy_rows(&src, 8, &mut dst, 3, 3, 4).expect("a copy");
        assert!(!dst.contains(&0xFF), "padding reached the picture: {dst:?}");
    }

    #[test]
    fn a_wider_destination_keeps_its_own_padding() {
        // The other direction: a destination with room to spare must be
        // written row by row, not filled.
        let src = source(2, 2, 3);
        let mut dst = vec![0xAAu8; 6 * 3];
        copy_rows(&src, 2, &mut dst, 6, 2, 3).expect("a copy");
        for y in 0..3u8 {
            let at = usize::from(y) * 6;
            assert_eq!(dst.get(at).copied(), Some(y + 1));
            assert_eq!(dst.get(at + 1).copied(), Some(y + 1));
            // Untouched.
            assert_eq!(dst.get(at + 2).copied(), Some(0xAA), "row {y}'s padding");
        }
    }

    #[test]
    fn the_row_length_is_given_and_not_inferred() {
        // Inferring it as the smaller pitch would copy padding into a wider
        // destination and truncate into a narrower one, and both look like a
        // working capture until somebody examines the right-hand edge.
        let src = source(4, 8, 2);
        let mut dst = vec![0u8; 8 * 2];
        // Both pitches are 8 and the row is 4: only four bytes per row move.
        copy_rows(&src, 8, &mut dst, 8, 4, 2).expect("a copy");
        assert_eq!(dst.get(4..8), Some(&[0, 0, 0, 0][..]), "beyond the row");
    }

    #[test]
    fn the_last_row_need_not_hold_a_whole_pitch() {
        // A driver supplies a full pitch, but demanding one would refuse a
        // buffer that is complete — and the symptom would be a capture that
        // never produced a frame on hardware that was behaving.
        let row = 4;
        let mut src = source(row, 16, 3);
        src.truncate(16 * 2 + row);
        let mut dst = vec![0u8; row * 3];
        copy_rows(&src, 16, &mut dst, row, row, 3).expect("the last row is whole");

        src.truncate(16 * 2 + row - 1);
        assert!(matches!(
            copy_rows(&src, 16, &mut dst, row, row, 3),
            Err(PitchError::TooShort { .. })
        ));
    }

    #[test]
    fn a_pitch_that_cannot_hold_a_row_is_refused() {
        let src = vec![0u8; 64];
        let mut dst = vec![0u8; 64];
        assert!(matches!(
            copy_rows(&src, 3, &mut dst, 8, 4, 2),
            Err(PitchError::RowTooWide { row: 4, pitch: 3 })
        ));
        assert!(matches!(
            copy_rows(&src, 8, &mut dst, 3, 4, 2),
            Err(PitchError::RowTooWide { row: 4, pitch: 3 })
        ));
    }

    #[test]
    fn a_short_buffer_is_refused_rather_than_read_past() {
        let src = vec![0u8; 10];
        let mut dst = vec![0u8; 100];
        match copy_rows(&src, 8, &mut dst, 8, 8, 4) {
            Err(PitchError::TooShort { which, want, .. }) => {
                assert_eq!(which, "the source");
                assert_eq!(want, 3 * 8 + 8);
            }
            other => panic!("{other:?}"),
        }
        let src = vec![0u8; 100];
        let mut dst = vec![0u8; 10];
        match copy_rows(&src, 8, &mut dst, 8, 8, 4) {
            Err(PitchError::TooShort { which, .. }) => assert_eq!(which, "the destination"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nothing_to_copy_is_not_an_error() {
        // A zero-height frame is refused by `Frame::new`, not here — this is
        // asked for by a caller that has already worked out there is nothing
        // to move, and refusing would make it carry a branch.
        assert_eq!(copy_rows(&[], 0, &mut [], 0, 0, 0), Ok(()));
        assert_eq!(copy_rows(&[1, 2], 2, &mut [0, 0], 2, 2, 0), Ok(()));
        assert_eq!(copy_rows(&[1, 2], 2, &mut [0, 0], 2, 0, 2), Ok(()));
    }

    #[test]
    fn a_realistic_frame_survives_a_realistic_pitch() {
        // 1920 pixels of BGRA is 7680 bytes; a driver commonly rounds to 8192.
        let (width, height) = (1920usize, 64usize);
        let row = width * 4;
        let pitch = 8192;
        let src = source(row, pitch, height);
        let mut dst = vec![0u8; row * height];
        copy_rows(&src, pitch, &mut dst, row, row, height).expect("a frame");
        assert!(!dst.contains(&0xFF), "padding reached the picture");
        for y in 0..height {
            assert_eq!(
                dst.get(y * row).copied(),
                Some(u8::try_from(y + 1).unwrap_or(0))
            );
        }
    }
}
