//! Pacing a media session at the rate the link can actually carry.
//!
//! # The problem, measured
//!
//! Two links carrying the same 800 kbit/s, differing only in how much buffer
//! sits in front of the bottleneck, deliver **20–59 pictures with 40 ms of
//! buffer and 144–148 with 200 ms**. Neither is short of capacity. What kills
//! the first is *shape*: a frame leaves this process as a burst of datagrams
//! handed over at once, and five kilobytes does not fit through a four
//! kilobyte buffer however slowly the average arrives.
//!
//! So **this product does its worst work on modern, well-managed gear** — the
//! gear with the shallow buffers — and it does it invisibly, because the round
//! trip never grows. The loss reads like a wireless link, and a receiver
//! spending resolution on a shape problem gets a smaller picture that is lost
//! just as often.
//!
//! # Why this is inside the transport
//!
//! Six attempts at pacing *above* the transport were built and measured, and
//! every one of them was worse than not pacing. The last, and the clearest:
//! gated on the short-buffer signature, latched, and fed by an accurate
//! capacity estimate, it looked like a win over ten seconds and inverted over
//! twenty — **150 and 135 pictures against 201, 278 and 281 unpaced, with half
//! the delivered throughput at every rate from one to four times capacity**.
//!
//! The cost was the mechanism rather than the number. A pacer above the
//! transport is a **second serialiser in series with the wire**: every datagram
//! waits its own transmission time twice, which is exactly what halved the
//! throughput. And it spends a timer per datagram on the runtime that also
//! carries the control stream.
//!
//! quinn already has a pacer, inside, driven by the congestion window and the
//! round trip: it refills at `1.25 × window / rtt` and it costs no timer of
//! ours and no second queue. **So the whole job here is to tell it a better
//! window**, and nothing else.
//!
//! # What is told, and by whom
//!
//! A sender cannot measure its own link. Measured: the congestion window over
//! the round trip reads two to three times high, and its own send rate measures
//! the overload rather than the capacity. What *can* measure it is the
//! receiver — every frame is a capacity probe, because a sender hands a frame's
//! datagrams over at once and the spread they arrive with is the wire
//! serialising them. That estimate is accurate to 8–15% across a fourfold
//! range, and it is reported back.
//!
//! So: the receiver measures, reports, and the sender's transport paces. See
//! [`Capacity`].
//!
//! # It can only ever slow this end down
//!
//! The window is `min(what Cubic says, what the measurement implies)`. Cubic
//! keeps its own loss response, and a report that is absent, stale, wrong or
//! hostile can make this end **more cautious and never more aggressive** — so
//! the failure mode of the whole arrangement is "no better than before".

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use quinn::congestion::{Controller, ControllerFactory, ControllerMetrics, CubicConfig};

/// What the far end has measured about the link, shared with the transport.
///
/// One of these per connection. The application writes what the far end
/// reported; the congestion controller inside the transport reads it on every
/// window query. An `AtomicU64` rather than a lock because it is read from the
/// middle of packet transmission and written a few times a second.
///
/// Zero means *nothing is known*, which is also where it starts and where it
/// returns when reports stop arriving — see [`Capacity::forget`].
#[derive(Debug, Clone, Default)]
pub struct Capacity(Arc<AtomicU64>);

impl Capacity {
    /// A fresh one, knowing nothing.
    #[must_use]
    pub fn unknown() -> Self {
        Self::default()
    }

    /// What the far end says the link carries, in kbit/s.
    ///
    /// **Only ever what a receiver measured.** A sender's own numbers were
    /// tried and are two to three times high; passing one in here would pace
    /// at a rate the link cannot carry, which is the same as not pacing.
    pub fn told(&self, kbit_per_second: u32) {
        self.0.store(u64::from(kbit_per_second), Ordering::Relaxed);
    }

    /// Go back to knowing nothing.
    ///
    /// Called when reports stop — a controller that went quiet, a session
    /// being torn down. **A stale estimate is worse than none**: a link that
    /// got faster would be held at the old rate for as long as the session
    /// lasted, and nothing would say why.
    pub fn forget(&self) {
        self.0.store(0, Ordering::Relaxed);
    }

