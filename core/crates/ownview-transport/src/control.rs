//! Framed control streams.
//!
//! A control stream is long-lived and carries many messages, so it cannot be
//! read with `read_to_end`: that waits for the sender to *finish* the stream,
//! which a stream still in use never does. Frames are read one at a time —
//! length prefix, then exactly that many bytes.
//!
//! The length is validated **before** the payload buffer is allocated. A peer
//! claiming a four-gigabyte frame must cost a rejected header, not four
//! gigabytes, and at this layer the peer may not yet be authenticated.

use ownview_proto::{FrameError, MAX_FRAME_LEN};
use quinn::{RecvStream, SendStream};

/// Size of the big-endian `u32` length prefix.
const LEN_PREFIX: usize = 4;

/// A bidirectional control stream carrying length-prefixed CBOR frames.
pub struct Control {
    send: SendStream,
    recv: RecvStream,
}

impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Control")
            .field("send_id", &self.send.id())
            .field("recv_id", &self.recv.id())
            .finish()
    }
}

/// What can go wrong on a control stream.
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    /// The declared frame length exceeds [`MAX_FRAME_LEN`]. Rejected before
    /// anything is allocated.
    #[error("frame length {len} exceeds maximum {MAX_FRAME_LEN}")]
    TooLarge { len: u32 },

    /// A zero-length frame carries no CBOR and is always a protocol error.
    #[error("frame length is zero")]
    Empty,

    /// The peer finished or reset the stream mid-frame.
    #[error("stream ended mid-frame: wanted {wanted} bytes, got {got}")]
    Truncated { wanted: usize, got: usize },

    /// The stream was closed cleanly with no frame in progress. Not an error in
    /// itself — it is how a peer says it is done — but the caller must decide.
    #[error("stream finished")]
    Finished,

    /// Framing or CBOR failure.
    #[error(transparent)]
    Frame(#[from] FrameError),

    /// Transport failure.
    #[error("stream: {0}")]
    Stream(String),
}

impl ControlError {
    /// The wire reason code to close with.
    #[must_use]
    pub const fn reason(&self) -> ownview_proto::Reason {
        match self {
            Self::TooLarge { .. } => ownview_proto::Reason::FRAME_TOO_LARGE,
            Self::Empty | Self::Truncated { .. } | Self::Frame(_) => {
                ownview_proto::Reason::MALFORMED_FRAME
            }
            Self::Finished | Self::Stream(_) => ownview_proto::Reason::ALL_TIERS_EXHAUSTED,
        }
    }

    /// Whether this is the peer closing cleanly rather than misbehaving.
    #[must_use]
    pub const fn is_clean_close(&self) -> bool {
        matches!(self, Self::Finished)
    }
}

impl Control {
    /// Wrap a bidirectional stream pair.
    #[must_use]
    pub const fn new(send: SendStream, recv: RecvStream) -> Self {
        Self { send, recv }
    }

    /// Encode and send one message.
    pub async fn send<T: serde::Serialize>(&mut self, value: &T) -> Result<(), ControlError> {
        // Encode the payload, then write the length and the payload. Using
        // `write_frame` into a Vec would do the same thing with an extra copy,
        // and would hide which layer owns the framing.
        let payload = ownview_proto::encode(value)?;
        let len =
            u32::try_from(payload.len()).map_err(|_| ControlError::TooLarge { len: u32::MAX })?;

        self.send
            .write_all(&len.to_be_bytes())
            .await
            .map_err(|e| ControlError::Stream(e.to_string()))?;
        self.send
            .write_all(&payload)
            .await
            .map_err(|e| ControlError::Stream(e.to_string()))
    }

    /// Read one message.
    ///
    /// Reads exactly one frame and leaves the stream positioned at the start of
    /// the next, so a long-lived stream can be read in a loop.
    pub async fn recv<T: serde::de::DeserializeOwned>(&mut self) -> Result<T, ControlError> {
        let mut header = [0u8; LEN_PREFIX];
        match self.read_exact(&mut header).await {
            Ok(()) => {}
            // Nothing at all arrived: the peer finished the stream between
            // frames, which is a clean close rather than a truncated frame.
            Err(ControlError::Truncated { got: 0, .. }) => return Err(ControlError::Finished),
            Err(e) => return Err(e),
        }

        let len = u32::from_be_bytes(header);

        // Validate before allocating. The peer may not be authenticated yet.
        if len == 0 {
            return Err(ControlError::Empty);
        }
        if len > MAX_FRAME_LEN {
            return Err(ControlError::TooLarge { len });
        }

        let mut payload = vec![0u8; len as usize];
        self.read_exact(&mut payload).await?;

        // `decode`, not `read_frame`: the length prefix has already been read,
        // so what remains is a bare CBOR body. `read_frame` would read the first
        // four bytes of that body as a length and fail with a nonsense number.
        ownview_proto::decode(&payload).map_err(ControlError::from)
    }

    /// Finish the send side, telling the peer no more frames are coming.
    pub fn finish(&mut self) -> Result<(), ControlError> {
        self.send
            .finish()
            .map_err(|e| ControlError::Stream(e.to_string()))
    }

    /// Fill `buf` exactly, reporting the shortfall on truncation.
    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ControlError> {
        let wanted = buf.len();
        let mut got = 0usize;

        while got < wanted {
            let rest = buf
                .get_mut(got..)
                .ok_or(ControlError::Truncated { wanted, got })?;
            match self.recv.read(rest).await {
                Ok(Some(0)) | Ok(None) => return Err(ControlError::Truncated { wanted, got }),
                Ok(Some(n)) => got = got.saturating_add(n),
                Err(e) => return Err(ControlError::Stream(e.to_string())),
            }
        }
        Ok(())
    }
}
