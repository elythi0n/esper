#!/usr/bin/env node
// The contrast of the text that is actually on screen, in the browser.
//
// # Why, when `check-contrast` already exists
//
// Because they answer different questions and only one of them is about the
// product. `check-contrast.mjs` reads the token files and holds 159 *pairings*
// to AA — it proves the palette is sound, and it proves nothing about a
// component using the wrong pair of it. A muted foreground on a muted fill
// is two perfectly good tokens making unreadable text, and the token checker
// cannot see it: neither token is wrong, the combination was never a pairing
// anybody listed.
//
// So this walks the rendered page, takes the colour of every piece of text
// and the colour of the ground it is actually painted on — the nearest
// ancestor that paints, composited, converted by the browser — and computes
// the ratio the specification defines.
//
// # What fails and what is only reported
//
// Required: ordinary text at 4.5:1, large text at 3:1, and in high contrast
// 7:1 and 4.5:1, because that theme exists for people who need more than AA.
//
// Watched, reported, never failed — each for a reason, not for convenience:
//
//   - **Disabled text.** It is meant to read as unavailable, the guidelines
//     exempt it, and raising it to 4.5 would make disabled and enabled
//     controls look the same.
//   - **Anything with opacity in the stack.** The effective ink depends on
//     what is behind a translucent ancestor at that pixel; this measures
//     colours rather than pixels, so it would be guessing.
//   - **Text over video.** The ground is somebody else's screen and no
//     palette can promise a ratio against it — which is why the presence
//     indicator has its own contract and its own gate.
//   - **Text over a gradient.** A gradient has no single colour, and picking
//     one of its stops would be inventing the number.
//
// And one bar that follows the palette rather than the setting: **inside a
// session the theme is the dark one**, whatever the app is set to, so a
// session is held to the dark theme's bar. The consequence is written down in
// the design-system doc rather than hidden here — under high contrast, the
// session's own chrome sits above AA and below AAA.
//
// Usage: node scripts/check-ink.mjs [--url URL]
import { spawn } from "node:child_process";

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : fallback;
};
const base = flag("--url", "http://127.0.1.1:5178/").replace(/\/$/, "");

