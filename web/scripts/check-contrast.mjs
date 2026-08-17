#!/usr/bin/env node
// Every colour pairing the system uses, measured, in every theme.
//
// # The gate, graded
//
// Fail-the-build-below-AA is the right end state and the wrong first step. A
// gate that fails on a disabled label or a 1px hairline blocks legitimate work
// while the pairing table is still being written, and a gate people work
// around is a gate nobody has. So this runs in three grades:
//
//   required  — the pairings the system has agreed to. A failure here fails
//               the build. Text on every surface, `-fg` on its fill, a tone on
//               its own soft fill, the strong border and the focus ring on
//               every surface, the signal as an indicator.
//   watched   — pairings that are *not* held to the bar and are exactly where
//               this product will be tempted to cheat: disabled text, and
//               anything drawn over video. Reported every run; never fail.
//   text pairs in high contrast are held to AAA (7:1), not AA.
//
// A new pairing gets a new row here, or it is not a pairing the system has
// agreed to.
//
// # What it reads
//
// The token files themselves, resolved the way a browser would: `var()`
// chains, `oklch()` with variable hue, `color-mix(in oklch, X N%, transparent)`
// as X at alpha N, and alpha composited over the surface the pair sits on.
// Nothing here is hand-copied from the CSS; if the CSS moves, this moves.
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { contrast, oklchToLinear, over } from "./color.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const tokens = join(here, "..", "src", "tokens");
const read = (f) => readFileSync(join(tokens, f), "utf8");

// ── Parsing ───────────────────────────────────────────────────────────────

/** Every `--name: value;` in a block of CSS text, as a map. */
function declarations(block) {
  const out = new Map();
  for (const m of block.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) out.set(m[1], m[2].trim());
  return out;
}

/** The body of the first rule whose selector matches. */
function block(css, selectorPattern) {
  const re = new RegExp(`${selectorPattern}\\s*\\{([\\s\\S]*?)\\n\\s*\\}`);
  const m = css.match(re);
  return m ? m[1] : "";
}

const primitive = declarations(block(read("primitive.css"), ":root"));
const semanticCss = read("semantic.css");
const namespace = declarations(block(semanticCss, "@theme(?:\\s+inline)?"));
const dark = declarations(block(semanticCss, ":root,\\s*\\[data-session\\]"));
const themesCss = read("themes.css");
const light = declarations(block(themesCss, '\\[data-theme="light"\\]'));
const hc = declarations(block(themesCss, '\\[data-theme="hc"\\]'));
const session = declarations(block(themesCss, "\\[data-session\\]"));

// A floor under the parser. Blocks that matched nothing would make every
// pairing below unresolvable — and an unresolvable pairing is *skipped*, so an
// empty table would print a clean report.
if (primitive.size < 20 || namespace.size < 40 || dark.size < 20 || light.size < 20 || hc.size < 20) {
  console.error(
    `  ✗ check-contrast: parsed ${primitive.size}/${namespace.size}/${dark.size}/${light.size}/${hc.size} ` +
      "declarations (primitive/namespace/dark/light/hc); at least one token block did not parse",
  );
  process.exit(1);
}

// **The session redefines no colour.** Its darkness comes from the value
// layer's selector, and a colour here would be a second dark palette that
// drifts from the first.
for (const k of session.keys()) {
  if (k.startsWith("--sem-") && !k.includes("dur")) {
    console.error(`  ✗ [data-session] redefines ${k}: the session is dark by inheriting the dark value layer, not by re-declaring colours`);
    process.exit(1);
  }
}

// ── Colour ───────────────────────────────────────────────────────────────

/** Resolve a value in a theme: theme overrides, then dark base, then primitives. */
function lookup(name, theme) {
  return theme.get(name) ?? dark.get(name) ?? namespace.get(name) ?? primitive.get(name);
}

/** Parse a colour expression to {r,g,b,a} in linear sRGB, or null. */
function color(expr, theme, depth = 0) {
  if (depth > 16) return null;
  expr = expr.trim();
  if (expr === "transparent") return { r: 0, g: 0, b: 0, a: 0 };
  let m;
  if ((m = expr.match(/^var\((--[\w-]+)\)$/))) {
    const v = lookup(m[1], theme);
    return v ? color(v, theme, depth + 1) : null;
  }
  if ((m = expr.match(/^color-mix\(in oklch,\s*(.+?)\s+([\d.]+)%,\s*transparent\)$/))) {
    const c = color(m[1], theme, depth + 1);
    return c ? { ...c, a: c.a * (Number(m[2]) / 100) } : null;
  }
  if ((m = expr.match(/^oklch\(\s*([\d.]+)\s+([\d.]+)\s+(\S+?)\s*(?:\/\s*([\d.]+))?\s*\)$/))) {
    let hue = m[3];
    if (hue.startsWith("var(")) {
      const hv = lookup(hue.slice(4, -1), theme);
      if (!hv) return null;
      hue = hv;
    }
    return oklchToLinear(Number(m[1]), Number(m[2]), Number(hue), m[4] === undefined ? 1 : Number(m[4]));
  }
  return null;
}

