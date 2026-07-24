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
    p.add_argument("--seed", type=int, default=None, help="for a repeatable run")
    p.add_argument("--report", type=float, default=0.0, help="seconds between counts")
    args = p.parse_args()

    rng = random.Random(args.seed)
    inbound = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    inbound.bind(args.listen)
    outbound = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    outbound.bind(("0.0.0.0", 0))
    print(f"relaying {args.listen} -> {args.to}, {args.loss}% loss, {args.delay}ms each way", flush=True)

    client = None
    counts = {"sent": 0, "dropped": 0}
    lock = threading.Lock()

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

    def forward(sock, payload, where):
        with lock:
            if rng.random() * 100.0 < args.loss:
                counts["dropped"] += 1
                return
            counts["sent"] += 1
        wait = args.delay
        if args.jitter:
            wait += rng.uniform(-args.jitter, args.jitter)
        if wait <= 0:
            try:
                sock.sendto(payload, where)
            except OSError:
                pass
            return
        with queue_lock:
            # The counter breaks ties without comparing payloads, and keeps
            # two packets due at the same microsecond in the order they
            # arrived.
            heapq.heappush(queue, (time.monotonic() + wait / 1000.0, next(order), sock, payload, where))
            queue_lock.notify()

    def from_far_end():
        while True:
            payload, _ = outbound.recvfrom(65535)
            if client is not None:
                forward(inbound, payload, client)

    threading.Thread(target=from_far_end, daemon=True).start()

    if args.report:
        def report():
            while True:
                time.sleep(args.report)
                with lock:
                    print(f"  {counts['sent']} through, {counts['dropped']} dropped", flush=True)
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
            forward(outbound, payload, args.to)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
