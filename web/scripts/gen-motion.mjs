#!/usr/bin/env node
// Generate src/motion.ts from the motion tokens.
//
// The CSS is the source of truth. The animation library wants seconds and
// arrays, not `120ms` and `cubic-bezier(...)`, so it needs the same numbers in
// a second shape — and a second shape maintained by hand is a second set of
// numbers within a month. This reads the tokens and writes the module; CI runs
// it with --check and fails if the committed file differs from what the CSS
// says.
//
//     node scripts/gen-motion.mjs          write src/motion.ts
//     node scripts/gen-motion.mjs --check  fail if src/motion.ts is stale
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");
const motionCss = readFileSync(join(root, "src/tokens/motion.css"), "utf8");
const semanticCss = readFileSync(join(root, "src/tokens/semantic.css"), "utf8");
const out = join(root, "src/motion.ts");

// Only the base `:root` block: the reduced-motion blocks also declare these
// tokens, as zeros, and reading them would generate a module that says every
// animation is instant.
const baseBlock = motionCss.match(/:root\s*\{([\s\S]*?)\n\}/);
if (!baseBlock) fail("motion.css has no :root block to read");

const durations = {};
for (const m of baseBlock[1].matchAll(/--dur-([a-z]+):\s*(\d+(?:\.\d+)?)ms;/g)) {
  durations[m[1]] = Number(m[2]) / 1000;
}
const distances = {};
for (const m of baseBlock[1].matchAll(/--(arrive-rise|arrive-leave):\s*(\d+)px;/g)) {
  distances[m[1].replace("arrive-", "")] = Number(m[2]);
}
const scaleFrom = baseBlock[1].match(/--overlay-from:\s*([\d.]+);/);
const hide = baseBlock[1].match(/--session-chrome-hide:\s*(\d+)ms;/);

const eases = {};
for (const m of semanticCss.matchAll(/--ease(?:-([a-z]+))?:\s*cubic-bezier\(([^)]*)\);/g)) {
  const name = m[1] ?? "default";
  eases[name] = m[2].split(",").map((n) => Number(n.trim()));
}

// **A floor.** A regex that stopped matching would generate an empty module
// that type-checks and animates nothing, which is a bug shaped exactly like a
// design choice.
if (Object.keys(durations).length < 5) fail(`found ${Object.keys(durations).length} durations; expected at least 5`);
if (Object.keys(eases).length < 3) fail(`found ${Object.keys(eases).length} easings; expected at least 3`);
if (!scaleFrom || !hide) fail("overlay-from or session-chrome-hide is missing");
if (Object.keys(distances).length !== 2) fail("arrive-rise and arrive-leave must both be present");

const body = `// GENERATED from src/tokens/motion.css and src/tokens/semantic.css by
// scripts/gen-motion.mjs. Do not edit: the CSS is the source of truth, and CI
// regenerates this file and fails if it has drifted.
//
// Seconds and bezier arrays, which is what the animation library takes. Use
// these for animations the library drives; CSS transitions read the tokens
// directly and never need this module.

/** Durations, in seconds, by purpose. */
export const duration = ${literal(durations)} as const;

/** Easing curves as cubic-bezier control points. */
export const ease = ${literal(eases)} as const;

/** The \`arrive\` motion: rise on enter, leave upward, in pixels. */
export const arrive = ${literal(distances)} as const;

/** The \`overlay\` motion opens from this scale. */
export const overlayFrom = ${Number(scaleFrom[1])} as const;

/** How long the session chrome waits before retracting, in seconds. */
export const sessionChromeHide = ${Number(hide[1]) / 1000} as const;
`;

const check = process.argv.includes("--check");
if (check) {
  const current = existsSync(out) ? readFileSync(out, "utf8") : "";
  if (current !== body) {
    console.error("  ✗ src/motion.ts is stale against the motion tokens — run `pnpm gen:motion`");
    process.exit(1);
  }
  console.log("  ✓ src/motion.ts matches the motion tokens");
} else {
  writeFileSync(out, body);
  console.log(`  ✓ wrote src/motion.ts (${Object.keys(durations).length} durations, ${Object.keys(eases).length} easings)`);
}

function literal(obj) {
  const entries = Object.entries(obj).map(([k, v]) => `  ${k}: ${JSON.stringify(v)},`);
  return `{\n${entries.join("\n")}\n}`;
}

function fail(why) {
  console.error(`  ✗ gen-motion: ${why}`);
  process.exit(1);
}
