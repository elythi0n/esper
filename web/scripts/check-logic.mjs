#!/usr/bin/env node
// The rules in the window that no browser is needed to check.
//
// # Why this exists
//
// Everything else here drives a real browser: `check-a11y` walks the tree,
// `check-ink` reads painted pixels, `check-viewer` decodes in two engines. All
// of them are worth what they cost, and none of them can tell you that a
// boundary is one second out — because a browser check renders one moment, and
// a rule about *when* a number changes unit has no moment where it shows.
//
// So the four rules below are the ones with edges in them and no home in any
// other check. They are held here, under node, with no browser and no test
// runner: the identicon check has read a module directly since it was written,
// and this is the same shape.
//
//   1. **How long ago.** Four units and three boundaries, and a device seen in
//      the future — two machines with two clocks — must not read as a negative
//      age.
//   2. **What a pairing admits.** `kept`, `once` and `spent` decide whether a
//      row offers a session. Saying `once` where the answer is `spent` offers
//      one that will be refused; the other way hides one that would work.
//   3. **Which theme the first paint chooses.** Compiled into the `<head>`
//      script by `toString()`, so this *is* the rule that runs before anything
//      is drawn, not a second telling of it.
//   4. **Joining class names.** The one utility every component is built on.
//
// Run from web/. Exit 1 on any finding.
import { shortAgo } from "../src/lib/ago.ts";
import { pairingState } from "../src/api/types.ts";
import { resolveTheme, BEFORE_PAINT } from "../src/lib/first-paint.ts";
import { cn } from "../src/lib/cn.ts";
import { readViewerStream } from "../src/session/stream.ts";
import { readFileSync } from "node:fs";

const findings = [];
let checked = 0;

const is = (got, want, what) => {
  checked++;
  if (got !== want) findings.push(`${what}: got ${JSON.stringify(got)}, want ${JSON.stringify(want)}`);
};

/*
  1. How long ago.

  `now` is passed in rather than read, because a rule about boundaries cannot
  be checked against a clock that moves between one assertion and the next —
  the 59/60 pair would pass or fail depending on which millisecond the process
  reached it in.
*/
const now = 1_700_000_000_000; // fixed, and the unit is milliseconds
const at = (secondsAgo) => shortAgo(Math.round(now / 1000) - secondsAgo, now);

is(shortAgo(null, now), "—", "a device never seen");
is(at(0), "0s", "seen just now");
is(at(1), "1s", "a second ago");
is(at(59), "59s", "the last second before a minute");
is(at(60), "1m", "the first minute");
is(at(90), "2m", "ninety seconds, rounded to the nearer minute");
is(at(3540), "59m", "fifty-nine minutes");
// Not "60m" and not "24h": the unit is chosen after rounding, so a number that
// has rounded up to a whole of the next unit is said in that unit. Both were
// true before and both were the longer way to say it.
is(at(3599), "1h", "a second short of an hour, which rounds to one");
is(at(3600), "1h", "the first hour");
is(at(86_399), "1d", "a second short of a day, which rounds to one");
is(at(86_400), "1d", "the first day");
is(at(86_400 * 3), "3d", "three days");

// Two machines, two clocks. A device seen "in the future" is ordinary, and a
// negative age is a number nobody can act on.
is(at(-5), "0s", "a device whose clock is ahead of this one");
is(at(-100_000), "0s", "a device whose clock is a day ahead");

/*
  2. What a pairing admits.

  `kept` lasts until somebody removes it. A one-time pairing is `once` while it
  can still open a session and `spent` once it cannot — and the engine is what
  decides that, through `admits_a_session`, because the rules for *why* it
  cannot (used, or lapsed) belong on the side that enforces them.
*/
const device = (pairing, admits) => ({ pairing, admits_a_session: admits });

is(pairingState(device("kept", true)), "kept", "a lasting pairing");
is(pairingState(device("kept", false)), "kept", "a lasting pairing the engine says is not open");
is(pairingState(device({ once: { used: false, expires_at: 0 } }, true)), "once", "a one-time pairing still good");
is(pairingState(device({ once: { used: true, expires_at: 0 } }, false)), "spent", "a one-time pairing already used");
is(pairingState(device({ once: { used: false, expires_at: 1 } }, false)), "spent", "a one-time pairing that lapsed unused");

/*
  3. Which theme the first paint chooses.

  An explicit choice wins. Anything else — `auto`, nothing stored, or a value
  from a newer version this build does not know — asks the desktop, and lands
  on dark when it cannot.
*/
is(resolveTheme("dark"), "dark", "an explicit dark");
is(resolveTheme("light"), "light", "an explicit light");
is(resolveTheme("hc"), "hc", "an explicit high contrast");
is(resolveTheme("auto"), "dark", "auto on a desktop asking for nothing");
is(resolveTheme(null), "dark", "nothing stored");
is(resolveTheme("solarized"), "dark", "a theme a later version stored");

