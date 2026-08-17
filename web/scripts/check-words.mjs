#!/usr/bin/env node
// The content rules, measured on what is actually on screen.
//
// # Why these of all the rules
//
// The design system lists six content rules and checks none of them. Most are
// not machine-checkable — "errors say what to do" is a judgement — but four
// are *exactly* checkable, and they are the four that break by accident:
//
//   - **Never OK, never Yes.** A button names the action. `OK` is what
//     somebody writes at half past five, and it is the one word that tells a
//     person nothing about what they are about to do.
//   - **No exclamation marks.** They arrive in copy written to be friendly
//     and read, on a screen about somebody's machine, as alarm.
//   - **No "Oops", no bare "Something went wrong".** A sentence that reports
//     a failure without a next step is a sentence that wastes the one moment
//     somebody was paying attention.
//   - **Sentence case, titles too.** Title Case creeps in one heading at a
//     time and the drift is invisible until half the product has it.
//
// # It reads the rendered page, not the source
//
// Copy is assembled: a title from a prop, a label from a map, a sentence with
// a device's name in the middle of it. Grepping the source finds the string
// literals and misses everything composed — and what a person reads is the
// composition. So this reads the gate, which renders every component and
// every screen the product has.
//
// # Sentence case, and how a check avoids being a nuisance about it
//
// The naive rule flags every proper noun. This one only flags a heading whose
// *second or later* word is capitalised **and** which is not in the allowlist
// of names this product legitimately uses — a small, visible list. A rule
// that cried wolf on `Esper` would be a rule somebody turns off.
//
// Usage: node scripts/check-words.mjs [--url URL]
import { spawn } from "node:child_process";

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : fallback;
};
const base = flag("--url", "http://127.0.1.1:5178/");

/** A button label that names nothing. */
const EMPTY_WORDS = [/^ok$/i, /^yes$/i, /^no$/i, /^submit$/i, /^done!$/i];

/** Words that report a failure and offer nothing. */
const HOLLOW = [/\boops\b/i, /\bwhoops\b/i, /\buh[- ]oh\b/i, /something went wrong/i];

/**
 * Proper nouns that are legitimately capitalised mid-sentence.
 *
 * Deliberately short. The first version of this list had eighty entries and
 * was still growing, which is the signal that the *rule* was wrong rather
 * than the list incomplete — see `shoutyWords` for what replaced it. What is
 * left is names of things outside this product, plus the two words that are
 * genuinely proper nouns inside it.
 */
const NAMES = new Set(
  [
    "Esper", "Tailscale", "WireGuard", "ZeroTier", "Nebula", "Wayland", "TeamViewer",
    "Radix", "WebKitGTK", "Chromium", "Linux", "Windows", "Adwaita",
  ].map((n) => n.toLowerCase()),
);

/**
 * The words in a heading that are Title Case rather than sentence case.
 *
 * # Why this is not "any capital after the first word"
 *
 * Because that flags every component name and every acronym, and a rule that
 * cries wolf on `TierBadge` is a rule somebody turns off. Three things are
 * not prose and are not flagged:
 *
 *   - **A phrase boundary.** A heading is often a list — `Dialog, Tooltip,
 *     Command palette` — and each phrase starts a new sentence. So the
 *     heading is split on commas, dashes and colons first, and only a
 *     capitalised word *inside* a phrase counts.
 *   - **An identifier.** `TierBadge`, `SortableTable`, `StatTile`: two or
 *     more capitals with lower case between them is a name in code, and code
 *     is not written in sentence case.
 *   - **An acronym.** `HUD`, `RTT`, `X11`, `AA`.
 *
 * What is left is the thing this rule is actually about: an ordinary English
 * word capitalised in the middle of a heading, which is how Title Case
 * arrives — one heading at a time, invisibly, until half the product has it.
 */
