//! The receiving side of a link.
//!
//! Reads the config the host announced, then reassembles and decodes what
//! arrives while sending back what it needs.

use esper_decode::decoder::VideoDecoder;
use esper_proto::{MediaMessage, StreamConfig};
use esper_stream::{ControllerStats, ControllerStream, Decoded};
use esper_transport::{Control, MediaChannel};

use crate::LinkError;

/// The receiving side of a media session.
/// How often the transport's statistics are worth re-reading.
///
/// Fifty milliseconds: twenty samples a second, against a ladder that judges
/// what it sees over windows of one. Frequent enough that the smallest round
/// trip in a window is really the smallest, and rare enough that a link
/// delivering five hundred datagrams a second does not spend its time taking
/// the transport's lock.
const OBSERVE_EVERY: std::time::Duration = std::time::Duration::from_millis(50);

pub struct ControllerLink {
    stream: ControllerStream,
    media: MediaChannel,
    control: Control,
    config: StreamConfig,
    /// An announcement the caller has not seen yet.
    announced: Option<StreamConfig>,
    /// When the transport's statistics were last read. See
    /// [`ControllerLink::observe_link`].
    observed_at: Option<std::time::Instant>,
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
            // The opening config is not an announcement: the caller has it
            // already, as the return of this function.
            announced: None,
            observed_at: None,
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

    /// Tell the stream what the round trip is, which is two things at once.
    ///
    /// It is part of what a keyframe request costs before it can become a
    /// picture, and it is the *signal* that a queue is filling — the one loss
    /// cannot give, because a bufferbloated link hands back every byte it is
    /// given and simply hands it back late. The transport is the only thing
    /// that knows the number, so the link is where the two are joined.
    ///
    /// **Called on every turn, not only when the stream is quiet.** It used to
    /// be read in the caller's timeout branch, which is exactly the branch a
    /// busy link does not reach — so on the link where the signal matters most
    /// it was sampled least.
    ///
    /// Self-limiting, because a stats snapshot takes a lock inside the
    /// transport and a datagram can arrive five hundred times a second, while
    /// the thing reading this judges it over windows of a second.
    pub fn observe_link(&mut self) {
        let now = std::time::Instant::now();
        if let Some(last) = self.observed_at
            && now.saturating_duration_since(last) < OBSERVE_EVERY
        {
            return;
        }
        self.observed_at = Some(now);
        let rtt = self.media.stats().rtt;
        self.stream.set_round_trip(rtt);
    }

    /// What the bloat ladder is asking the host for, if anything.
    #[must_use]
    pub fn link_fps(&self) -> Option<u16> {
        self.stream.link_fps()
    }

    /// How fast this link carried a burst, in kbit/s.
    ///
    /// The only measurement of a link's *capacity* a receiver can make: see
    /// [`esper_stream::ControllerStream::link_capacity`].
    #[must_use]
    pub fn link_capacity(&self) -> Option<f64> {
        self.stream.link_capacity()
    }

    /// The round trip this path costs with nothing queued on it.
    #[must_use]
    pub fn baseline_round_trip(&self) -> Option<std::time::Duration> {
        self.stream.baseline_round_trip()
    }

    /// What the link ladder is asking the host for, if anything.
    ///
    /// So a session can say why it looks softer than it started: the honest
    /// answer is "the link was losing frames and this asked for smaller
    /// ones", and without it the only visible fact is a blurrier picture.
    #[must_use]
    pub fn link_ceiling(&self) -> Option<u32> {
        self.stream.link_ceiling()
    }

    /// What the arrivals have looked like.
    #[must_use]
    pub fn jitter(&self) -> esper_stream::JitterStats {
        self.stream.jitter()
    }

    /// Interarrival jitter in milliseconds.
    #[must_use]
    pub fn jitter_ms(&self) -> f64 {
        self.stream.jitter_ms()
    }

    /// What the transport says about the path.
    #[must_use]
    pub fn transport(&self) -> esper_transport::Stats {
        self.media.stats()
    }

    /// Close the session, saying why.
    pub fn close(&self, reason: esper_proto::Reason) {
        self.media.close(reason);
    }

    /// Why the peer closed, once it has.
    #[must_use]
    pub fn peer_close_reason(&self) -> Option<(esper_proto::Reason, String)> {
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

    /// Wait for whichever comes first: a datagram, or something the host says.
    ///
    /// **Both, not just datagrams.** A host that switched monitor says so on
    /// the control stream, and a controller reading only datagrams would go on
    /// reassembling a stream that no longer exists — every packet rejected for
    /// the wrong SSRC, the window frozen on the last picture of the old
    /// screen, and nothing to say why.
    ///
    /// Sound because [`esper_transport::Control::recv`] is cancel-safe and
    /// so is reading a datagram: whichever loses keeps whatever it had read.
    /// The send in `flush_outbound` happens after the select has resolved,
    /// where nothing can cancel it half-written.
    pub async fn pump(&mut self, out: &mut Vec<Decoded>) -> Result<(), LinkError> {
        enum Arrived {
            Datagram(bytes::Bytes),
            Said(MediaMessage),
        }
        let arrived = tokio::select! {
            datagram = self.media.recv() => Arrived::Datagram(datagram?),
            said = self.control.recv::<MediaMessage>() => Arrived::Said(said?),
        };
        match arrived {
            Arrived::Datagram(datagram) => self.stream.receive(&datagram, out)?,
            Arrived::Said(message) => self.handle(&message)?,
        }
        self.observe_link();
        self.flush_outbound().await
    }

    /// Send whatever the receiving half has asked for.
    pub async fn flush_outbound(&mut self) -> Result<(), LinkError> {
        for message in self.stream.take_outbound() {
            self.control.send(&message).await?;
        }
        Ok(())
    }

    /// Ask the host for a different monitor.
    ///
    /// Queued rather than sent, like every other thing this side says, and
    /// carried out by the next [`Self::flush_outbound`] or [`Self::pump`]. The
    /// answer is a new config on the control stream, or a refusal — the host
    /// owns the screen and decides.
    pub fn select_monitor(&mut self, monitor: u32) {
        self.stream.select_monitor(monitor);
    }

    /// Tell the host what this viewer would prefer.
    pub async fn hint(
        &mut self,
        max_fps: Option<u16>,
        max_long_edge: Option<u32>,
        profile: esper_proto::Profile,
    ) -> Result<(), LinkError> {
        self.stream.hint(max_fps, max_long_edge, profile);
        self.flush_outbound().await
    }

    /// And how sharp a picture it will settle for.
    pub async fn want_sharpness(
        &mut self,
        sharpness: Option<esper_proto::Sharpness>,
    ) -> Result<(), LinkError> {
        self.stream.want_sharpness(sharpness);
        self.flush_outbound().await
    }

    /// The host's latest announcement, once, if it has made one.
    ///
    /// **Separate from comparing configs**, because the answer to a refused
    /// request is the config the host is *keeping* — identical to the one
    /// already held. A caller watching for a change would see none and report
    /// nothing, which is the silence the refusal exists to break.
    pub fn take_announcement(&mut self) -> Option<StreamConfig> {
        self.announced.take()
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
            self.announced = Some(config.clone());
        }
        Ok(())
    }

    /// Everything the host has said about the pointer since this was last
    /// called.
    pub fn take_cursor(&mut self) -> Vec<esper_proto::CursorMessage> {
        self.stream.take_cursor()
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