// ── The table ─────────────────────────────────────────────────────────────

const AA = 4.5, AAA = 7, GRAPHIC = 3;
const surfaces = ["--color-surface-0", "--color-surface-1", "--color-surface-2"];
const tones = ["ok", "warn", "danger", "info"];

/**
 * Every row: what is on what, the bar, and whether the bar is enforced.
 * `text: true` marks a text pairing, which is held to AAA in high contrast.
 */
function rows() {
  const r = [];
  const req = (fg, bg, min, text, why) => r.push({ fg, bg, min, text, required: true, why });
  const watch = (fg, bg, min, text, why) => r.push({ fg, bg, min, text, required: false, why });

  for (const s of surfaces) {
    req("--color-text", s, AA, true, "body text");
    req("--color-text-2", s, AA, true, "secondary text");
    req("--color-text-3", s, AA, true, "tertiary text");
    // Disabled text is exempt from WCAG and is exactly where a palette gets
    // lazy — a disabled label nobody can read is a control nobody knows about.
    watch("--color-text-disabled", s, GRAPHIC, false, "disabled text (watched: WCAG exempts it, we do not)");
    req("--color-border-strong", s, GRAPHIC, false, "a control's boundary");
    req("--color-focus", s, GRAPHIC, false, "the focus ring");
    req("--color-signal", s, GRAPHIC, false, "the signal, as an indicator");
    for (const t of tones) {
      req(`--color-${t}`, s, AA, true, `${t} as text on the surface`);
      // The tone's text on its own soft fill, composited over this surface.
      r.push({ fg: `--color-${t}`, bg: s, soft: `--color-${t}-soft`, min: AA, text: true, required: true, why: `${t} text on its own soft fill` });
    }
  }
  req("--color-accent-fg", "--color-accent", AA, true, "text on a primary (inverse) button");
  req("--color-signal-fg", "--color-signal", AA, true, "text on a signal fill");
  for (const t of tones) req(`--color-${t}-fg`, `--color-${t}`, AA, true, `text on a solid ${t} fill`);

  // Over video. The chrome is opaque, but the temptation to float a label on
  // the picture is real and this is where it would be cheated.
  watch("--color-text", "--color-video", AA, true, "text drawn directly over video (watched)");
  watch("--color-border-strong", "--color-video", GRAPHIC, false, "a control edge over video (watched)");
  return r;
}

// ── Run ───────────────────────────────────────────────────────────────────

const themes = [["dark", new Map()], ["light", light], ["hc", hc]];
let failures = 0, warnings = 0, evaluated = 0, unresolved = 0;
const lines = [];

for (const [name, theme] of themes) {
  lines.push(`\n  ${name}`);
  for (const row of rows()) {
    let fg = color(`var(${row.fg})`, theme);
    let bg = color(`var(${row.bg})`, theme);
    if (!fg || !bg) { unresolved++; lines.push(`  ? ${row.fg} on ${row.bg} — could not resolve`); continue; }
    if (row.soft) {
      const soft = color(`var(${row.soft})`, theme);
      if (!soft) { unresolved++; continue; }
      bg = over(soft, bg);
    }
    if (fg.a < 1) fg = over(fg, bg);
    const min = row.text && name === "hc" ? AAA : row.min;
    const got = contrast(fg, bg);
    evaluated++;
    const label = `${row.fg.replace("--color-", "")} on ${row.bg.replace("--color-", "")}${row.soft ? " (soft)" : ""}`;
    if (got >= min) {
      lines.push(`  ✓ ${got.toFixed(2).padStart(5)} ≥ ${min}  ${label}`);
    } else if (row.required) {
      failures++;
      lines.push(`  ✗ ${got.toFixed(2).padStart(5)} < ${min}  ${label} — ${row.why}`);
    } else {
      warnings++;
      lines.push(`  ⚠ ${got.toFixed(2).padStart(5)} < ${min}  ${label} — ${row.why}`);
    }
  }
}

const verbose = process.argv.includes("--verbose");
if (verbose || failures) console.log(lines.filter((l) => verbose || !l.startsWith("  ✓")).join("\n"));

// Another floor: a table that evaluated almost nothing is a broken resolver,
// not a clean palette.
if (evaluated < 60 || unresolved > 0) {
  console.error(`\n  ✗ check-contrast: evaluated ${evaluated} pairings with ${unresolved} unresolved — the resolver is not reading the tokens`);
  process.exit(1);
}

console.log(
  `\n  ${failures ? "✗" : "✓"} contrast: ${evaluated} pairings across 3 themes, ` +
    `${failures} required failure(s), ${warnings} watched below the bar`,
);
if (failures) {
  console.log("    A required pairing is below its bar. Solve the lightness in primitive.css; do not lower the bar.");
  process.exit(1);
}
