//! Answering queries about this device.
//!
//! # What it answers, and what it ignores
//!
//! Only questions about `_ownview._udp.local` and about this device's own
//! instance. Everything else on the segment — printers, media players, other
//! people's laptops — is somebody else's business, and answering it would be
//! both wrong and rude.
//!
//! # Announce, and say goodbye
//!
//! A responder announces itself unsolicited when it starts, because a browser
//! that was already open would otherwise not see it until it next asked. And
//! it sends the same records with a TTL of zero when it stops, which is how a
//! machine that has gone stops being offered as connectable — otherwise every
//! browser on the segment keeps it for two minutes and a person clicks a
//! device that is not there.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::name::Name;
use crate::service::{Advertisement, SERVICE};
use crate::wire::{Data, Message, Record, TYPE_ANY, TYPE_PTR, TYPE_SRV, TYPE_TXT};
use crate::{DiscoveryError, socket};

/// The least time between two responses.
///
/// A querier that asks repeatedly, or several queriers at once, must not turn
/// into a flood from us. The documents ask for a second between responses to
/// the same question; this applies it to responses in general, which is
/// stricter and simpler.
const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Announces this device and answers questions about it.
#[derive(Debug)]
pub struct Responder {
    socket: tokio::net::UdpSocket,
    advertisement: Advertisement,
    service: Name,
    instance: Name,
    last_response: Option<Instant>,
    /// Counted rather than logged one by one: a segment with a misbehaving
    /// implementation on it would otherwise fill the log.
    dropped: u64,
    answered: u64,
}

impl Responder {
    /// Open a socket and announce.
    pub async fn start(advertisement: Advertisement) -> Result<Self, DiscoveryError> {
        let socket = socket::open().map_err(|e| DiscoveryError::Socket(e.to_string()))?;
        let service = Name::parse(SERVICE)?;
        let instance = advertisement.instance()?;
        let mut me = Self {
            socket,
            advertisement,
            service,
            instance,
            last_response: None,
            dropped: 0,
            answered: 0,
        };
        me.announce().await?;
        Ok(me)
    }

    /// What is being advertised.
    #[must_use]
    pub const fn advertisement(&self) -> &Advertisement {
        &self.advertisement
    }

    /// How many questions have been answered, and how many packets dropped.
    #[must_use]
    pub const fn counts(&self) -> (u64, u64) {
        (self.answered, self.dropped)
    }

    /// Send the records unsolicited.
    pub async fn announce(&mut self) -> Result<(), DiscoveryError> {
        let message = self.response()?;
        self.send_to(&message, socket::destination()).await
    }

    /// Tell the segment this device is going.
    ///
    /// The same records with a TTL of zero, which is how the protocol spells
    /// "forget this". Best-effort by nature: it is one packet at shutdown, and
    /// the TTL covers the case where it does not arrive.
    pub async fn goodbye(&mut self) -> Result<(), DiscoveryError> {
        let mut message = self.response()?;
        for record in message
            .answers
            .iter_mut()
            .chain(message.additional.iter_mut())
        {
            record.ttl = 0;
        }
        self.send_to(&message, socket::destination()).await
    }

    /// Wait for one packet and answer it if it was for us.
    ///
    /// A malformed packet is counted and dropped: it is a peer's bytes, not a
    /// fault of ours, and a responder that stopped on one could be stopped by
    /// anyone on the segment.
    pub async fn serve_one(&mut self) -> Result<(), DiscoveryError> {
        let mut buffer = vec![0u8; crate::wire::MAX_PACKET];
        let (len, from) = self
            .socket
            .recv_from(&mut buffer)
            .await
            .map_err(|e| DiscoveryError::Socket(e.to_string()))?;
        let Some(packet) = buffer.get(..len) else {
            self.dropped = self.dropped.saturating_add(1);
            return Ok(());
        };
        let Ok(message) = Message::decode(packet) else {
            self.dropped = self.dropped.saturating_add(1);
            return Ok(());
        };
        if message.response {
            return Ok(());
        }
        let wants = message.questions.iter().find(|q| self.concerns(q));
        let Some(question) = wants else {
            return Ok(());
        };

        let now = Instant::now();
        if self
            .last_response
            .is_some_and(|then| now.saturating_duration_since(then) < MIN_INTERVAL)
        {
            return Ok(());
        }
        self.last_response = Some(now);
        self.answered = self.answered.saturating_add(1);

        let reply = self.response()?;
        // A querier that asked for a direct answer gets one; otherwise the
        // group hears it, which is what lets a browser learn about a device
        // somebody else asked about.
        let to = if question.unicast {
            from
        } else {
            socket::destination()
        };
        self.send_to(&reply, to).await
    }

    /// Whether a question is one this device should answer.
    fn concerns(&self, question: &crate::wire::Question) -> bool {
        let about_service =
            question.name == self.service && matches!(question.kind, TYPE_PTR | TYPE_ANY);
        let about_us = question.name == self.instance
            && matches!(question.kind, TYPE_SRV | TYPE_TXT | TYPE_ANY);
        about_service || about_us
    }

    /// The full answer: the pointer, and everything needed to act on it.
    fn response(&self) -> Result<Message, DiscoveryError> {
        let (ptr, extra) = self.advertisement.records()?;
        Ok(Message {
            id: 0,
            response: true,
            questions: Vec::new(),
            answers: vec![ptr],
            additional: extra,
        })
    }

    async fn send_to(&self, message: &Message, to: SocketAddr) -> Result<(), DiscoveryError> {
        let bytes = message.encode();
        self.socket
            .send_to(&bytes, to)
            .await
            .map(|_| ())
            .map_err(|e| DiscoveryError::Socket(e.to_string()))
    }
}

/// The records a responder would send, without a socket.
///
/// For testing what goes on the wire, and for the interoperability check that
/// parses it with something else.
pub fn records_of(advertisement: &Advertisement) -> Result<Vec<Record>, DiscoveryError> {
    let (ptr, extra) = advertisement.records()?;
    let mut all = vec![ptr];
    all.extend(extra);
    Ok(all)
}

/// Whether a record set says a device has gone.
#[must_use]
pub fn is_goodbye(records: &[Record]) -> bool {
    records
        .iter()
        .any(|r| r.ttl == 0 && matches!(r.data, Data::Ptr(_)))
}
