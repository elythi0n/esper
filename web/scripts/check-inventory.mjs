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
    return { name: field("name"), from: field("from"), what: field("what"), why: field("why") };
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

// ── Report ────────────────────────────────────────────────────────────────
if (failures.length) {
  for (const f of failures) console.error(`  ✗ inventory: ${f}`);
  process.exit(1);
}
console.log(
  `  ✓ inventory: ${tier1.length} Tier 1 components, all resolving; ${tier2.length} Tier 2 named with the screen that asks for them; ` +
    `${notBuilding.length} deliberately not built`,
);
if (unlisted.length) {
  console.log(`    ${unlisted.length} component(s) in a module no tier mentions — add them, or agree not to:`);
  for (const u of unlisted) console.log(`      ${u}`);
}
