#!/usr/bin/env node
// The viewer's frame path, from the engine's bytes to a decoded picture.
//
// # What this proves that nothing else can
//
// The stream a window's decoder reads is written in Rust and parsed in
// TypeScript — two implementations of one format, which is exactly the drift
// `testdata/fixtures/README.md` exists to catch. Rust checks its own half
// against the same committed fixture; this is the other half, and it does not
// stop at parsing: every picture goes into the browser's own `VideoDecoder`,
// because a reader that agrees with the writer about the bytes and disagrees
// about what they mean produces a session that shows nothing.
//
// # Why it runs in two browsers
//
// Because the shipped window is not Chromium. The spike behind this path found
// that **WebKit refuses Annex-B outright** and takes only length-prefixed AVCC
// with an `avcC` description, which is the whole reason the repackaging exists
// — so a gate that only ran in Chromium would pass on a stream the product's
// own window cannot play. WebKitGTK is checked when it is present, skipped
// loudly when it is not, and never silently.
//
// Usage: node scripts/check-viewer.mjs [--url DEV_SERVER]
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { createServer as createSocketServer } from "node:net";
import { readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");
const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : fallback;
};
const dev = flag("--url", "http://127.0.1.1:5178").replace(/\/$/, "");

/*
  The committed fixture by default, and any stream on request.

  `--stream` is not a convenience: it is how a stream this machine just
  produced gets the same reading. `esper-engine loopback --viewer <socket>`
  writes one with no peer and no display, and pointing this at it checks the
  live path rather than the fixture's memory of it.
*/
const FIXTURE = flag("--stream", join(root, "..", "testdata", "fixtures", "viewer_stream.bin"));
if (!existsSync(FIXTURE)) {
  console.error(`  ✗ viewer: ${FIXTURE} is missing. Run \`make viewer-fixture\`.`);
  process.exit(1);
}
const fixture = readFileSync(FIXTURE);

/*
  The page, served from this script's own origin.

  Same-origin matters: it imports the *real* reader out of the dev server, so
  what is checked is the module the product ships rather than a copy that
  agrees with itself. Anything under /src or /@ is proxied through to Vite for
  that reason alone.
*/
const PAGE = `<!doctype html><meta charset="utf-8"><title>viewer check</title>
<body style="background:#0b0b0d;color:#eee;font:12px monospace">
<canvas id="c" width="320" height="240"></canvas><pre id="out">reading…</pre>
<script type="module">
import { readViewerStream } from "/src/session/stream.ts";

const report = (r) => {
  document.getElementById("out").textContent = JSON.stringify(r, null, 1);
  return fetch("/report", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(r) });
};

(async () => {
  const seen = {
    // What the hand sent, when this run asked for one.
    sent: 0, sendFailed: null,
    engine: navigator.userAgent.includes("Chrome") ? "chromium" : "webkitgtk",
    configurations: [], pictures: 0, keyframes: 0, decoded: 0, sizes: [],
    firstRecord: null, errors: [], timestamps: [], unknown: 0, configuredFirst: true, session: null,
    pointer: null, shape: undefined,
    // The hop the stream's own clock makes measurable: engine to drawn.
    hops: [], clocked: 0,
  };
  try {
    const response = await fetch(new URLSearchParams(location.search).get("from") ?? "/fixture.bin");
    const ctx = document.getElementById("c").getContext("2d");
    const handed = new Map();
    let decoder = null;
    for await (const record of readViewerStream(response.body)) {
      seen.firstRecord ??= record.kind;
      if (record.kind === "unknown") { seen.unknown++; continue; }
      if (record.kind === "session") { seen.session = { screens: record.screens.length, showing: record.showing, names: record.screens.map((s) => s.name), lefts: record.screens.map((s) => s.x), asked: [record.askedFps, record.askedLongEdge] }; continue; }
      if (record.kind === "pointer") { seen.pointer = { inPicture: record.inPicture, draw: record.draw, x: record.x, y: record.y }; continue; }
      if (record.kind === "pointer-shape") {
        seen.shape = record.image
          ? { width: record.image.width, height: record.image.height, hotX: record.image.hotX, hotY: record.image.hotY, bytes: record.image.rgba.length, first: [...record.image.rgba.subarray(0, 8)] }
          : null;
        continue;
      }
      // What matters is the order of these two, not which record came first:
      // a stream may say what the session is of before it says how to decode
      // it, and neither is a decoder being fed early.
      if (record.kind === "picture" && seen.configurations.length === 0) seen.configuredFirst = false;
      if (record.kind === "configuration") {
        seen.configurations.push({ width: record.width, height: record.height, codec: record.codec, description: record.description.length });
        decoder?.close();
        decoder = new VideoDecoder({
          output: (frame) => {
            seen.decoded++;
            const size = frame.displayWidth + "x" + frame.displayHeight;
            if (!seen.sizes.includes(size)) seen.sizes.push(size);
            ctx.drawImage(frame, 0, 0);
            frame.close();
            // Measured after the draw, because a decoder that keeps up and a
            // canvas that does not are the same session to everybody except
            // the person watching. A fixture's pictures were handed over long
            // ago, so this number is only meaningful for a live stream — it is
            // reported either way and judged only where it means something.
            const at = handed.get(frame.timestamp);
            if (at !== undefined) seen.hops.push(Math.max(0, Date.now() - at));
          },
          error: (e) => seen.errors.push(e.name + ": " + e.message),
        });
        decoder.configure({ codec: record.codec, description: record.description, optimizeForLatency: true });
      } else {
        seen.pictures++;
        if (record.key) seen.keyframes++;
        if (record.handedAt !== null) { seen.clocked++; handed.set(record.timestamp, record.handedAt); }
        if (seen.timestamps.length < 4) seen.timestamps.push(record.timestamp);
        decoder?.decode(new EncodedVideoChunk({ type: record.key ? "key" : "delta", timestamp: record.timestamp, data: record.data }));
      }
    }
    await decoder?.flush();
    decoder?.close();

    // **And the other direction**, when the run asked for it: the page's own
    // hand, on the page's own canvas, through the page's own encoder. What
    // the engine makes of it is judged on the engine's side — here the claim
    // is only that the batch left.
    if (new URLSearchParams(location.search).get("drive") === "1") {
      const { driveWith } = await import("/src/session/hand.ts");
      const canvas = document.getElementById("c");
      const hand = driveWith(canvas, {
        url: "/input",
        picture: () => ({ width: 640, height: 480 }),
      });
      /*
        A surface deliberately the wrong shape for its picture.

        The canvas is made square while the picture is 4:3, so the drawn
        picture is 400 wide and 300 tall inside a 400 box with 50px of bar
        above and below. A pointer mapped against the *element* lands
        somewhere it is not — the whole point of the letterbox — and a check
        on a surface that happens to match the picture cannot tell the two
        mappings apart.
      */
      canvas.style.width = "400px";
      canvas.style.height = "400px";
      canvas.style.objectFit = "contain";
      const at = canvas.getBoundingClientRect();
      // 0.625 is 400/640, and the bar above the picture is 50px, so this is
      // (8, 16) in the picture and would be (8, 72) mapped against the box.
      const pointAt = { clientX: at.left + 5, clientY: at.top + 60 };
      canvas.dispatchEvent(new PointerEvent("pointerdown", { ...pointAt, button: 0, bubbles: true }));
      canvas.dispatchEvent(new PointerEvent("pointerup", { ...pointAt, button: 0, bubbles: true }));
      canvas.dispatchEvent(new KeyboardEvent("keydown", { code: "KeyA", bubbles: true }));
      canvas.dispatchEvent(new KeyboardEvent("keyup", { code: "KeyA", bubbles: true }));
      canvas.dispatchEvent(new WheelEvent("wheel", { deltaY: 120, bubbles: true }));

      /*
        And the way back out.

        Tab goes to the far machine while this has the keyboard, so the
        ordinary way out of a control is the one that cannot work — two
        Escapes inside half a second do it instead. One does not, because
        Escape is a key the far machine needs, and a check that only tried two
        would pass on an implementation that released on every one.
      */
      canvas.tabIndex = 0;
      canvas.focus();
      const escape = () => {
        canvas.dispatchEvent(new KeyboardEvent("keydown", { code: "Escape", bubbles: true }));
        canvas.dispatchEvent(new KeyboardEvent("keyup", { code: "Escape", bubbles: true }));
      };
      escape();
      seen.keptOnOne = document.activeElement === canvas;
      // Long enough that the next press is a first press and not a second.
      await new Promise((r) => setTimeout(r, 700));
      escape();
      escape();
      seen.gaveBack = document.activeElement !== canvas;

      // A frame for the batch, then let go — which is what a session ending
      // has to do and is the event nothing else would produce.
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
      hand.stop();
      await new Promise((r) => setTimeout(r, 120));
      seen.sent = 1;
    }
  } catch (e) {
    seen.threw = (e && e.name) + ": " + (e && e.message);
  }
  await report(seen);
})();
</script>`;

