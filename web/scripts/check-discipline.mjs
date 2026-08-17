#!/usr/bin/env node
// The rules the tokens only work if everybody keeps.
//
// A design system is a set of decisions made once. Each rule here is a way of
// making a decision twice — a colour in a component, a z-index picked to win a
// fight, a breakpoint typed from memory, a duration that felt right — and each
// is caught by reading the source rather than by review, because review is how
// the first one gets in.
//
//   1. No raw colour outside the token files. `#1a1a1c`, `rgb(`, `oklch(` in
//      a component is a colour the theme cannot change. The one exception is
//      the presence indicator, which is *unthemeable by design* and says so.
//   2. No ad-hoc z-index. `z-index: var(--z-…)` or nothing.
//   3. Every media query *and container query* uses a breakpoint token's
//      value, so the number is written in one place.
//   4. Every transition and animation duration is a `--dur-*` token.
//   5. No `@keyframes` animates a layout property. A hand-written keyframe
//      cannot know the height a thing is about to be, so it guesses, and it
//      makes the browser lay the page out again on every frame of the guess.
//      Anything that changes size or position uses Motion's `layout` prop,
//      which measures before and after and animates the difference on the
//      compositor. Keyframes are for opacity, translate and scale.
//
// Run from web/. Exit 1 on any finding.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");
const src = join(root, "src");

// Where raw values are *allowed*, each with the reason.
const exempt = new Map([
  ["src/tokens/primitive.css", "the primitive layer is where raw values live"],
  ["src/tokens/semantic.css", "the semantic layer composes primitives; shadows carry literal alpha"],
  ["src/tokens/themes.css", "theme overrides may lift a lightness; they are the value layer"],
  ["src/tokens/motion.css", "the motion tokens and the 0.01ms sweep"],
  ["src/motion.ts", "generated from the tokens"],
  ["src/session/presence.contract.ts", "the presence contract names the two literal tones the indicator is held to"],
  [
    "src/session/presence.css",
    "the presence indicator is unthemeable by design: its colours are a contract, not a choice",
  ],
]);

function* files(dir) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) yield* files(p);
    else if (/\.(css|ts|tsx)$/.test(name)) yield p;
  }
}

const breakpoints = [...readFileSync(join(src, "tokens/semantic.css"), "utf8").matchAll(/--breakpoint-\w+:\s*(\d+)px/g)]
  .map((m) => m[1]);
if (breakpoints.length < 4) {
  console.error("  ✗ check-discipline: could not read the breakpoint tokens");
  process.exit(1);
}

/**
 * Properties whose animation makes the browser lay the page out again, and
 * which a keyframe therefore has no business naming: it would be guessing at
 * a number the browser already knows, once per frame.
 */
const LAYOUT_PROPERTIES = new Set([
  "width", "height", "min-width", "max-width", "min-height", "max-height",
  "inline-size", "block-size", "min-inline-size", "max-inline-size",
  "min-block-size", "max-block-size",
  "top", "right", "bottom", "left", "inset", "inset-block", "inset-inline",
  "inset-block-start", "inset-block-end", "inset-inline-start", "inset-inline-end",
  "margin", "margin-top", "margin-right", "margin-bottom", "margin-left",
  "margin-block", "margin-inline",
  "padding", "padding-top", "padding-right", "padding-bottom", "padding-left",
  "padding-block", "padding-inline",
  "flex-basis", "gap", "row-gap", "column-gap", "font-size",
]);

