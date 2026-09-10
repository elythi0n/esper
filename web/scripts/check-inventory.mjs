#!/usr/bin/env node
// The component inventory names components that exist.
//
// `src/components/inventory.ts` is the agreed Tier 1, the Tier 2 that is
// coming, and the list of things deliberately not being built. The first of
// those is a claim about the codebase, and a claim nothing checks is a
// document that goes stale the first time somebody renames an export.
//
// So: every Tier 1 entry must name a module that exists and a symbol that
// module exports. And the reverse, reported rather than failed — a component
// in `src/components/` that no tier mentions is either a Tier 1 entry
// somebody forgot to add or a component nobody agreed to.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const src = join(here, "..", "src");
const inventoryPath = join(src, "components", "inventory.ts");
const inventory = readFileSync(inventoryPath, "utf8");

// ── Read the lists ────────────────────────────────────────────────────────
// The file is data with no logic in it, so it is read rather than imported:
// this stays a plain script with no build step in front of it.
function entries(constName) {
  const m = inventory.match(new RegExp(`export const ${constName}[^=]*=\\s*\\[([\\s\\S]*?)\\n\\];`));
  if (!m) fail(`inventory.ts has no ${constName}`);
  return [...m[1].matchAll(/\{[^}]*\}/g)].map((o) => {
    const field = (k) => o[0].match(new RegExp(`${k}:\\s*"((?:[^"\\\\]|\\\\.)*)"`))?.[1];
    return {
      name: field("name"),
      from: field("from"),
      what: field("what"),
      why: field("why"),
      waits: field("waits"),
      becauseOf: field("becauseOf"),
    };
  });
}

const failures = [];
function fail(msg) {
  failures.push(msg);
}

const tier1 = entries("TIER_1");
const tier2 = entries("TIER_2");
const notBuilding = entries("NOT_BUILDING");

// A floor, because a regex that stopped matching would otherwise verify an
// empty set and print a clean report.
//
// **Per-list floors were the wrong shape.** Tier 2 had a floor of five, and
// the way to fail it is to *build* five of them — which is the work
// succeeding, reported as the inventory being broken. All three lists come
// out of one `entries()`, so a rotted regex empties all three at once; what
// guards that is a floor on the total and a requirement that none of them is
// empty. Tier 1 keeps its own floor: it is the list that must not quietly
// shrink, because every entry in it is a component something imports.
if (tier1.length < 25) fail(`Tier 1 parsed as ${tier1.length} entries, which is not the agreed inventory`);
for (const [name, list] of [["Tier 2", tier2], ["Not building", notBuilding]]) {
  if (list.length === 0) fail(`${name} parsed as no entries at all — the pattern has rotted`);
}
const total = tier1.length + tier2.length + notBuilding.length;
if (total < 40) fail(`the whole inventory parsed as ${total} entries, which is not this one`);
if (notBuilding.length < 5) fail(`the not-building list parsed as ${notBuilding.length} entries — it is as important as the other two`);

/*
  **A reason that rests on another component is held to it existing.**

  Several entries on the not-building list are not "we do not want one" but
  "we already have one": no MonitorPicker because `MonitorSwitcher` is one, no
  windowing library because `LogView` is twenty lines, no charting library
  because `Sparkline` is the only chart. Each of those is a decision that
  holds *only while the named component does* — delete or rename it and the
  entry still reads as a considered choice while having quietly become an
  omission.

  That is the state somebody signing a twenty-entry list off cannot see, and
  it is the difference between "declared out of scope" and "about to be
  needed".
*/
const tier1Names = new Set(tier1.map((e) => e.name));
let resting = 0;
for (const entry of notBuilding) {
  if (!entry.becauseOf) continue;
  resting++;
  if (!tier1Names.has(entry.becauseOf)) {
    fail(
      `"${entry.name}" is not built because ${entry.becauseOf} already does that job, ` +
        `and ${entry.becauseOf} is not in Tier 1 — the reason has outlived the component ` +
        `it rests on, and the entry now reads as a decision while being an omission`,
    );
  }
}
if (resting < 3) {
  fail(
    `only ${resting} not-building entries name the component they rest on — the field is not ` +
      `being read, and an entry whose reason has gone false would pass`,
  );
}

