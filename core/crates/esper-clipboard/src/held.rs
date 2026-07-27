use esper_proto::ClipboardKind;
use zeroize::Zeroize;

/// Clipboard content this process is holding, wiped when it is let go.
///
/// # Why the wiping is not theatre
///
/// The bytes are usually a password. A `Vec<u8>` dropped normally leaves its
/// contents in the allocator, where the next allocation of that size can read
/// them — and this process makes a great many allocations of every size, most
/// of them buffers that are sent somewhere. Wiping on drop closes that, and
/// costs one pass over at most a few megabytes at the end of a paste.
///
/// It is not a guarantee against a debugger, a core dump, or a page that was
/// swapped before the wipe. Those are the operating system's to answer, and
/// this crate does not pretend otherwise.
#[derive(Clone, PartialEq, Eq)]
pub struct Held {
    kind: ClipboardKind,
    bytes: Vec<u8>,
}

impl Held {
    /// Hold this.
    #[must_use]
    pub fn new(kind: ClipboardKind, bytes: Vec<u8>) -> Self {
        Self { kind, bytes }
    }

    /// What kind it is.
    #[must_use]
    pub const fn kind(&self) -> ClipboardKind {
        self.kind
    }

    /// The bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// How many there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Wipe it now rather than waiting for a drop.
    pub fn wipe(&mut self) {
        self.bytes.zeroize();
        self.bytes = Vec::new();
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

/// **Printed as a shape, never as content.**
///
/// A `Debug` that showed the bytes would put the thing this whole crate is
/// careful about into a log line, which is the one place it is hardest to
/// notice and easiest to keep.
impl std::fmt::Debug for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Held")
            .field("kind", &self.kind)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_debug_line_never_carries_the_content() {
        let held = Held::new(ClipboardKind::Text, b"hunter2".to_vec());
        let printed = format!("{held:?}");
        assert!(
            !printed.contains("hunter2"),
            "the content reached a log line: {printed}"
        );
        assert!(
            printed.contains('7'),
            "the length is worth having: {printed}"
        );
    }

    #[test]
    fn wiping_leaves_nothing_behind() {
        let mut held = Held::new(ClipboardKind::Text, b"hunter2".to_vec());
        assert_eq!(held.len(), 7);
        held.wipe();
        assert!(held.is_empty());
        assert_eq!(held.bytes(), b"");
        // And wiping twice is not a problem, because a session end can follow
        // an explicit forget.
        held.wipe();
        assert!(held.is_empty());
    }

    #[test]
    fn a_held_value_keeps_its_kind() {
        let held = Held::new(ClipboardKind::Image, vec![1, 2, 3]);
        assert_eq!(held.kind(), ClipboardKind::Image);
        assert_eq!(held.bytes(), &[1, 2, 3]);
    }
}
