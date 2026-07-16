//! Datagrams onto a connection.

use ownview_stream::{DatagramSink, SinkError};
use ownview_transport::MediaChannel;

/// What the sink has done, for the session's stats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SinkStats {
    pub datagrams: u64,
    pub bytes: u64,
    /// Datagrams the transport would not take.
    ///
    /// Counted rather than propagated as a session failure. A send buffer that
    /// is momentarily full is a slow link, not a broken one, and a video
    /// packet dropped locally is the same thing as one dropped in the network:
    /// the receiver asks for a keyframe and the session continues. Ending the
    /// session instead would turn a stutter into a disconnection.
    pub refused: u64,
}

/// A [`DatagramSink`] over a session's media channel.
#[derive(Debug)]
pub struct SessionSink {
    media: MediaChannel,
    stats: SinkStats,
}

impl SessionSink {
    /// Wrap a session's media channel.
    #[must_use]
    pub fn new(media: MediaChannel) -> Self {
        Self {
            media,
            stats: SinkStats::default(),
        }
    }

    /// What has been sent.
    #[must_use]
    pub const fn stats(&self) -> SinkStats {
        self.stats
    }

    /// Whether the connection is still up.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.media.is_open()
    }

    /// Close the session, saying why.
    pub fn close(&self, reason: ownview_proto::Reason) {
        self.media.close(reason);
    }

    /// Why the peer closed, once it has.
    #[must_use]
    pub fn peer_close_reason(&self) -> Option<(ownview_proto::Reason, String)> {
        self.media.peer_close_reason()
    }
}

impl DatagramSink for SessionSink {
    fn send(&mut self, payload: &[u8]) -> Result<(), SinkError> {
        if !self.media.is_open() {
            return Err(SinkError::Gone("the connection closed".to_owned()));
        }
        // A copy, because quinn queues the buffer and the packetizer reuses
        // its own. It is one allocation and a memcpy of about twelve hundred
        // bytes per datagram — at thirty frames a second that is under two
        // megabytes a second, against an encode measured in milliseconds.
        let owned = bytes::Bytes::copy_from_slice(payload);
        match self.media.send(owned) {
            Ok(()) => {
                self.stats.datagrams = self.stats.datagrams.saturating_add(1);
                self.stats.bytes = self.stats.bytes.saturating_add(payload.len() as u64);
                Ok(())
            }
            Err(e) => {
                // Not an error the session dies of. See `SinkStats::refused`.
                self.stats.refused = self.stats.refused.saturating_add(1);
                tracing::debug!("the transport refused a datagram: {e}");
                Ok(())
            }
        }
    }

    fn max_datagram_size(&self) -> Option<usize> {
        self.media.max_datagram_size()
    }
}
