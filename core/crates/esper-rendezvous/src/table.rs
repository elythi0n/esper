//! Who is waiting at the broker, and who they will meet.
//!
//! Pure, and separate from the server, because this is where the broker's
//! decisions are: who may occupy a place, how many places there are, and who
//! gets introduced to whom. A server that also owned this would only be
//! testable through a socket.
//!
//! **Nothing here survives a restart, by construction.** There is no file, no
//! database and no serialisation — the table is a `Vec` in a process, and a
//! broker that is restarted has forgotten everybody. That is a stated
//! property of the design and it is cheaper to have than to claim.

use std::net::{IpAddr, SocketAddr};

use esper_crypto::identity::KEY_LEN;
use tokio::sync::mpsc::Sender;

use crate::wire::{Answer, Introduction, Meet, Refusal};

/// How many peers one broker will hold at once.
///
/// A broker is internet-facing and unauthenticated, so the table is the
/// resource an attacker reaches first. Ten thousand peers is far more than the
/// handful a personal broker sees and small enough that the memory is trivial;
/// an operator with more than that has a different problem.
pub const MAX_WAITING: usize = 10_000;

/// How many peers one address may have waiting.
///
/// A household behind one address might have a few machines; sixteen is
/// generous. Without this, one host can fill the table on its own.
pub const MAX_PER_ADDRESS: usize = 16;

/// Somebody waiting to be introduced.
#[derive(Debug)]
struct Waiting {
    /// What they asked for.
    meet: Meet,
    /// Where they connected from, as the broker saw it.
    ///
    /// Two jobs: the per-address limit's key, and the reflexive candidate the
    /// other side is told about — the one address this peer cannot know about
    /// itself.
    from: SocketAddr,
    /// Which connection this is.
    id: u64,
    /// The address that peer reached this broker on.
    ///
    /// Not where it is — that is `from` — but which of *our* addresses it
    /// used, which is the only one known to work from where it is standing. A
    /// broker listening on every address has no other way to say which to use
    /// for a relay.
    local: Option<IpAddr>,
    /// How to reach the task holding that connection.
    ///
    /// The table does the delivering, which is why it holds these: an
    /// introduction that arrives at one side and not the other is a hole punch
    /// with one fist, and "tell both" is a decision about the meeting rather
    /// than about a socket.
    to_peer: Sender<Answer>,
}

/// Everyone currently waiting.
#[derive(Debug, Default)]
pub struct Table {
    waiting: Vec<Waiting>,
    next_id: u64,
}

/// What happened when somebody arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arrival {
    /// Held, waiting for somebody to name them.
    Waiting {
        /// The place they now occupy.
        id: u64,
    },
    /// Introduced, because the other side was already here.
    ///
    /// The peers already waiting have **already been told** through their own
    /// channels by the time this returns; this is what the arriving peer
    /// hears.
    Met {
        /// The place they occupy, since a host may be met many times.
        id: u64,
        /// What to send the arriving peer.
        theirs: Introduction,
    },
    /// Not held. The peer is told why.
    Refused(Refusal),
}

impl Table {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many peers are waiting.
    #[must_use]
    pub fn len(&self) -> usize {
        self.waiting.len()
    }

    /// Whether anybody is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }

    /// Take somebody's request.
    ///
    /// The caller has already checked that it is well formed, signed, for this
    /// broker and not stale — those are facts about the message, and this is
    /// about the table.
    pub fn arrive(
        &mut self,
        meet: Meet,
        from: SocketAddr,
        local: Option<IpAddr>,
        to_peer: Sender<Answer>,
    ) -> Arrival {
        if self.waiting.len() >= MAX_WAITING {
            return Arrival::Refused(Refusal::Full);
        }
        if self
            .waiting
            .iter()
            .filter(|w| w.from.ip() == from.ip())
            .count()
            >= MAX_PER_ADDRESS
        {
            return Arrival::Refused(Refusal::TooManyFromYou);
        }

        // A device that connects twice replaces its earlier place rather than
        // holding two. Otherwise a host that reconnects on a flapping link
        // accumulates places until it hits its own per-address limit, and the
        // stale ones hand out addresses that no longer work.
        self.waiting.retain(|w| w.meet.me != meet.me);

        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);

