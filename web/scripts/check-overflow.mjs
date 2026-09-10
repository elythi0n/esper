#!/usr/bin/env node
// Nothing runs off the side, nothing gets cut off, and text keeps its measure.
//
// # Why a gate for this
//
// Because the bug was reported by a person looking at the window, and every
// other check here passed while it was true. The window had grown a
// horizontal scrollbar and the top bar appeared to go missing as content slid
// under it — two faults with one cause each, and both invisible to a contrast
// checker, a token lint, an accessibility tree and a frame timer. A layout
// fault is only a fault at a size, and nothing was looking at sizes.
//
// Four claims, all read from a real browser at four widths in three themes:
//
//   - **Nothing extends past the viewport.** A window that scrolls sideways
//     is a window whose chrome is somewhere off to the right.
//   - **No copy is clipped.** An element that hides its overflow and holds
//     text wider than itself has cut a sentence in half — unless it says so
//     with an ellipsis, which is the deliberate case and is allowed.
//   - **Running text keeps the measure.** Past roughly a hundred characters
//     the return sweep fails; `--measure` exists so that cannot happen, and
//     an uncapped paragraph on a wide display is the way it comes back.
//   - **The pages have to be the pages.** With nothing answering on the
//     control plane every route renders the same "nothing is answering" card,
//     and a run that measures that card seven times finds nothing while
//     reporting twenty-one clean renders. Seven routes that draw fewer than
//     four different screens is not a measurement.
//   - **The scroll boundary appears.** The topbar draws its stronger hairline
//     only when something is behind it, through `:has(+ …)` — a selector I
//     cannot know works by reading it, so this scrolls the page and reads the
//     computed style back.
//
// Usage: node scripts/check-overflow.mjs [--url URL]
import { spawn } from "node:child_process";

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : fallback;
};
const base = flag("--url", "http://127.0.1.1:5178/").replace(/\/$/, "");

/*
  Every route, and the sizes that matter.

  380 is a phone, where the shell falls back to tabs. 900 is a window dragged
  to half a laptop screen, which is where the rail arrangement lives and where
  the reported bug was. 1400 is the ordinary case. 2560 is the one nobody
  tests and the only width at which an uncapped measure is visible.
*/
const ROUTES = ["devices", "sessions", "pair", "invite", "activity", "settings", "design"];
const SIZES = [
  { w: 380, h: 720 },
  { w: 900, h: 700 },
  { w: 1400, h: 1000 },
  { w: 2560, h: 1200 },
];
const THEMES = ["dark", "light", "hc"];

/*
  Device pixel ratios, and the fractional one is the point.

  Every browser gate in this project has run at a scale factor of exactly 1,
  and the review before Phase 11 said so as a limit: "every browser number
  comes from this machine's Chromium at one device pixel ratio". At 1 and at 2
  a CSS pixel lands on whole device pixels and layout arithmetic is exact.

  **At 1.5 it is not**, and 1.5 is not exotic — it is GNOME's fractional
  scaling and Windows' 150%, which is a very large share of laptops. A width
  that exactly fills its container rounds up by half a device pixel and the
  page gains a horizontal scrollbar; a hairline lands between two rows of
  device pixels and the browser has to choose. The first of those is the exact
  bug this gate was built for, reported by a person and passed by every gate
  here at the time.

  One extra pass rather than the full cross product: the rounding is a property
  of the layout and the width, not of the palette, so a second scale factor
  over every route and width in one theme is where a finding would be — and
  three times the renders for a dimension that cannot interact with it would
  be a slower gate answering the same question.
*/
const SCALES = [1, 1.5];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const port = 9700 + (process.pid % 90);
const chrome = spawn(
  process.env.CHROME ?? "/usr/bin/chromium",
  [
    "--headless=new", `--remote-debugging-port=${port}`, "--no-first-run", "--no-sandbox",
    "--disable-gpu", "--window-size=1400,1000", "--hide-scrollbars",
    `--user-data-dir=/tmp/esper-overflow-${process.pid}`, "about:blank",
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
  console.error("  ✗ overflow: chromium did not start. Set CHROME to a browser binary.");
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
/** For an expression that answers after a frame or two. */
const evaluateAsync = async (expression) =>
  (await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true })).result?.result?.value;

