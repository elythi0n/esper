#!/usr/bin/env node
// Every animation that ships, measured — driven, timed, and held to its token.
//
// # Why this exists
//
// "It feels smooth" is not a measurement, and a design system whose motion is
// reviewed by watching it is a system where a duration drifts and nobody can
// say when. This drives each of the five named motions the way a person would
// — it finds the control by its label and clicks it — and then reads two
// things back from the browser.
//
//   **What the browser was told.** `document.getAnimations()` returns the
//   animations actually running, with the duration the browser computed for
//   each. That is the token having survived the whole chain: CSS → utility →
//   component → animation. It is deterministic and it fails the build.
//
//   **What the browser managed.** rAF timestamps across the animation, giving
//   the frame count, the worst single gap, and how many gaps ran long. This
//   is reported and never fails: a headless browser on a shared machine is
//   not a display, and a gate that failed on its jitter is a gate people turn
//   off. The worst frame is kept separately because a mean of 4ms hides a
//   40ms one.
//
// # The vacuous-pass hole, and what closes it
//
// With motion reduced, the correct result is "no animation longer than a
// frame" — which is also what a broken trigger produces. So the reduced pass
// asserts nothing on its own: it only runs for motions the full pass already
// proved animate, and it fails if one of those is still animating. A trigger
// that stopped working fails the full pass first.
//
// Usage: node scripts/check-frames.mjs [--url URL] [--keep]
// With no --url it serves ./dist itself, so this needs `vite build` first.
import { spawn } from "node:child_process";
import { createServer, request as httpRequest } from "node:http";
import { existsSync, readFileSync } from "node:fs";
import { dirname, extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");
const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : fallback;
};

// ── What each motion is, and what it should last ─────────────────────────
// The seconds come from src/motion.ts, which is generated from the CSS and
// checked against it by gen-motion.mjs --check. Nothing here is a second
// copy of a duration.
const motion = JSON.parse(
  (() => {
    const ts = readFileSync(join(root, "src", "motion.ts"), "utf8");
    const m = ts.match(/export const duration = (\{[\s\S]*?\}) as const;/);
    if (!m) throw new Error("src/motion.ts has no duration table — run pnpm gen:motion");
    return m[1].replace(/(\w+):/g, '"$1":').replace(/,(\s*})/g, "$1");
  })(),
);

const MOTIONS = [
  {
    name: "arrive",
    trigger: "Add a row",
    expect: motion.base,
    about: "a row joining a list",
  },
  {
    name: "overlay",
    trigger: "Open a dialog",
    expect: motion.slow,
    about: "a dialog opening",
    after: "Escape",
  },
  {
    name: "drawer",
    trigger: "Open a drawer",
    expect: motion.slow,
    about: "a drawer entering from its edge",
    after: "Escape",
  },
  {
    name: "toast",
    trigger: "Show a toast",
    expect: motion.base,
    about: "a toast arriving in its band",
  },
  {
    name: "chrome hide",
    // The demo retracts on its own after --session-chrome-hide, so by the
    // time this runs the chrome may already be away and clicking Hide would
    // change nothing. Each direction is put into the state it animates out
    // of first, and that click is not the one being measured.
    before: "Show chrome",
    trigger: "Hide chrome",
    // Inside a session, and the session theme caps --dur-base at --dur-fast.
    // Measuring this is how we know that cap is real at runtime and not just
    // in a stylesheet.
    expect: motion.fast,
    about: "the session chrome retracting",
  },
  {
    name: "chrome show",
    before: "Hide chrome",
    trigger: "Show chrome",
    expect: motion.fast,
    about: "the session chrome returning",
  },
];

/** How far a measured duration may sit from its token before it is a finding. */
const TOLERANCE_MS = 12;
/** A frame this long or longer is a long frame. Two frames at 60Hz, plus slack. */
const LONG_FRAME_MS = 34;

// ── A server for ./dist, so this needs nothing else running ──────────────
const MIME = {
  ".html": "text/html", ".js": "text/javascript", ".css": "text/css",
  ".woff2": "font/woff2", ".woff": "font/woff", ".svg": "image/svg+xml",
  ".json": "application/json", ".png": "image/png",
};