function shoutyWords(heading) {
  const phrases = heading.split(/\s*[,—·:/()]\s*/).filter(Boolean);
  const out = [];
  for (const phrase of phrases) {
    const words = phrase.split(/\s+/).filter(Boolean);
    for (const word of words.slice(1)) {
      const bare = word.replace(/[^A-Za-z0-9]/g, "");
      if (!/^[A-Z][a-z]/.test(bare)) continue; // not Capitalised-then-lower
      if (/[A-Z]/.test(bare.slice(1))) continue; // an identifier: TierBadge
      if (NAMES.has(bare.toLowerCase())) continue;
      out.push(word);
    }
  }
  return out;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const port = 9720 + (process.pid % 80);
const chrome = spawn(
  process.env.CHROME ?? "/usr/bin/chromium",
  [
    "--headless=new", `--remote-debugging-port=${port}`, "--no-first-run", "--no-sandbox",
    "--disable-gpu", "--window-size=1400,1000", "--hide-scrollbars",
    `--user-data-dir=/tmp/esper-words-${process.pid}`, "about:blank",
  ],
  { stdio: "ignore" },
);

let target;
for (let i = 0; i < 60 && !target; i++) {
  try {
    target = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find((t) => t.type === "page");
  } catch {
    await sleep(200);
  }
}
if (!target) {
  chrome.kill();
  console.error("  ✗ words: chromium did not start. Set CHROME to a browser binary.");
  process.exit(1);
}

const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let id = 0;
const pending = new Map();
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) pending.get(m.id)(m), pending.delete(m.id);
};
ws.onclose = () => {
  for (const [, resolve] of pending) resolve({ gone: true });
  pending.clear();
};
const send = (method, params = {}) =>
  new Promise((resolve, reject) => {
    const i = ++id;
    pending.set(i, (m) => (m?.gone ? reject(new Error("the browser went away")) : resolve(m)));
    ws.send(JSON.stringify({ id: i, method, params }));
  });
const evaluate = async (expression) =>
  (await send("Runtime.evaluate", { expression, returnByValue: true })).result?.result?.value;

const findings = [];
let buttons = 0;
let headings = 0;
let sentences = 0;
let figures = 0;

