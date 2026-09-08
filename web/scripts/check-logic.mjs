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
//   5. **Reading a CSS token as a number.** `ms` is tested before `s`, and
//      `"200ms"` ends with `"s"` — so the wrong order reads every duration in
//      the product as a thousand times itself, in the timers that exist to
//      agree with the CSS, with nothing about the result looking like a parse
//      error.
//
// Run from web/. Exit 1 on any finding.
import { shortAgo } from "../src/lib/ago.ts";
import { pairingState } from "../src/api/types.ts";
import { resolveTheme, BEFORE_PAINT } from "../src/lib/first-paint.ts";
import { cn } from "../src/lib/cn.ts";
import { millisecondsIn, pixelsIn } from "../src/lib/css-values.ts";
import { readViewerStream } from "../src/session/stream.ts";
import { FIT_CLASS, FITS, inPicture, placement } from "../src/session/fitting.ts";
import { askState, narrowing } from "../src/session/narrowing.ts";
import { checklist, stepsFor } from "../src/screens/permissions.ts";
import { asWhen, asWindow, readSchedule } from "../src/api/schedule.ts";
import { readFileSync } from "node:fs";

const findings = [];
let checked = 0;

const is = (got, want, what) => {
  checked++;
  if (got !== want) findings.push(`${what}: got ${JSON.stringify(got)}, want ${JSON.stringify(want)}`);
};