/*
  The surface page: the product's own session screen, in a box.

  Everything about it that matters is in `scripts/probes/session-surface.tsx`
  — this is the frame it mounts into. The module is served through the dev
  server like any other, so what mounts is `SessionView` itself rather than a
  copy of it that agrees with itself.
*/
const SURFACE = `<!doctype html><meta charset="utf-8"><title>surface check</title>
<body style="margin:0;background:#0b0b0d">
<div id="surface" style="position:relative;width:900px;height:560px"></div>
<!--
  The refresh preamble the React plugin's own HTML transform injects. This page
  is not Vite's index.html, so it has to carry it: without it every component
  module throws on import and the page reports nothing at all.
-->
<script type="module">
import RefreshRuntime from "/@react-refresh";
RefreshRuntime.injectIntoGlobalHook(window);
window.$RefreshReg$ = () => {};
window.$RefreshSig$ = () => (type) => type;
window.__vite_plugin_react_preamble_installed__ = true;
</script>
<script type="module" src="/scripts/probes/session-surface.tsx"></script>`;

const reports = [];
/** Every batch of input the page posted, in order. */
const typed = [];
/** And what the session screen asked the session for, in its own batches. */
const asked = [];
/*
  The live leg's socket, and whatever the engine has written to it.

  The window's own route does the same thing — accept one connection and copy
  it into an HTTP body — and it is deliberately the same shape here: what this
  measures is the hop the product has, not a hop this script invented.
*/
const liveSocket = `/tmp/esper-check-viewer-${process.pid}.sock`;
/*
  The surface leg gets a stream of its own.

  One connection can only be read once, and both legs read one to the end —
  so the surface leg runs its own engine on its own socket rather than sharing
  the hop leg's. It also needs a longer one: the chrome's own clock is three
  seconds, and a session that stopped sending before the chrome retracted
  would measure the retract on a still.
*/
const surfaceSocket = `/tmp/esper-check-surface-${process.pid}.sock`;
let engine = null;
let liveConn = null;
let surfaceConn = null;
/** The engine feeding the surface leg, so the leg can have it taken away. */
let surfaceEngine = null;
let startSurfaceEngine = () => null;