/*
  What one rendered page is, measured.

  A pixel of slack on every comparison. Sub-pixel layout is ordinary — a
  centred element at a fractional width, a border that resolves to 0.5px — and
  a check that fails on 0.4px is a check somebody switches off. What it is
  looking for is a *scrollbar*, which is tens of pixels.

  Two exemptions, both narrow and both stated here rather than in a list of
  element ids:

    - An element inside a deliberate scroller (`overflow-x: auto`) may be
      wider than the viewport. That is what the scroller is for — a wide table
      inside a narrow window — and the page still does not scroll.
    - Text that ends in an ellipsis is truncated on purpose and says so. A
      device name in a 200px cell has to end somewhere, and the full value is
      elsewhere (a title, a detail panel).
*/
const measurePage = `(() => {
  const SLACK = 1;
  const root = document.documentElement;
  const page = root.scrollWidth - root.clientWidth;
  const body = document.querySelector(".page-body");
  const bodyScroll = body ? body.scrollWidth - body.clientWidth : 0;

  // A canvas measures the element's own font, so "how many characters wide is
  // this" is answered in the font it is set in rather than by a guess at an
  // average glyph.
  // Which screen this is, so a run cannot measure one page seven times and
  // call it seven routes. The first heading is the page's own title on every
  // route that has one; a screen that is a single sentence — the one the app
  // shows when nothing is answering — has none, so its first line stands in,
  // because "(no heading)" names nothing anybody can act on.
  const firstLine = (document.body.innerText || "").split("\\n").map((l) => l.trim()).find(Boolean);
  const heading = (document.querySelector("h1, h2")?.textContent?.trim() || firstLine || "").slice(0, 60) || null;

  const ruler = document.createElement("canvas").getContext("2d");
  const charWidth = (style) => {
    ruler.font = style.font || (style.fontWeight + " " + style.fontSize + " " + style.fontFamily);
    const m = ruler.measureText("0123456789");
    return m.width / 10 || Number.parseFloat(style.fontSize) * 0.5;
  };

  const scrollerOf = (el) => {
    for (let at = el.parentElement; at; at = at.parentElement) {
      const o = getComputedStyle(at).overflowX;
      if (o === "auto" || o === "scroll") return at;
    }
    return null;
  };

  const past = [];
  const clipped = [];
  const long = [];
  const width = root.clientWidth;
  for (const el of document.querySelectorAll("body *")) {
    const style = getComputedStyle(el);
    if (style.display === "none" || style.visibility === "hidden") continue;
    const box = el.getBoundingClientRect();
    if (box.width === 0 && box.height === 0) continue;

    // 1. Off the right-hand edge, unless a scroller owns it.
    if (box.right > width + SLACK && !scrollerOf(el)) {
      past.push({ at: describe(el), by: Math.round(box.right - width) });
    }

    // 2. Text cut off with nothing saying so.
    const hides = style.overflowX === "hidden" || style.overflow === "hidden";
    const says = style.textOverflow === "ellipsis" || style.webkitLineClamp !== "none";
    const over = el.scrollWidth - el.clientWidth;
    if (hides && !says && over > SLACK && el.textContent && el.textContent.trim().length > 0) {
      // Only where the *text* is what overflows: a wrapper holding a wide
      // child is the child's problem and is reported as that child.
      const kids = [...el.children];
      const widest = kids.length ? Math.max(...kids.map((k) => k.scrollWidth)) : 0;
      if (widest <= el.clientWidth + SLACK) {
        clipped.push({ at: describe(el), by: Math.round(over), text: el.textContent.trim().slice(0, 60) });
      }
    }

    // 3. A line of running text too long to read.
    const own = [...el.childNodes].filter((n) => n.nodeType === 3).map((n) => n.textContent.trim()).join(" ");
    if (own.length > 160) {
      const chars = box.width / charWidth(style);
      if (chars > 108) long.push({ at: describe(el), chars: Math.round(chars), text: own.slice(0, 60) });
    }
  }

  function describe(el) {
    const cls = (el.className || "").toString().trim().split(/\\s+/).slice(0, 3).join(".");
    return el.tagName.toLowerCase() + (cls ? "." + cls : "");
  }

  // Worst first. Document order is useless here: the widest thing is the one
  // that made the window scroll, and it is rarely the first one found.
  past.sort((a, b) => b.by - a.by);
  clipped.sort((a, b) => b.by - a.by);
  long.sort((a, b) => b.chars - a.chars);
  return JSON.stringify({ page, bodyScroll, heading, past: past.slice(0, 4), clipped: clipped.slice(0, 6), long: long.slice(0, 6), seen: document.querySelectorAll("body *").length });
})()`;

