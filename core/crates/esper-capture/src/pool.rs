//! Reused frame buffers.
//!
//! A 1080p frame is 8 MB and a 4K frame is 33 MB. Allocating one per frame at
//! 30 fps means a quarter of a gigabyte a second through the allocator, and the
//! cost is not the bookkeeping — it is the page faults on fresh pages, which
//! land as jitter in exactly the stage whose latency budget is two
//! milliseconds.
//!
//! So buffers are recycled.
//!
//! # What a recycled buffer contains
//!
//! **Whatever the last frame that used it contained.** That is deliberate and
//! it is also a trap, so it is stated rather than left to be discovered:
//!
//! - A backend that writes every pixel of every frame is fine.
//! - A backend that writes only the damaged region is **not** fine with a
//!   pool. It needs one persistent buffer of its own, because the pixels it
//!   leaves alone have to be the previous frame's, not those of whichever
//!   frame last happened to release a buffer.
//!
//! In debug builds a taken buffer is filled with a poison pattern, so a
//! backend that forgets to write part of a frame produces something obviously
//! wrong rather than plausible-looking stale pixels. That is the difference
//! between a test failing and a test passing for the wrong reason.

use std::sync::{Arc, Mutex, Weak};

/// The debug-build poison pattern.
///
/// `0xCD` in all four channels is a mid-grey that is not black, not white, and
/// not any colour a real screen produces evenly across a region — so an
/// unwritten area is visible at a glance rather than mistakable for content.
#[cfg(debug_assertions)]
const POISON: u8 = 0xCD;

/// How many buffers a pool keeps by default.
///
/// Three: one being captured into, one being encoded, one in hand. A fourth
/// would only be held for a pipeline stall, and holding 33 MB against a stall
/// that may not come is the wrong trade.
pub const DEFAULT_KEEP: usize = 3;

/// The shared free list.
type Free = Arc<Mutex<Vec<Vec<u8>>>>;

/// A source of reusable frame buffers.
///
/// One pool per stream. Sharing a pool between monitors would let a buffer
/// that held one screen come back holding another, which matters for any
/// backend that does not overwrite every pixel.
#[derive(Debug, Clone)]
pub struct BufferPool {
    free: Free,
    keep: usize,
}

impl Default for BufferPool {
    fn default() -> Self {
        Self::new(DEFAULT_KEEP)
    }
}

impl BufferPool {
    /// A pool retaining at most `keep` buffers.
    #[must_use]
    pub fn new(keep: usize) -> Self {
        Self {
            free: Arc::new(Mutex::new(Vec::new())),
            keep,
        }
    }

    /// A buffer of at least `len` bytes, whose contents are unspecified.
    #[must_use]
    pub fn take(&self, len: usize) -> Pixels {
        let mut data = self
            .lock()
            .pop()
            .filter(|b| b.capacity() >= len)
            .unwrap_or_default();

        data.clear();
        data.resize(len, 0);

        #[cfg(debug_assertions)]
        data.fill(POISON);

        Pixels {
            data,
            home: Arc::downgrade(&self.free),
            keep: self.keep,
        }
    }

    /// How many buffers are currently free. For tests and for the HUD.
    #[must_use]
    pub fn free_count(&self) -> usize {
        self.lock().len()
    }

    /// A poisoned lock is recovered rather than propagated: the guarded value
    /// is a list of byte buffers with no invariant a panic could have left
    /// half-applied, and refusing to hand out buffers because one unrelated
    /// thread panicked would stop the session for no reason.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Vec<u8>>> {
        self.free.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A frame buffer that returns to its pool when dropped.
pub struct Pixels {
    data: Vec<u8>,
    /// **Weak**, so a live buffer cannot keep its pool alive. A pool is owned
    /// by the capture loop; when that ends, the last frames in flight should
    /// free their memory rather than hand it back to something nobody will
    /// ever take from again.
    home: Weak<Mutex<Vec<Vec<u8>>>>,
    keep: usize,
}

impl std::fmt::Debug for Pixels {
    /// The length, never the contents.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pixels")
            .field("len", &self.data.len())
            .finish_non_exhaustive()
    }
}

impl Pixels {
    /// An unpooled buffer, for tests and for a caller that keeps its own.
    #[must_use]
    pub fn owned(data: Vec<u8>) -> Self {
        Self {
            data,
            home: Weak::new(),
            keep: 0,
        }
    }