/** Every `@keyframes name { … }` in a stylesheet, braces matched rather than guessed. */
function keyframeBlocks(css) {
  const out = [];
  for (const m of css.matchAll(/@keyframes\s+([\w-]+)\s*\{/g)) {
    let depth = 1;
    let i = m.index + m[0].length;
    while (i < css.length && depth > 0) {
      if (css[i] === "{") depth++;
      else if (css[i] === "}") depth--;
      i++;
    }
    out.push({ name: m[1], at: m.index, body: css.slice(m.index + m[0].length, i - 1) });
  }
  return out;
}

const findings = [];
let scanned = 0;

for (const file of files(src)) {
  const rel = relative(root, file);
  const text = readFileSync(file, "utf8");
  scanned++;
  const isCss = file.endsWith(".css");
  const lines = text.split("\n");

  lines.forEach((line, i) => {
    const at = `${rel}:${i + 1}`;
    const code = line.replace(/\/\/.*$/, "").replace(/\/\*.*?\*\//g, "");

    // 1. Raw colour.
    if (!exempt.has(rel)) {
      if (/#(?:[0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})\b/i.test(code) && !/#\{|#root|#[a-z_-]+\b/i.test(code.replace(/#(?:[0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})\b/gi, ""))) {
        findings.push(`${at}: a hex colour — use a semantic token (${code.trim().slice(0, 70)})`);
      }
      if (/\b(?:rgba?|hsla?|oklch|oklab|lab|lch|hwb|color)\(/.test(code)) {
        findings.push(`${at}: a raw colour function — use a semantic token (${code.trim().slice(0, 70)})`);
      }
      if (/\b(?:bg|text|border|fill|stroke|ring|shadow|from|to|via)-\[(?:#|rgb|hsl|oklch)/.test(code)) {
        findings.push(`${at}: an arbitrary Tailwind colour — the token exists or should (${code.trim().slice(0, 70)})`);
      }
    }

    // 2. z-index.
    if (!rel.startsWith("src/tokens/")) {
      if (/z-index\s*:(?!\s*var\(--z-)/.test(code)) {
        findings.push(`${at}: an ad-hoc z-index — if two things fight, one is on the wrong layer (${code.trim().slice(0, 60)})`);
      }
      if (/\bz-(?:\d+\b|\[(?!var\(--z-))/.test(code)) {
        findings.push(`${at}: a Tailwind z utility — use z-[var(--z-…)] through a class that names the layer (${code.trim().slice(0, 60)})`);
      }
    }

    // 3. Breakpoints.
    if (isCss) {
      // rem as well as px: a query written in rem is the same decision made
      // in different units, and a rule that only read px would be walked
      // past by the first person who preferred rem.
      for (const m of code.matchAll(/@(media|container)[^{]*?\((?:min|max)-width:\s*([\d.]+)(px|rem)\)/g)) {
        const px = String(m[3] === "rem" ? Number(m[2]) * 16 : Number(m[2]));
        if (!breakpoints.includes(px)) {
          findings.push(`${at}: @${m[1]} at ${m[2]}${m[3]} (${px}px), which is not a breakpoint token (${breakpoints.join("/")})`);
        }
      }
    }

    // 3b. The same decision in Tailwind's arbitrary variants, which live in
    //     .tsx and so never reach the stylesheet check above.
    for (const m of code.matchAll(/(?:^|[\s"'`])(?:@|min-|max-)\[([\d.]+)(px|rem)\]:/g)) {
      const px = String(m[2] === "rem" ? Number(m[1]) * 16 : Number(m[1]));
      if (!breakpoints.includes(px)) {
        findings.push(`${at}: a ${m[1]}${m[2]} (${px}px) variant, which is not a breakpoint token (${breakpoints.join("/")})`);
      }
    }

    // 4. Durations.
    if (isCss && !exempt.has(rel)) {
      if (/(?:transition|animation)(?:-duration)?\s*:[^;]*\b\d+(?:\.\d+)?m?s\b/.test(code)) {
        findings.push(`${at}: a literal duration — durations are assigned by purpose, use var(--dur-…) (${code.trim().slice(0, 60)})`);
      }
    }
  });

  // 5. Keyframes that animate layout.
  //
  //    Whole-file rather than per-line: a keyframe block spans lines and
  //    nests, so the braces are matched rather than guessed at. The token
  //    files are not exempt from this one — a hand-tuned layout keyframe is
  //    wrong wherever it is written.
  if (isCss) {
    for (const block of keyframeBlocks(text)) {
      for (const m of block.body.matchAll(/(?:^|[;{])\s*([a-z-]+)\s*:/g)) {
        if (LAYOUT_PROPERTIES.has(m[1])) {
          const at = `${rel}:${text.slice(0, block.at).split("\n").length}`;
          findings.push(
            `${at}: @keyframes ${block.name} animates ${m[1]} — a keyframe cannot know the size a thing is about to be. ` +
              "Use Motion's layout prop; keyframes are for opacity, translate and scale",
          );
        }
      }
    }
  }
}

if (scanned < 5) {
  console.error(`  ✗ check-discipline: scanned only ${scanned} files, which is not the tree`);
  process.exit(1);
}

if (findings.length) {
  console.log(`  ✗ ${findings.length} finding(s) in ${scanned} files:\n`);
  for (const f of findings) console.log(`    ${f}`);
  process.exit(1);
}
console.log(`  ✓ discipline: ${scanned} files, no raw colour, no ad-hoc z-index, no layout keyframes, breakpoints and durations from tokens`);
