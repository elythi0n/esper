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
    firstRecord: null, errors: [], timestamps: [], unknown: 0,
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
      const stop = driveWith(canvas, {
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
      stop();
      await new Promise((r) => setTimeout(r, 120));
      seen.sent = 1;
    }
  } catch (e) {
    seen.threw = (e && e.name) + ": " + (e && e.message);
  }
  await report(seen);
})();
</script>`;

const reports = [];
/** Every batch of input the page posted, in order. */
const typed = [];
/*
  The live leg's socket, and whatever the engine has written to it.

  The window's own route does the same thing — accept one connection and copy
  it into an HTTP body — and it is deliberately the same shape here: what this
  measures is the hop the product has, not a hop this script invented.
*/
const liveSocket = `/tmp/esper-check-viewer-${process.pid}.sock`;
let engine = null;
let liveConn = null;

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
  if (req.method === "POST" && path === "/input") {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    typed.push(Buffer.concat(chunks));
    res.writeHead(204).end();
    return;
  }
  if (path === "/live.bin") {
    if (!liveConn) {
      res.writeHead(503).end("no engine is sending pictures");
      return;
    }
    res.writeHead(200, { "Content-Type": "application/octet-stream", "Cache-Control": "no-store" });
    // Flushed per chunk, like the window's route: a buffered copy would add
    // exactly the latency this leg exists to measure.
    liveConn.on("data", (chunk) => {
      res.write(chunk);
      res.flushHeaders?.();
    });
    liveConn.on("end", () => res.end());
    liveConn.on("error", () => res.end());
    return;
  }
  if (path === "/fixture.bin") {
    res.writeHead(200, { "Content-Type": "application/octet-stream", "Content-Length": fixture.length }).end(fixture);
    return;
  }
  if (path.startsWith("/src/") || path.startsWith("/@") || path.startsWith("/node_modules/")) {
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
const inChromium = (from) => () =>
  spawn(chromium, ["--headless=new", "--no-sandbox", "--disable-gpu", "--no-first-run", `--user-data-dir=${profile}`, url + from], { stdio: "ignore" });
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

server.close();

for (const r of [seen, webkit, live].filter(Boolean)) {
  const who = r === live ? "the live leg" : r.engine;
  if (r.threw) findings.push(`${who}: the reader threw — ${r.threw}`);
  for (const e of r.errors ?? []) findings.push(`${who}: the decoder reported ${e}`);
  if (r.firstRecord !== "configuration") {
    findings.push(`${who}: the first record was ${r.firstRecord}, so a decoder would be fed before it was configured`);
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