    /// An unpooled buffer of `len` zero bytes.
    #[must_use]
    pub fn zeroed(len: usize) -> Self {
        Self::owned(vec![0u8; len])
    }
}

impl std::ops::Deref for Pixels {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.data
    }
}

impl std::ops::DerefMut for Pixels {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl Drop for Pixels {
    fn drop(&mut self) {
        let Some(free) = self.home.upgrade() else {
            return;
        };
        let mut list = free.lock().unwrap_or_else(|p| p.into_inner());
        if list.len() < self.keep {
            list.push(std::mem::take(&mut self.data));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_buffer_comes_back() {
        let pool = BufferPool::new(2);
        assert_eq!(pool.free_count(), 0);
        {
            let b = pool.take(1024);
            assert_eq!(b.len(), 1024);
            assert_eq!(pool.free_count(), 0, "still in use");
        }
        assert_eq!(pool.free_count(), 1);
    }

    /// The point of the pool: a steady stream reuses one allocation.
    #[test]
    fn a_steady_stream_reuses_the_same_allocation() {
        let pool = BufferPool::new(2);
        let first = {
            let b = pool.take(4096);
            b.as_ptr() as usize
        };
        for _ in 0..50 {
            let b = pool.take(4096);
            assert_eq!(
                b.as_ptr() as usize,
                first,
                "a recycled buffer should be the same memory"
            );
            assert_eq!(b.len(), 4096);
        }
    }

    #[test]
    fn the_pool_does_not_grow_past_its_limit() {
        let pool = BufferPool::new(2);
        let a = pool.take(64);
        let b = pool.take(64);
        let c = pool.take(64);
        drop(a);
        drop(b);
        drop(c);
        assert_eq!(pool.free_count(), 2, "the third is freed, not hoarded");
    }

    /// A larger request must not be served a smaller buffer.
    #[test]
    fn a_buffer_too_small_for_the_request_is_not_reused() {
        let pool = BufferPool::new(2);
        drop(pool.take(64));
        assert_eq!(pool.free_count(), 1);

        let big = pool.take(1_000_000);
        assert_eq!(big.len(), 1_000_000);
    }

    /// A resolution change asks for a different size. The buffer must be the
    /// right length whatever it was before, or the frame validation that
    /// follows would reject a frame for a reason the caller cannot see.
    #[test]
    fn a_recycled_buffer_is_always_the_requested_length() {
        let pool = BufferPool::new(4);
        for len in [1024usize, 4096, 512, 8192, 8192, 100] {
            let b = pool.take(len);
            assert_eq!(b.len(), len, "asked for {len}");
            drop(b);
        }
    }

    /// A buffer must not keep its pool alive. When the capture loop ends, the
    /// frames still in flight should free their memory rather than hand it to
    /// something nobody will take from again.
    #[test]
    fn a_buffer_outliving_its_pool_just_frees_itself() {
        let pool = BufferPool::new(2);
        let mut b = pool.take(256);
        drop(pool);
        // Still usable...
        if let Some(first) = b.first_mut() {
            *first = 7;
        }
        assert_eq!(b.first().copied(), Some(7));
        // ...and dropping it must not panic looking for a pool that is gone.
        drop(b);
    }

    /// In a debug build an unwritten region is obviously wrong rather than
    /// plausible. This is the guard against a backend that forgets to fill
    /// part of a frame and a test that passes anyway.
    #[cfg(debug_assertions)]
    #[test]
    fn a_taken_buffer_is_poisoned_in_debug_builds() {
        let pool = BufferPool::new(2);
        {
            let mut b = pool.take(128);
            b.fill(0x42);
        }
        let b = pool.take(128);
        assert!(
            b.iter().all(|x| *x == POISON),
            "a recycled buffer must not come back holding the last frame's \
             pixels in a debug build, or a partial write looks like content"
        );
    }

    #[test]
    fn an_owned_buffer_is_usable_without_a_pool() {
        let mut b = Pixels::zeroed(16);
        assert_eq!(b.len(), 16);
        b.fill(3);
        assert!(b.iter().all(|x| *x == 3));
        assert_eq!(Pixels::owned(vec![1, 2, 3]).as_ref(), &[1, 2, 3]);
    }

    /// A `Debug` that printed the contents would put a frame into a log line.
    #[test]
    fn debug_does_not_print_the_contents() {
        let b = Pixels::zeroed(8_294_400);
        let s = format!("{b:?}");
        assert!(s.len() < 100, "{} characters", s.len());
        assert!(s.contains("8294400"), "{s}");
    }
}