/*
  The scroll boundary, before and after.

  The page body is scrolled by hand rather than by dispatching a wheel event,
  because a headless browser with no scrollbars still scrolls a container
  whose `scrollTop` is set — and what the claim is about is the attribute the
  React handler writes and the selector that reads it, not the input device.
*/
const boundary = `(() => {
  // The app's own shell, and only if there is exactly one: the design page
  // renders nine working shells as examples, and the boundary of an example
  // is not the claim. The body is the topbar's *next sibling*, which is what
  // the \`:has(+ …)\` selector under test requires.
  if (document.querySelectorAll(".topbar").length !== 1) {
    return JSON.stringify({ why: "this route draws no shell of its own, or several" });
  }
  const bar = document.querySelector(".topbar");
  const body = document.querySelector(".topbar + .page-body");
  if (!bar || !body) return JSON.stringify({ why: "no topbar and page body on this route" });
  if (body.scrollHeight <= body.clientHeight + 2) return JSON.stringify({ short: true });
  const read = () => {
    const s = getComputedStyle(bar);
    return { border: s.borderBottomColor, shadow: s.boxShadow };
  };
  body.scrollTop = 0;
  const atTop = read();
  body.scrollTop = 200;
  body.dispatchEvent(new Event("scroll", { bubbles: false }));
  return new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => {
    const scrolled = read();
    const marked = body.dataset.scrolled === "true";
    body.scrollTop = 0;
    body.dispatchEvent(new Event("scroll", { bubbles: false }));
    done(JSON.stringify({ atTop, scrolled, marked }));
  })));
})()`;

/*
  Findings are a set, keyed by their own text.

  A layout fault is not a theme fault: the same over-long line is found in
  dark, light and high contrast, and printing it three times buries the
  fifteen other things the run found. Where a fault *is* theme-specific — high
  contrast draws wider borders — the theme is in the message and the two do
  not collapse.
*/
/** Pass ("900|dark") to route to the heading that route drew. */
const screens = new Map();
const found = new Set();
const note = (line) => found.add(line);
const findings = [];
const rows = [];
let boundaryChecked = 0;

