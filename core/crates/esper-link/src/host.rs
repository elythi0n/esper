//! The sending side of a link.
//!
//! Announces what it will send, then runs the pipeline on a thread of its own
//! while the control stream is served on the async side.

use std::time::Duration;

use esper_pipeline::pipeline::Pipeline;
use esper_proto::{MediaMessage, StreamConfig};
use esper_stream::resume::Parked;
use esper_stream::{Handled, HostStep, HostStream};
use esper_transport::{Control, MediaChannel, Session};

use crate::{LinkError, SessionSink};

/// The smallest path worth attempting.
///
/// **The packetizer's floor, not a second opinion about it.** Two constants
/// that have to agree is a bug waiting to happen, and it happened: both were
/// 1200, both were measured against the wrong quantity, and both were wrong in
/// the same direction. See [`esper_media::MIN_MTU`] for what the number is
/// and why it is expressed in datagram payload rather than path MTU.
pub const MIN_PATH: usize = esper_media::MIN_MTU;

/// The sending side of a media session.
pub struct HostLink {
    stream: HostStream,
    sink: SessionSink,
    control: Control,
}

impl std::fmt::Debug for HostLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostLink")
            .field("stream", &self.stream)
            .field("sent", &self.sink.stats())
            .finish_non_exhaustive()
    }
}

impl HostLink {
    /// Build a sender for an open session.
    ///
    /// The MTU comes from the transport rather than a guess, because the
    /// transport is the only thing that knows what the path actually carries —
    /// and it is *checked*: a peer with datagrams disabled or a path too small
    /// to be worth using is told so, rather than discovering it as every frame
    /// being refused.
    pub fn new(
        media: &MediaChannel,
        control: Control,
        pipeline: Pipeline,
    ) -> Result<Self, LinkError> {
        let mtu = check_path(media.max_datagram_size())?;
        let stream = HostStream::new(pipeline, mtu)?;
        Ok(Self {
            stream,
            sink: SessionSink::new(media.clone()),
            control,
        })
    }

    /// Put a parked session back on a new transport.
    ///
    /// The path is checked first and the device second, so a refusal hands the
    /// [`Parked`] session back untouched: a resume that cannot happen must not
    /// destroy the session it was trying to save.
    // The large `Err` carries that session back. See `Parked::resume`.
    #[allow(clippy::result_large_err)]
    pub fn resume(
        media: &MediaChannel,
        control: Control,
        parked: Parked,
        device: &[u8; 32],
    ) -> Result<Self, (Parked, LinkError)> {
        let mtu = match check_path(media.max_datagram_size()) {
            Ok(mtu) => mtu,
            Err(e) => return Err((parked, e)),
        };
        match parked.resume(device, mtu) {
            Ok(stream) => Ok(Self {
                stream,
                sink: SessionSink::new(media.clone()),
                control,
            }),
            Err((parked, refused)) => Err((parked, LinkError::Resume(refused))),
        }
    }

    /// The configuration this host will announce.
    #[must_use]
    pub fn config(&self) -> StreamConfig {
        self.stream.config()
    }

    /// What has been sent.
    #[must_use]
    pub fn sent(&self) -> crate::SinkStats {
        self.sink.stats()
    }

    /// The sending half, for a caller that wants to drive it itself.
    #[must_use]
    pub const fn stream(&self) -> &HostStream {
        &self.stream
    }

    /// Tell the controller what is coming.
    ///
    /// Sent before any datagram. A controller that received pixels first would
    /// not know their size, their codec or which monitor they are — and every
    /// buffer on its side is sized from this.
    pub async fn announce(&mut self) -> Result<StreamConfig, LinkError> {
        let config = self.stream.config();
        self.control
            .send(&MediaMessage::Config(config.clone()))
            .await?;
        Ok(config)
    }

    /// Tell the controller this session may do less than it was granted.
    ///
    /// **Not a revocation** — the grant stands, and the next session gets it
    /// again. This is for a host that cannot deliver what it agreed to: one
    /// with nothing to inject input through has a session that is view-only in
    /// fact, whatever the permissions say, and a controller that was not told
    /// would offer a cursor that does nothing.
    ///
    /// The complete set, never a delta, for the reason
    /// [`esper_proto::PermissionChange`] gives: two ends that disagree about
    /// the current state drift further apart with every message.
    pub async fn narrowed_to(
        &mut self,
        granted: Vec<esper_proto::Permission>,
    ) -> Result<(), LinkError> {
        self.control
            .send(&MediaMessage::Permissions(esper_proto::PermissionChange {
                granted,
            }))
            .await?;
        Ok(())
    }

    /// Run one turn of capture, encode and send.
    ///
    /// Synchronous, and meant to be called from a thread of its own: capture
    /// blocks by design, so an async task doing this would stall its executor
    /// for as long as the screen took to change.
    pub fn pump(&mut self, timeout: Duration) -> Result<HostStep, LinkError> {
        Ok(self.stream.step(timeout, &mut self.sink)?)
    }

