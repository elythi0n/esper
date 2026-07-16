//! A media session over a real transport.
//!
//! [`ownview_stream`] has the two halves of a media session and deliberately
//! knows nothing about QUIC. This is the layer that gives them a connection:
//! a [`SessionSink`] that puts datagrams on one, and the control exchange
//! around it.
//!
//! # Two threads, and which one does what
//!
//! Capture blocks. `XShmGetImage`, `AcquireNextFrame` and a PipeWire buffer
//! wait are all calls that return when the screen is ready and not before, so
//! the capture-and-encode loop gets a thread of its own — that is the
//! `Capturer` trait's stated contract and the reason it is synchronous.
//!
//! Sending a datagram is *not* blocking: quinn queues it and returns. So the
//! sending thread is the capture thread, and there is no channel between them
//! and no copy beyond the one quinn needs anyway. The async side runs the
//! control stream, which carries about ten messages a second.
//!
//! What that buys is the thing this design keeps insisting on: a slow control
//! handler cannot show up as dropped frames, and a slow frame cannot delay a
//! keyframe request.

pub mod controller;
pub mod host;
pub mod sink;

pub use controller::ControllerLink;
pub use host::{HostLink, HostPump, MIN_PATH, check_path, parts};
pub use sink::{SessionSink, SinkStats};

/// Anything that went wrong running a session over a transport.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error(transparent)]
    Stream(#[from] ownview_stream::StreamError),
    #[error(transparent)]
    Transport(#[from] ownview_transport::SessionError),
    #[error(transparent)]
    Control(#[from] ownview_transport::ControlError),
    /// The peer never sent the config that describes what follows.
    #[error("the host did not announce a stream configuration")]
    NoConfig,
    /// The session's control stream was already taken.
    ///
    /// A caller error rather than a peer's: the `Hello` exchange leaves one
    /// control stream behind and it can only be taken once.
    #[error("this session's control stream has already been taken")]
    NoControl,
    /// Datagrams are not available, so this peer cannot carry video.
    ///
    /// Its own variant because it is a clear thing to tell an operator: the
    /// session is up, the peer is authenticated, and video is impossible.
    #[error("the peer did not enable datagrams, so it cannot carry video")]
    NoDatagrams,
    /// The path will not carry a datagram large enough to be worth using.
    #[error(
        "the path carries at most {found} bytes, below the {needed} this \
         protocol needs; a link this small cannot carry video"
    )]
    PathTooSmall { found: usize, needed: usize },
}