// A desktop asking for more contrast gets it, whatever the stored value says
// short of an explicit one.
const withContrast = (matches) => {
  globalThis.matchMedia = (q) => ({ matches: matches && q.includes("prefers-contrast"), media: q });
};
withContrast(true);
is(resolveTheme(null), "hc", "a desktop asking for more contrast");
is(resolveTheme("auto"), "hc", "auto on a desktop asking for more contrast");
is(resolveTheme("light"), "light", "an explicit light outranks the desktop's contrast");
withContrast(false);
is(resolveTheme(null), "dark", "a desktop asking for nothing");
delete globalThis.matchMedia;

/*
  And the rule the head script runs is this one.

  `BEFORE_PAINT` is built with `resolveTheme.toString()` precisely so the two
  cannot drift. That is only true while it keeps doing so, and a refactor that
  inlined the rule would leave this check holding a function nothing runs.
*/
checked++;
if (!BEFORE_PAINT.includes("prefers-contrast")) {
  findings.push("the first-paint script no longer carries the theme rule, so this check holds a function nothing runs");
}
checked++;
if (!BEFORE_PAINT.includes("d.theme")) {
  findings.push("the first-paint script no longer sets the theme attribute");
}

/*
  4. Joining class names.

  Every component is built on it, and what it has to get right is the falsy
  cases: a conditional class that is `false` must vanish rather than appear as
  the word "false" in a class attribute.
*/
is(cn("a", "b"), "a b", "two classes");
is(cn("a", false, "b"), "a b", "a condition that did not hold");
is(cn("a", null, undefined, "b"), "a b", "nothing and nothing again");
is(cn(), "", "no classes at all");
is(cn(false, null), "", "only conditions that did not hold");
is(cn("a", "", "b"), "a b", "an empty string between two classes");

/*
  5. The stream reader, fed the way a socket feeds it.

  `check-viewer` decodes this fixture in two real browsers, which is the check
  that matters — and it hands the reader the bytes in whatever chunks the
  network gave it. The property the buffering exists for is the one it cannot
  force: **a record split across arrivals is still one record.** A picture
  arrives in four pieces and two arrive in one, and the reader has to hold
  anything short of a whole record and re-examine it when more comes.

  So the same fixture is read twice here, once whole and once a byte at a
  time, and the two have to agree. A reader that quietly assumed a record
  begins at the start of a chunk passes every browser check and fails on the
  first slow network.
*/
const fixture = new Uint8Array(readFileSync(new URL("../../testdata/fixtures/viewer_stream.bin", import.meta.url)));

const streamOf = (bytes, chunk) =>
  new ReadableStream({
    start(controller) {
      for (let at = 0; at < bytes.length; at += chunk) {
        controller.enqueue(bytes.subarray(at, Math.min(at + chunk, bytes.length)));
      }
      controller.close();
    },
  });

const readAll = async (chunk) => {
  const out = [];
  for await (const record of readViewerStream(streamOf(fixture, chunk))) {
    out.push(
      record.kind === "picture"
        ? `picture ${record.key ? "key" : "delta"} ${record.timestamp} ${record.data.length}`
        : record.kind === "configuration"
          ? `configuration ${record.width}x${record.height} ${record.codec} ${record.description.length}`
          : `unknown ${record.record}`,
    );
  }
  return out;
};

const whole = await readAll(fixture.length);
checked++;
if (whole.length < 5) {
  findings.push(`the fixture read as ${whole.length} record(s), which is not a stream of pictures`);
}
checked++;
if (!whole[0]?.startsWith("configuration ")) {
  findings.push(`the first record is ${whole[0]}, and a decoder cannot start without a configuration`);
}
checked++;
if (!whole.some((r) => r.startsWith("picture key"))) {
  findings.push("no picture in the fixture is one a decoder could begin at");
}

for (const chunk of [1, 2, 7, 1024]) {
  const piecemeal = await readAll(chunk);
  checked++;
  if (piecemeal.join("\n") !== whole.join("\n")) {
    findings.push(
      `the same stream read ${chunk} byte(s) at a time gave ${piecemeal.length} record(s) ` +
        `and ${whole.length} when read whole — a record split across arrivals was not put back together`,
    );
  }
}

// And a stream that is not this one is refused rather than decoded as noise.
checked++;
try {
  const wrong = new Uint8Array([...Buffer.from("ESPRINP\0"), 1, 0, 0, 0, 0, 0]);
  for await (const _ of readViewerStream(streamOf(wrong, wrong.length))) break;
  findings.push("a stream naming another format was read as pictures");
} catch (e) {
  if (!/not a video stream/.test(String(e))) {
    findings.push(`a stream naming another format was refused with ${e}`);
  }
}

// A floor: a run that asserted nothing passes everything.
if (checked < 30) {
  console.error(`  ✗ logic: only ${checked} assertion(s) ran, which is not this file`);
  process.exit(1);
}

if (findings.length) {
  console.error("");
  for (const f of findings) console.error(`  ✗ logic: ${f}`);
  process.exit(1);
}
console.log(
  `  ✓ logic: ${checked} assertions — how long ago at every boundary, what a pairing admits, the theme ` +
    `the first paint chooses, the class join every component is built on, and ${whole.length} records read ` +
    `identically whole and one byte at a time`,
);