    /// Whether the connection is still up.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.sink.is_open()
    }

    /// Close the session, saying why.
    pub fn close(&self, reason: esper_proto::Reason) {
        self.sink.close(reason);
    }

    /// Act on one message from the controller, and say what it must be told.
    ///
    /// Whatever comes back has to reach the controller. See
    /// [`esper_stream::Handled`].
    pub fn handle(&mut self, message: &MediaMessage) -> Result<Handled, LinkError> {
        Ok(self.stream.handle(message)?)
    }

    /// Wait for the next message from the controller.
    pub async fn recv(&mut self) -> Result<MediaMessage, LinkError> {
        Ok(self.control.recv().await?)
    }

    /// Split into the parts two threads need.
    ///
    /// The pump goes to a blocking thread and the control stream stays on the
    /// async side. Splitting rather than sharing is what makes it impossible
    /// for the sending thread to touch the control stream — and a compile
    /// error is a better guarantee than a comment asking it not to.
    #[must_use]
    pub fn split(self) -> (HostPump, Control) {
        (
            HostPump {
                stream: self.stream,
                sink: self.sink,
            },
            self.control,
        )
    }
}

/// The sending half, on its own thread.
#[derive(Debug)]
pub struct HostPump {
    stream: HostStream,
    sink: SessionSink,
}

impl HostPump {
    /// What the controller has asked the picture to be capped at, if
    /// anything. See [`esper_stream::HostStream::asked_ceiling`].
    #[must_use]
    pub const fn asked_ceiling(&self) -> Option<u32> {
        self.stream.asked_ceiling()
    }

    /// What the controller said it was doing. See
    /// [`esper_stream::HostStream::profile`].
    #[must_use]
    pub const fn profile(&self) -> esper_proto::Profile {
        self.stream.profile()
    }

    /// Run one turn.
    pub fn pump(&mut self, timeout: Duration) -> Result<HostStep, LinkError> {
        Ok(self.stream.step(timeout, &mut self.sink)?)
    }

    /// Act on a message the async side received.
    ///
    /// Whatever comes back has to go to the async side and out on the control
    /// stream: the sending thread has no way to send it itself.
    pub fn handle(&mut self, message: &MediaMessage) -> Result<Handled, LinkError> {
        Ok(self.stream.handle(message)?)
    }

    /// What has been sent.
    #[must_use]
    pub fn sent(&self) -> crate::SinkStats {
        self.sink.stats()
    }

    /// Whether the connection is still up.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.sink.is_open()
    }

    /// Close the session, saying why.
    pub fn close(&self, reason: esper_proto::Reason) {
        self.sink.close(reason);
    }

    /// Why the peer closed, once it has.
    #[must_use]
    pub fn peer_close_reason(&self) -> Option<(esper_proto::Reason, String)> {
        self.sink.peer_close_reason()
    }

    /// What this host is sending, for a re-announcement after a change.
    #[must_use]
    pub fn config(&self) -> StreamConfig {
        self.stream.config()
    }

    /// Keep the session alive without a transport, for `owner` to come back to.
    ///
    /// Consumes the pump because the sink is the part that has gone: what is
    /// worth keeping is the pipeline, and the sequence numbering so the stream
    /// reads as one interrupted rather than two.
    #[must_use]
    pub fn park(self, owner: [u8; 32], now: std::time::Instant) -> Parked {
        Parked::park(self.stream, owner, now)
    }
}

/// Whether a path can carry video, and how much of it at a time.
///
/// Pure, so it can be tested without a network that has an unusual MTU — and
/// the two things it refuses are worth refusing separately. A peer with
/// datagrams disabled and a path too small to use are both "video is
/// impossible", and an operator needs to be told which.
pub fn check_path(reported: Option<usize>) -> Result<usize, LinkError> {
    let mtu = reported.ok_or(LinkError::NoDatagrams)?;
    if mtu < MIN_PATH {
        return Err(LinkError::PathTooSmall {
            found: mtu,
            needed: MIN_PATH,
        });
    }
    Ok(mtu)
}

/// A session's media channel and control stream, taken apart.
///
/// A convenience for the common shape: take the control stream the `Hello`
/// exchange left behind, and a media channel for the sending thread.
pub fn parts(session: &mut Session) -> Result<(MediaChannel, Control), LinkError> {
    let media = session.media_channel();
    let control = session.take_control().ok_or(LinkError::NoControl)?;
    Ok((media, control))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_peer_without_datagrams_cannot_carry_video() {
        assert!(matches!(check_path(None), Err(LinkError::NoDatagrams)));
    }

    #[test]
    fn a_path_too_small_is_refused_with_both_numbers() {
        for found in [0usize, 1, 576, MIN_PATH - 1] {
            match check_path(Some(found)) {
                Err(LinkError::PathTooSmall { found: f, needed }) => {
                    assert_eq!(f, found);
                    assert_eq!(needed, MIN_PATH);
                }
                other => panic!("a {found}-byte path gave {other:?}"),
            }
        }
    }

    #[test]
    fn a_usable_path_is_reported_as_it_came() {
        // Passed through rather than clamped: the transport knows the path and
        // the packetizer must fragment to what it actually said, not to a
        // rounder number.
        for mtu in [MIN_PATH, 1382, 1500, 9000] {
            assert_eq!(check_path(Some(mtu)).ok(), Some(mtu));
        }
    }
}