const ROUTES = ["design", "devices", "sessions", "pair", "invite", "activity", "settings"];
const THEMES = ["dark", "light", "hc"];
/** AA, and AAA where the theme's whole purpose is more than AA. */
const NEED = { dark: [4.5, 3], light: [4.5, 3], hc: [7, 4.5] };

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const port = 9680 + (process.pid % 90);
const chrome = spawn(
  process.env.CHROME ?? "/usr/bin/chromium",
  [
    "--headless=new", `--remote-debugging-port=${port}`, "--no-first-run", "--no-sandbox",
    "--disable-gpu", "--window-size=1400,1000", "--hide-scrollbars",
    `--user-data-dir=/tmp/esper-ink-${process.pid}`, "about:blank",
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
  console.error("  ✗ ink: chromium did not start. Set CHROME to a browser binary.");
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

/*
  One page, measured.

  The colour arithmetic is the specification's, not an approximation: sRGB
  channels linearised, weighted 0.2126/0.7152/0.0722, and (L1+0.05)/(L2+0.05).
  A luma shortcut is fine for telling dark from light — `check-live` uses one —
  and wrong for a threshold somebody's eyesight depends on.

  Every colour goes through a 1×1 canvas, because this product writes OKLCH
  and a computed style comes back in the notation it was written in. Parsing
  that by hand is how a check reports 0.025 for bone.
*/
const measure = `(() => {
  const paint = document.createElement("canvas").getContext("2d");
  const rgba = (colour) => {
    if (!colour) return null;
    paint.clearRect(0, 0, 1, 1);
    paint.fillStyle = "#000";
    paint.fillStyle = colour;
    if (paint.fillStyle === "#000000" && !/^#000|black|rgba?\\(0, 0, 0/.test(colour)) return null;
    paint.fillRect(0, 0, 1, 1);
    const [r, g, b, a] = paint.getImageData(0, 0, 1, 1).data;
    return { r, g, b, a: a / 255 };
  };
  const over = (top, bottom) => {
    if (top.a >= 0.999) return top;
    const k = top.a;
    return { r: top.r * k + bottom.r * (1 - k), g: top.g * k + bottom.g * (1 - k), b: top.b * k + bottom.b * (1 - k), a: 1 };
  };
  const lum = (c) => {
    const f = (v) => { const s = v / 255; return s <= 0.04045 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4); };
    return 0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b);
  };
  const ratio = (a, b) => {
    const [hi, lo] = lum(a) >= lum(b) ? [lum(a), lum(b)] : [lum(b), lum(a)];
    return (hi + 0.05) / (lo + 0.05);
  };

  /*
    The ground under an element: the first ancestor that paints, composited
    down — and *which* element that was, because a finding without it is a
    ratio nobody can act on.

    An ancestor with a background **image** stops the walk and marks the
    reading: a gradient has no single colour, and picking one of its stops
    would be inventing the number. The gate's own display heading is painted
    that way on purpose.
  */
  const groundOf = (el) => {
    const stack = [];
    for (let at = el; at; at = at.parentElement) {
      const style = getComputedStyle(at);
      if (style.backgroundImage && style.backgroundImage !== "none") return { gradient: true, at: describe(at) };
      const c = rgba(style.backgroundColor);
      if (!c || c.a === 0) continue;
      stack.push({ c, at: describe(at) });
      if (c.a >= 0.999) break;
    }
    if (stack.length === 0) return null;
    let ground = stack[stack.length - 1].c;
    for (let i = stack.length - 2; i >= 0; i--) ground = over(stack[i].c, ground);
    return { ...ground, at: stack[stack.length - 1].at };
  };
  const show = (c) => "rgb(" + Math.round(c.r) + " " + Math.round(c.g) + " " + Math.round(c.b) + ")";

  const describe = (el) => {
    const cls = (el.className || "").toString().trim().split(/\\s+/).slice(0, 3).join(".");
    return el.tagName.toLowerCase() + (cls ? "." + cls : "");
  };

  const required = [];
  const watched = [];
  let measured = 0;
  for (const el of document.querySelectorAll("body *")) {
    // Its own text, not its subtree's: a container's ratio is its children's.
    const own = [...el.childNodes].filter((n) => n.nodeType === 3).map((n) => n.textContent.trim()).join(" ").trim();
    if (own.length < 2) continue;
    const style = getComputedStyle(el);
    if (style.display === "none" || style.visibility === "hidden") continue;
    const box = el.getBoundingClientRect();
    if (box.width < 2 || box.height < 2) continue;

    // Painted by its own background rather than by its colour — a clipped
    // gradient. Its colour property is transparent and means nothing here.
    if (style.webkitBackgroundClip === "text" || style.backgroundClip === "text") continue;
    const ink = rgba(style.color);
    if (!ink || ink.a === 0) continue;
    const ground = groundOf(el);
    if (!ground) continue;
    measured++;
    if (ground.gradient) {
      watched.push({ at: describe(el), text: own.slice(0, 40), ratio: 0, large: false, why: "over a gradient", fg: show(ink), bg: ground.at });
      continue;
    }
    const seen = ratio(over(ink, ground), ground);

    const size = Number.parseFloat(style.fontSize);
    const weight = Number.parseInt(style.fontWeight, 10) || 400;
    const large = size >= 24 || (size >= 18.66 && weight >= 700);

    const disabledInk = getComputedStyle(document.documentElement).getPropertyValue("--color-text-disabled").trim();
    let why = null;
    if (el.closest("[disabled], [aria-disabled='true'], [data-disabled]")) why = "disabled";
    // The disabled *token*, wherever it is used. The token checker watches
    // this pairing rather than requiring it — WCAG exempts disabled text —
    // and two gates disagreeing about one pairing is one of them being wrong.
    else if (disabledInk && rgba(disabledInk) && show(rgba(disabledInk)) === show(ink)) why = "disabled ink";
    else if (el.closest(".video-surface, [data-video]")) why = "over video";
    else {
      for (let at = el; at; at = at.parentElement) {
        if (Number.parseFloat(getComputedStyle(at).opacity) < 0.999) { why = "translucent"; break; }
      }
    }

    const row = {
      at: describe(el), text: own.slice(0, 40), ratio: Math.round(seen * 100) / 100, large, why,
      fg: show(over(ink, ground)), bg: show(ground), on: ground.at,
      // The session layer is a theme of its own — always the dark values,
      // whatever the app is set to — so it is held to that theme's bar.
      session: !!el.closest("[data-session]"),
    };
    (why ? watched : required).push(row);
  }
  // Which theme was actually in force when this ran.
  //
  // Not paranoia: the app owns this attribute too. A re-render inside the
  // window between "set the theme" and "measure it" can put the stored
  // choice back, and the first version of this check read a *mixture* — the
  // base theme's ink against the light theme's ground — which looks exactly
  // like a real contrast defect and is not one. So the reading carries the
  // theme it was taken under and the run fails if that is not what it asked
  // for, rather than reporting the mixture as a finding.
  return JSON.stringify({ measured, required, watched, theme: document.documentElement.dataset.theme || "dark" });
})()`;

const findings = [];
const watchedWorst = [];
let measured = 0;
let checked = 0;

try {
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: "reduce" }] });

  for (const route of ROUTES) {
    for (const theme of THEMES) {
      await send("Page.navigate", { url: `${base}/#/${route}` });
      await sleep(route === "design" ? 3200 : 1200);
      /*
        Kill every transition and animation before measuring.

        Not tidiness — the first run reported two contrast failures that were
        **the previous theme's ink caught mid-transition**: a control whose
        colour was still travelling from the dark theme's tertiary text to the
        light theme's read as the dark value against the light ground, which
        is exactly what a real defect looks like and was not one. A gate that
        measures colour has to measure a settled one, and emulating reduced
        motion is not enough: it zeroes the *token* durations, and a
        transition written in any other unit is untouched by it.
      */
      await evaluate(`(() => {
        let stop = document.getElementById("ink-stop");
        if (!stop) {
          stop = document.createElement("style");
          stop.id = "ink-stop";
          stop.textContent = "*, *::before, *::after { transition: none !important; animation: none !important; }";
          document.head.append(stop);
        }
        return 1;
      })()`);
      await evaluate(`(() => { document.documentElement.dataset.theme = ${JSON.stringify(theme)}; return 1; })()`);
      await sleep(300);
      // Set again in the same turn as the measurement, so nothing can render
      // between the two.
      const seen = JSON.parse(
        (await evaluate(`(() => { document.documentElement.dataset.theme = ${JSON.stringify(theme)}; return (${measure}); })()`)) ?? "{}",
      );
      if (seen.theme && seen.theme !== theme) {
        findings.push(`${route}: asked for the ${theme} theme and measured under ${seen.theme}, so the reading is a mixture`);
        continue;
      }
      if (!seen.measured || seen.measured < 5) {
        findings.push(`${route} in ${theme} measured ${seen.measured ?? 0} pieces of text, so nothing was checked`);
        continue;
      }
      measured += seen.measured;
      checked++;
      const [ordinary, big] = NEED[theme];
      const [darkOrdinary, darkBig] = NEED.dark;
      const failing = new Map();
      for (const row of seen.required) {
        // Inside a session the palette is the dark one — the whole point of
        // the session layer — so the bar is the dark theme's.
        const need = row.session ? (row.large ? darkBig : darkOrdinary) : row.large ? big : ordinary;
        if (row.ratio >= need) continue;
        const key = `${row.at}|${row.ratio}`;
        if (!failing.has(key)) failing.set(key, { ...row, need });
      }
      for (const f of failing.values()) {
        findings.push(
          `${route} in ${theme}: ${f.at} reads ${f.ratio.toFixed(2)}:1, under ${f.need}:1 — ${f.fg} on ` +
            `${f.bg} (from ${f.on})${f.session ? ", inside a session" : ""} — "${f.text}"`,
        );
      }
      const worst = seen.watched.reduce((a, b) => (a && a.ratio <= b.ratio ? a : b), null);
      if (worst) watchedWorst.push({ route, theme, ...worst });
    }
  }
} finally {
  ws.close();
  chrome.kill();
}

const pad = (s, n) => String(s).padEnd(n);
if (watchedWorst.length) {
  console.log(`    watched (never failed) — the worst on each screen it appears`);
  const seenAt = new Set();
  for (const w of watchedWorst) {
    const key = `${w.at}|${w.why}`;
    if (seenAt.has(key)) continue;
    seenAt.add(key);
    console.log(`    ${pad(w.why, 14)}${pad(`${w.ratio.toFixed(2)}:1`, 9)}${pad(w.theme, 6)}${w.at}`);
  }
}

if (checked === 0) {
  findings.push("no screen rendered, so no contrast was measured at all");
}

if (findings.length) {
  console.error("");
  for (const f of findings.slice(0, 20)) console.error(`  ✗ ink: ${f}`);
  if (findings.length > 20) console.error(`  ✗ ink: and ${findings.length - 20} more`);
  process.exit(1);
}
console.log(
  `  ✓ ink: ${measured} pieces of rendered text across ${checked} screen-and-theme pairs, every one at or above ` +
    `the bar its theme sets — measured on the ground it is actually painted on, not on the pairing somebody listed`,
);