const server = createServer(async (req, res) => {
  // The path, without the query — the page carries which stream to read in
  // one, and matching on the raw URL is how a page silently 404s.
  const path = (req.url ?? "/").split("?")[0];
  if (req.method === "POST" && path === "/report") {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    try {
      reports.push(JSON.parse(Buffer.concat(chunks).toString("utf8")));
    } catch {
      reports.push({ broken: true });
    }
    res.writeHead(204).end();
    return;
  }
  if (path === "/" || path === "/index.html") {
    res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" }).end(PAGE);
    return;
  }
  /*
    The window's own input route, which is where the surface leg's bytes go.

    Recorded *and* written on to the engine, exactly as the window's route
    does: a gate that only recorded them would be checking that the page can
    produce bytes rather than that a session can be asked for something.
  */
  if (req.method === "POST" && path === "/shell/video/input") {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    const bytes = Buffer.concat(chunks);
    asked.push(bytes);
    surfaceConn?.write(bytes);
    res.writeHead(204).end();
    return;
  }
  if (req.method === "POST" && path === "/input") {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    typed.push(Buffer.concat(chunks));
    res.writeHead(204).end();
    return;
  }
  if (path === "/surface" || path === "/surface.html") {
    res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" }).end(SURFACE);
    return;
  }
  /*
    The window's own route names, answered the same way the window answers
    them — because the surface leg mounts the product's component, and the
    component fetches `/shell/video`. A gate that passed it a different URL
    would be checking a path the product does not take.
  */
  /*
    What the window says it is sending, which the session screen shows.

    Fabricated here on purpose: the transfer itself belongs to the engine and
    has its own session, its own permission and its own checks. What this leg
    is for is the other half — that a window saying it is sending something
    reaches the screen, and that a failure arrives as the host's sentence
    rather than as silence.
  */
  if (path === "/shell/transfers") {
    res.writeHead(200, { "Content-Type": "application/json" }).end(
      JSON.stringify({
        transfers: [
          { id: 1, name: "notes.txt", state: "sent" },
          { id: 2, name: "a photo.png", state: "sending" },
          { id: 3, name: "big.iso", state: "failed", trouble: "the host does not allow files in" },
        ],
      }),
    );
    return;
  }
  if (path === "/shell/video/socket") {
    res.writeHead(200, { "Content-Type": "application/json" }).end(JSON.stringify({ path: liveSocket }));
    return;
  }
  if (req.method === "POST" && path === "/shell/session") {
    res.writeHead(200, { "Content-Type": "application/json" }).end(JSON.stringify({ showing: "a gate" }));
    return;
  }
  if (req.method === "DELETE" && path === "/shell/session") {
    res.writeHead(204).end();
    return;
  }
  /*
    Take the pictures away, and give them back.

    The reconnect the viewer promises is not something a page can stage for
    itself: the engine has to actually go. So the page asks for it here, this
    kills the engine feeding it, and another starts a couple of seconds later
    — long enough that the notice has to count rather than flash.
  */
  if (path === "/break-the-link") {
    try {
      surfaceEngine?.kill();
    } catch {
      // Already gone, which is the thing being asked for.
    }
    surfaceEngine = null;
    res.writeHead(204).end();
    setTimeout(() => {
      surfaceEngine = startSurfaceEngine();
    }, 2500);
    return;
  }
  if (path === "/live.bin" || path === "/shell/video") {
    const conn = path === "/shell/video" ? surfaceConn : liveConn;
    if (!conn) {
      res.writeHead(503).end("no engine is sending pictures");
      return;
    }
    res.writeHead(200, { "Content-Type": "application/octet-stream", "Cache-Control": "no-store" });
    // Flushed per chunk, like the window's route: a buffered copy would add
    // exactly the latency this leg exists to measure.
    conn.on("data", (chunk) => {
      res.write(chunk);
      res.flushHeaders?.();
    });
    conn.on("end", () => res.end());
    conn.on("error", () => res.end());
    return;
  }
  if (path === "/fixture.bin") {
    res.writeHead(200, { "Content-Type": "application/octet-stream", "Content-Length": fixture.length }).end(fixture);
    return;
  }
  if (path.startsWith("/src/") || path.startsWith("/@") || path.startsWith("/node_modules/") || path.startsWith("/scripts/")) {
    try {
      const upstream = await fetch(dev + req.url);
      const body = Buffer.from(await upstream.arrayBuffer());
      res.writeHead(upstream.status, { "Content-Type": upstream.headers.get("content-type") ?? "application/javascript" }).end(body);
    } catch (e) {
      res.writeHead(502).end(String(e));
    }
    return;
  }
  res.writeHead(404).end();
});
const port = 8760 + (process.pid % 90);
await new Promise((r) => server.listen(port, "127.0.0.1", r));
const url = `http://127.0.0.1:${port}/`;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const waitForReport = async (n, ms) => {
  for (let waited = 0; waited < ms; waited += 200) {
    if (reports.length >= n) return true;
    await sleep(200);
  }
  return false;
};

const findings = [];
const ran = [];

const pad = (s, n) => String(s).padEnd(n);
const worst = (hops) => (hops && hops.length ? Math.round(Math.max(...hops)) : null);
const middle = (hops) => {
  if (!hops || hops.length === 0) return null;
  const sorted = [...hops].sort((a, b) => a - b);
  return Math.round(sorted[Math.floor(sorted.length / 2)]);
};
/*
  Only the live leg's hop is a latency.

  A stream read out of a file was handed over whenever the file was written,
  so its "hop" is the file's age — 184 seconds for the committed fixture, ten
  for one written a moment before. Printing that as a number somebody might
  read as a latency would be worse than printing nothing.
*/
const hopOf = (r) => {
  const p50 = middle(r.hops);
  if (p50 === null) return "—";
  return r === live ? `${p50}ms / ${worst(r.hops)}ms` : "— (a file's age)";
};


/** One browser, given the page, and what it said. */
async function drive(name, start, seconds) {
  const before = reports.length;
  const child = start();
  const arrived = await waitForReport(before + 1, seconds * 1000);
  try {
    child.kill();
  } catch {
    // It may already be gone, which is the ordinary case.
  }
  if (!arrived) {
    findings.push(`${name} never reported — the page did not run to the end`);
    return null;
  }
  ran.push(name);
  return reports[reports.length - 1];
}

const chromium = process.env.CHROME ?? "/usr/bin/chromium";
const profile = `/tmp/esper-viewer-${process.pid}`;
/*
  `ESPER_VIEWER_LOG=1` puts the page's own console on this terminal.

  A page that never reports is the one failure this script cannot explain by
  itself — the report is how it speaks — so the way to hear it is kept rather
  than rebuilt each time somebody needs it.
*/
const loud = process.env.ESPER_VIEWER_LOG === "1";
const inChromium = (from) => () =>
  spawn(
    chromium,
    [
      "--headless=new", "--no-sandbox", "--disable-gpu", "--no-first-run",
      ...(loud ? ["--enable-logging=stderr", "--v=0"] : []),
      `--user-data-dir=${profile}`, url + from,
    ],
    { stdio: loud ? ["ignore", "ignore", "inherit"] : "ignore" },
  );
const MINI = "/usr/lib/webkit2gtk-4.1/MiniBrowser";
const inWebKit = (from) => () => spawn(MINI, [url + from], { stdio: "ignore", env: { ...process.env } });
const haveWebKit = existsSync(MINI) && process.env.DISPLAY;

