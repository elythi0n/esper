#!/usr/bin/env python3
"""A UDP relay that loses and delays packets, for measuring what a session does
on a link that is not a loopback.

Two Espers on one machine talk over a link with no loss, no jitter and a round
trip measured in microseconds. Every number that comes out of that says the
pipeline works; none of them say what happens on a hotel wifi. This sits
between them and makes the link worse on purpose.

    flaky-link.py --listen 127.0.0.1:7700 --to 127.0.0.1:7645 --loss 5 --delay 100

One client at a time, which is all a measurement needs: the first address to
send anything is the client, and everything from the far end goes back to it.

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
import itertools
import random
import socket
import threading
import time


def address(text):
    host, _, port = text.rpartition(":")
    return (host, int(port))


def main():
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
    p.add_argument("--seed", type=int, default=None, help="for a repeatable run")
    p.add_argument("--report", type=float, default=0.0, help="seconds between counts")
    args = p.parse_args()

    rng = random.Random(args.seed)
    inbound = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    inbound.bind(args.listen)
    outbound = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    outbound.bind(("0.0.0.0", 0))
    ceiling = f"{args.rate:.0f} kbit/s with {args.queue:.0f}ms of queue" if args.rate > 0 else "no rate limit"
    print(
        f"relaying {args.listen} -> {args.to}, {args.loss}% loss,"
        f" {args.delay}ms each way, {ceiling}",
        flush=True,
    )

    client = None

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
    import heapq

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

    def serialise(direction, size, now):
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
        busy_until[direction] = free + (size * 8.0) / (args.rate * 1000.0)
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

    def forward(sock, payload, where, direction):
        now = time.monotonic()
        with lock:
            c = counts[direction]
            if rng.random() * 100.0 < args.loss:
                c["dropped"] += 1
                return
            # **The rate ceiling is applied before the delay**, so `--delay`
            # stays what it says it is: the wire's own latency, with queueing
            # on top of it rather than folded into it.
            leaves = serialise(direction, len(payload), now)
            if leaves is None:
                c["overflowed"] += 1
                c["dropped"] += 1
                return
            c["sent"] += 1
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

    def from_far_end():
        while True:
            payload, _ = outbound.recvfrom(65535)
            if client is not None:
                forward(inbound, payload, client, "down")

    threading.Thread(target=from_far_end, daemon=True).start()

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
        # The first address to send anything is the client. One at a time is
        # all a measurement needs, and guessing between several would make the
        # numbers meaningless anyway.
        if client is None:
            client = sender
            print(f"client is {client[0]}:{client[1]}", flush=True)
        if sender == client:
            forward(outbound, payload, args.to, "up")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