try {
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: "reduce" }] });

  for (const scale of SCALES) {
  for (const size of SIZES) {
    await send("Emulation.setDeviceMetricsOverride", { width: size.w, height: size.h, deviceScaleFactor: scale, mobile: false });
    // Only the primary theme at the second scale: see `SCALES`.
    for (const theme of scale === 1 ? THEMES : ["dark"]) {
      for (const route of ROUTES) {
        await send("Page.navigate", { url: `${base}/#/${route}` });
        await sleep(route === "design" ? 2600 : 1100);
        await evaluate(`(() => { document.documentElement.dataset.theme = ${JSON.stringify(theme)}; return 1; })()`);
        await sleep(250);
        // **The emulation is asked whether it took.** A scale factor the
        // browser silently ignored would make the second pass twenty-eight
        // more renders at exactly the ratio already swept — a gate reporting
        // that it measured something it did not, which is the failure this
        // whole file exists to avoid.
        const ratio = Number(await evaluate("window.devicePixelRatio"));
        if (Math.abs(ratio - scale) > 0.01) {
          note(
            `the browser reports a device pixel ratio of ${ratio} where ${scale} was asked for — ` +
              `the emulation did not take, so this pass measured the ratio before it`,
          );
          continue;
        }
        const seen = JSON.parse((await evaluate(measurePage)) ?? "{}");
        if (!seen.seen || seen.seen < 20) {
          note(`${route} at ${size.w}px (scale ${scale}) rendered ${seen.seen ?? 0} elements, so nothing was measured`);
          continue;
        }
        rows.push({ route, theme, w: size.w, page: seen.page, elements: seen.seen });
        const pass = `${size.w}|${theme}`;
        if (!screens.has(pass)) screens.set(pass, new Map());
        screens.get(pass).set(route, seen.heading ?? "(no heading)");
        if (seen.page > 1) {
          const widest = seen.past.map((p) => `${p.at} (+${p.by}px)`).join(", ");
          note(
            `${route} at ${size.w}px (scale ${scale}) scrolls sideways by ${seen.page}px` +
              (widest ? ` — widest past the edge: ${widest}` : ""),
          );
        }
        for (const c of seen.clipped) {
          note(`${route} at ${size.w}px (scale ${scale}): ${c.at} hides ${c.by}px of its own text with no ellipsis — "${c.text}"`);
        }
        for (const l of seen.long) {
          note(`${route} at ${size.w}px (scale ${scale}): ${l.at} sets a line ${l.chars} characters wide — "${l.text}"`);
        }

        /*
          The boundary, wherever a page is long enough to have one.

          It was measured at 1400×1000 first, where every screen in this
          product fits without scrolling, so the claim came back unmeasured
          on all seven routes; then at 900 alone, where exactly one screen
          scrolls and a single sample stood for every page in the product.
          Narrow windows are where content runs past the fold, so every width
          is tried and the run says how many pages answered.
        */
        if (theme === "dark") {
          const b = JSON.parse((await evaluateAsync(boundary)) ?? "{}");
          if (b.atTop && b.scrolled) {
            boundaryChecked++;
            if (!b.marked) {
              note(`${route}: scrolling the page body left no data-scrolled on it, so the chrome cannot know`);
            } else if (b.atTop.shadow === b.scrolled.shadow && b.atTop.border === b.scrolled.border) {
              note(
                `${route}: the topbar looks identical scrolled and at the top (${b.scrolled.border}, ` +
                  `${b.scrolled.shadow}) — the boundary is what says content is behind it`,
              );
            }
          }
        }
      }
    }
  }
  }
} finally {
  ws.close();
  chrome.kill();
}

const pad = (s, n) => String(s).padEnd(n);
const widths = [...new Set(rows.map((r) => r.w))];
console.log(`    ${pad("route", 10)}${widths.map((w) => pad(`${w}px`, 9)).join("")}`);
for (const route of ROUTES) {
  const cells = widths.map((w) => {
    const mine = rows.filter((r) => r.route === route && r.w === w);
    const worst = Math.max(0, ...mine.map((r) => r.page));
    return pad(mine.length === 0 ? "—" : `${worst}px`, 9);
  });
  console.log(`    ${pad(route, 10)}${cells.join("")}`);
}

for (const [pass, byRoute] of screens) {
  const drawn = new Set(byRoute.values());
  if (drawn.size >= 4) continue;
  const [w, theme] = pass.split("|");
  // The heading most routes drew, which is the one standing in for the rest.
  const tally = new Map();
  for (const h of byRoute.values()) tally.set(h, (tally.get(h) ?? 0) + 1);
  const common = [...tally].sort((x, y) => y[1] - x[1])[0]?.[0];
  note(
    `at ${w}px in ${theme} the ${byRoute.size} routes drew only ${drawn.size} different screen(s) — ` +
      `"${common}" — so the run measured one page over and over`,
  );
}

if (boundaryChecked < 2) {
  note(
    boundaryChecked === 0
      ? "no route had a page body long enough to scroll, so the boundary claim was not measured"
      : "one page in the whole run was long enough to scroll, which is one sample for a claim about every screen",
  );
}
findings.push(...found);

if (findings.length) {
  console.error("");
  for (const f of findings.slice(0, 24)) console.error(`  ✗ overflow: ${f}`);
  if (findings.length > 24) console.error(`  ✗ overflow: and ${findings.length - 24} more`);
  process.exit(1);
}
const drawn = new Set([...screens.values()].flatMap((byRoute) => [...byRoute.values()]));
console.log(
  `  ✓ overflow: ${rows.length} renders across ${ROUTES.length} routes, ${SIZES.length} widths, ` +
    `${THEMES.length} themes and ${SCALES.length} device pixel ratios (including ${SCALES.filter((s) => s % 1 !== 0).join(", ") || "none"} fractional) — ` +
    `${drawn.size} different screens, nothing off the side, no copy clipped without an ellipsis, no line past 108 ` +
    `characters, and the scroll boundary appears on all ${boundaryChecked} page(s) long enough to have one`,
);
