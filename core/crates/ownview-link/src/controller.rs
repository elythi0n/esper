//! The receiving side of a link.
//!
//! Reads the config the host announced, then reassembles and decodes what
//! arrives while sending back what it needs.

use ownview_decode::decoder::VideoDecoder;
use ownview_proto::{MediaMessage, StreamConfig};
use ownview_stream::{ControllerStats, ControllerStream, Decoded};
use ownview_transport::{Control, MediaChannel};

use crate::LinkError;

/// The receiving side of a media session.
pub struct ControllerLink {
    stream: ControllerStream,
    media: MediaChannel,
    control: Control,
    config: StreamConfig,
}

impl std::fmt::Debug for ControllerLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControllerLink")
            .field("stream", &self.stream)
            .field("encoding", &(self.config.width, self.config.height))
            .finish_non_exhaustive()
    }
}

impl ControllerLink {
    /// Wait for the host's configuration, then start receiving.
    ///
    /// The first message has to be a config. Anything else is a peer that does
    /// not speak this protocol, and refusing by name beats decoding pixels
    /// whose size and codec are guesses.
    pub async fn open(media: MediaChannel, mut control: Control) -> Result<Self, LinkError> {
        let first: MediaMessage = control.recv().await?;
        let MediaMessage::Config(config) = first else {
            return Err(LinkError::NoConfig);
        };
        let stream = ControllerStream::new(&config)?;
        let mut me = Self {
            stream,
            media,
            control,
            config,
        };
        // A fresh controller has no reference picture, so it asks at once.
        //
        // This used to wait for whatever the caller did next to flush the
        // queue — which meant the host sent nothing a decoder could start
        // from until then, and a caller that only ever received would wait for
        // the host's periodic keyframe. Several seconds of black at the start
        // of every session, for want of a flush.
        me.flush_outbound().await?;
        Ok(me)
    }

    /// Attach a decoder.
    ///
    /// **A controller decoding a peer's bitstream must use the sandboxed
    /// decoder.** That is not this crate's decision to make — a loopback
    /// checking its own output has different needs — so it is left to whoever
    /// wires it up, and stated here so the choice is deliberate rather than
    /// whatever the first caller happened to pass.
    #[must_use]
    pub fn with_decoder(mut self, decoder: Box<dyn VideoDecoder>) -> Self {
        self.stream = self.stream.with_decoder(decoder);
        self
    }

    /// What the host said it would send.
    #[must_use]
    pub const fn config(&self) -> &StreamConfig {
        &self.config
    }

    /// Running totals.
    #[must_use]
    pub fn stats(&self) -> ControllerStats {
        self.stream.stats()
    }

    /// How long to wait before deciding the frame in hand is complete.
    ///
    /// `None` when nothing is waiting, so a caller may block indefinitely.
    /// Forwarded from the stream rather than reinvented here: the number is
    /// derived from the stream's own framerate and belongs in one place.
    #[must_use]
    pub fn due_in(&self, now: std::time::Instant) -> Option<std::time::Duration> {
        self.stream.due_in(now)
    }

    /// Act on the deadline if it has passed.
    pub fn tick(&mut self, now: std::time::Instant, out: &mut Vec<Decoded>) {
        self.stream.tick(now, out);
    }

    /// What the arrivals have looked like.
    #[must_use]
    pub fn jitter(&self) -> ownview_stream::JitterStats {
        self.stream.jitter()
    }

    /// Interarrival jitter in milliseconds.
    #[must_use]
    pub fn jitter_ms(&self) -> f64 {
        self.stream.jitter_ms()
    }

    /// What the transport says about the path.
    #[must_use]
    pub fn transport(&self) -> ownview_transport::Stats {
        self.media.stats()
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

    /// Whether a keyframe is outstanding.
    #[must_use]
    pub const fn awaiting_keyframe(&self) -> bool {
        self.stream.awaiting_keyframe()
    }

    /// Wait for the next datagram and take whatever it produced.
    ///
    /// Anything the controller needs to say is sent before returning, because
    /// the one thing it says — a keyframe request — is only useful if it
    /// arrives promptly. Batching it behind a caller's next turn would add a
    /// frame interval to every recovery.
    pub async fn receive(&mut self, out: &mut Vec<Decoded>) -> Result<(), LinkError> {
        let datagram = self.media.recv().await?;
        self.stream.receive(&datagram, out)?;
        self.flush_outbound().await
    }

    /// Send whatever the receiving half has asked for.
    pub async fn flush_outbound(&mut self) -> Result<(), LinkError> {
        for message in self.stream.take_outbound() {
            self.control.send(&message).await?;
        }
        Ok(())
    }

    /// Tell the host what this viewer would prefer.
    pub async fn hint(
        &mut self,
        max_fps: Option<u16>,
        max_long_edge: Option<u32>,
        profile: ownview_proto::Profile,
    ) -> Result<(), LinkError> {
        self.stream.hint(max_fps, max_long_edge, profile);
        self.flush_outbound().await
    }

    /// Handle a message the host sent on the control stream.
    ///
    /// The only one a host sends is a new configuration, which means the
    /// stream's shape changed — a different monitor, or a resolution the
    /// governor moved to.
    pub fn handle(&mut self, message: &MediaMessage) -> Result<(), LinkError> {
        self.stream.handle(message)?;
        if let MediaMessage::Config(config) = message {
            self.config = config.clone();
        }
        Ok(())
    }

    /// Wait for the next control message from the host.
    pub async fn recv_control(&mut self) -> Result<MediaMessage, LinkError> {
        Ok(self.control.recv().await?)
    }

    /// Decide whatever frame is part-assembled, for the end of a stream.
    pub fn flush(&mut self, out: &mut Vec<Decoded>) {
        self.stream.flush(out);
    }
}