/**
 * The API token, read the way the dev server reads it.
 *
 * The page never holds it: this server adds the header on the way through,
 * exactly as the desktop shell does. Absent is not an error — the screens
 * then say the control plane refused, which is a state worth being able to
 * measure too.
 */
function apiToken() {
  const home = process.env.ESPER_HOME || join(process.env.HOME ?? "", ".esper");
  try {
    return readFileSync(join(home, "api.token"), "utf8").trim() || null;
  } catch {
    return null;
  }
}

/**
 * Serve ./dist, and forward `/api/*` to the local control plane.
 *
 * # Why this proxies at all
 *
 * The gate renders every product screen **live**, so a static server makes
 * those screens show their unreachable state — and then this measures the
 * frame cost of a failure message rather than of the product. The paths pass
 * below drives the real Devices, Sessions and Settings screens, and they are
 * only real if something answers them.
 */
async function serveDist() {
  const dist = join(root, "dist");
  if (!existsSync(join(dist, "index.html"))) {
    console.error("  ✗ check-frames: no dist/index.html — run `pnpm build` first, or pass --url");
    process.exit(1);
  }
  const token = apiToken();
  const upstream = process.env.ESPER_API ?? "http://127.0.0.1:8687";
  const server = createServer((req, res) => {
    const url = req.url ?? "/";
    if (url.startsWith("/api/")) {
      const target = new URL(url.replace(/^\/api/, ""), upstream);
      const headers = { ...req.headers, host: target.host };
      if (token) headers.authorization = `Bearer ${token}`;
      const out = httpRequest(
        { hostname: target.hostname, port: target.port, path: target.pathname + target.search, method: req.method, headers },
        (answer) => {
          res.writeHead(answer.statusCode ?? 502, answer.headers);
          answer.pipe(res);
        },
      );
      out.on("error", () => {
        res.writeHead(502, { "content-type": "text/plain" });
        res.end("nothing is answering the control plane");
      });
      req.pipe(out);
      return;
    }
    // The window answers these; here there is no window, and the page has to
    // hear a definite no rather than be served an HTML page as JSON.
    if (url.startsWith("/shell/")) {
      res.writeHead(404, { "content-type": "text/plain" });
      res.end("no window");
      return;
    }
    const path = normalize(decodeURIComponent(url.split("?")[0])).replace(/^(\.\.[/\\])+/, "");
    let file = join(dist, path === "/" ? "index.html" : path);
    if (!existsSync(file)) file = join(dist, "index.html");
    res.writeHead(200, { "content-type": MIME[extname(file)] ?? "application/octet-stream" });
    res.end(readFileSync(file));
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  return { server, url: `http://127.0.0.1:${server.address().port}/` };
}

// ── The browser, over CDP ────────────────────────────────────────────────
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function browser(url) {
  const port = 9400 + (process.pid % 200);
  const chrome = spawn(
    process.env.CHROME ?? "/usr/bin/chromium",
    [
      "--headless=new", `--remote-debugging-port=${port}`, "--no-first-run", "--no-sandbox",
      "--disable-gpu", "--window-size=1400,900", "--hide-scrollbars",
      `--user-data-dir=/tmp/esper-frames-${process.pid}`, "about:blank",
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
    console.error("  ✗ check-frames: chromium did not start. Set CHROME to a browser binary.");
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
  // **A dead browser must fail, not hang.** Every request here is a promise
  // resolved by a message; when the socket goes — a crashed renderer, a
  // machine out of memory — nothing ever resolves them and the check sits
  // there for ever, which is indistinguishable from a slow pass and wasted
  // an afternoon once.
  ws.onclose = () => {
    for (const [, reject] of pending) reject({ gone: true });
    pending.clear();
  };
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const i = ++id;
      pending.set(i, (m) => (m?.gone ? reject(new Error("the browser went away")) : resolve(m)));
      ws.send(JSON.stringify({ id: i, method, params }));
    });
  const evaluate = async (expression) => {
    const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (r.result?.exceptionDetails) throw new Error(r.result.exceptionDetails.exception?.description ?? "page threw");
    return r.result?.result?.value;
  };
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Page.navigate", { url });
  await sleep(2500);
  return { evaluate, close: () => (ws.close(), chrome.kill()) };
}

// ── What runs inside the page ────────────────────────────────────────────
const HARNESS = `
window.__esper = {
  // Find a control the way a person does: by the words on it.
  click(text, scope) {
    const root = scope ? document.querySelector(scope) : document;
    if (!root) return false;
    const el = [...root.querySelectorAll('button,[role="radio"]')]
      .find((b) => (b.textContent || "").trim().toLowerCase().includes(text.toLowerCase()));
    if (!el) return false;
    el.click();
    return true;
  },
  // Every animation that has *just* started, with the duration the browser
  // computed for it. Just-started matters: the session demo retracts its own
  // chrome on a timer, and an animation already half over is not the one
  // this click caused.
  running() {
    return document.getAnimations()
      .map((a) => ({
        kind: a.constructor.name,
        name: a.animationName || a.transitionProperty || "",
        ms: Number(a.effect && a.effect.getComputedTiming().duration) || 0,
        at: Number(a.currentTime) || 0,
      }))
      .filter((a) => a.at <= 60);
  },
  // React commits, then the library starts the animation, so one frame after
  // the click is too early to look. Poll until something is running or the
  // window runs out — nothing after 150ms is a real finding, not a race.
  async started(maxMs) {
    const deadline = performance.now() + maxMs;
    for (;;) {
      const running = this.running();
      if (running.length || performance.now() > deadline) return running;
      await new Promise((r) => requestAnimationFrame(r));
    }
  },
  // Frame timestamps across a window, for the smoothness half.
  frames(ms) {
    return new Promise((resolve) => {
      const t = [];
      const start = performance.now();
      const tick = (now) => { t.push(now); (now - start < ms) ? requestAnimationFrame(tick) : resolve(t); };
      requestAnimationFrame(tick);
    });
  },
  async measure(text, windowMs) {
    if (!this.click(text)) return { missing: true };
    const running = await this.started(150);
    const frames = await this.frames(windowMs);
    return { running, frames };
  },
};
true;
`;

const gaps = (frames) => frames.slice(1).map((t, i) => t - frames[i]);

async function measure(page, m) {
  if (m.before) {
    await page.evaluate(`window.__esper.click(${JSON.stringify(m.before)})`);
    await sleep(260);
  }
  const windowMs = Math.max(240, m.expect * 1000 + 160);
  const r = await page.evaluate(`window.__esper.measure(${JSON.stringify(m.trigger)}, ${windowMs})`);
  if (r === undefined) {
    const state = await page.evaluate(
      `JSON.stringify({harness: typeof window.__esper, hash: location.hash, text: (document.body.innerText||"").slice(0,120)})`,
    );
    throw new Error(`the page did not answer while measuring ${m.name}: ${state}`);
  }
  if (m.after === "Escape") {
    await page.evaluate(`document.dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true})), true`);
    await sleep(220);
  }
  return r;
}

// ── The paths somebody actually walks ────────────────────────────────────
//
// The motions above are measured on the gate, where each has a control that
// replays it. That proves a duration survived the chain; it does not prove
// the *screens* are smooth. These are the four the exit criterion names —
// arriving at Devices, opening a session, the command palette, a dialog —
// driven on the real screens against the real control plane, and timed for
// frames rather than for duration.
//
// Frames only, deliberately: a navigation has no single animation to hold to
// a token, and asserting one would be inventing a number. What can fail is
// dropping frames, which is the thing a person notices.
const PATHS = [
  { name: "to Devices", go: "/devices", about: "the screen the window opens on" },
  { name: "to Sessions", go: "/sessions", about: "the dock, with its tab strip and watermark" },
  { name: "to Settings", go: "/settings", about: "two columns of live values" },
  { name: "open the palette", go: "/devices", press: { key: "k", ctrl: true }, about: "every command, filtered as you type" },
  { name: "open a dialog", go: "/devices", press: { key: "?" }, about: "the shortcut sheet over a screen" },
];

/** More than this many long frames on one path is a finding. */
const LONG_FRAMES_ALLOWED = 2;

const PATH_HARNESS = `
window.__paths = {
  async walk(hash, press, windowMs) {
    const before = performance.now();
    // Frame timing starts before the navigation, so the cost of unmounting
    // the old screen is counted too — that is part of what somebody waits
    // for, and measuring only the new screen's paint would flatter it.
    const frames = [];
    let done = false;
    const tick = (now) => { frames.push(now); if (!done) requestAnimationFrame(tick); };
    requestAnimationFrame(tick);

    if (hash && location.hash !== "#" + hash) location.hash = hash;
    if (press) {
      await new Promise((r) => setTimeout(r, 500));
      window.dispatchEvent(new KeyboardEvent("keydown", {
        key: press.key, ctrlKey: !!press.ctrl, bubbles: true, cancelable: true,
      }));
    }
    await new Promise((r) => setTimeout(r, windowMs));
    done = true;
    return {
      frames,
      dialogs: document.querySelectorAll('[role="dialog"]').length,
      // What is actually on screen, so a path that measured a failure
      // message cannot be reported as a path that measured the product.
      shell: !!document.querySelector(".shell"),
      firstRun: /isn't running on this machine|won't accept this window/.test(document.body.innerText || ""),
      rows: document.querySelectorAll(".device-row, [data-device]").length,
    };
  },
};
true;
`;

async function walkPaths(page) {
  await page.evaluate(PATH_HARNESS);
  const walked = [];
  for (const p of PATHS) {
    // Somewhere else first, so navigating to the path is a real transition
    // rather than a no-op that measures an idle window.
    await page.evaluate(`location.hash = "#/activity"`);
    await sleep(500);
    const r = await page.evaluate(
      `window.__paths.walk(${JSON.stringify(p.go)}, ${JSON.stringify(p.press ?? null)}, 900)`,
    );
    if (p.press) {
      await page.evaluate(`document.dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true})), true`);
      await sleep(200);
    }
    const g = gaps(r.frames ?? []);
    walked.push({
      ...p,
      frames: r.frames?.length ?? 0,
      worst: g.length ? Math.max(...g) : 0,
      long: g.filter((x) => x >= LONG_FRAME_MS).length,
      dialogs: r.dialogs,
      shell: r.shell,
      firstRun: r.firstRun,
      rows: r.rows,
    });
  }
  return walked;
}

// ── Run ───────────────────────────────────────────────────────────────────
const findings = [];
const rows = [];
let paths = [];
let served;
// The gate is a hash route, so the hash is part of the address.
const base = flag("--url", null) ?? (await (async () => ((served = await serveDist()), served.url))());
const url = base.includes("#") ? base : `${base.replace(/\/$/, "")}/#/design`;
const page = await browser(url);

try {
  await page.evaluate(HARNESS);

  // Motion at full, explicitly: the machine running this may have reduced
  // motion set, and then every measurement below would be of nothing.
  const setMotion = async (which) => {
    const ok = await page.evaluate(
      `window.__esper.click(${JSON.stringify(which)}, '[role="radiogroup"][aria-label="Motion"]')`,
    );
    await sleep(200);
    return ok;
  };
  if (!(await setMotion("Full"))) {
    findings.push("the motion override is not on the page — /design must offer Full / Reduced");
  }

  for (const m of MOTIONS) {
    const r = await measure(page, m);
    if (r?.missing) {
      findings.push(`${m.name}: no control labelled "${m.trigger}" — the gate no longer replays this motion`);
      rows.push({ ...m, missing: true });
      continue;
    }
    const running = r.running ?? [];
    const longest = running.reduce((a, b) => Math.max(a, b.ms), 0);
    if (running.length === 0) {
      findings.push(`${m.name}: clicking "${m.trigger}" started no animation at all`);
    }
    // *Every* animation the click started, not just the longest. One motion
    // is often several animations — a dialog is a backdrop and a panel — and
    // checking only the longest lets a component whose duration got shorter
    // hide behind one that did not. Found by sabotage: pointing the dialog
    // panel at --dur-fast while its backdrop kept --dur-slow passed a check
    // that only read the maximum.
    for (const a of running) {
      if (Math.abs(a.ms - m.expect * 1000) > TOLERANCE_MS) {
        findings.push(
          `${m.name}: ${a.name || a.kind} ran for ${a.ms.toFixed(0)}ms, and this motion's token says ` +
            `${(m.expect * 1000).toFixed(0)}ms`,
        );
      }
    }
    const g = gaps(r.frames ?? []);
    rows.push({
      ...m,
      declared: longest,
      animations: running.length,
      frames: r.frames?.length ?? 0,
      worst: g.length ? Math.max(...g) : 0,
      long: g.filter((x) => x >= LONG_FRAME_MS).length,
    });
  }

  // The paths, still at full motion. Done before the reduced pass so what is
  // measured is what somebody with motion on actually gets.
  paths = await walkPaths(page);
  await page.evaluate(`location.hash = "#/design"`);
  await sleep(600);

  // The reduced pass. Only motions the full pass proved animate are checked,
  // so a trigger that silently stopped working cannot pass this by producing
  // nothing.
  const animating = rows.filter((r) => !r.missing && r.animations > 0);
  if (!(await setMotion("Reduced"))) {
    findings.push("the motion override has no Reduced option");
  } else {
    for (const m of animating) {
      const r = await measure(page, m);
      const longest = (r?.running ?? []).reduce((a, b) => Math.max(a, b.ms), 0);
      if (longest > 1) {
        findings.push(`${m.name}: still runs for ${longest.toFixed(0)}ms with motion reduced; every duration should be 0`);
      }
      m.reduced = longest;
    }
    if (animating.length < 4) {
      findings.push(`only ${animating.length} motions animated at all, which is not the named set`);
    }
  }
} finally {
  page.close();
  served?.server.close();
}

// ── Report ────────────────────────────────────────────────────────────────
const pad = (s, n) => String(s).padEnd(n);
console.log(`    ${pad("motion", 14)}${pad("token", 8)}${pad("ran", 8)}${pad("reduced", 9)}${pad("frames", 8)}${pad("worst", 8)}long`);
for (const r of rows) {
  if (r.missing) {
    console.log(`    ${pad(r.name, 14)}— no control labelled "${r.trigger}"`);
    continue;
  }
  console.log(
    `    ${pad(r.name, 14)}${pad(`${(r.expect * 1000).toFixed(0)}ms`, 8)}${pad(`${r.declared.toFixed(0)}ms`, 8)}` +
      `${pad(r.reduced === undefined ? "—" : `${r.reduced.toFixed(0)}ms`, 9)}${pad(r.frames, 8)}${pad(`${r.worst.toFixed(1)}ms`, 8)}${r.long}`,
  );
}

if (paths.length) {
  console.log("");
  console.log(`    ${pad("path", 20)}${pad("frames", 8)}${pad("worst", 9)}${pad("long", 6)}what was on screen`);
  for (const p of paths) {
    let what = p.shell ? "the product" : "not the shell";
    if (p.firstRun) what = "the first-run screen — nothing answered";
    if (p.dialogs) what += `, ${p.dialogs} dialog${p.dialogs === 1 ? "" : "s"}`;
    console.log(
      `    ${pad(p.name, 20)}${pad(p.frames, 8)}${pad(`${p.worst.toFixed(1)}ms`, 9)}${pad(p.long, 6)}${what}`,
    );
    if (p.firstRun) {
      findings.push(
        `${p.name}: the control plane did not answer, so this measured a failure message rather than the screen. ` +
          `Start \`esper api\` (or set ESPER_API) and run it again.`,
      );
      continue;
    }
    if (!p.shell) {
      findings.push(`${p.name}: no shell on screen, so nothing was measured`);
      continue;
    }
    if (p.press && p.dialogs === 0) {
      findings.push(`${p.name}: the key was pressed and nothing opened`);
    }
    if (p.long > LONG_FRAMES_ALLOWED) {
      findings.push(`${p.name}: ${p.long} long frames (worst ${p.worst.toFixed(0)}ms) — this path drops frames`);
    }
  }
}

if (findings.length) {
  console.error("");
  for (const f of findings) console.error(`  ✗ frames: ${f}`);
  process.exit(1);
}
const worst = rows.reduce((a, r) => Math.max(a, r.worst ?? 0), 0);
const animationCount = rows.reduce((a, r) => a + (r.animations ?? 0), 0);
const worstPath = paths.reduce((a, p) => Math.max(a, p.worst ?? 0), 0);
console.log(
  `  ✓ frames: ${rows.length} motions driven, ${animationCount} animations timed — every one within ${TOLERANCE_MS}ms of its ` +
    `token and 0ms with motion reduced; worst single frame ${worst.toFixed(1)}ms`,
);
console.log(
  `  ✓ paths: ${paths.length} walked on the live product — worst single frame ${worstPath.toFixed(1)}ms, ` +
    `no path over ${LONG_FRAMES_ALLOWED} long frames`,
);