        // **Everybody here who names this peer and is named by it, told now.**
        // `try_send` rather than an await: a peer that has stopped reading its
        // own introductions must not be able to hold up an arrival, and a full
        // queue means it is gone. So it is dropped, which also means the
        // arriving peer is not handed addresses belonging to a connection that
        // is no longer being served.
        let mut gone = Vec::new();
        let mut theirs = None;
        for w in &self.waiting {
            if !w.meet.mutual_with(&meet) {
                continue;
            }
            if w.to_peer
                .try_send(Answer::Introduced(meet.introduction(Some(from))))
                .is_err()
            {
                gone.push(w.id);
                continue;
            }
            // What the arriving peer hears: the first side that took it. One
            // rather than all, because a controller looking for its host wants
            // the host — and a host met by two controllers is told about each
            // as it arrives.
            if theirs.is_none() {
                theirs = Some(w.meet.introduction(Some(w.from)));
            }
        }
        self.waiting.retain(|w| !gone.contains(&w.id));

        self.waiting.push(Waiting {
            meet,
            from,
            id,
            local,
            to_peer,
        });

        match theirs {
            Some(theirs) => Arrival::Met { id, theirs },
            None => Arrival::Waiting { id },
        }
    }

    /// Forget a place, because its connection has gone.
    pub fn leave(&mut self, id: u64) {
        self.waiting.retain(|w| w.id != id);
    }

    /// The peer waiting under this key, if it is here and will meet `asker`.
    ///
    /// Mutual again: a relay is a path between two devices that have named
    /// each other, and asking for one is not a way to reach somebody who has
    /// not asked for you.
    #[must_use]
    pub fn mutual_peer(
        &self,
        asker: &[u8; KEY_LEN],
        wanted: &[u8; KEY_LEN],
    ) -> Option<(Sender<Answer>, Option<IpAddr>)> {
        let me = self.waiting.iter().find(|w| w.meet.me == *asker)?;
        let them = self.waiting.iter().find(|w| w.meet.me == *wanted)?;
        (me.meet.mutual_with(&them.meet)).then(|| (them.to_peer.clone(), them.local))
    }

    /// The address one waiting peer reached this broker on.
    #[must_use]
    pub fn local_for(&self, key: &[u8; KEY_LEN]) -> Option<IpAddr> {
        self.waiting
            .iter()
            .find(|w| w.meet.me == *key)
            .and_then(|w| w.local)
    }

    /// Who is waiting, for a broker's own status output.
    #[must_use]
    pub fn keys_waiting(&self) -> Vec<[u8; KEY_LEN]> {
        self.waiting.iter().map(|w| w.meet.me).collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::net::Ipv4Addr;

    use esper_crypto::Identity;
    use tokio::sync::mpsc::{Receiver, channel};

    use super::*;

    fn address(n: u16) -> SocketAddr {
        (Ipv4Addr::new(192, 168, 1, 10), n).into()
    }

    fn from(n: u8) -> SocketAddr {
        (Ipv4Addr::new(203, 0, 113, n), 40000 + u16::from(n)).into()
    }

    /// A signed request, the way a peer makes one.
    fn meet(me: &Identity, with: &[[u8; KEY_LEN]]) -> Meet {
        Meet::new(me, with, vec![address(7645)], [9u8; KEY_LEN], 1_000_000)
    }

    /// A place with a channel, so what a waiting peer was told can be read.
    fn arrive(
        table: &mut Table,
        who: &Identity,
        with: &[[u8; KEY_LEN]],
        ip: SocketAddr,
    ) -> (Arrival, Receiver<Answer>) {
        let (tx, rx) = channel(8);
        let arrival = table.arrive(meet(who, with), ip, None, tx);
        (arrival, rx)
    }

    /// The introduction inside an answer, for a test that expects one.
    fn introduction(answer: Answer) -> Introduction {
        match answer {
            Answer::Introduced(i) => i,
            other => panic!("expected an introduction, got {other:?}"),
        }
    }

    #[test]
    fn two_peers_that_name_each_other_are_introduced() {
        let host = Identity::generate().unwrap();
        let ctrl = Identity::generate().unwrap();
        let mut table = Table::new();

        // The host arrives first and waits.
        let (first, mut host_told) = arrive(&mut table, &host, &[ctrl.public_key()], from(1));
        assert!(matches!(first, Arrival::Waiting { .. }));
        assert!(host_told.try_recv().is_err(), "nobody has come yet");

        // The controller arrives; both are told.
        let (second, _) = arrive(&mut table, &ctrl, &[host.public_key()], from(2));
        match second {
            Arrival::Met { theirs, .. } => {
                assert_eq!(theirs.peer, host.public_key());
                assert_eq!(theirs.candidates, vec![address(7645)]);
            }
            other => panic!("expected a meeting, got {other:?}"),
        }
        let told = introduction(
            host_told
                .try_recv()
                .expect("the waiting side must be told as well"),
        );
        assert_eq!(told.peer, ctrl.public_key());
    }

    #[test]
    fn a_peer_that_was_not_named_learns_nothing() {
        // **The property that makes a broker safe to run.** An unauthenticated
        // party that knows a device's key must not be able to collect its
        // addresses by asking for it.
        let host = Identity::generate().unwrap();
        let stranger = Identity::generate().unwrap();
        let ctrl = Identity::generate().unwrap();
        let mut table = Table::new();

        // The host will meet the controller, and only the controller.
        let (_, mut host_told) = arrive(&mut table, &host, &[ctrl.public_key()], from(1));

        // The stranger asks for the host. The host has not named it.
        let (asked, _) = arrive(&mut table, &stranger, &[host.public_key()], from(2));
        assert!(
            matches!(asked, Arrival::Waiting { .. }),
            "a stranger must be left waiting, not introduced: {asked:?}"
        );
        assert!(
            host_told.try_recv().is_err(),
            "and the host must not even learn that somebody asked"
        );
    }

    #[test]
    fn a_host_is_introduced_to_each_controller_that_comes() {
        let host = Identity::generate().unwrap();
        let one = Identity::generate().unwrap();
        let two = Identity::generate().unwrap();
        let mut table = Table::new();

        let (_, mut host_told) = arrive(
            &mut table,
            &host,
            &[one.public_key(), two.public_key()],
            from(1),
        );
        for who in [&one, &two] {
            let (met, _) = arrive(&mut table, who, &[host.public_key()], from(2));
            assert!(matches!(met, Arrival::Met { .. }), "{met:?}");
            assert_eq!(
                introduction(host_told.try_recv().expect("the host is told each time")).peer,
                who.public_key()
            );
        }
    }

    #[test]
    fn a_device_that_reconnects_replaces_its_own_place() {
        // Otherwise a flapping link fills a host's own per-address limit with
        // stale places, each handing out addresses that no longer work.
        let host = Identity::generate().unwrap();
        let ctrl = Identity::generate().unwrap();
        let mut table = Table::new();

        let mut held = Vec::new();
        for _ in 0..5 {
            held.push(arrive(&mut table, &host, &[ctrl.public_key()], from(1)));
        }
        assert_eq!(table.len(), 1);
        assert_eq!(table.keys_waiting(), vec![host.public_key()]);
    }

    #[test]
    fn a_peer_that_has_stopped_reading_loses_its_place() {
        // A queue that has filled is a peer that is not there any more. It is
        // dropped rather than the broker growing a buffer for it — and the
        // arriving peer must not be handed the addresses of a connection
        // nobody is serving.
        let host = Identity::generate().unwrap();
        let ctrl = Identity::generate().unwrap();
        let mut table = Table::new();

        // A channel with no room at all, and a receiver that is dropped: both
        // are "stopped reading" as far as `try_send` is concerned.
        let (tx, rx) = channel(1);
        drop(rx);
        table.arrive(meet(&host, &[ctrl.public_key()]), from(1), None, tx);
        assert_eq!(table.len(), 1);

        let (asked, _) = arrive(&mut table, &ctrl, &[host.public_key()], from(2));
        assert!(
            matches!(asked, Arrival::Waiting { .. }),
            "a peer that cannot be told is not a peer to be introduced to: {asked:?}"
        );
        assert_eq!(
            table.keys_waiting(),
            vec![ctrl.public_key()],
            "the unreachable place should be gone"
        );
    }

    #[test]
    fn one_address_cannot_fill_the_table() {
        let mut table = Table::new();
        let other = Identity::generate().unwrap();
        let mut held = Vec::new();
        let mut ids = Vec::new();
        for _ in 0..MAX_PER_ADDRESS {
            let who = Identity::generate().unwrap();
            let (arrival, rx) = arrive(&mut table, &who, &[other.public_key()], from(1));
            match arrival {
                Arrival::Waiting { id } => ids.push(id),
                other => panic!("expected a place, got {other:?}"),
            }
            held.push(rx);
        }

        let extra = Identity::generate().unwrap();
        let (refused, _) = arrive(&mut table, &extra, &[other.public_key()], from(1));
        assert_eq!(refused, Arrival::Refused(Refusal::TooManyFromYou));

        // Another address is unaffected: the limit is per address, not global.
        let (allowed, _) = arrive(&mut table, &extra, &[other.public_key()], from(2));
        assert!(matches!(allowed, Arrival::Waiting { .. }));

        // And a place that leaves makes room again.
        table.leave(*ids.first().expect("a place was taken"));
        let another = Identity::generate().unwrap();
        let (again, _) = arrive(&mut table, &another, &[other.public_key()], from(1));
        assert!(matches!(again, Arrival::Waiting { .. }));
    }

    #[test]
    fn leaving_takes_the_place_with_it() {
        let host = Identity::generate().unwrap();
        let ctrl = Identity::generate().unwrap();
        let mut table = Table::new();

        let (arrival, _held) = arrive(&mut table, &host, &[ctrl.public_key()], from(1));
        let Arrival::Waiting { id } = arrival else {
            panic!("expected a place");
        };
        table.leave(id);
        assert!(table.is_empty());

        // And the controller that arrives now meets nobody, rather than being
        // handed the addresses of a peer that has gone.
        let (alone, _) = arrive(&mut table, &ctrl, &[host.public_key()], from(2));
        assert!(matches!(alone, Arrival::Waiting { .. }));
    }

    #[test]
    fn two_strangers_with_the_same_invitation_meet() {
        // **The one case where two devices meet without knowing each other's
        // keys.** Somebody waiting to be joined cannot name who is coming, and
        // whoever was sent the link does not have their key yet — the
        // invitation is what admits them both.
        let helper = Identity::generate().unwrap();
        let guest = Identity::generate().unwrap();
        let token = [42u8; KEY_LEN];
        let mut table = Table::new();

        let waiting = Meet::invited(
            &helper,
            &[],
            vec![address(7645)],
            [9u8; KEY_LEN],
            1_000_000,
            Some(token),
        );
        let (tx, mut helper_told) = channel(8);
        assert!(matches!(
            table.arrive(waiting, from(1), None, tx),
            Arrival::Waiting { .. }
        ));

        let arriving = Meet::invited(
            &guest,
            &[],
            vec![address(51000)],
            [9u8; KEY_LEN],
            1_000_000,
            Some(token),
        );
        let (tx, _) = channel(8);
        match table.arrive(arriving, from(2), None, tx) {
            Arrival::Met { theirs, .. } => assert_eq!(theirs.peer, helper.public_key()),
            other => panic!("expected the invitation to admit them, got {other:?}"),
        }
        assert_eq!(
            introduction(helper_told.try_recv().expect("the waiting side is told")).peer,
            guest.public_key()
        );
    }

    #[test]
    fn a_different_invitation_is_a_different_meeting() {
        // Two invitations open at once must not cross. The token is the whole
        // of what admits somebody, so a token that does not match admits
        // nobody.
        let helper = Identity::generate().unwrap();
        let guest = Identity::generate().unwrap();
        let mut table = Table::new();

        let (tx, mut helper_told) = channel(8);
        table.arrive(
            Meet::invited(
                &helper,
                &[],
                vec![address(7645)],
                [9u8; KEY_LEN],
                1_000_000,
                Some([1u8; KEY_LEN]),
            ),
            from(1),
            None,
            tx,
        );
        let (tx, _) = channel(8);
        let arrival = table.arrive(
            Meet::invited(
                &guest,
                &[],
                vec![address(51000)],
                [9u8; KEY_LEN],
                1_000_000,
                Some([2u8; KEY_LEN]),
            ),
            from(2),
            None,
            tx,
        );
        assert!(matches!(arrival, Arrival::Waiting { .. }), "{arrival:?}");
        assert!(helper_told.try_recv().is_err());
    }

    #[test]
    fn an_invitation_does_not_admit_somebody_who_only_knows_a_key() {
        // A peer that names the helper by key and has no token does not meet
        // an invitation: the helper named nobody, so there is no mutual
        // naming, and the token is what stands in for it.
        let helper = Identity::generate().unwrap();
        let stranger = Identity::generate().unwrap();
        let mut table = Table::new();

        let (tx, mut helper_told) = channel(8);
        table.arrive(
            Meet::invited(
                &helper,
                &[],
                vec![address(7645)],
                [9u8; KEY_LEN],
                1_000_000,
                Some([7u8; KEY_LEN]),
            ),
            from(1),
            None,
            tx,
        );
        let (arrival, _) = arrive(&mut table, &stranger, &[helper.public_key()], from(2));
        assert!(matches!(arrival, Arrival::Waiting { .. }), "{arrival:?}");
        assert!(
            helper_told.try_recv().is_err(),
            "and the helper is not even told that somebody asked"
        );
    }

    #[test]
    fn naming_somebody_who_does_not_name_you_back_is_not_a_meeting() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        let c = Identity::generate().unwrap();
        let mut table = Table::new();

        // A will meet B. B will meet C, not A.
        let (_, mut a_told) = arrive(&mut table, &a, &[b.public_key()], from(1));
        let (b_arrival, _) = arrive(&mut table, &b, &[c.public_key()], from(2));
        assert!(matches!(b_arrival, Arrival::Waiting { .. }));
        assert!(a_told.try_recv().is_err());
    }
}
