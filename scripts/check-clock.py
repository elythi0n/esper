#!/usr/bin/env python3
"""The clock against another implementation of the same documents.

`esper-clock` reads `/usr/share/zoneinfo` itself: RFC 8536's 64-bit block and
the POSIX rule its files end with. A parser checked only against bytes it also
wrote is checked against its own misunderstanding of the format — and this
format has corners where a plausible misreading is right most of the time and
wrong for half of every year.

So this generates the truth from Python's `zoneinfo`, which is an independent
reading of the same files, and hands it to a test that compares. Python is the
oracle; the assertions live in Rust, so nothing here parses program output.

    make check-clock

Skips, saying why, where Python's timezone data or the database is missing.
"""

import datetime
import os
import pathlib
import subprocess
import sys
import tempfile

# Zones chosen for the shapes they have, not for where people live:
#
#   Europe/Helsinki   the ordinary northern rule, and the machine this was
#                     written on
#   America/Sao_Paulo daylight saving that was abolished, so the table stops
#                     changing and the footer has no rule
#   Pacific/Auckland  a southern rule, whose window wraps the new year
#   Asia/Kathmandu    an offset that is not a whole number of hours
#   Pacific/Kiritimati  a zone that skipped a day, changing its offset by a day
#   Africa/Casablanca   a rule with several transitions a year, for Ramadan
#   Europe/Dublin     negative daylight saving: its footer marks winter as the
#                     saved time, which is the case that reads backwards
#   Australia/Lord_Howe  a half-hour daylight-saving change
#   UTC               no rule at all
ZONES = [
    "Europe/Helsinki",
    "America/Sao_Paulo",
    "Pacific/Auckland",
    "Asia/Kathmandu",
    "Pacific/Kiritimati",
    "Africa/Casablanca",
    "Europe/Dublin",
    "Australia/Lord_Howe",
    "America/New_York",
    "Asia/Tokyo",
    "UTC",
]

ZONEINFO = pathlib.Path("/usr/share/zoneinfo")

# 1970 to 2050, so the cases cover the table and the footer past the end of it.
YEARS = range(1970, 2051)

SHORT = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]


def instants():
    """Instants worth asking about, in seconds.

    Every fortnight across eighty years catches both sides of every transition
    without anybody having to know when they are; the extra ones nail the hours
    a change actually happens, where an off-by-one-hour hides.
    """
    out = []
    for year in YEARS:
        start = datetime.datetime(year, 1, 1, tzinfo=datetime.UTC)
        for fortnight in range(0, 26):
            out.append(int((start + datetime.timedelta(days=14 * fortnight)).timestamp()))
        # The mornings daylight saving usually starts and ends on, hour by
        # hour: the last Sunday of March and of October, and the second Sunday
        # of March and first of November, which is what North America uses.
        for month, day in ((3, 25), (4, 5), (10, 25), (11, 5), (9, 25), (1, 15)):
            try:
                noon = datetime.datetime(year, month, day, tzinfo=datetime.UTC)
            except ValueError:
                continue
            for hour in range(0, 24, 3):
                out.append(int((noon + datetime.timedelta(hours=hour)).timestamp()))
    return sorted(set(out))


def main():
    try:
        from zoneinfo import ZoneInfo
    except ImportError:
        print("  — skipped: this Python has no zoneinfo module")
        return 0
    if not ZONEINFO.is_dir():
        print(f"  — skipped: this machine has no {ZONEINFO}")
        return 0

    root = pathlib.Path(__file__).resolve().parent.parent
    cases = []
    used = []
    for name in ZONES:
        if not (ZONEINFO / name).exists():
            continue
        try:
            zone = ZoneInfo(name)
        except Exception as e:  # noqa: BLE001 — a zone Python will not load is a skip
            print(f"  — {name}: Python will not load it ({e})")
            continue
        used.append(name)
        for unix in instants():
            when = datetime.datetime.fromtimestamp(unix, zone)
            offset = when.utcoffset()
            cases.append(
                "\t".join(
                    str(x)
                    for x in (
                        name,
                        unix,
                        when.year,
                        when.month,
                        when.day,
                        when.hour,
                        when.minute,
                        when.second,
                        SHORT[when.weekday()],
                        int(offset.total_seconds()) if offset else 0,
                    )
                )
            )

    if not cases:
        print("  — skipped: none of the chosen zones are on this machine")
        return 0

    with tempfile.NamedTemporaryFile("w", suffix=".tsv", delete=False) as f:
        f.write("\n".join(cases))
        table = f.name

    print(f"  {len(cases)} cases across {len(used)} zones, 1970 to {YEARS.stop - 1}")
    env = dict(os.environ, ESPER_CLOCK_CASES=table)
    result = subprocess.run(
        ["cargo", "test", "-p", "esper-clock", "--test", "oracle", "--", "--nocapture"],
        cwd=root / "core",
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    os.unlink(table)
    if result.returncode != 0:
        print("  ✗ the clock disagrees with Python's zoneinfo\n")
        print(result.stdout)
        print(result.stderr, file=sys.stderr)
        return 1
    for line in result.stdout.splitlines():
        if "agree" in line:
            print(f"  ✓ {line.strip()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