    /// Whether anybody other than the endpoint's own list holds this.
    ///
    /// Used to prune the list: a cell with one owner belongs to a connection
    /// whose session has gone.
    #[must_use]
    pub fn is_shared(&self) -> bool {
        Arc::strong_count(&self.0) > 1
    }

    /// What is known, if anything.
    #[must_use]
    pub fn known(&self) -> Option<u32> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            kbit => u32::try_from(kbit).ok(),
        }
    }
}

/// The smallest window this will ask for, in packets.
///
/// RFC 9002 puts the minimum congestion window at two packets, and going below
/// it stops the connection making progress at all. It is also why pacing
/// cannot slow a *low-latency* link down: the window that would pace at
/// 800 kbit/s over a one millisecond round trip is eighty bytes.
///
/// That turns out not to matter, and the reason is worth writing down. The
/// round trip on a link being overloaded is not one millisecond — the queue
/// fills, and the round trip becomes the depth of the queue. So the floor binds
/// exactly when the queue is empty, which is exactly when there is nothing to
/// pace.
const LEAST_PACKETS: u64 = 2;

/// The pacer refills at this multiple of `window / rtt`.
///
/// quinn's own figure, from the IETF recovery draft's N = 1.25. Named here
/// because the window that paces at a given rate is derived from it, and a
/// version of quinn that changed it would silently change this.
const PACER_GAIN: f64 = 1.25;

/// Cubic, with its window capped by what the far end says the link carries.
///
/// Everything about congestion response — slow start, the cubic growth curve,
/// the multiplicative decrease on loss, persistent congestion — is Cubic's,
/// unchanged. The only difference is the answer to "how many bytes may be in
/// flight", which is additionally bounded by the measurement.
struct Paced {
    inner: Box<dyn Controller>,
    capacity: Capacity,
    /// The most recent smoothed round trip, kept because [`Controller::window`]
    /// is asked without one and the answer depends on it.
    rtt: Duration,
    mtu: u64,
}

impl Paced {
    /// The window that would pace at the measured rate, if there is one.
    ///
    /// `bytes per second × seconds ÷ gain` — the bandwidth-delay product, over
    /// the pacer's own gain, because the pacer refills at `gain × window / rtt`
    /// and the target is the rate itself rather than a quarter more than it.
    fn paced_window(&self) -> Option<u64> {
        let kbit = self.capacity.known()?;
        let rtt = self.rtt.as_secs_f64();
        if rtt <= 0.0 {
            return None;
        }
        let bytes_per_second = f64::from(kbit) * 1000.0 / 8.0;
        let window = bytes_per_second * rtt / PACER_GAIN;
        if !window.is_finite() || window <= 0.0 {
            return None;
        }
        // Never below what keeps a connection moving.
        Some((window as u64).max(self.mtu.saturating_mul(LEAST_PACKETS)))
    }
}

impl Controller for Paced {
    fn on_sent(&mut self, now: Instant, bytes: u64, last_packet_number: u64) {
        self.inner.on_sent(now, bytes, last_packet_number);
    }

    fn on_ack(
        &mut self,
        now: Instant,
        sent: Instant,
        bytes: u64,
        app_limited: bool,
        rtt: &quinn_proto::RttEstimator,
    ) {
        // **The one thing taken from acknowledgements, and the reason this
        // works where BBR did not.** BBR needs a *delivery rate* from acks,
        // which is noise on traffic that is almost all unacknowledged
        // datagrams — measured 27/55/32/13 pictures against a Cubic control of
        // 28/9/25/16. A round trip is not a rate: every packet carrying a
        // datagram is ack-eliciting, so this is as good here as anywhere.
        self.rtt = rtt.get();
        self.inner.on_ack(now, sent, bytes, app_limited, rtt);
    }

    fn on_end_acks(
        &mut self,
        now: Instant,
        in_flight: u64,
        app_limited: bool,
        largest_packet_num_acked: Option<u64>,
    ) {
        self.inner
            .on_end_acks(now, in_flight, app_limited, largest_packet_num_acked);
    }

