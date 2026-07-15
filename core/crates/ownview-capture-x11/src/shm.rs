//! A shared memory segment the X server writes frames into.
//!
//! # Why shared memory at all
//!
//! `GetImage` returns the pixels in the reply, over the socket. A 1080p frame
//! is eight megabytes, and at thirty frames a second that is a quarter of a
//! gigabyte a second through a local socket, copied at both ends. MIT-SHM
//! replaces the reply body with a blit the server does directly into memory
//! both processes can see, and the reply becomes a few bytes saying it is
//! done.
//!
//! # Why the server allocates it
//!
//! `ShmCreateSegment` has the server allocate and hand back a file
//! descriptor, which is both simpler and safer than the older
//! `ShmAttach`: that one takes a System V `shmid`, which means calling
//! `shmget` — a libc call, so `unsafe`, in a crate that would rather not have
//! any — and a System V segment is a global name in a namespace any process
//! on the machine can enumerate and attach to.
//!
//! A file descriptor is a capability. It is passed over the X socket to one
//! server and cannot be found by anything that was not handed it, which for a
//! buffer holding a picture of somebody's screen is the difference that
//! matters.
//!
//! # Why the pixels are read rather than mapped
//!
//! Mapping the segment would let the conversion read it in place. It also
//! needs `mmap`, whose safety contract cannot be discharged by a caller — the
//! file could be resized underneath the mapping — so every mapping crate
//! exposes it as `unsafe`, and this crate has none.
//!
//! `pread` costs a copy. That copy was going to happen anyway: the frames this
//! backend hands out are owned buffers, and the segment has to keep holding
//! the whole screen so that the parts nothing damaged are still the previous
//! frame's. So the copy is not overhead, it is the copy that had to happen,
//! done by the kernel.

use std::os::fd::OwnedFd;

use x11rb::connection::Connection;
use x11rb::protocol::shm::{self, ConnectionExt as _};

/// The MIT-SHM version that can pass file descriptors.
const NEEDS_MAJOR: u16 = 1;
const NEEDS_MINOR: u16 = 2;

/// Why shared memory is not available.
#[derive(Debug, thiserror::Error)]
pub enum ShmError {
    /// The extension is missing or too old to pass descriptors.
    #[error("MIT-SHM {found} cannot pass file descriptors; {NEEDS_MAJOR}.{NEEDS_MINOR} is needed")]
    TooOld { found: String },
    /// The X server refused a request.
    #[error("X11: {0}")]
    X11(String),
    /// The segment was created but no descriptor came with it.
    #[error("the X server created a segment without returning a descriptor")]
    NoDescriptor,
    /// Reading the segment failed.
    #[error("reading the shared segment: {0}")]
    Read(String),
    /// A read asked for bytes outside the segment.
    #[error("a read of {len} bytes at {offset} does not fit a {size}-byte segment")]
    OutOfRange {
        offset: usize,
        len: usize,
        size: usize,
    },
}

/// Whether this server can pass a segment descriptor, and what it reports.
pub fn probe<C: Connection>(conn: &C) -> Result<(u16, u16), ShmError> {
    let v = conn
        .shm_query_version()
        .map_err(|e| ShmError::X11(e.to_string()))?
        .reply()
        .map_err(|e| ShmError::X11(e.to_string()))?;
    let ok = v.major_version > NEEDS_MAJOR
        || (v.major_version == NEEDS_MAJOR && v.minor_version >= NEEDS_MINOR);
    if !ok {
        return Err(ShmError::TooOld {
            found: format!("{}.{}", v.major_version, v.minor_version),
        });
    }
    Ok((v.major_version, v.minor_version))
}

/// A segment the server writes into and this process reads out of.
#[derive(Debug)]
pub struct Segment {
    seg: shm::Seg,
    fd: OwnedFd,
    size: usize,
}

impl Segment {
    /// Ask the server for a segment of `size` bytes.
    pub fn create<C: Connection>(conn: &C, size: usize) -> Result<Self, ShmError> {
        let bytes = u32::try_from(size).map_err(|_| ShmError::OutOfRange {
            offset: 0,
            len: size,
            size: u32::MAX as usize,
        })?;
        let seg = conn
            .generate_id()
            .map_err(|e| ShmError::X11(e.to_string()))?;
        // `read_only: false` — the server is the one writing the pixels.
        let reply = shm::create_segment(conn, seg, bytes, false)
            .map_err(|e| ShmError::X11(e.to_string()))?
            .reply()
            .map_err(|e| ShmError::X11(e.to_string()))?;
        let fd = reply.shm_fd;
        Ok(Self { seg, fd, size })
    }

    /// The identifier to name this segment in a request.
    #[must_use]
    pub const fn id(&self) -> shm::Seg {
        self.seg
    }

    /// How large it is.
    #[must_use]
    pub const fn size(&self) -> usize {
        self.size
    }

    /// Copy `buf.len()` bytes from `offset` in the segment into `buf`.
    ///
    /// Short reads are looped rather than treated as an error: `pread` is
    /// allowed to return fewer bytes than asked for, and a partial frame that
    /// looks complete is worse than a slow one.
    pub fn read_into(&self, offset: usize, buf: &mut [u8]) -> Result<(), ShmError> {
        let end = offset.checked_add(buf.len()).ok_or(ShmError::OutOfRange {
            offset,
            len: buf.len(),
            size: self.size,
        })?;
        if end > self.size {
            return Err(ShmError::OutOfRange {
                offset,
                len: buf.len(),
                size: self.size,
            });
        }
        let mut done = 0usize;
        while done < buf.len() {
            let at = u64::try_from(offset.saturating_add(done)).unwrap_or(u64::MAX);
            let rest = buf.get_mut(done..).unwrap_or_default();
            let n =
                rustix::io::pread(&self.fd, rest, at).map_err(|e| ShmError::Read(e.to_string()))?;
            if n == 0 {
                return Err(ShmError::Read(format!(
                    "the segment ended after {done} of {} bytes",
                    buf.len()
                )));
            }
            done = done.saturating_add(n);
        }
        Ok(())
    }

    /// Tell the server to forget the segment.
    ///
    /// Not in `Drop`, because detaching needs the connection and a segment
    /// outliving its connection has nothing to detach from. A dropped
    /// `Segment` still closes its descriptor, which is what frees the memory.
    pub fn detach<C: Connection>(&self, conn: &C) {
        let _ = shm::detach(conn, self.seg);
    }
}