const seen = await drive("chromium", inChromium(""), 40);

// WebKitGTK: the engine the window actually ships, and the fussy one.
let webkit = null;
if (haveWebKit) {
  webkit = await drive("webkitgtk", inWebKit(""), 40);
} else {
  console.log(`    webkitgtk: skipped — ${existsSync(MINI) ? "no DISPLAY" : "MiniBrowser is not installed"}`);
}

/*
  The live leg: the real engine, writing to a real socket, read by the page as
  it is written.

  This is the only way the hop means anything. A stream read out of a file
  measures the age of the file — 184 seconds for the committed fixture, 10
  seconds for one written a moment earlier — which is a number that looks like
  a latency and is not one. Here the engine is running while the page is
  reading, so what is measured is the thing the product has: encode, socket,
  body, decode, draw.
*/
const ENGINE = join(root, "..", "build", "esper-engine");
let live = null;
if (existsSync(ENGINE)) {
  const sockets = createSocketServer((conn) => {
    liveConn = conn;
  });
  await new Promise((r) => sockets.listen(liveSocket, r));
  engine = spawn(
    ENGINE,
    ["loopback", "--backend", "synthetic", "--size", "640x480", "--fps", "30", "--seconds", "4", "--viewer", liveSocket],
    { stdio: ["ignore", "ignore", "pipe"] },
  );
  // What it said, so "never dialled" can name its own cause — an engine too
  // old to know `--viewer` is the likely one, and a silent gate would send
  // somebody looking at sockets.
  let complained = "";
  engine.stderr.on("data", (chunk) => {
    complained += String(chunk);
  });
  // The page asks for /live.bin only once the engine has dialled, so the
  // route does not answer 503 to a browser that started too quickly.
  for (let waited = 0; waited < 100 && !liveConn; waited++) await sleep(100);
  if (!liveConn) {
    const said = complained.trim().split("\n").slice(0, 2).join(" — ");
    findings.push(
      `the engine never dialled the live socket, so the hop was not measured` +
        (said ? `: ${said}` : " (it said nothing; is it older than --viewer?)"),
    );
  } else {
    live = await drive("chromium (live)", inChromium("?from=/live.bin&drive=1"), 45);
  }
  try {
    engine.kill();
  } catch {
    // It exits on its own when the seconds are up, which is the usual case.
  }
  sockets.close();
} else {
  console.log(`    live: skipped — ${ENGINE} is not built, so the hop was not measured`);
}

/*
  The surface leg: the product's session screen, with pictures arriving.

  The two things it exists to settle cannot be settled on a still. "Nothing
  animates over the video" is true of every session that is not running, and a
  stylesheet cannot answer it at all — a duration written as a Tailwind utility
  never reaches a `.css` file. So `SessionView` is mounted for real, fed by a
  real engine through the route the product fetches, and the document is read
  while the canvas is provably changing.

  The chrome's own clock is measured here for the same reason: the rule is that
  it retracts after `--session-chrome-hide`, comes back at the edge, and does
  **not** come back for a pointer moving over the picture — which is the
  gesture somebody makes constantly.
*/
let surface = null;
if (existsSync(ENGINE)) {
  const sockets = createSocketServer((conn) => {
    surfaceConn = conn;
    // So the route answers 503 between engines rather than handing the page a
    // socket nothing is writing to — which is what the page retries against.
    conn.on("close", () => {
      if (surfaceConn === conn) surfaceConn = null;
    });
  });
  await new Promise((r) => sockets.listen(surfaceSocket, r));
  // Long enough to outlast the chrome's three-second clock with the retract,
  // the edge and the pointer over the picture still on a live session — and
  // then the reconnect after it.
  startSurfaceEngine = () =>
    spawn(
      ENGINE,
      ["loopback", "--backend", "synthetic", "--size", "640x480", "--fps", "30", "--seconds", "25", "--viewer", surfaceSocket],
      { stdio: ["ignore", "ignore", "ignore"] },
    );
  surfaceEngine = startSurfaceEngine();
  for (let waited = 0; waited < 100 && !surfaceConn; waited++) await sleep(100);
  if (!surfaceConn) {
    findings.push("the engine never dialled the surface socket, so the session screen was measured on a still");
  } else {
    surface = await drive("chromium (surface)", inChromium("surface"), 60);
  }
  try {
    surfaceEngine?.kill();
  } catch {
    // It exits when its seconds are up, which is the usual case.
  }
  sockets.close();
}

server.close();