// ── Every Tier 1 entry resolves ──────────────────────────────────────────
/** The symbols a module exports, by the four forms this codebase uses. */
function exportsOf(text) {
  const names = new Set();
  for (const m of text.matchAll(/export\s+(?:async\s+)?(?:function|const|class|type|interface)\s+([A-Za-z_$][\w$]*)/g)) names.add(m[1]);
  for (const m of text.matchAll(/export\s*\{([^}]*)\}/g)) {
    for (const part of m[1].split(",")) {
      const as = part.split(/\bas\b/);
      const name = (as[1] ?? as[0]).trim().replace(/^type\s+/, "");
      if (name) names.add(name);
    }
  }
  return names;
}

/** The same, minus the types: a `type` or an `interface` is not a component. */
function valueExportsOf(text) {
  const types = new Set();
  for (const m of text.matchAll(/export\s+(?:type|interface)\s+([A-Za-z_$][\w$]*)/g)) types.add(m[1]);
  return new Set([...exportsOf(text)].filter((n) => !types.has(n)));
}

const cache = new Map();
for (const e of tier1) {
  if (!e.name || !e.from) {
    fail(`a Tier 1 entry is missing its name or module: ${JSON.stringify(e)}`);
    continue;
  }
  const path = join(src, e.from);
  let text = cache.get(path);
  if (text === undefined) {
    try {
      text = readFileSync(path, "utf8");
    } catch {
      text = null;
    }
    cache.set(path, text);
  }
  if (text === null) {
    fail(`Tier 1 names src/${e.from}, which does not exist (${e.name})`);
    continue;
  }
  if (!exportsOf(text).has(e.name)) fail(`src/${e.from} does not export ${e.name}, which Tier 1 says it does`);
  if (!e.what) fail(`${e.name} has no description; an inventory entry that does not say what it is for is a name`);
}

// ── And the reverse, as a report ─────────────────────────────────────────
// Not a failure: a helper beside a component is not a component, and this
// cannot tell the difference. It can tell somebody to look.
function* files(dir) {
  for (const n of readdirSync(dir)) {
    const p = join(dir, n);
    if (statSync(p).isDirectory()) yield* files(p);
    else if (/\.tsx$/.test(n)) yield p;
  }
}

// A module with a Tier 1 entry is accounted for, and the parts it also
// exports — CardHeader, SelectTrigger, TD — are parts of that component
// rather than components of their own. What this looks for is a module no
// tier mentions at all.
const listedModules = new Set(tier1.map((e) => e.from));
const named = new Set([...tier1, ...tier2].map((e) => e.name));
const unlisted = [];
// `components/` **and the screens' own vocabulary**, which is where this
// sweep was not looking. `screens/parts.tsx` holds the seven things every
// product screen is built from — the most-used set in the codebase — and
// because it sits beside the screens rather than under `components/`, nothing
// asked whether it was in a tier. It was not, for as long as it had existed.
// Scanning it prints those names on every run until somebody either adds them
// or agrees not to, which is all this report is for.
const swept = [...files(join(src, "components")), join(src, "screens", "parts.tsx")];
for (const file of swept) {
  const rel = relative(src, file);
  if (listedModules.has(rel)) continue;
  for (const name of valueExportsOf(readFileSync(file, "utf8"))) {
    if (!/^[A-Z]/.test(name) || named.has(name)) continue;
    unlisted.push(`${rel}:${name}`);
  }
}