/** For a claim with no second value to compare against. */
const ok = (held, what) => {
  checked++;
  if (!held) findings.push(what);
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
  5. Reading a CSS token as a number.

  `ms` is tested before `s`, and that is the whole of the risk: `"200ms"` ends
  with `"s"`, so a version that asked the shorter question first would read
  every duration in the product as a thousand times itself — a 200ms
  transition becoming 200 seconds, in the timers that exist precisely to agree
  with the CSS. Nothing about the result would look like a parse error, and a
  browser check cannot see the order of two string tests.
*/
is(millisecondsIn("200ms"), 200, "milliseconds");
is(millisecondsIn("0.2s"), 200, "a fraction of a second");
is(millisecondsIn("2s"), 2000, "whole seconds");
is(millisecondsIn("0.01ms"), 0.01, "the sweep a reduced-motion override leaves");
is(millisecondsIn("  120ms  "), 120, "a computed value with space around it");
is(millisecondsIn("0s"), 0, "no time at all");
is(millisecondsIn(""), 0, "a token that is not set");
is(millisecondsIn("200"), 0, "a number with no unit is not a duration");
is(millisecondsIn("fast"), 0, "a word is not a duration");

is(pixelsIn("28px"), 28, "pixels");
is(pixelsIn("  0.5px "), 0.5, "a sub-pixel value with space around it");
is(pixelsIn("2rem"), 0, "a unit this cannot convert without guessing");
is(pixelsIn(""), 0, "a token that is not set");
is(pixelsIn("28"), 0, "a number with no unit is not a length");

/*
  6. The stream reader, fed the way a socket feeds it.

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

/*
  7. Where the picture sits, and therefore where a click landed.

  Two consumers read this and must never disagree: the canvas is told how to
  draw with `FIT_CLASS`, and the pointer is mapped with `inPicture`. If they
  part company every click is offset along one axis and scaled along the
  other — worse the further the window is from the session's shape, and
  looking like a calibration problem rather than a bug. This project has had
  that once, when the pointer was mapped against the element rather than
  against the drawn picture.

  The surface below is deliberately a different shape from the picture: a
  square box and a 2:1 picture, so "contained" and "stretched" cannot give the
  same answer by accident.
*/
{
  const box = { left: 0, top: 0, width: 400, height: 400 };
  const size = { width: 200, height: 100 };

  // Fit: scaled by the smaller ratio (2), centred, so bars top and bottom.
  const fitted = placement("fit", box, size);
  is(fitted?.scaleX, 2, "fit scales by the smaller ratio");
  is(fitted?.scaleY, 2, "fit scales both axes the same");
  is(fitted?.left, 0, "fit fills the wider axis");
  is(fitted?.top, 100, "fit centres on the other axis");

  // Actual: never scaled, whatever the box is.
  const actual = placement("actual", box, size);
  is(actual?.scaleX, 1, "actual size does not scale");
  is(actual?.top, 150, "actual size is centred");

  // Stretch: each axis its own ratio, origin at the box.
  const stretched = placement("stretch", box, size);
  is(stretched?.scaleX, 2, "stretch scales x to fill");
  is(stretched?.scaleY, 4, "stretch scales y independently");
  is(stretched?.left, 0, "stretch starts at the box");

  // **The corners.** In every mode, the picture's own corners map back to its
  // own coordinates — which is the property a click depends on, and the one
  // that breaks when the drawing and the mapping disagree.
  for (const fit of FITS) {
    const at = placement(fit, box, size);
    const topLeft = inPicture(fit, box, size, at.left, at.top);
    is(topLeft?.x, 0, `${fit}: the picture's left edge is x=0`);
    is(topLeft?.y, 0, `${fit}: the picture's top edge is y=0`);

    const bottomRight = inPicture(
      fit,
      box,
      size,
      at.left + size.width * at.scaleX,
      at.top + size.height * at.scaleY,
    );
    is(bottomRight?.x, size.width, `${fit}: the right edge is the picture's width`);
    is(bottomRight?.y, size.height, `${fit}: the bottom edge is the picture's height`);
  }

  // A surface with no area, and a picture with none: no placement rather than
  // a division by zero that travels onward as Infinity.
  is(placement("fit", { left: 0, top: 0, width: 0, height: 400 }, size), null, "a surface with no width places nothing");
  is(placement("fit", box, { width: 200, height: 0 }), null, "a picture with no height places nothing");
  is(inPicture("fit", box, { width: 0, height: 0 }, 10, 10), null, "no picture, no position");

  // Every mode has a class, and they are not all the same one — a table where
  // two modes shared a class would draw two of them identically while the
  // mapping treated them differently.
  is(new Set(FITS.map((f) => FIT_CLASS[f])).size, FITS.length, "every fit draws differently");
}

/*
  What a session has given up, said in the fewest words that are true.

  Zero is not a ceiling: it is the ordinary state of a link that is keeping up,
  and a readout that said "0 fps" would report a stalled session on a perfect
  one. Both numbers are asks rather than settings, so an empty answer has to
  mean *nothing was taken away* rather than "nothing is known".
*/
{
  is(narrowing(0, 0), "", "a link keeping up has given up nothing");
  is(narrowing(15, 0), "15 fps", "frames given up");
  is(narrowing(0, 1280), "1280px", "size given up");
  is(narrowing(15, 1280), "15 fps, 1280px", "both, in the order a ladder spends them");
}

/*
  Where an ask for a screen stands.

  A switch is a round trip, a decision at the host and then a keyframe, so the
  ask is acknowledged where it was made. What matters is what settles it:
  **agreement, never the clock alone** — a slow host reported as a refusal it
  did not give is worse than an ask that took a moment — and asking for the
  screen already on show is settled from the start, because chrome that said
  otherwise would be reporting progress towards where it already is.
*/
{
  is(askState(null, [0], false), "settled", "nothing asked is nothing to say");
  is(askState(1, [0], false), "asking", "asked, and not there yet");
  is(askState(1, [1], false), "settled", "the session arrived at what was asked for");
  is(askState(1, [1], true), "settled", "and late does not unsettle an ask that was met");
  is(askState(1, [0], true), "kept", "late and still not there is the host keeping its own");
  is(askState(0, [0], false), "settled", "asking for the one on show is settled from the start");
  // Several screens at once is a session showing a desktop rather than a
  // screen: an ask for one of them has not been met while the picture is
  // still of both.
  is(askState(1, [0, 1], false), "asking", "one of two is not the one asked for");
}

/*
  When a device may walk in, as a screen says it.

  `unattended` is the permission that removes the question, and it can be
  bounded. A screen that showed the same lamp for "may walk in at three in the
  morning" and "may walk in on weekday afternoons" **overstates** most of them,
  which is the one direction a screen about trust must never be wrong in.

  Nothing here re-derives the engine's rule: what is checked is that what the
  engine stored is read back as the same thing, and that the two answers a
  person must not confuse — no bound, and a window through midnight — are not
  the same sentence.
*/
{
  is(readSchedule(undefined), null, "no schedule is no sentence");
  is(readSchedule(""), null, "and neither is an empty one");
  // `always` is what the engine writes for an unbounded grant, and the lamp
  // being on already says it.
  is(readSchedule("always"), null, "always is what the lamp already says");
  is(readSchedule("weekdays 08:00-18:00"), "Weekdays 08:00–18:00", "office hours");
  is(readSchedule("weekends 10:00-23:00"), "Weekends 10:00–23:00", "the other half of a week");
  is(readSchedule("mon,wed,fri 09:00-17:00"), "Mon, Wed, Fri 09:00–17:00", "named days");
  // Eight hours, not sixteen: a window whose end is at or before its start
  // runs through midnight, and a reader who takes it for an ordinary range
  // has it backwards.
  is(readSchedule("daily 22:00-06:00"), "Every day 22:00–06:00 (overnight)", "through midnight");
  is(readSchedule("daily 08:00-18:00"), "Every day 08:00–18:00", "and a day that does not");
  // Seven named days is every day, however it was written.
  is(readSchedule("mon,tue,wed,thu,fri,sat,sun 0-24"), "Every day 0–24", "all seven is every day");
}

/*
  And back again, because a control that cannot write what it read is a control
  that changes something every time it is opened.

  The engine's vocabulary is words — `weekdays`, `daily`, a day list — and the
  form control's is day numbers and two times. Every schedule somebody can
  store has to survive both translations unchanged, or opening a device's panel
  and saving it without touching anything would quietly rewrite the bound.
*/
{
  for (const when of [
    "weekdays 08:00-18:00",
    "weekends 10:00-23:00",
    "daily 22:00-06:00",
    "mon,wed,fri 09:00-17:00",
    "tue 00:00-23:59",
  ]) {
    is(asWhen(asWindow(when)), when, `${when} survives the round trip`);
  }
  // No bound is `always` in both directions, and never a schedule naming every
  // hour of every week — which would make an unbounded grant look bounded.
  is(asWhen(asWindow(undefined)), "always", "no bound stays no bound");
  is(asWhen(asWindow("always")), "always", "and always stays always");
  // Hours a person typed are hours the engine writes: it stores `09:00`.
  is(asWhen(asWindow("mon,wed,fri 9-17")), "mon,wed,fri 09:00-17:00", "a typed hour is padded");
  // Seven named days is `daily` when the hours are bounded, and `always` when
  // they are not — the engine's own two words for the two different facts.
  is(asWhen(asWindow("mon,tue,wed,thu,fri,sat,sun 08:00-18:00")), "daily 08:00-18:00", "seven days is daily");
}

/*
  What a person has to go and do, per platform.

  The table is the part that has to be right on machines nobody here can test,
  so what is asserted is the shape rather than the wording: that a machine
  which *can* do a thing is told nothing about it, that every platform this
  product ships on has an answer, and that the answer differs — a table that
  gave the same advice everywhere would be a table nobody had filled in.
*/
{
  const machines = [
    { os: "macos", display: "" },
    { os: "linux", display: "wayland" },
    { os: "linux", display: "x11" },
    { os: "linux", display: "headless" },
    { os: "windows", display: "" },
  ];
  for (const machine of machines) {
    const name = `${machine.os}${machine.display ? `/${machine.display}` : ""}`;
    is(checklist(machine, { capture: true, input: true }).length, 0, `${name}: nothing to do is nothing said`);
    ok(
      checklist(machine, { capture: false, input: false }).length >= 2,
      `${name}: a machine that can do neither has something to say about both`,
    );
    ok(
      stepsFor(machine, "capture", false).every((s) => s.todo.length > 20),
      `${name}: the capture remedy is a sentence`,
    );
  }
  // An engine too old to say what it is still gets an answer, and it is a
  // different one: sending somebody to the wrong settings pane is worse than
  // telling them to ask the machine.
  ok(stepsFor({ os: "", display: "" }, "capture", false).length === 1, "an unknown machine gets one step");
  const advice = new Set(machines.map((m) => stepsFor(m, "capture", false)[0]?.todo));
  is(advice.size, machines.length, "every platform's capture remedy is its own");
  // And input, which is the one that differs most: a Mac asks for a
  // permission, Wayland asks for a device node, Windows asks for privilege.
  const forInput = new Set(machines.map((m) => stepsFor(m, "input", false)[0]?.todo));
  is(forInput.size, machines.length, "every platform's input remedy is its own");
}

// A floor: a run that asserted nothing passes everything.
if (checked < 80) {
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
    `the first paint chooses, the class join every component is built on, a CSS duration read in the right ` +
    `unit, where a picture sits in all three fittings and that its corners map back to its own ` +
    `coordinates in each, what a person has to go and do on each of five platforms, and ${whole.length} ` +
    `records read identically whole and one byte at a time`,
);