for (const r of [seen, webkit, live].filter(Boolean)) {
  const who = r === live ? "the live leg" : r.engine;
  if (r.threw) findings.push(`${who}: the reader threw — ${r.threw}`);
  for (const e of r.errors ?? []) findings.push(`${who}: the decoder reported ${e}`);
  if (r.configuredFirst === false) {
    findings.push(`${who}: a picture arrived before any configuration, so a decoder would be fed before it was set up`);
  }
  if ((r.configurations ?? []).length !== 1) {
    findings.push(`${who}: ${(r.configurations ?? []).length} configurations for one unchanging size`);
  }
  const config = (r.configurations ?? [])[0];
  if (config && !/^avc1\.4D/.test(config.codec)) {
    findings.push(`${who}: the codec string reads ${config.codec}, which is not the Main profile this encoder emits`);
  }
  if (config && config.description < 11) {
    findings.push(`${who}: an avcC description of ${config.description} bytes cannot hold two parameter sets`);
  }
  if ((r.pictures ?? 0) < 20) findings.push(`${who}: only ${r.pictures} pictures were read out of the stream`);
  if ((r.keyframes ?? 0) < 1) findings.push(`${who}: no keyframe, so a viewer could never begin`);
  if ((r.decoded ?? 0) < (r.pictures ?? 0) - 1) {
    findings.push(`${who}: ${r.decoded} of ${r.pictures} pictures decoded — the page and the engine disagree about the bytes`);
  }
  const declared = config ? `${config.width}x${config.height}` : null;
  if ((r.sizes ?? []).length !== 1 || (declared && r.sizes[0] !== declared)) {
    findings.push(
      `${who}: decoded ${JSON.stringify(r.sizes)}, and the stream says it is ${declared}`,
    );
  }
  if ((r.clocked ?? 0) < (r.pictures ?? 0)) {
    findings.push(`${who}: ${(r.pictures ?? 0) - (r.clocked ?? 0)} picture(s) arrived with no clock on them, so the hop cannot be measured`);
  }
  if (r === live) {
    const p50 = middle(r.hops);
    if (p50 === null) {
      findings.push("the live leg measured no hop at all, so the clock in the stream is not reaching the page");
    } else if (p50 > 2000) {
      // Not a performance bar — a headless browser under a virtual
      // framebuffer is not a display, and `check-frames` refuses to fail on
      // its jitter for the same reason. This catches the *class* of bug where
      // the number is the age of something rather than a latency.
      findings.push(`the live leg's hop is ${p50}ms at the median, which is not a latency — something is being measured that is not the hop`);
    }
  }
  /*
    The records that are not pictures, read by the page against bytes Rust
    wrote.

    This is the whole reason they went into the fixture: a record kind that is
    never in it is a kind whose two readers have never been compared, and the
    drift shows up as a session that shows the wrong thing rather than as an
    error anybody can read. Only the fixture legs are held to it — the live
    engine writes what it has, which is a different stream.
  */
  if (r !== live) {
    if (!r.session) {
      findings.push(`${who}: the fixture says what the session is of and the page read nothing`);
    } else {
      if (r.session.screens !== 2) findings.push(`${who}: read ${r.session.screens} screens out of a fixture with two`);
      if (r.session.asked?.[0] !== 15 || r.session.asked?.[1] !== 1280) {
        findings.push(
          `${who}: read a ladder asking for ${JSON.stringify(r.session.asked)}, and the fixture wrote 15 fps ` +
            `and a 1280px edge — the two fields are different widths, and reading a framerate out of half a ` +
            `resolution is the mistake this catches`,
        );
      }
      if (r.session.lefts?.[1] !== -640) {
        findings.push(
          `${who}: the screen left of the primary one is at x ${r.session.lefts?.[1]}, and the fixture put it at -640 — ` +
            `read as unsigned it lands four billion pixels to the right`,
        );
      }
    }
    if (!r.pointer) {
      findings.push(`${who}: the fixture says where the pointer is and the page read nothing`);
    } else if (r.pointer.x !== 100 || r.pointer.y !== 50 || !r.pointer.draw) {
      findings.push(`${who}: read the pointer at ${r.pointer.x},${r.pointer.y} draw=${r.pointer.draw}, and it was written at 100,50 draw=true`);
    }
    if (!r.shape) {
      findings.push(`${who}: the fixture carries a pointer shape and the page read ${r.shape === null ? "a hidden one" : "nothing"}`);
    } else {
      if (r.shape.width !== 16 || r.shape.height !== 16) findings.push(`${who}: the shape is ${r.shape.width}x${r.shape.height}, and it was written 16x16`);
      // A hotspot read as the corner still looks like a cursor and points a
      // few pixels wrong, which is why the fixture's is not the corner.
      if (r.shape.hotX !== 4 || r.shape.hotY !== 3) findings.push(`${who}: the hotspot read ${r.shape.hotX},${r.shape.hotY} and was written 4,3`);
      if (r.shape.bytes !== 16 * 16 * 4) findings.push(`${who}: ${r.shape.bytes} bytes for a 16x16 shape, and four a pixel is ${16 * 16 * 4}`);
      // Straight RGBA: the first pixel opaque, the second transparent.
      if (r.shape.first?.[3] !== 255 || r.shape.first?.[7] !== 0) {
        findings.push(`${who}: the first two pixels read ${JSON.stringify(r.shape.first)}, and the fixture alternates opaque and clear`);
      }
    }
  }

  const ts = r.timestamps ?? [];
  if (ts.length >= 2 && !(ts[1] > ts[0])) {
    findings.push(`${who}: timestamps do not advance (${ts.join(", ")}) — WebCodecs orders on them`);
  }
}

console.log(`    ${pad("source", 13)}${pad("read", 6)}${pad("decoded", 9)}${pad("size", 9)}${pad("codec", 14)}hop p50/worst`);
for (const r of [seen, webkit, live].filter(Boolean)) {
  console.log(
    `    ${pad(r === live ? "live engine" : r.engine, 13)}${pad(r.pictures ?? 0, 6)}${pad(r.decoded ?? 0, 9)}` +
      `${pad((r.sizes ?? ["—"])[0], 9)}${pad((r.configurations ?? [{}])[0]?.codec ?? "—", 14)}${hopOf(r)}`,
  );
}
/*
  The hop is *reported*, not failed on, and the reason is the rig.

  A fixture's pictures were handed over whenever the fixture was made, so the
  number here is the age of a file. Pointed at a live stream with `--stream`
  it is the real thing — and even then a headless browser under a virtual
  framebuffer is not a display, which is the same argument that keeps
  `check-frames` from failing on its own jitter. What *is* failed on is a
  picture arriving with no clock at all, because that is the format being
  wrong rather than the machine being slow.
*/

