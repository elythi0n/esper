#!/usr/bin/env python3
"""A UDP relay that loses and delays packets, for measuring what a session does
on a link that is not a loopback.

Two Espers on one machine talk over a link with no loss, no jitter and a round
trip measured in microseconds. Every number that comes out of that says the
pipeline works; none of them say what happens on a hotel wifi. This sits
between them and makes the link worse on purpose.

    flaky-link.py --listen 127.0.0.1:7700 --to 127.0.0.1:7645 --loss 5 --delay 100

Several clients at once, **sharing one wire**. Each gets its own socket to the
far end so replies can be told apart, and the rate ceiling and queue are shared
between all of them — which is the point when the question is what one flow
does to another. A file transfer and a screen session on one home uplink is a
question this tool could not ask when it carried one client.

Loss is applied per packet and per direction, so a five per cent setting is a
five per cent chance each way — which is what a link with five per cent loss
does, rather than a five per cent chance of a round trip surviving.

`--rate` and `--queue` are the bufferbloat pair, and they are the reason this
tool exists at all beyond loss:

    flaky-link.py --listen 127.0.0.1:7700 --to 127.0.0.1:7645 \
        --rate 1000 --queue 1000

`--rate` is a ceiling in kbit/s, each way, applied by *serialising* packets:
each one occupies the link for as long as its bytes take, and the next waits.
`--queue` is how much of that waiting the link will hold, in milliseconds,
before it starts dropping the tail.

Together they are a home uplink. A sender that offers more than the ceiling
does not lose anything at first — it fills the queue, and the round trip grows
by however deep the queue is. That is bufferbloat, and it is the common case:
loss is the *late* signal, and a receiver waiting for loss to react has
already made the link a second deep.
"""

import argparse
import heapq
import itertools
import random
import socket
import sys
import threading
import time


def address(text):
    host, _, port = text.rpartition(":")
    return (host, int(port))


def self_test():
    """The shaper's arithmetic, against cases with known answers.

    **A shaper nobody measured is an opinion.** The difference between one
    queue and several is a claim about numbers, and the only way to know this
    tool makes that claim correctly is to ask it questions whose answers are
    arithmetic rather than judgement. It has already earned its place once:
    the first version of the fair queue handed out *twice* the rate, because a
    departure assigned at arrival cannot let a later arrival overtake — and
    that is the entire behaviour a scheduler exists for.

    Run with `--self-test`. It opens no sockets.
    """
    # 1 Mbit/s: a 1000-byte packet occupies the wire for exactly 8ms.
    RATE, MS = 1000.0, 0.008

    def model(shaper, packets, queue_ms=1000.0):
        """The shaper, on a wire with no latency, offered everything at t=0.

        Returns the departure time of each packet in *arrival* order, so a
        reordering shows up as an out-of-order list.
        """
        wire_free = 0.0
        wire_virtual = 0.0
        flow_clock = {}
        held = []
        departures = {}
        arrival = 0

        for flow, size in packets:
            on_wire = (size * 8.0) / (RATE * 1000.0)
            if shaper == "fifo":
                departures[arrival] = wire_free
                wire_free += on_wire
            else:
                start = max(flow_clock.get(flow, 0.0), wire_virtual)
                if (start - wire_virtual) * 1000.0 > queue_ms:
                    departures[arrival] = None  # the flow's own tail
                    arrival += 1
                    continue
                finish = start + on_wire
                flow_clock[flow] = finish
                heapq.heappush(held, (finish, arrival, size))
            arrival += 1

        # The wire is handed out in virtual-finish order, which is where the
        # reordering happens. Everything was offered at once, so this is the
        # whole schedule.
        while held:
            finish, which, size = heapq.heappop(held)
            departures[which] = wire_free
            wire_free += (size * 8.0) / (RATE * 1000.0)
            wire_virtual = max(wire_virtual, finish)

        return [departures[i] for i in range(len(packets))]

    problems = []

    def expect(why, got, want):
        if [None if g is None else round(g, 6) for g in got] != [
            None if w is None else round(w, 6) for w in want
        ]:
            problems.append(f"  ✗ {why}\n      wanted {want}\n      got    {got}")

    # One queue serialises everything, whoever sent it: b waits behind all of a.
    expect(
        "fifo serialises everything, whoever sent it",
        model("fifo", [("a", 1000), ("a", 1000), ("a", 1000), ("b", 1000)]),
        [0.0, MS, 2 * MS, 3 * MS],
    )

    # **The property a scheduler exists for.** The same offer, fairly queued:
    # the quiet flow's one packet takes the *second* slot rather than the
    # fourth, and the busy flow's backlog moves down to make room.
    expect(
        "fair lets a quiet flow overtake a busy one's backlog",
        model("fair", [("a", 1000), ("a", 1000), ("a", 1000), ("b", 1000)]),
        [0.0, 2 * MS, 3 * MS, MS],
    )

    # And it shares the wire rather than multiplying it: four packets still
    # take four slots, whoever sent them.
    expect(
        "fair shares the rate rather than multiplying it",
        model("fair", [("a", 1000), ("b", 1000), ("a", 1000), ("b", 1000)]),
        [0.0, MS, 2 * MS, 3 * MS],
    )

    # A small packet from an idle flow beats a big one from another idle flow,
    # and the flow that sent more bytes finishes last.
    #
    # Worked by hand, because the first expectation written here was wrong and
    # the model was right: `a` offers 2000 bytes (16ms of wire) and `b` offers
    # 1000 (8ms), both at t=0, so `b`'s virtual finish is 8ms against `a`'s 16
    # and `b` goes first. That is not a quirk — it is why fair queueing helps
    # interactive traffic at all. Over the four packets `a` sends 3000 bytes
    # and `b` 2000, and `a` finishes last.
    expect(
        "a small packet overtakes a big one, and the flow that sent more finishes last",
        model("fair", [("a", 2000), ("b", 1000), ("a", 1000), ("b", 1000)]),
        [MS, 0.0, 4 * MS, 3 * MS],
    )

    # The drop is per flow: `a` overruns a 10ms buffer and `b`, which has sent
    # nothing, is unaffected. Under one queue the second flow loses packets it
    # never queued.
    fair = model("fair", [("a", 1000)] * 6 + [("b", 1000)], queue_ms=10.0)
    if None not in fair[:6]:
        problems.append("  ✗ fair should drop the overrunning flow's tail\n      got " + str(fair))
    if fair[-1] is None:
        problems.append("  ✗ fair should not drop a quiet flow's packet\n      got " + str(fair))

    if problems:
        print("the shaper does not do what it says:")
        for line in problems:
            print(line)
        return 1
    print("  ✓ shaper: five properties, including the per-flow drop, all as expected")
    return 0


