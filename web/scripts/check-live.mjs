#!/usr/bin/env node
// Claims about the *rendered* document, driven in a browser.
//
// # Why these cannot be checked from the source
//
// `check-contrast` and `check-discipline` read the token files, which is the
// right instrument for "what do the tokens say". Four of the design system's
// claims are not about what the tokens say — they are about what the browser
// *resolves* after cascade, layers, media queries and a nested theme have all
// had their turn:
//
//   - **The session layer is always dark**, whatever the app's theme. It works
//     by re-declaring the value layer on `[data-session]`, and whether that
//     actually wins is a cascade question with three layers and a theme
//     attribute in it.
//   - **The focus ring is 2px, and 3px in high contrast.** One rule, never
//     removed — and "never removed" is a claim about every component, which
//     no grep can make.
//   - **A select inside a dialog is not clipped.** The reason `--z-popover`
//     sits above `--z-modal`, and the failure is a list rendered behind the
//     thing it was opened from.
//   - **Three fonts, and nothing uses a fourth.** A missing face falls back
//     silently to something that looks nearly right.
//
// Each one has been true by construction and by argument. This is the first
// thing that reads it back.
//
// Usage: node scripts/check-live.mjs [--url URL]
import { spawn } from "node:child_process";

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : fallback;
};
const base = flag("--url", "http://127.0.1.1:5178/");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const port = 9800 + (process.pid % 80);
const chrome = spawn(
  process.env.CHROME ?? "/usr/bin/chromium",
  [
    "--headless=new", `--remote-debugging-port=${port}`, "--no-first-run", "--no-sandbox",
    "--disable-gpu", "--window-size=1400,1000", "--hide-scrollbars",
    `--user-data-dir=/tmp/esper-live-${process.pid}`, "about:blank",
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
  console.error("  ✗ live: chromium did not start. Set CHROME to a browser binary.");
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
// `awaitPromise` so an expression may be `async`. Without it a promise comes
// back as an object with no value, which reads as an empty page rather than as
// "this check does not know how to wait".
const evaluate = async (expression) =>
  (await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true })).result?.result
    ?.value;

const findings = [];
const rows = [];