    fn on_congestion_event(
        &mut self,
        now: Instant,
        sent: Instant,
        is_persistent_congestion: bool,
        lost_bytes: u64,
    ) {
        self.inner
            .on_congestion_event(now, sent, is_persistent_congestion, lost_bytes);
    }

    fn on_mtu_update(&mut self, new_mtu: u16) {
        self.mtu = u64::from(new_mtu);
        self.inner.on_mtu_update(new_mtu);
    }

    /// **The whole point of this file.**
    ///
    /// `min`, never `max`: a measurement that is absent, stale, wrong or
    /// hostile can make this end more cautious and never more aggressive, so
    /// the worst this arrangement can do is nothing.
    fn window(&self) -> u64 {
        let cubic = self.inner.window();
        match self.paced_window() {
            Some(paced) => cubic.min(paced),
            None => cubic,
        }
    }

    fn metrics(&self) -> ControllerMetrics {
        // `ControllerMetrics` is non-exhaustive, so it is taken from the
        // inner controller and corrected rather than rebuilt: a field added
        // upstream should arrive here rather than being silently defaulted.
        let mut seen = self.inner.metrics();
        seen.congestion_window = self.window();
        seen
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(Self {
            inner: self.inner.clone_box(),
            capacity: self.capacity.clone(),
            rtt: self.rtt,
            mtu: self.mtu,
        })
    }