/*
  And the other direction, decoded the way the engine decodes it.

  The page's encoder is checked here rather than in the page: what matters is
  that the bytes a window writes are the bytes an engine reads, and the only
  way to know that is to read them the same way — magic, version, then a kind
  and a length per event, exactly as `esper-media`'s reader does.
*/
if (live) {
  const bytes = Buffer.concat(typed);
  if (bytes.length === 0) {
    findings.push("the page drove the surface and sent nothing, so the input path is not connected");
  } else {
    const magic = bytes.subarray(0, 8).toString("latin1");
    if (magic !== "ESPRINP\u0000") {
      findings.push(`the window's input begins ${JSON.stringify(magic)}, which is not an input stream`);
    } else if (bytes[8] !== 1) {
      findings.push(`the window writes input at version ${bytes[8]}, and the engine reads 1`);
    } else {
      const kinds = [];
      let at = 9;
      while (at + 5 <= bytes.length) {
        const kind = bytes[at];
        const len = bytes.readUInt32BE(at + 1);
        kinds.push({ kind, body: bytes.subarray(at + 5, at + 5 + len) });
        at += 5 + len;
      }
      if (at !== bytes.length) {
        const off = at - bytes.length;
        findings.push(
          `the window's input ${off > 0 ? `overran its own batch by ${off}` : `left ${-off}`} byte(s) — ` +
            `an event's length is wrong, which is the whole framing`,
        );
      }
      const seenKinds = new Set(kinds.map((k) => k.kind));
      for (const [kind, what] of [[1, "a key"], [2, "a pointer move"], [3, "a button"], [4, "a scroll"], [5, "the release"]]) {
        if (!seenKinds.has(kind)) findings.push(`the page drove the surface and ${what} never arrived`);
      }
      const key = kinds.find((k) => k.kind === 1);
      // KeyA is 0x04 by the HID table the W3C's code values are defined
      // against — the one fact this whole mapping rests on.
      if (key && key.body.readUInt16BE(0) !== 0x04) {
        findings.push(`the page sent KeyA as usage 0x${key.body.readUInt16BE(0).toString(16)}, and it is 0x04`);
      }
      /*
        Which way the wheel went.

        A session that scrolls backwards looks exactly like one that scrolls
        correctly until somebody tries it, and the sign then gets fixed twice
        in opposite directions by two people who each tried it once. The page
        rolled the wheel down (`deltaY: 120`) and the protocol says positive
        notches are down — the one place that inverts is `esper-input`'s
        `wheel` module, for Windows, and it says so.
      */
      /*
        Where the pointer said it was.

        The gate's surface is square and its picture is 4:3, so the picture is
        drawn 400x300 with a 50px bar above it. The click was 5px right and
        60px down from the surface's corner, which is (8, 16) in the picture —
        and (8, 72) for anything that maps against the element instead. A
        surface that happens to share its picture's shape cannot tell those
        apart, which is why this one deliberately does not.
      */
      const moved = kinds.find((k) => k.kind === 2);
      if (moved) {
        const x = moved.body.readInt32BE(0);
        const y = moved.body.readInt32BE(4);
        if (x !== 8 || y !== 16) {
          findings.push(
            `the page put a click 5px right and 60px down on a letterboxed surface at (${x}, ${y}) ` +
              `in the picture, and it is (8, 16) — a pointer mapped against the element, not the picture`,
          );
        }
      }
      const scroll = kinds.find((k) => k.kind === 4);
      if (scroll) {
        const dy = scroll.body.readInt16BE(2);
        const dx = scroll.body.readInt16BE(0);
        if (dy !== 1 || dx !== 0) {
          findings.push(
            `the page rolled the wheel down and sent dx ${dx}, dy ${dy} — down is dy +1, ` +
              `so this session scrolls backwards`,
          );
        }
      }
      const letGo = kinds.filter((k) => k.kind === 5).length;
      if (letGo < 1) findings.push("the page stopped driving and never let go of what it held");
      if (!live.keptOnOne) {
        findings.push(
          "one Escape took the keyboard off the surface — a key the far machine needs cannot be the way out",
        );
      }
      if (!live.gaveBack) {
        findings.push(
          "two Escapes left the keyboard on the surface, so a session entered with the keyboard cannot be left with it",
        );
      }
      console.log(
        `    input: ${kinds.length} event(s) in ${typed.length} batch(es), ` +
          `${bytes.length} bytes — key, pointer, button, scroll and a release, ` +
          `and the keyboard came back on two Escapes and stayed on one`,
      );
    }
  }
}