try {
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Page.addScriptToEvaluateOnNewDocument", {
    source: `(() => {
      const write = (el) => { el.dataset.desktop = "true"; el.dataset.api = "127.0.0.1:8687"; };
      if (document.documentElement) return write(document.documentElement);
      new MutationObserver((_, o) => {
        if (document.documentElement) { write(document.documentElement); o.disconnect(); }
      }).observe(document, { childList: true, subtree: true });
    })();`,
  });
  await send("Page.navigate", { url: base.includes("#") ? base : `${base.replace(/\/$/, "")}/#/design` });
  await sleep(5000);

  const read = await evaluate(`(() => {
    const text = (el) => (el.textContent || "").replace(/\\s+/g, " ").trim();
    return JSON.stringify({
      // The words on every control, which is what "a button names the action"
      // is about. The accessible name, because an icon button's label is not
      // its text.
      buttons: [...document.querySelectorAll('button, [role="button"], [role="radio"], [role="menuitem"], a[href]')]
        .map((el) => (el.getAttribute("aria-label") || text(el)).trim())
        .filter(Boolean),
      headings: [...document.querySelectorAll("h1, h2, h3, h4, h5, h6")].map(text).filter(Boolean),
      /*
        Readouts: a piece of text that is a number, with a unit or without.

        # What this rule can and cannot be

        "Numbers are tabular" is **already guaranteed at the root**:
        index.css sets font-variant-numeric on the html element and the
        property inherits, so every digit in the product is tabular without
        any component asking. Proved by trying to break it from the other
        end — Figure was set in a proportional, non-mono font and all 108
        numbers on the page still resolved tabular, because the root had
        already said so.

        So a check that asks "is this number tabular" can only ever pass, and
        the useful question is the opposite one: **has anything turned it
        off.** Tailwind has proportional-nums, a component can write
        font-variant-numeric: normal, and either would be invisible except as
        a column of numbers that shifts as it counts. That is what this
        looks for.

        **Text nodes, not elements.** The first version looked for *elements*
        whose whole text is a number, and Figure renders the value and its
        unit as two nodes — so the outer span was skipped for having a child
        with text and the inner one for holding "ms" rather than a digit. The
        page is full of figures and the check found one. (No backticks in this
        comment: the whole block is a template literal, and one ends it.)
      */
      figures: (() => {
        const out = [];
        const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
        for (let n = walk.nextNode(); n; n = walk.nextNode()) {
          const value = (n.textContent || "").trim();
          if (!/^[+-]?\\d[\\d.,:\\s]*$/.test(value)) continue;
          const el = n.parentElement;
          if (!el) continue;
          const style = getComputedStyle(el);
          if (style.display === "none" || style.visibility === "hidden") continue;
          const box = el.getBoundingClientRect();
          if (box.width < 2 || box.height < 2) continue;
          out.push({
            text: value.slice(0, 12),
            at: el.tagName.toLowerCase() + "." + (el.className || "").toString().trim().split(/\\s+/).slice(0, 2).join("."),
            // Whether anything has turned the root's rule off here. A
            // monospaced family is tabular by construction whatever this says.
            proportional: /proportional-nums|normal/.test(style.fontVariantNumeric) && !/mono|JetBrains/i.test(style.fontFamily),
          });
        }
        return out;
      })(),
      // Everything a person reads. Bounded to elements that hold their own
      // text, so a container's whole subtree is not counted as one sentence.
      prose: [...document.querySelectorAll("p, li, dd, dt, span, div, td, th, label, figcaption")]
        // **The gate states the rules, which is not the product breaking
        // them.** The baseline section lists "No exclamation marks. No
        // \"Oops\"." — and a check that read that as a violation would make
        // documenting a rule impossible. The opt-out is an attribute on the
        // one container that quotes them, so it is visible in the markup
        // rather than a section name buried in this script.
        .filter((el) => !el.closest("[data-quotes-rules]"))
        .filter((el) => ![...el.children].some((c) => (c.textContent || "").trim().length > 0))
        .map(text)
        .filter((t) => t.length > 1),
    });
  })()`);
  const page = JSON.parse(read ?? '{"buttons":[],"headings":[],"prose":[]}');
  buttons = page.buttons.length;
  headings = page.headings.length;
  sentences = page.prose.length;

  if (buttons < 50 || sentences < 100) {
    findings.push(`the page has ${buttons} controls and ${sentences} pieces of text, so it did not render`);
  }

  // 1. A button names the action.
  for (const label of new Set(page.buttons)) {
    if (EMPTY_WORDS.some((rx) => rx.test(label))) {
      findings.push(`a control is labelled "${label}", which names no action`);
    }
  }

  // 2. No exclamation marks, anywhere a person reads.
  for (const line of new Set([...page.buttons, ...page.headings, ...page.prose])) {
    // A code sample or a shell line may legitimately carry one.
    if (/!/.test(line) && !/[`${}]|=>|!==|!=/.test(line)) {
      findings.push(`an exclamation mark: "${line.slice(0, 90)}"`);
    }
  }

  // 3. Nothing hollow.
  for (const line of new Set([...page.headings, ...page.prose])) {
    for (const rx of HOLLOW) {
      if (rx.test(line)) findings.push(`a sentence that reports a failure and offers nothing: "${line.slice(0, 90)}"`);
    }
  }

  // 4. Numbers are tabular.
  figures = page.figures.length;
  const jittery = new Map();
  for (const f of page.figures) {
    if (!f.proportional) continue;
    const key = `${f.at} ("${f.text}")`;
    jittery.set(key, (jittery.get(key) ?? 0) + 1);
  }
  for (const [where, count] of jittery) {
    findings.push(
      `${where} has the document's tabular figures turned off${count > 1 ? ` (${count} of them)` : ""} — a ` +
        `number that changes in place has to be tabular, or it shifts sideways as it counts`,
    );
  }
  // A floor: the product is an instrument panel, and a run that found no
  // readouts found nothing.
  if (figures < 20) {
    findings.push(`only ${figures} numbers were found on a page of readouts, so this rule measured nothing`);
  }

  // 5. Sentence case in headings.
  for (const heading of new Set(page.headings)) {
    const shouty = shoutyWords(heading);
    if (shouty.length > 0) {
      findings.push(`Title Case in a heading: "${heading}" — ${shouty.join(", ")}`);
    }
  }
} finally {
  ws.close();
  chrome.kill();
}

if (findings.length) {
  console.error("");
  for (const f of findings.slice(0, 20)) console.error(`  ✗ words: ${f}`);
  if (findings.length > 20) console.error(`  ✗ words: …and ${findings.length - 20} more`);
  process.exit(1);
}
console.log(
  `  ✓ words: ${buttons} control labels, ${headings} headings, ${sentences} sentences and ${figures} readouts — ` +
    `every control names an action, nothing shouts, nothing is hollow, nothing has turned the document's ` +
    `tabular figures off, sentence case throughout`,
);