    fn initial_window(&self) -> u64 {
        self.inner.initial_window()
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

/// Builds [`Paced`] controllers for one connection.
#[derive(Debug)]
pub struct PacedCubic {
    capacity: Capacity,
}

impl PacedCubic {
    /// A factory whose controllers read this capacity.
    #[must_use]
    pub fn reading(capacity: Capacity) -> Self {
        Self { capacity }
    }
}

impl ControllerFactory for PacedCubic {
    fn build(self: Arc<Self>, now: Instant, current_mtu: u16) -> Box<dyn Controller> {
        Box::new(Paced {
            inner: Arc::new(CubicConfig::default()).build(now, current_mtu),
            capacity: self.capacity.clone(),
            // Until an acknowledgement arrives there is no round trip to work
            // from, so nothing is capped: Cubic's own initial window is what
            // gets the connection started, exactly as it would without this.
            rtt: Duration::ZERO,
            mtu: u64::from(current_mtu),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    const MTU: u16 = 1200;

    /// A controller with a known capacity and round trip, without a connection.
    fn paced(kbit: Option<u32>, rtt: Duration) -> Paced {
        let capacity = Capacity::unknown();
        if let Some(kbit) = kbit {
            capacity.told(kbit);
        }
        Paced {
            inner: Arc::new(CubicConfig::default()).build(Instant::now(), MTU),
            capacity,
            rtt,
            mtu: u64::from(MTU),
        }
    }

    /// The rate quinn's pacer would refill at, given a window and a round trip.
    ///
    /// Reproduced from `quinn_proto::connection::pacing`, which is private:
    /// the pacer adds `window × 1.25 × elapsed / rtt` tokens, which is a rate
    /// of `1.25 × window / rtt` bytes per second. **This is the number the
    /// whole file exists to control**, so a test that did not compute it would
    /// be testing an arithmetic identity rather than a pacing rate.
    fn paced_kbit(window: u64, rtt: Duration) -> f64 {
        let bytes_per_second = window as f64 * PACER_GAIN / rtt.as_secs_f64();
        bytes_per_second * 8.0 / 1000.0
    }

    /// The smallest `capacity × round trip` this mechanism can pace at all.
    ///
    /// Computed rather than written down, so the table in the module docs
    /// cannot drift from the arithmetic that produces it.
    fn least_pacing_product() -> f64 {
        let floor_bytes = f64::from(MTU) * LEAST_PACKETS as f64;
        // window = kbit × 1000 / 8 × rtt / gain, so the product at the floor
        // is floor × gain × 8 / 1000.
        floor_bytes * PACER_GAIN * 8.0 / 1000.0
    }

    /// **The property the whole thing rests on.** A window derived from a
    /// measured capacity has to make quinn pace at that capacity — not at a
    /// quarter more, which is what using the bandwidth-delay product directly
    /// would give.
    #[test]
    fn the_window_paces_at_the_rate_that_was_measured() {
        let least = least_pacing_product();
        let mut checked = 0;
        for kbit in [200u32, 800, 4_000, 25_000] {
            for rtt in [
                Duration::from_millis(40),
                Duration::from_millis(120),
                Duration::from_millis(300),
            ] {
                // Skipped rather than fudged where the floor binds: the next
                // test is about exactly those.
                if f64::from(kbit) * rtt.as_secs_f64() < least {
                    continue;
                }
                let p = paced(Some(kbit), rtt);
                let window = p.paced_window().expect("a capacity and a round trip");
                let paced_at = paced_kbit(window, rtt);
                let off_by = (paced_at - f64::from(kbit)).abs() / f64::from(kbit);
                assert!(
                    off_by < 0.02,
                    "{kbit} kbit/s over {rtt:?} paced at {paced_at:.0} — {:.0}% out",
                    off_by * 100.0
                );
                checked += 1;
            }
        }
        assert!(checked >= 8, "only {checked} combinations were in range");
    }

    /// **And what it cannot do, said out loud.**
    ///
    /// Pacing slowly needs a small window, and a window has a floor of two
    /// packets. Below about 24 kbit·s of `capacity × round trip` the floor
    /// binds and the rate it implies is above the link's — so a slow link with
    /// a shallow buffer cannot be paced into at all, whatever is measured.
    ///
    /// That is not a defect to fix here. It is the regime where 2.5 kilobytes
    /// of queue faces a five kilobyte keyframe, and the answer is not to send
    /// a five kilobyte frame.
    #[test]
    fn a_slow_link_with_a_shallow_buffer_cannot_be_paced_into() {
        let least = least_pacing_product();
        assert!(
            (least - 24.0).abs() < 1.0,
            "the limit moved to {least:.1} kbit·s; the table in the docs says 24"
        );

        // 200 kbit/s with 40 ms of buffer: the floor binds, and the rate it
        // implies is three times the link. Named rather than hidden.
        let shallow = paced(Some(200), Duration::from_millis(40));
        let window = shallow.paced_window().expect("a capacity");
        assert_eq!(window, u64::from(MTU) * LEAST_PACKETS);
        let paced_at = paced_kbit(window, Duration::from_millis(40));
        assert!(
            paced_at > 200.0 * 2.0,
            "the floor implied {paced_at:.0} kbit/s, which would be a help after all"
        );

        // The same link with the deeper buffer of the other bench: now it works.
        let deep = paced(Some(200), Duration::from_millis(200));
        let window = deep.paced_window().expect("a capacity");
        let paced_at = paced_kbit(window, Duration::from_millis(200));
        assert!(
            (paced_at - 200.0).abs() < 4.0,
            "200 kbit/s with 200ms of buffer paced at {paced_at:.0}"
        );
    }

    /// **Never more aggressive than Cubic, whatever arrives.** A report that is
    /// absent, absurd, or hostile can make this end more cautious and nothing
    /// else, so the worst the arrangement can do is behave as it did before.
    #[test]
    fn a_measurement_can_only_ever_narrow_the_window() {
        let rtt = Duration::from_millis(50);
        let plain = paced(None, rtt);
        let cubic = plain.window();
        assert_eq!(
            cubic,
            plain.inner.window(),
            "with nothing known, it is Cubic"
        );

        for kbit in [1u32, 64, 1_000, 100_000, 10_000_000, u32::MAX] {
            let p = paced(Some(kbit), rtt);
            assert!(
                p.window() <= cubic,
                "a report of {kbit} kbit/s widened the window to {} from {cubic}",
                p.window()
            );
        }
    }

    /// A connection has to keep moving. The window that would pace at
    /// 800 kbit/s over a one millisecond round trip is eighty bytes, and a
    /// connection given that sends nothing at all.
    #[test]
    fn it_never_asks_for_a_window_too_small_to_send_with() {
        let floor = u64::from(MTU) * LEAST_PACKETS;
        for rtt in [
            Duration::from_micros(100),
            Duration::from_millis(1),
            Duration::from_millis(5),
        ] {
            let p = paced(Some(64), rtt);
            let window = p.paced_window().expect("a capacity and a round trip");
            assert!(
                window >= floor,
                "{rtt:?} gave a window of {window}, below the {floor} that keeps a \
                 connection moving"
            );
        }
    }

    /// **Why that floor is harmless**, which is the part worth writing down.
    ///
    /// The floor binds when the round trip is short, and the round trip on a
    /// link being overloaded is not short — the queue fills and the round trip
    /// becomes the depth of the queue. So the floor binds exactly when the
    /// queue is empty, which is exactly when there is nothing to pace.
    #[test]
    fn the_floor_stops_binding_as_soon_as_a_queue_forms() {
        let kbit = 800;
        let floor = u64::from(MTU) * LEAST_PACKETS;

        // An empty queue: the floor binds, and the rate it implies is far above
        // the link's — which is fine, because nothing is queued.
        let empty = paced(Some(kbit), Duration::from_micros(200));
        assert_eq!(empty.paced_window(), Some(floor));
        assert!(paced_kbit(floor, Duration::from_micros(200)) > f64::from(kbit) * 10.0);

        // Forty milliseconds of queue — the buffer in the measurement that
        // motivated all of this. Now the window is above the floor and the
        // rate it implies is the link's.
        let queued = paced(Some(kbit), Duration::from_millis(40));
        let window = queued.paced_window().expect("a capacity");
        assert!(window > floor, "still at the floor with 40ms of queue");
        let paced_at = paced_kbit(window, Duration::from_millis(40));
        assert!(
            (paced_at - f64::from(kbit)).abs() < f64::from(kbit) * 0.02,
            "40ms of queue paced at {paced_at:.0} rather than {kbit}"
        );
    }

    /// Nothing is capped before there is a round trip to work from.
    #[test]
    fn nothing_is_capped_until_an_acknowledgement_has_arrived() {
        let p = paced(Some(800), Duration::ZERO);
        assert_eq!(
            p.paced_window(),
            None,
            "capped with no round trip to divide by"
        );
        assert_eq!(p.window(), p.inner.window(), "so it is Cubic's own window");
    }

    /// A stale estimate is worse than none: a link that got faster would be
    /// held at the old rate for the rest of the session with nothing saying why.
    #[test]
    fn a_forgotten_estimate_stops_capping() {
        let capacity = Capacity::unknown();
        capacity.told(200);
        assert_eq!(capacity.known(), Some(200));

        let mut p = paced(None, Duration::from_millis(50));
        p.capacity = capacity.clone();
        let capped = p.window();
        assert!(capped < p.inner.window(), "200 kbit/s did not cap anything");

        capacity.forget();
        assert_eq!(capacity.known(), None);
        assert_eq!(p.window(), p.inner.window(), "it is still capped");
    }

    /// The cell is shared with whoever is writing reports into it, and the
    /// endpoint prunes by exactly that.
    #[test]
    fn a_cell_knows_whether_anybody_else_holds_it() {
        let alone = Capacity::unknown();
        assert!(!alone.is_shared());
        let held = alone.clone();
        assert!(alone.is_shared());
        drop(held);
        assert!(!alone.is_shared());
    }

    /// Cloning a controller carries the estimate with it, or a migrated path
    /// would go back to pacing at whatever Cubic allows.
    #[test]
    fn a_cloned_controller_still_reads_the_same_estimate() {
        let p = paced(Some(500), Duration::from_millis(60));
        let capped = p.window();
        let copy = p.clone_box();
        assert_eq!(copy.window(), capped);

        // And it is the same cell, not a copy of its value.
        p.capacity.told(2_000);
        assert!(
            copy.window() > capped,
            "the clone kept an old reading rather than the cell"
        );
    }
}