/*
  And what the session screen did while the pictures were arriving.

  Every claim below is conditional on `arriving` — the canvas showing something
  different from a moment earlier. Without that this is a still, and a still
  proves nothing about motion: it is the state where nothing animates however
  the session is written.
*/
if (surface) {
  if (surface.threw) findings.push(`the surface leg threw — ${surface.threw}`);
  if (!surface.mounted) findings.push("the session screen did not mount, so nothing about it was measured");
  else if (!surface.playing) {
    findings.push(
      "the session screen never got a picture, so every claim about a live session is unmeasured" +
        (surface.said ? ` — the screen said: ${surface.said}` : ""),
    );
  } else if (!surface.arriving) {
    findings.push("the canvas did not change while it was watched — this was a still, which is what the item says not to measure");
  } else {
    const ceiling = surface.ceilingMs ?? 0;
    if (!(ceiling > 0)) findings.push(`--dur-fast resolved to ${ceiling}ms inside the session, so there is no ceiling to hold anything to`);

    // A session declares itself, or it gets none of the session theme: not the
    // dark values, not the duration ceiling, not the chrome's own layers.
    if (!surface.session) {
      findings.push("the session screen carries no [data-session], so the session theme applies to none of it");
    }

    // The ceiling, on the tokens the session actually resolves.
    for (const [what, ms] of [["--dur-base", surface.baseMs], ["--dur-slow", surface.slowMs]]) {
      if (ms > ceiling) {
        findings.push(
          `inside the session ${what} resolves to ${ms}ms against a --dur-fast ceiling of ${ceiling}ms — ` +
            `the cap on [data-session] is not winning, so anything over the video using it runs long`,
        );
      }
    }
    if (surface.videoMs !== 0) {
      findings.push(`--dur-video resolves to ${surface.videoMs}ms inside a session, and the picture may not ease`);
    }

    // The surface itself: never a transition, never an animation.
    for (const el of surface.surface ?? []) {
      if (el.transitionMs !== 0) {
        findings.push(`${el.what} carries a ${el.transitionMs}ms transition — the frame that arrives is the frame that is shown`);
      }
      if (el.animation && el.animation !== "none") {
        findings.push(`${el.what} is running the ${el.animation} animation — the video surface never animates`);
      }
    }

    /*
      Everything over the picture, held to the ceiling.

      The list is also the floor: the HUD does transition, so an empty list
      means the walk found nothing and every assertion above it is vacuous.
    */
    const over = [...(surface.timed ?? []), ...(surface.timedOnResize ?? [])];
    if ((surface.timed ?? []).length === 0) {
      findings.push("nothing over the video was found to have any timing at all — the walk measured nothing");
    }
    for (const el of over) {
      if (el.transitionMs > ceiling) {
        findings.push(
          `over the video, ${el.what} transitions ${el.property} over ${el.transitionMs}ms — ` +
            `${ceiling}ms is the ceiling in a live session`,
        );
      }
      if (el.animationMs > ceiling) {
        findings.push(`over the video, ${el.what} runs ${el.animation} for ${el.animationMs}ms, over the ${ceiling}ms ceiling`);
      }
    }
    for (const a of [...(surface.running ?? []), ...(surface.runningOnResize ?? [])]) {
      if (a.overVideo && a.ms > ceiling) {
        findings.push(`while pictures were arriving, ${a.what} animated ${a.cause} for ${a.ms}ms over the video`);
      }
    }

    /*
      The HUD's numbers, which the design system singles out: a counting-up
      animation on a latency readout is a lie about latency.
    */
    const hud = surface.hud ?? {};
    if (!hud.found) {
      findings.push("the HUD showed no frame rate on a live session, so its readouts were not measured");
    } else {
      if (!hud.sameNode) {
        findings.push("the HUD's frame rate was a different element from one sample to the next — a readout that is replaced is not updating in place");
      }
      if (hud.transitionMs !== 0) findings.push(`the HUD's frame rate carries a ${hud.transitionMs}ms transition`);
      if (hud.animation) findings.push(`the HUD's frame rate is running the ${hud.animation} animation`);
      if (!/tabular-nums/.test(hud.numerals ?? "")) {
        findings.push(`the HUD's frame rate renders with font-variant-numeric "${hud.numerals}" — without tabular figures a number changing in place shifts everything beside it`);
      }
    }

    // A resize swaps frames rather than fading between them.
    if (!surface.resized) {
      findings.push("the picture did not change across a resize, so the surface was not live through it");
    }

    /*
      **The host's pointer, worn and drawn.**

      Both, because they are different claims: a surface that wears it draws
      it at the person's own hand with no network in the path, and one that
      composites it shows where the *host's* pointer is — which is what a
      session nobody is driving needs. The engine says which; the window is
      held to doing what it was told.
    */
    const pointer = surface.pointer ?? {};
    /*
      **A session with no peer says nothing about a path it does not have.**

      The loopback reports `real: false` — no round trip, no cost, no tier —
      and the honest rendering of that is nothing at all. A panel of zeroes, or
      a badge saying "Direct" about a session with no far end, is a readout
      that teaches people to stop reading the one fact on it that matters:
      whether the bytes are crossing a machine that is not an endpoint.
    */
    if (surface.stats) {
      findings.push("the session drew its readouts panel on a session with no path to report");
    }
    if (surface.tierBadge) {
      findings.push("the session claimed a path — the badge says how a far machine was reached, and there is no far machine");
    }

    // Nothing was given up on a loopback with no link, so the readout that
    // says what was given up is *not there*. An indicator that is always on
    // says nothing at all.
    if (surface.narrowed !== null) {
      findings.push(`the session says it gave up ${JSON.stringify(surface.narrowed)} on a link that asked for nothing`);
    }
    if (!pointer.worn) {
      findings.push("the surface is not wearing the remote pointer — its CSS cursor is not the shape the engine sent");
    } else if (!pointer.hotspot) {
      findings.push("the remote pointer is worn without a hotspot, so it points wherever its top-left corner is");
    }
    if (!pointer.drawn) {
      findings.push("the engine said to draw the pointer and nothing was drawn over the video");
    } else {
      if (!pointer.inPicture) findings.push("the drawn pointer is outside the picture it belongs to");
      if (!pointer.moves) findings.push("the drawn pointer never moved, so it is not following the position the engine sends");
    }

    /*
      **What the window may ask of the session.**

      The host's screens reach a window on the picture stream and nowhere
      else, so this is two claims at once: the engine said what it can see,
      and the window could act on it. Both were missing — the controller had
      the list from the first message of the session and never passed it on,
      and the two controls in the HUD were drawn `disabled` because there was
      nothing to put in them.
    */
    if (!surface.switcher?.found) {
      findings.push("the session screen has no way to switch monitor at all");
    } else if (!surface.switcher.offered) {
      findings.push("the monitor switcher is still disabled on a live session — the engine said nothing about the host's screens");
    } else if ((surface.screensOffered ?? []).length === 0) {
      findings.push("the monitor switcher opened on an empty list");
    }
    /*
      **A file may be dropped on the picture, and the window is told so in
      CSS.**

      That property is the whole of the page's part in a drop: the window
      reads it to decide whether a drop is for the product or for the webview,
      so a surface that does not declare it is a surface a file falls through.
    */
    if (surface.dropTarget !== "drop") {
      findings.push(
        `the picture declares --wails-drop-target as ${JSON.stringify(surface.dropTarget)}, and the window ` +
          `reads "drop" — a file dropped on it would be handled by the webview instead`,
      );
    }
    for (const wanted of ["Sent notes.txt", "Sending a photo.png", "the host does not allow files in"]) {
      if (!(surface.transfers ?? "").includes(wanted)) {
        findings.push(`the session screen does not say ${JSON.stringify(wanted)} while the window is sending it`);
      }
    }

    if (surface.acknowledged) {
      findings.push(
        "the window acknowledged an ask for the screen it is already showing — there is nothing to wait for, " +
          "and chrome that says otherwise is reporting progress towards where it already is",
      );
    }
    if (!surface.saysWhichIsShowing) {
      findings.push(
        "no screen in the switcher is marked as the one being shown — a list of screens with nothing " +
          "checked makes somebody guess which one they are looking at",
      );
    }
    if (!surface.quality?.offered) {
      findings.push("the quality control is disabled on a live session, so nothing can be asked for");
    }

    /*
      And the bytes, decoded the way the engine decodes them.

      The same reader as the input above, because it is the same stream: the
      window has one way to speak, and a screen or a quality travels on it
      beside the keystrokes.
    */
    const bytes = Buffer.concat(asked);
    if (bytes.length === 0) {
      findings.push("the window picked a monitor and a quality and sent nothing at all");
    } else {
      const kinds = [];
      let at = bytes.subarray(0, 8).toString("latin1") === "ESPRINP\u0000" ? 9 : 0;
      if (at === 0) findings.push(`what the window asked for begins ${JSON.stringify(bytes.subarray(0, 8).toString("latin1"))}, which is not an input stream`);
      while (at + 5 <= bytes.length) {
        const kind = bytes[at];
        const len = bytes.readUInt32BE(at + 1);
        kinds.push({ kind, body: bytes.subarray(at + 5, at + 5 + len) });
        at += 5 + len;
      }
      if (at !== bytes.length) {
        findings.push(`the window's asks left ${bytes.length - at} byte(s) unclaimed — a length is wrong`);
      }
      const screens = kinds.find((k) => k.kind === 6);
      if (!screens) {
        findings.push("a monitor was picked from the switcher and no screens event was sent");
      } else if (screens.body[0] !== 1) {
        findings.push(`the window asked for ${screens.body[0]} screens, and one was picked`);
      }
      const quality = kinds.find((k) => k.kind === 7);
      if (!quality) {
        findings.push("a quality was picked and no quality event was sent");
      } else if (quality.body[0] !== 2 || quality.body[1] !== 3) {
        // The last item in the menu is the motion profile at the soft rung.
        // A default would be 0,0, which is the whole reason the *last* one is
        // the one this picks.
        findings.push(
          `the window asked for profile ${quality.body[0]} at sharpness ${quality.body[1]}, ` +
            `and the motion profile is 2 at 3 — a control that sends a default whatever is picked ` +
            `looks exactly like one that works`,
        );
      }
      console.log(
        `    asked: ${(surface.screensOffered ?? []).length} screen(s) offered, ` +
          `${(surface.qualitiesOffered ?? []).length} qualities, and ${kinds.length} ask(s) left as bytes`,
      );
    }

    // ── The chrome's clock, Phase 10's rule driven ────────────────────────
    if (surface.hiddenWhileUsed) findings.push("the chrome was already gone while the session was being used");
    if (!surface.retracted) {
      findings.push(`the chrome never retracted — ${surface.hideMs}ms is what --session-chrome-hide says it waits`);
    } else if (Math.abs(surface.retractedAfterMs - surface.hideMs) > 900) {
      findings.push(
        `the chrome retracted after ${surface.retractedAfterMs}ms and --session-chrome-hide is ${surface.hideMs}ms`,
      );
    }
    if (!surface.stayedGoneOverVideo) {
      findings.push(
        "a pointer moving over the picture brought the chrome back — that is the gesture somebody makes " +
          "constantly, so chrome that answers it is never gone",
      );
    }
    if (!surface.cameBackAtEdge) {
      findings.push("the pointer reached the chrome's edge and it did not come back, so it cannot be got at");
    }
    if (!surface.stillArriving) {
      findings.push("pictures stopped before the chrome was measured, so the chrome was measured on a still");
    }
    /*
      And the reconnect, with the engine really taken away.

      "Never a dead window" is the claim, and a dead window is exactly what
      this used to be: the stream ended, the surface said the session was over
      and nothing asked again. What is held here is the whole of the other
      behaviour — the last picture kept rather than blanked, a notice that
      *counts* rather than sits, and the picture back with nobody touching
      anything.
    */
    const said = surface.noticed ?? [];
    if (said.length === 0) {
      findings.push("the engine was killed and the window said nothing — a session that stops with no notice is a dead window");
    } else if (said.length < 2) {
      findings.push(
        `the window said ${JSON.stringify(said[0])} and it never changed — a notice that does not count ` +
          `cannot be told from a window that has stopped`,
      );
    }
    if (!surface.flatIsFlat) {
      findings.push("the page's own test for a cleared canvas does not recognise one, so \"the picture was kept\" means nothing");
    }
    if (!surface.keptThePicture) {
      findings.push("the canvas was blanked when the pictures stopped — the last frame is the most recent true thing about that machine");
    }
    if (surface.resizedOnReturn > 0) {
      findings.push(
        `the canvas was resized ${surface.resizedOnReturn} time(s) coming back from a reconnect at the same ` +
          `size — setting width or height clears it, so every reconnect is a black flash`,
      );
    }
    if (surface.wentBlank) {
      findings.push(
        "the canvas went to one flat colour while the picture was coming back — a black flash on every reconnect, " +
          "which is what resizing a canvas to the size it already is does",
      );
    }
    if (surface.cameBackMs === null) {
      findings.push("a new engine started and the picture never came back, so the reconnect is not automatic");
    }
    console.log(
      `    path: the engine said ${JSON.stringify(surface.pathSaid)}, ` +
        `readouts ${surface.stats ? "drawn" : "not drawn"}, ` +
        `tier badge ${surface.tierBadge ? "drawn" : "not drawn"}`,
    );
    console.log(
      `    pointer: worn as a ${surface.shapeSize ?? "16x16"} shape with its hotspot, ` +
        `and drawn over the picture, following the engine's position`,
    );
    console.log(
      `    reconnect: ${said.map((t) => JSON.stringify(t)).join(" then ")}, last picture kept, ` +
        `back in ${surface.cameBackMs}ms with nobody touching anything and no blank frame`,
    );
    console.log(
      `    surface: ${surface.size} live, --dur-fast ${surface.ceilingMs}ms ceiling ` +
        `(base ${surface.baseMs}ms, slow ${surface.slowMs}ms, video ${surface.videoMs}ms), ` +
        `${(surface.timed ?? []).length} timed element(s) over the picture, ` +
        `fps read ${(surface.hud?.said ?? []).join("/") || "nothing"} in one node with no transition, ` +
        `chrome retracted at ${surface.retractedAfterMs}ms and came back at the edge`,
    );
  }
}

if (ran.length === 0) findings.push("no browser ran the page, so nothing was checked");

if (findings.length) {
  console.error("");
  for (const f of findings) console.error(`  ✗ viewer: ${f}`);
  process.exit(1);
}
const liveSaid = live ? `, and live from the engine at ${middle(live.hops)}ms from handed to drawn` : "";
console.log(
  `  ✓ viewer: ${FIXTURE.endsWith("viewer_stream.bin") ? "the committed stream" : FIXTURE} parsed by the ` +
    `page's own reader and decoded picture for picture in ${ran.join(", ")} — ${seen.pictures} pictures, ` +
    `${seen.keyframes} keyframe(s), ${seen.sizes[0]}${liveSaid}`,
);