/*
  ── Every Tier 1 component reaches a screen, or says why it does not ───────

  # The state a design system rots in

  "Built, and nothing renders it." The component is written, reviewed and
  forgotten, and by the time a screen wants one somebody writes a second
  inline. That happened twice here: a transfer readout joined into a string
  beside a queue component that draws bars, and an age computed once beside an
  element whose whole job is to keep one current.

  Nothing could see it. Every gate here asked whether a component *resolves*,
  which a component nobody renders does perfectly.

  # Why the walk is transitive

  Because most components are reached through another one. `TransferProgress`
  is drawn by `TransferQueue`, which a screen opens; a check that only looked
  at screen files would report it missing and be wrong. So this starts from
  what the screens use and follows the graph out through the modules those
  components live in, until it stops growing.

  What is left is held to one of two things: something renders it, or the
  entry says why not.
*/
/*
  The product's own screens: not `components/`, and **not the gallery**.

  `design/` renders every component there is — that is what it is for — so
  counting it would make this check pass by construction and say nothing. The
  question is whether the product uses a component, not whether the catalogue
  shows it.
*/
const screenFiles = [...files(src)].filter((f) => {
  const rel = relative(src, f);
  return /\.tsx?$/.test(rel) && !/^(components|design)[/\\]/.test(rel);
});
const sourceOf = new Map();
const readOnce = (file) => {
  if (!sourceOf.has(file)) sourceOf.set(file, readFileSync(file, "utf8"));
  return sourceOf.get(file);
};
/*
  Rendered or imported, never merely mentioned.

  A plain word search says yes to a comment. `LogView` explains itself by
  arguing against a virtualised `<tbody>` and names `Table` while doing it, so
  a search for the word found `Table` "reaching a screen" through a paragraph
  about why it does not. What counts is the two ways a component is actually
  put to work: written as an element, or named in an import.

  Word-bounded either way, so `Table` does not match `SortableTable` and
  `Card` does not match `CardHeader`.
*/
const mentions = (name, text) =>
  new RegExp(`<${name}\\b`).test(text) ||
  [...text.matchAll(/import[^;]*?from\s*["'][^"']+["']/gs)].some((m) =>
    new RegExp(`\\b${name}\\b`).test(m[0]),
  );

const reached = new Set();
let searching = screenFiles;
// A bound, and a failure if it is ever reached: a walk that quietly stopped
// early would report components as stranded that are not, which is the one way
// this check could start lying.
const mostRounds = 24;
let rounds = 0;
for (; rounds < mostRounds && searching.length > 0; rounds += 1) {
  const found = [];
  const text = searching.map(readOnce).join("\n");
  for (const entry of tier1) {
    if (reached.has(entry.name)) continue;
    if (mentions(entry.name, text)) {
      reached.add(entry.name);
      found.push(join(src, entry.from));
    }
  }
  // The modules those components live in, which is where the next layer is.
  searching = [...new Set(found)].filter((f) => {
    try {
      return statSync(f).isFile();
    } catch {
      return false;
    }
  });
}

if (rounds >= mostRounds) {
  fail(
    `the walk through the component graph was still finding things after ${mostRounds} rounds — ` +
      `it is either cycling or the graph is deeper than this expects, and either way what it ` +
      `reports below cannot be trusted`,
  );
}

const stranded = tier1.filter((e) => !reached.has(e.name) && !e.waits);
for (const e of stranded) {
  failures.push(
    `${e.name} (${e.from}) is built and no screen reaches it, directly or through another component — ` +
      `render it, or give its entry a \`waits\` saying why not`,
  );
}
// And the opposite: a reason that has stopped being true is a reason nobody
// re-reads. A component that now reaches a screen must not still claim to be
// waiting for one.
for (const e of tier1) {
  if (e.waits && reached.has(e.name)) {
    failures.push(
      `${e.name} says it is waiting for a screen and a screen renders it — take the \`waits\` off, ` +
        `or the next person reads a decision that was reversed`,
    );
  }
}

// ── Report ────────────────────────────────────────────────────────────────
if (failures.length) {
  for (const f of failures) console.error(`  ✗ inventory: ${f}`);
  process.exit(1);
}
console.log(
  `  ✓ inventory: ${tier1.length} Tier 1 components, all resolving; ${reached.size} reaching a screen and ` +
    `${tier1.length - reached.size} saying why they do not; ${tier2.length} Tier 2 named with the screen that ` +
    `asks for them; ${notBuilding.length} deliberately not built, ${resting} of those ` +
      `resting on a component that exists`,
);
if (unlisted.length) {
  console.log(`    ${unlisted.length} component(s) in a module no tier mentions — add them, or agree not to:`);
  for (const u of unlisted) console.log(`      ${u}`);
}