def main():
    # `--self-test` asks the model a question and opens no sockets, so it must
    # not be made to name addresses it will never use.
    if "--self-test" in sys.argv:
        return self_test()

    p = argparse.ArgumentParser()
    p.add_argument("--listen", required=True, type=address)
    p.add_argument("--to", required=True, type=address)
    p.add_argument("--loss", type=float, default=0.0, help="per cent, each way")
    p.add_argument("--delay", type=float, default=0.0, help="ms, each way")
    p.add_argument("--jitter", type=float, default=0.0, help="ms, uniform ±")
    p.add_argument("--rate", type=float, default=0.0, help="kbit/s ceiling, each way")
    p.add_argument(
        "--queue",
        type=float,
        default=1000.0,
        help="ms of queueing the link will hold before dropping the tail",
    )
    p.add_argument(
        "--loss-from",
        type=float,
        default=None,
        help="seconds after start when loss begins (default: from the start)",
    )
    p.add_argument(
        "--loss-for",
        type=float,
        default=None,
        help="seconds the loss lasts, then the link is clean again",
    )
    p.add_argument(
        "--shaper",
        choices=("fifo", "fair"),
        default="fifo",
        help="fifo: one queue per direction. fair: one per flow, sharing the rate",
    )
    p.add_argument(
        "--cut-in-frame",
        type=int,
        default=0,
        help="packets into a burst this size at which the link dies (with --loss-for)",
    )
    p.add_argument("--seed", type=int, default=None, help="for a repeatable run")
    p.add_argument(
        "--self-test",
        action="store_true",
        help="check the shaper's arithmetic against known cases and exit",
    )
    p.add_argument("--report", type=float, default=0.0, help="seconds between counts")
    args = p.parse_args()

    rng = random.Random(args.seed)
    inbound = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    inbound.bind(args.listen)
    ceiling = f"{args.rate:.0f} kbit/s with {args.queue:.0f}ms of queue" if args.rate > 0 else "no rate limit"
    print(
        f"relaying {args.listen} -> {args.to}, {args.loss}% loss,"
        f" {args.delay}ms each way, {ceiling}",
        flush=True,
    )

    # One socket to the far end per client, so a reply can be told apart from
    # every other client's. Bounded, because a stray packet from anywhere
    # would otherwise be a thread and a socket.
    clients = {}
    MOST_CLIENTS = 8

    # **Counted per direction.** A shared counter reads as nonsense the first
    # time it matters: "358 through" against a receiver that saw 188 looks
    # like catastrophic loss, when the other 170 were acknowledgements going
    # the other way. Video goes one way and almost nothing comes back, so the
    # two numbers are not comparable and must not be added.
    def fresh():
        return {"sent": 0, "dropped": 0, "queued": 0, "overflowed": 0, "worst_wait": 0.0}

    counts = {"up": fresh(), "down": fresh()}
    lock = threading.Lock()

    # **The link is busy until it has finished sending, and each direction has
    # its own link.** Serialising is what makes a rate ceiling behave like a
    # rate ceiling rather than a token bucket that lets bursts through: a
    # keyframe of forty datagrams at 1 Mbit/s occupies the link for nearly
    # half a second, and everything behind it waits.
    #
    # Keyed by direction rather than shared, because an uplink and a downlink
    # are different wires. A shared value would make a controller's keyframe
    # request wait behind the host's video, which is not what happens.
    busy_until = {}

    # **Delayed packets go through one queue, in order.**
    #
    # The first version started a thread per packet, each sleeping the same
    # amount. Threads do not wake in the order they were started, so a link
    # with a hundred milliseconds of delay and no jitter reordered packets by
    # a few milliseconds — and a video pipeline reads reordering as loss. The
    # first measurement it produced said a perfect link lost forty frames,
    # which was the instrument and not the product.
    #
    # A heap keyed by due time, drained by one thread, delivers in the order
    # things were due. Reordering now happens only when jitter is asked for,
    # which is what asking for jitter means.
    queue = []
    queue_lock = threading.Condition()

    def drain():
        while True:
            ready = []
            with queue_lock:
                while not queue:
                    queue_lock.wait()
                now = time.monotonic()
                if queue[0][0] > now:
                    queue_lock.wait(queue[0][0] - now)
                    continue
                # **Everything due goes out together.** A keyframe is forty
                # datagrams arriving in a burst; sending them one per wake-up
                # spreads them over milliseconds and the far end reads that
                # spreading as jitter. Measured: it was the difference between
                # nine frames lost and none.
                while queue and queue[0][0] <= now:
                    ready.append(heapq.heappop(queue))
            for _, _, sock, payload, where in ready:
                try:
                    sock.sendto(payload, where)
                except OSError:
                    pass

    threading.Thread(target=drain, daemon=True).start()
    order = itertools.count()

    # **A shaper is not a queue, and until now this tool only had a queue.**
    #
    # One FIFO per direction is a fair model of a cheap link and a bad model
    # of most real ones: a home router, a phone's uplink and every managed
    # switch in between run *several* queues with a scheduler over them, and
    # the difference is visible exactly where it matters — one flow's burst
    # stops delaying another flow's small packets.
    #
    # `fair` is **start-time fair queueing**, which is the smallest thing that
    # is honestly a scheduler. Each flow keeps a virtual clock that advances
    # by its own bytes; a packet's virtual start is the later of its flow's
    # clock and the wire's progress, and the wire is then handed to the queued
    # packet with the earliest virtual *finish*. A flow that has been quiet
    # has a clock behind everybody else's, so its next packet overtakes a
    # backlog rather than joining the end of it — which is the whole
    # behavioural difference, and the reason a scheduler is not a queue.
    #
    # **It has to hold packets, and that is the part a formula cannot fake.**
    # The first attempt assigned each packet a departure as it arrived, which
    # cannot let a later arrival overtake an earlier one — so two flows each
    # departed at their own clock and the model handed out *twice* the rate.
    # The self-test caught it, which is what the self-test is for.
    #
    # **What it cannot model, said plainly.** Media and control travel inside
    # one QUIC connection, so from the path they are one flow and no shaper on
    # the wire can separate them. A fair queue here changes what one *client*
    # does to another — a file transfer beside a screen session, which is what
    # this tool grew several clients for — and says nothing about a keyframe
    # burst against its own connection's acknowledgements. `fifo` stays the
    # default so every measurement taken before this remains comparable.
    flow_clock = {}
    # Per direction: packets waiting for the wire, a heap of
    # (virtual_finish, arrival, size, sock, payload, where).
    waiting_for_wire = {"up": [], "down": []}
    # How far the wire has got in virtual time, which is the finish of the
    # last packet it was handed.
    wire_virtual = {"up": 0.0, "down": 0.0}

    def on_the_wire(size):
        return (size * 8.0) / (args.rate * 1000.0)

    def admit(direction, size, now, flow):
        """Take a packet into the shaper, or refuse it as the tail.

        Returns the virtual finish it was queued with, or None if this flow's
        own backlog is already deeper than the link will hold.
        """
        key = (direction, flow)
        start = max(flow_clock.get(key, 0.0), wire_virtual[direction])
        # **The flow's own backlog decides the drop**, which is the second
        # half of what a scheduler buys: a flow filling the buffer used to
        # drop everybody's packets, and now drops only its own tail.
        behind = start - wire_virtual[direction]
        if behind * 1000.0 > args.queue:
            return None
        finish = start + on_the_wire(size)
        flow_clock[key] = finish
        return finish

    def hand_wire_out(direction, now):
        """Give the wire to whatever is queued, in virtual-finish order.

        Called when a packet arrives and when the wire frees. Everything it
        schedules is in the future, so the delivery heap does the sending.
        """
        out = []
        while waiting_for_wire[direction]:
            free = max(busy_until.get(direction, 0.0), now)
            if free > now:
                break
            finish, _, size, sock, payload, where = heapq.heappop(waiting_for_wire[direction])
            busy_until[direction] = free + on_the_wire(size)
            wire_virtual[direction] = max(wire_virtual[direction], finish)
            out.append((free, sock, payload, where))
        return out

    def serialise(direction, size, now, flow="one"):
        """When this packet finishes going down the wire, or None if the queue
        is full.

        The queue's depth is time, not packets, which is the unit bufferbloat
        is measured in and the unit a router's buffer actually amounts to. A
        packet that would have to wait longer than the link will hold is the
        tail, and the tail is what a full buffer drops.
        """
        if args.rate <= 0:
            return now
        free = max(busy_until.get(direction, 0.0), now)
        waiting = free - now
        if waiting * 1000.0 > args.queue:
            return None
        busy_until[direction] = free + on_the_wire(size)
        c = counts[direction]
        c["worst_wait"] = max(c["worst_wait"], waiting)
        if waiting > 0:
            c["queued"] += 1
        return free

    def depth(direction, now):
        """How much waiting the link is holding right now, in milliseconds.

        The high-water mark says how bad it got; this says whether it is still
        bad, which is the question when watching a ladder try to drain it.
        """
        return max(busy_until.get(direction, 0.0) - now, 0.0) * 1000.0

    started = time.monotonic()

    def losing(now):
        """Whether the link is dropping right now.

        **A transient is a different question from a lossy link**, and until
        this existed the harness could only ask the second. What a session does
        about one bad second on an otherwise fine link is the whole of how fast
        it comes back, and that cannot be measured with loss that never stops.
        """
        if args.loss <= 0.0:
            return False
        since = now - started
        if args.loss_from is not None and since < args.loss_from:
            return False
        if args.loss_for is not None:
            begins = args.loss_from or 0.0
            if since > begins + args.loss_for:
                return False
        return True

    # **A link that dies mid-frame.**
    #
    # `--loss-from` starts an outage at a wall-clock moment, which lands
    # inside a frame only by luck — and the interesting case is precisely the
    # unlucky one: a keyframe half delivered, so the decoder holds a fragment
    # of the frame it asked for and the session has to notice and ask again.
    # That is the state a periodic keyframe used to paper over.
    #
    # So this counts consecutive packets and cuts *inside* a burst: a
    # keyframe is the only thing in this product that sends forty datagrams
    # back to back, so a burst of `--cut-in-frame` packets is a keyframe with
    # high confidence, and the cut happens partway through it.
    burst = {"run": 0, "last": 0.0, "cut_at": None}
    # A gap this long ends a burst. One frame interval at 30fps is 33ms; a
    # keyframe's datagrams leave within a millisecond of each other.
    BURST_GAP = 0.005

    def cutting(now, direction):
        """Whether the link is dying in the middle of a frame right now."""
        if args.cut_in_frame <= 0 or direction != "down":
            return False
        if burst["cut_at"] is not None:
            # Already cut. It lasts --loss-for, then the link returns.
            if args.loss_for is None:
                return True
            return now - burst["cut_at"] <= args.loss_for
        if now - burst["last"] > BURST_GAP:
            burst["run"] = 0
        burst["last"] = now
        burst["run"] += 1
        if burst["run"] >= args.cut_in_frame:
            burst["cut_at"] = now
            print(
                f"  cutting the link {burst['run']} packets into a frame"
                + (f", for {args.loss_for:.1f}s" if args.loss_for else ", for good"),
                flush=True,
            )
            return True
        return False

    def post(leaves, sock, payload, where):
        """Send it when the wire has finished with it, plus the wire's latency."""
        now = time.monotonic()
        wait = args.delay
        if args.jitter:
            wait += rng.uniform(-args.jitter, args.jitter)
        due = leaves + max(wait, 0.0) / 1000.0
        if due <= now:
            try:
                sock.sendto(payload, where)
            except OSError:
                pass
            return
        with queue_lock:
            # The counter breaks ties without comparing payloads, and keeps
            # two packets due at the same microsecond in the order they
            # arrived.
            heapq.heappush(queue, (due, next(order), sock, payload, where))
            queue_lock.notify()

    def pump():
        """Hand the wire out as it frees, for the flows that are waiting.

        The scheduler cannot assign a packet the wire before the wire is free,
        and a packet that arrived while it was busy has to be *held* — so
        something has to come back for it. This is that something: it wakes
        when the wire next frees rather than on a fixed tick, so an idle link
        costs nothing.
        """
        while True:
            soonest = None
            with lock:
                now = time.monotonic()
                for direction in ("up", "down"):
                    for at, sock, payload, where in hand_wire_out(direction, now):
                        post(at, sock, payload, where)
                    if waiting_for_wire[direction]:
                        free = max(busy_until.get(direction, 0.0), now)
                        soonest = free if soonest is None else min(soonest, free)
            if soonest is None:
                time.sleep(0.002)
            else:
                time.sleep(max(soonest - time.monotonic(), 0.0005))

    if args.shaper == "fair" and args.rate > 0:
        threading.Thread(target=pump, daemon=True).start()

    def forward(sock, payload, where, direction, flow):
        now = time.monotonic()
        with lock:
            c = counts[direction]
            if cutting(now, direction):
                c["dropped"] += 1
                return
            if losing(now) and rng.random() * 100.0 < args.loss:
                c["dropped"] += 1
                return
            # **The rate ceiling is applied before the delay**, so `--delay`
            # stays what it says it is: the wire's own latency, with queueing
            # on top of it rather than folded into it.
            # **The flow is the client**, and *which* address that is depends
            # on the direction: going up it is the sender, coming down it is
            # who the reply is for. Keying on the destination was the first
            # version and made every client one flow going up — the shaper
            # then behaved exactly like a FIFO, which the measurement caught
            # because the difference it exists to show was 5ms wide.
            scheduled = []
            if args.shaper == "fair" and args.rate > 0:
                finish = admit(direction, len(payload), now, flow)
                if finish is None:
                    c["overflowed"] += 1
                    c["dropped"] += 1
                    return
                heapq.heappush(
                    waiting_for_wire[direction],
                    (finish, next(order), len(payload), sock, payload, where),
                )
                scheduled = hand_wire_out(direction, now)
                if not scheduled:
                    # Held for the wire. It leaves when the scheduler reaches
                    # it, which the pump below sees to.
                    c["sent"] += 1
                    return
                c["sent"] += 1
                leaves = None
            else:
                leaves = serialise(direction, len(payload), now, flow=flow)
                if leaves is None:
                    c["overflowed"] += 1
                    c["dropped"] += 1
                    return
                c["sent"] += 1
        if scheduled:
            for at, s_sock, s_payload, s_where in scheduled:
                post(at, s_sock, s_payload, s_where)
            return
        post(leaves, sock, payload, where)

    def from_far_end(sock, client):
        while True:
            try:
                payload, _ = sock.recvfrom(65535)
            except OSError:
                return
            forward(inbound, payload, client, "down", str(client))

    if args.report:
        def report():
            while True:
                time.sleep(args.report)
                now = time.monotonic()
                with lock:
                    for which in ("up", "down"):
                        c = counts[which]
                        print(
                            f"  {which:<4} {c['sent']} through,"
                            f" {c['dropped']} dropped ({c['overflowed']} to a full queue),"
                            f" queue {depth(which, now):.0f}ms now,"
                            f" {c['worst_wait'] * 1000.0:.0f}ms worst",
                            flush=True,
                        )
        threading.Thread(target=report, daemon=True).start()

    while True:
        payload, sender = inbound.recvfrom(65535)
        sock = clients.get(sender)
        if sock is None:
            if len(clients) >= MOST_CLIENTS:
                continue
            sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            sock.bind(("0.0.0.0", 0))
            clients[sender] = sock
            threading.Thread(
                target=from_far_end, args=(sock, sender), daemon=True
            ).start()
            print(f"client {len(clients)} is {sender[0]}:{sender[1]}", flush=True)
        # **The wire is shared and the sockets are not.** Every client's
        # packets go through the same rate ceiling and the same queue, which is
        # what makes "what does a transfer do to a session" a question this can
        # answer.
        forward(sock, payload, args.to, "up", str(sender))


if __name__ == "__main__":
    try:
        sys.exit(main() or 0)
    except KeyboardInterrupt:
        pass