try {
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Page.navigate", { url: base.includes("#") ? base : `${base.replace(/\/$/, "")}/#/design` });
  await sleep(5000);

  // ── 1. The session layer is dark under every theme ──────────────────────
  //
  // Read as a *colour*, not as a token name: what the claim is about is what
  // somebody sees, and a token that resolved to the wrong value would still
  // have the right name.
  // **The nearest ancestor that actually paints, converted by the browser.**
  //
  // Two bugs live here, and both produced plausible numbers rather than
  // errors — which is why the floor below exists.
  //
  // The first read `backgroundColor` off the element it was given, and the
  // page body is transparent: every reading came out luma 0.000, including
  // the light theme, and the check passed by measuring nothing.
  //
  // The second is the interesting one. This product writes colour in
  // **OKLCH**, and a computed style comes back as `oklch(0.93 0.012 87)` —
  // so pulling the numbers out with a regex and treating them as r, g, b
  // gave 0.025 for bone. Perfectly plausible, entirely wrong, and no amount
  // of care with the regex fixes it: the conversion is the browser's job.
  // A canvas does it exactly, for any notation the browser accepts.
  const lightness = `(el) => {
    const paint = document.createElement("canvas").getContext("2d");
    for (let at = el; at; at = at.parentElement) {
      const colour = getComputedStyle(at).backgroundColor;
      if (!colour || colour === "transparent") continue;
      paint.clearRect(0, 0, 1, 1);
      paint.fillStyle = "#000";
      paint.fillStyle = colour;
      paint.fillRect(0, 0, 1, 1);
      const [r, g, b, a] = paint.getImageData(0, 0, 1, 1).data;
      if (a < 128) continue;
      // Rec.709 luma, which is close enough to say dark from light.
      return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
    }
    return null;
  }`;

  /*
    The same conversion, for a token value rather than an element.

    **The token, not the element's own fill.** The session root paints its
    ground with a component class, so reading that element's background says
    dark whatever the tokens do — which is how the first version of this check
    passed a sabotage that disabled the session's token block entirely. What
    the claim is about is the values *inside* the session, so read the value
    layer on the session element and on the document, and compare.

    `--sem-*` rather than `--color-*`: with `@theme inline` a utility is
    `var(--sem-surface-1)`, resolved on the element it is used on. A
    `--color-*` custom property is resolved once on `:root` and inherited
    already-substituted, so it reads the same everywhere and would be a second
    way to measure nothing.
  */
  const tokenIn = `(el, name) => getComputedStyle(el).getPropertyValue(name).trim()`;
  const tone = `(colour) => {
    if (!colour) return null;
    const paint = document.createElement("canvas").getContext("2d");
    paint.fillStyle = "#000";
    paint.fillStyle = colour;
    paint.fillRect(0, 0, 1, 1);
    const [r, g, b] = paint.getImageData(0, 0, 1, 1).data;
    return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
  }`;

  for (const theme of ["dark", "light", "hc"]) {
    await evaluate(`(() => { document.documentElement.dataset.theme = ${JSON.stringify(theme)}; return 1; })()`);
    await sleep(400);
    const read = await evaluate(`(() => {
      const luma = ${lightness};
      const token = ${tokenIn};
      const tone = ${tone};
      const session = document.querySelector("[data-session]");
      // The document, not a page body. On the gate every page body is a
      // specimen inside a gallery — one of them is inside a session layer,
      // which is always dark by design — so reading the first one measured a
      // demo rather than the app surface. The html element is where the base
      // layer paints the surface colour, which is what the theme is about.
      // (No backticks in here: this string is a template literal.)
      const page = document.documentElement;
      return JSON.stringify({
        session: session ? luma(session) : null,
        page: luma(page),
        ring: getComputedStyle(document.documentElement).getPropertyValue("--focus-width").trim(),
        // The value layer, as the session resolves it and as the app does.
        surfaceIn: session ? token(session, "--sem-surface-1") : null,
        surfaceOut: token(document.documentElement, "--sem-surface-1"),
        textIn: session ? token(session, "--sem-text") : null,
        textOut: token(document.documentElement, "--sem-text"),
        toneIn: session ? tone(token(session, "--sem-surface-1")) : null,
      });
    })()`);
    const seen = JSON.parse(read ?? "{}");
    rows.push({ theme, ...seen });

    // **A floor under the measurement itself.** The light theme is light; if
    // the page reads dark under it, this is measuring the wrong element and
    // everything below is meaningless.
    if (seen.page === null) {
      findings.push(`nothing on the page paints a background, so none of this is measuring anything`);
    } else if (theme === "light" && seen.page < 0.5) {
      findings.push(
        `under the light theme the page reads luma ${seen.page.toFixed(3)}, which is dark — ` +
          `this is measuring the wrong element, so the session reading below means nothing`,
      );
    } else if (theme !== "light" && seen.page > 0.25) {
      findings.push(`under the ${theme} theme the page reads luma ${seen.page.toFixed(3)}, which is not dark`);
    }

    if (seen.session === null || !seen.surfaceIn) {
      findings.push(`no [data-session] on the page, so "the session is always dark" is unmeasured`);
    } else {
      // Under a light or high-contrast app, a session that re-declares the
      // value layer resolves a *different* surface and a different ink. Equal
      // values mean the declaration is not winning, and everything inside a
      // session follows the app's theme.
      if (theme !== "dark" && seen.surfaceIn === seen.surfaceOut) {
        findings.push(
          `under the ${theme} theme a session resolves --sem-surface-1 to ${seen.surfaceIn}, the same value ` +
            `the app around it resolves — so the session is not re-declaring the dark values`,
        );
      }
      if (theme !== "dark" && seen.textIn === seen.textOut) {
        findings.push(
          `under the ${theme} theme a session resolves --sem-text to ${seen.textIn}, the same value the app ` +
            `around it resolves — text inside a session would be the app's ink on the session's ground`,
        );
      }
      // And the value it lands on is dark, not merely different.
      if (seen.toneIn === null || seen.toneIn > 0.25) {
        findings.push(
          `under the ${theme} theme a session's --sem-surface-1 is ${seen.surfaceIn} at luma ` +
            `${seen.toneIn === null ? "nothing" : seen.toneIn.toFixed(3)}, which is not dark`,
        );
      }
      if (seen.session > 0.25) {
        findings.push(
          `under the ${theme} theme the session layer paints at luma ${seen.session.toFixed(3)}, which is not dark`,
        );
      }
    }
    // The ring is 2px, and 3px in high contrast. Read from the resolved
    // token, which is what every focus outline in the product uses.
    const want = theme === "hc" ? "3px" : "2px";
    if (seen.ring !== want) {
      findings.push(`under the ${theme} theme the focus ring resolves to ${seen.ring || "nothing"}, not ${want}`);
    }
  }
  await evaluate(`(() => { document.documentElement.dataset.theme = "dark"; return 1; })()`);
  await sleep(300);

  // ── 2. A select inside a dialog is above it ─────────────────────────────
  const layered = await evaluate(`(() => {
    const zOf = (name) => {
      const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
      return Number.parseInt(v, 10);
    };
    const overlay = zOf("--z-overlay");
    const modal = zOf("--z-modal");
    const popover = zOf("--z-popover");
    return JSON.stringify({ overlay, modal, popover });
  })()`);
  const z = JSON.parse(layered ?? "{}");
  if (!(z.overlay < z.modal && z.modal < z.popover)) {
    findings.push(
      `the layering is overlay ${z.overlay}, modal ${z.modal}, popover ${z.popover} — a popover has to be ` +
        `above a modal, or a select inside a dialog is drawn behind the dialog it was opened from`,
    );
  }

  // And driven, because the tokens being in order says nothing about a
  // component using the right one.
  const drawn = await evaluate(`(() => {
    const open = [...document.querySelectorAll("button")]
      .find((b) => /open a dialog/i.test((b.textContent || "").trim()));
    if (!open) return JSON.stringify({ why: "no dialog to open" });
    open.click();
    return JSON.stringify({ ok: true });
  })()`);
  if (JSON.parse(drawn ?? "{}").why) {
    findings.push(JSON.parse(drawn).why);
  } else {
    await sleep(600);
    const inside = await evaluate(`(() => {
      const dialog = document.querySelector('[role="dialog"]');
      if (!dialog) return JSON.stringify({ why: "the dialog did not open" });
      const trigger = dialog.querySelector('[role="combobox"], button[aria-haspopup="listbox"]');
      if (!trigger) return JSON.stringify({ noSelect: true });
      trigger.click();
      return JSON.stringify({ ok: true });
    })()`);
    const state = JSON.parse(inside ?? "{}");
    if (state.ok) {
      await sleep(600);
      const above = await evaluate(`(() => {
        const list = document.querySelector('[role="listbox"]');
        const dialog = document.querySelector('[role="dialog"]');
        if (!list || !dialog) return JSON.stringify({ why: "the list did not open" });
        const zi = (el) => Number.parseInt(getComputedStyle(el).zIndex, 10) || 0;
        const box = list.getBoundingClientRect();
        return JSON.stringify({
          list: zi(list.closest("[style*='z-index'], [class*='z-']") || list),
          dialog: zi(dialog),
          visible: box.width > 0 && box.height > 0,
        });
      })()`);
      const seen = JSON.parse(above ?? "{}");
      if (seen.why) findings.push(seen.why);
      else if (!seen.visible) findings.push("a select inside a dialog opened a list with no box — it is clipped");
    }
    await send("Input.dispatchKeyEvent", { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
    await send("Input.dispatchKeyEvent", { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
    await sleep(300);
    await send("Input.dispatchKeyEvent", { type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
    await send("Input.dispatchKeyEvent", { type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
    await sleep(300);
  }

  /*
    **A bounded permission does not read as an unbounded one.**

    `unattended` is the permission that removes the question — a device holding
    it walks in with nobody at the host being asked — and it can be bounded to
    hours. The screen showed one lamp for both, which overstates what most such
    devices may do, and overstating a permission is the one direction a screen
    about trust must never be wrong in.

    Driven on the real Devices screen against the rig's own store, where one
    device has held `weekdays 09:00-17:00` all along with nothing showing it.
  */
  const bounded = await evaluate(`(async () => {
    if (!location.hash.startsWith("#/devices")) {
      location.hash = "#/devices";
      await new Promise((r) => setTimeout(r, 600));
    }
    // The row for the device the fixture bounds, opened the way a person opens
    // it: the bound is in the panel beside the list, which is where what a
    // device may do is said.
    // The row's name is inside a button, which is what carries the selection —
    // see device-row.tsx. Matching on the button rather than on anything
    // that contains the word means this clicks the thing a person clicks.
    // Waited for rather than assumed: the list arrives from the control plane,
    // and a probe that looked while one row was on the page would report a
    // missing device as a missing feature.
    const rowFor = (name) =>
      [...document.querySelectorAll("button")].find((b) => (b.textContent || "").trim().startsWith(name));
    let row = null;
    for (let waited = 0; waited < 8000; waited += 200) {
      row = rowFor("rack");
      if (row) break;
      await new Promise((r) => setTimeout(r, 200));
    }
    if (!row) {
      return JSON.stringify({
        why: "no row for the device the fixture bounds",
        saw: [...document.querySelectorAll("button")].slice(0, 12).map((b) => (b.textContent || "").trim().slice(0, 40)),
      });
    }
    row.click();
    await new Promise((r) => setTimeout(r, 700));
    const bound = document.querySelector("[data-bounded]");
    const unattended = [...document.querySelectorAll("*")].find(
      (el) => el.children.length === 0 && /Connect unasked/.test(el.textContent || ""),
    );
    return JSON.stringify({
      said: bound ? (bound.textContent || "").trim() : null,
      hasUnattended: Boolean(unattended),
    });
  })()`);
  {
    const seen = JSON.parse(bounded ?? "{}");
    if (seen.why) {
      findings.push(`${seen.why}${seen.saw ? ` — the buttons on the page were ${JSON.stringify(seen.saw)}` : ""}`);
    } else if (!seen.hasUnattended) {
      findings.push("the device panel never showed the unattended permission, so its bound is unmeasured");
    } else if (!seen.said) {
      findings.push(
        "a device whose unattended access is bounded to weekday hours reads the same as one that may " +
          "walk in at three in the morning — the screen overstates what it may do",
      );
    } else if (!/09:00/.test(seen.said) || !/Weekdays/i.test(seen.said)) {
      findings.push(`the bound reads ${JSON.stringify(seen.said)}, and the store says weekdays 09:00-17:00`);
    }
  }

  // ── 3. The wordmark's letters are in five places ───────────────────────
  //
  // A CSS transform on an SVG element **replaces** its `transform`
  // presentation attribute — `transform: none` included, which is what an
  // animation library leaves behind. The mark's letters carried their x
  // offset as that attribute *and* were the elements whose transform was
  // animated, so every letter sat at x = 0: a blob at rest in Chromium, and
  // in WebKit a blob for the length of every animation. It had been that way
  // since the mark was drawn, and no gate here could see it — a frame timer
  // measures durations, an accessibility tree measures names, and neither
  // asks where anything is.
  //
  // The rule is general because the trap is: nothing that is animated may
  // also carry a transform attribute.
  const mark = await evaluate(`(() => {
    const marks = [...document.querySelectorAll('svg[aria-label="Esper"]')]
      .filter((s) => s.getBoundingClientRect().width > 20);
    const spread = marks.map((svg) => {
      const lefts = [...svg.querySelectorAll("path")].map((p) => Math.round(p.getBoundingClientRect().left));
      return { letters: lefts.length, places: new Set(lefts).size };
    });
    // Any element in the document that is transformed both ways at once.
    const both = [];
    for (const el of document.querySelectorAll("[transform]")) {
      const inline = el.style.transform;
      if (inline && inline !== "") {
        both.push(el.tagName.toLowerCase() + " transform=" + el.getAttribute("transform") + " style=" + inline);
      }
    }
    return JSON.stringify({ marks: spread, both: both.slice(0, 4) });
  })()`);
  const lettering = JSON.parse(mark ?? "{}");
  if (!lettering.marks || lettering.marks.length === 0) {
    findings.push("no wordmark on the page, so nothing checked that its letters are in different places");
  }
  for (const m of lettering.marks ?? []) {
    if (m.places < m.letters) {
      findings.push(
        `the wordmark draws ${m.letters} letters in ${m.places} place(s) — they are stacked, which is what ` +
          `an animated element carrying its own transform attribute does`,
      );
    }
  }
  for (const clash of lettering.both ?? []) {
    findings.push(`transformed two ways at once, and the CSS one wins: ${clash}`);
  }

  // ── 4. Three fonts, all loaded, and nothing uses a fourth ──────────────
  const fonts = await evaluate(`(() => {
    const wanted = ["Inter Variable", "Space Grotesk Variable", "JetBrains Mono Variable"];
    const loaded = wanted.filter((f) => document.fonts.check('12px "' + f + '"'));
    // What is actually asked for, across everything on screen.
    const families = new Set();
    for (const el of document.querySelectorAll("*")) {
      const first = getComputedStyle(el).fontFamily.split(",")[0].trim().replace(/^["']|["']$/g, "");
      if (first) families.add(first);
    }
    return JSON.stringify({ loaded, families: [...families].sort() });
  })()`);
  const face = JSON.parse(fonts ?? '{"loaded":[],"families":[]}');
  if (face.loaded.length !== 3) {
    findings.push(
      `only ${face.loaded.length} of the three faces are loaded (${face.loaded.join(", ") || "none"}) — ` +
        `a missing face falls back to something that looks nearly right`,
    );
  }
  const allowed = new Set([
    "Inter Variable", "Space Grotesk Variable", "JetBrains Mono Variable",
    // The generic that every stack ends with, and the one the reset sets.
    "ui-sans-serif", "ui-monospace", "system-ui", "inherit",
  ]);
  const strangers = face.families.filter((f) => !allowed.has(f));
  if (strangers.length > 0) {
    findings.push(`something asks for a face outside the three: ${strangers.join(", ")}`);
  }
} finally {
  ws.close();
  chrome.kill();
}

const pad = (s, n) => String(s).padEnd(n);
console.log(`    ${pad("theme", 8)}${pad("page", 7)}${pad("ring", 6)}sem-surface-1: the app → a session`);
for (const r of rows) {
  console.log(
    `    ${pad(r.theme, 8)}${pad((r.page ?? 0).toFixed(3), 7)}${pad(r.ring, 6)}` +
      `${r.surfaceOut || "—"} → ${r.surfaceIn || "—"}`,
  );
}

if (findings.length) {
  console.error("");
  for (const f of findings) console.error(`  ✗ live: ${f}`);
  process.exit(1);
}
console.log(
  `  ✓ live: the session layer stays dark under all three themes, the ring is 2px and 3px in high ` +
    `contrast, a popover outranks a modal, the wordmark's letters are in five places, and all three faces ` +
    `are loaded with nothing asking for a fourth`,
);
