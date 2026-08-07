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
    /// The session's sound, once the host has announced it and a caller has
    /// built a decoder for it.
    ///
    /// `None` until then, which is most sessions: sound needs a permission the
    /// host granted, a device at both ends, and a viewer that wants it.
    sound: Option<esper_stream::ControllerAudio>,
    /// The shape of that sound, kept for anything that has to describe it —
    /// a recording's header names the audio track before the first packet
    /// goes into it.
    sound_shape: Option<(u32, u8)>,
    /// Frames decoded and waiting to be played.
    ///
    /// Held rather than played here, because playing blocks on a sound card
    /// and this is the loop that reads the network. See the engine's playback
    /// thread.
    heard: Vec<esper_audio::Frame>,
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
            sound: None,
            sound_shape: None,
            heard: Vec::new(),
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

    /// What this link carries, and how that was established.
    ///
    /// The only measurement of a link's *capacity* a receiver can make — and
    /// it comes with its provenance, because one of the two ways of arriving
    /// at it is a lower bound rather than a reading: see
    /// [`esper_stream::Capacity`].
    #[must_use]
    pub fn link_capacity(&self) -> Option<esper_stream::Capacity> {
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
        self.route(&datagram, out)?;
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
            Arrived::Datagram(datagram) => self.route(&datagram, out)?,
            Arrived::Said(message) => self.handle(&message)?,
        }
        self.observe_link();
        self.flush_outbound().await
    }

    /// The sound the host has announced, once, if it has.
    ///
    /// See [`esper_stream::ControllerStream::take_audio`]: taken rather than
    /// read, so a caller cannot build two decoders for one announcement.
    #[must_use]
    pub fn take_audio(&mut self) -> Option<esper_proto::AudioConfig> {
        self.stream.take_audio()
    }

    /// Start receiving the sound the host announced.
    ///
    /// **The decoder is passed in** so that this crate does not decide where
    /// it runs: a session must use the sandboxed one, and a loopback checking
    /// its own output must not have to. See [`esper_audio::Decode`].
    ///
    /// # Errors
    ///
    /// If the announced shape and the decoder's disagree, or the shape is not
    /// one a decoder can be built for.
    pub fn with_sound(
        &mut self,
        config: &esper_proto::AudioConfig,
        decoder: Box<dyn esper_audio::Decode>,
    ) -> Result<(), LinkError> {
        self.sound = Some(esper_stream::ControllerAudio::new(config, decoder)?);
        self.sound_shape = Some((config.sample_rate_hz, config.channels));
        Ok(())
    }

    /// Whether this session is receiving sound.
    #[must_use]
    pub const fn has_sound(&self) -> bool {
        self.sound.is_some()
    }

    /// Keep the encoded frames and sound packets, for a recording.
    ///
    /// **One switch for both**, because a recording of the picture without the
    /// sound is not what anybody asked for — and two switches would be two
    /// things to forget.
    pub fn keep_for_recording(&mut self, keeping: bool) {
        self.stream.keep_encoded(keeping);
        if let Some(sound) = self.sound.as_mut() {
            sound.keep_packets(keeping);
        }
    }

    /// Ask the host for a keyframe now.
    ///
    /// **For a recording that starts mid-session**, which cannot begin until
    /// one arrives: the container's header is built from the parameter sets a
    /// keyframe carries. A session that has just opened already has one on the
    /// way, so this is about the later case.
    ///
    /// # Errors
    ///
    /// If the control stream will not carry the request.
    pub async fn ask_for_keyframe(&mut self) -> Result<(), LinkError> {
        self.control
            .send(&MediaMessage::Keyframe(esper_proto::KeyframeRequest {
                reason: esper_proto::KeyframeReason::Joined,
            }))
            .await?;
        Ok(())
    }

    /// Tell the host whether this side is recording the session.
    ///
    /// **Sent rather than queued**, unlike every other thing this side says:
    /// a recording notice that waited for the next flush would be a notice
    /// that arrives after the first frames are already in the file, and the
    /// whole point of it is that the person being recorded knows *while* it
    /// happens.
    ///
    /// # Errors
    ///
    /// If the control stream will not carry it. A caller decides what to do —
    /// the honest answer is to say so and let whoever asked for the recording
    /// choose, because a host that cannot be told is a host that will not show
    /// it.
    pub async fn announce_recording(&mut self, recording: bool) -> Result<(), LinkError> {
        self.control
            .send(&MediaMessage::Recording(esper_proto::Recording {
                recording,
            }))
            .await?;
        Ok(())
    }

    /// The shape of the sound this session is carrying, if any.
    ///
    /// For a recording's header, which has to describe the audio track before
    /// the first packet goes in it.
    #[must_use]
    pub const fn sound_shape(&self) -> Option<(u32, u8)> {
        self.sound_shape
    }

    /// Take the encoded frames kept since the last call.
    pub fn take_encoded(&mut self) -> Vec<esper_stream::Encoded> {
        self.stream.take_encoded()
    }

    /// Take the sound packets kept since the last call.
    pub fn take_audio_packets(&mut self) -> Vec<Vec<u8>> {
        self.sound
            .as_mut()
            .map(esper_stream::ControllerAudio::take_packets)
            .unwrap_or_default()
    }

    /// What has happened to the sound, if there is any.
    #[must_use]
    pub fn sound_stats(&self) -> Option<esper_stream::AudioStats> {
        self.sound
            .as_ref()
            .map(esper_stream::ControllerAudio::stats)
    }

    /// Whether the sound is being decoded in a jail.
    ///
    /// A session says so in its summary, because "sandboxed" is a claim
    /// somebody should be able to check rather than trust.
    #[must_use]
    pub fn sound_is_sandboxed(&self) -> bool {
        self.sound
            .as_ref()
            .is_some_and(esper_stream::ControllerAudio::is_sandboxed)
    }

    /// The frames of sound decoded so far, taken.
    ///
    /// Taken rather than read, because each frame is played once.
    #[must_use]
    pub fn take_sound(&mut self) -> Vec<esper_audio::Frame> {
        std::mem::take(&mut self.heard)
    }

    /// Give one datagram to whichever stream it belongs to.
    ///
    /// **The routing is here and not deeper**, because it is the only place
    /// that knows about both streams. One media channel carries the picture
    /// and the sound, told apart by SSRC and payload type — which is what RTP
    /// is for, and is why a session needs no second channel for sound.
    fn route(&mut self, datagram: &[u8], out: &mut Vec<Decoded>) -> Result<(), LinkError> {
        if let Some(sound) = self.sound.as_mut()
            && let Ok((header, _)) = esper_media::RtpHeader::parse(datagram)
            && header.ssrc == sound.config().ssrc
            && header.payload_type == esper_stream::PAYLOAD_TYPE_OPUS
        {
            self.heard.extend(sound.receive(datagram)?);
            return Ok(());
        }
        self.stream.receive(datagram, out)?;
        Ok(())
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

    /// Ask the host for these screens, as one picture.
    ///
    /// See [`esper_stream::ControllerStream::select_monitors`]: the latest ask
    /// replaces any earlier one that has not gone out yet.
    pub fn select_monitors(&mut self, monitors: Vec<u32>) {
        self.stream.select_monitors(monitors);
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

    /// Whether the host has said the picture is held, if it has said so.
    ///
    /// See [`esper_stream::ControllerStream::take_paused`].
    #[must_use]
    pub fn take_paused(&mut self) -> Option<bool> {
        self.stream.take_paused()
    }

    /// What the host has said this session may now do, if it has said so.
    ///
    /// See [`esper_stream::ControllerStream::take_permissions`]: taken rather
    /// than read, and held until taken, so a narrowing cannot be missed by a
    /// caller that was busy.
    #[must_use]
    pub fn take_permissions(&mut self) -> Option<Vec<esper_proto::Permission>> {
        self.stream.take_permissions()
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
