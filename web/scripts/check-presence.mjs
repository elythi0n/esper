#!/usr/bin/env node
// The presence indicator, held to its contract.
//
// presence.contract.ts is the one place the indicator's numbers are written.
// This reads that file and presence.css and refuses to let them disagree —
// on size, ring, the two tones, the prohibitions, and the contrast the pair
// clears against the worst pictures a video frame can put behind it. It is a
// contract test rather than a visual review: nobody has to look at it to know
// it is still visible.
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { contrast, parseOklch } from "./color.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const session = join(here, "..", "src", "session");
const contractTs = readFileSync(join(session, "presence.contract.ts"), "utf8");
const css = readFileSync(join(session, "presence.css"), "utf8");

// ── The contract, read from the TypeScript source ────────────────────────
// A tiny reader for `key: value,` lines. The file is `as const` data with no
// logic, and reading it rather than importing it keeps this a plain script.
function field(name) {
  const m = contractTs.match(new RegExp(`^\\s*${name}:\\s*(.+?),?\\s*$`, "m"));
  if (!m) fail(`contract has no field ${name}`);
  return m[1].replace(/\s+as const$/, "").trim();
}
const num = (name) => Number(field(name));
const str = (name) => JSON.parse(field(name));
const list = (name) => JSON.parse(field(name));
const bool = (name) => field(name) === "true";

const contract = {
  minSizePx: num("minSizePx"),
  ringPx: num("ringPx"),
  fill: str("fill"),
  ring: str("ring"),
  minContrastAgainstVideo: num("minContrastAgainstVideo"),
  videoWorstCases: list("videoWorstCases"),
  themeable: bool("themeable"),
  animated: bool("animated"),
  opacity: num("opacity"),
  position: str("position"),
};

const failures = [];
function fail(msg) {
  failures.push(msg);
}

// The contract itself must say what the spec says. A contract edited to be
// satisfiable is the failure mode this exists to catch.
if (!(contract.minSizePx >= 24)) fail(`minSizePx ${contract.minSizePx} is below the 24px target minimum`);
if (!(contract.ringPx >= 2)) fail(`ringPx ${contract.ringPx} is thinner than 2px`);
if (contract.themeable !== false) fail("themeable must be false");
if (contract.animated !== false) fail("animated must be false");
if (contract.opacity !== 1) fail(`opacity must be 1, contract says ${contract.opacity}`);
if (contract.position !== "fixed") fail(`position must be fixed, contract says ${contract.position}`);
if (!(contract.minContrastAgainstVideo >= 3)) fail("minContrastAgainstVideo is below WCAG's 3:1 for graphics");
if (contract.videoWorstCases.length < 3) fail("fewer than three worst-case video tones");

// ── The stylesheet, against the contract ─────────────────────────────────
const rule = css.match(/\.presence\s*\{([\s\S]*?)\n\s*\}/)?.[1] ?? "";
const dot = css.match(/\.presence::before\s*\{([\s\S]*?)\n\s*\}/)?.[1] ?? "";
if (!rule) fail("no .presence rule in presence.css");
if (!dot) fail("no .presence::before rule in presence.css");

const decl = (block, prop) => block.match(new RegExp(`(?:^|\\n)\\s*${prop}\\s*:\\s*([^;]+);`))?.[1].trim();
const px = (v) => (v && /^\d+px$/.test(v) ? Number(v.slice(0, -2)) : NaN);

if (decl(rule, "position") !== contract.position) fail(`position is ${decl(rule, "position")}, contract says ${contract.position}`);
if (!(px(decl(rule, "min-width")) >= contract.minSizePx)) fail(`min-width ${decl(rule, "min-width")} is below ${contract.minSizePx}px`);
if (!(px(decl(rule, "min-height")) >= contract.minSizePx)) fail(`min-height ${decl(rule, "min-height")} is below ${contract.minSizePx}px`);
if (decl(rule, "z-index") !== "var(--z-session-hud)") fail("the indicator is not on the session HUD layer");

// Colours: the literals in the CSS are the literals in the contract, and no
// theme token is anywhere near them.
const fillCss = decl(rule, "background");
const ringCss = decl(rule, "color");
const shadow = decl(rule, "box-shadow") ?? "";
const ringMatch = shadow.match(/^0 0 0 (\d+)px (oklch\([^)]*\))$/);
if (fillCss !== contract.fill) fail(`fill is ${fillCss}, contract says ${contract.fill}`);
if (ringCss !== contract.ring) fail(`text colour is ${ringCss}, contract says ${contract.ring}`);
if (!ringMatch) fail(`box-shadow "${shadow}" is not a plain ring`);
else {
  if (Number(ringMatch[1]) < contract.ringPx) fail(`ring is ${ringMatch[1]}px, contract says ${contract.ringPx}px`);
  if (ringMatch[2] !== contract.ring) fail(`ring colour is ${ringMatch[2]}, contract says ${contract.ring}`);
}
if (decl(dot, "background") !== contract.ring) fail("the dot is not the ring colour");
for (const m of css.matchAll(/var\((--[\w-]+)\)/g)) {
  const v = m[1];
  if (/^--(?:color|sem)-/.test(v) || /^--(?:bone|ink|pitch|paper|n-\d+|hue-)/.test(v)) {
    fail(`presence.css reads ${v}: the indicator is unthemeable and may reference no colour token`);
  }
}

// Prohibitions: written with !important so nothing below can win.
const important = (prop, value) => new RegExp(`(?:^|\\n)\\s*${prop}\\s*:\\s*${value}\\s*!important;`).test(rule);
if (!important("opacity", "1")) fail("opacity: 1 !important is missing");
if (!important("animation", "none")) fail("animation: none !important is missing");
if (!important("transition", "none")) fail("transition: none !important is missing");
if (/@keyframes|transition-duration|animation-name/.test(css)) fail("presence.css declares motion");

// The pair against the worst pictures. Whichever tone the frame resembles, the
// other one is what the eye finds, so the *better* of the two must clear.
const fill = parseOklch(contract.fill);
const ring = parseOklch(contract.ring);
if (!fill || !ring) fail("contract colours are not literal oklch()");
else {
  for (const bgExpr of contract.videoWorstCases) {
    const bg = parseOklch(bgExpr);
    if (!bg) { fail(`worst case ${bgExpr} is not literal oklch()`); continue; }
    const best = Math.max(contrast(fill, bg), contrast(ring, bg));
    if (best < contract.minContrastAgainstVideo) {
      fail(`over ${bgExpr} the better tone reaches ${best.toFixed(2)}:1, below ${contract.minContrastAgainstVideo}:1`);
    }
  }
  // And the two tones against each other, so the ring is a ring and not a smear.
  const pair = contrast(fill, ring);
  if (pair < 7) fail(`fill against ring is ${pair.toFixed(2)}:1; the pair must clear 7:1`);
}

// ── Report ────────────────────────────────────────────────────────────────
if (failures.length) {
  for (const f of failures) console.error(`  ✗ presence: ${f}`);
  process.exit(1);
}
console.log(
  `  ✓ presence: ${contract.minSizePx}px minimum, ${contract.ringPx}px ring, ` +
    `${contract.videoWorstCases.length} worst-case frames cleared at ≥${contract.minContrastAgainstVideo}:1, no motion, no theme`,
);
