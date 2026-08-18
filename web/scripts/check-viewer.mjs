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
    engine: navigator.userAgent.includes("Chrome") ? "chromium" : "webkitgtk",
    configurations: [], pictures: 0, keyframes: 0, decoded: 0, sizes: [],
    firstRecord: null, errors: [], timestamps: [],
  };
  try {
    const response = await fetch("/fixture.bin");
    const ctx = document.getElementById("c").getContext("2d");
    let decoder = null;
    for await (const record of readViewerStream(response.body)) {
      seen.firstRecord ??= record.kind;
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
          },
          error: (e) => seen.errors.push(e.name + ": " + e.message),
        });
        decoder.configure({ codec: record.codec, description: record.description, optimizeForLatency: true });
      } else {
        seen.pictures++;
        if (record.key) seen.keyframes++;
        if (seen.timestamps.length < 4) seen.timestamps.push(record.timestamp);
        decoder?.decode(new EncodedVideoChunk({ type: record.key ? "key" : "delta", timestamp: record.timestamp, data: record.data }));
      }
    }
    await decoder?.flush();
    decoder?.close();
  } catch (e) {
    seen.threw = (e && e.name) + ": " + (e && e.message);
  }
  await report(seen);
})();
</script>`;

const reports = [];
const server = createServer(async (req, res) => {
  if (req.method === "POST" && req.url === "/report") {
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
  if (req.url === "/" || req.url === "/index.html") {
    res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" }).end(PAGE);
    return;
  }
  if (req.url === "/fixture.bin") {
    res.writeHead(200, { "Content-Type": "application/octet-stream", "Content-Length": fixture.length }).end(fixture);
    return;
  }
  if (req.url.startsWith("/src/") || req.url.startsWith("/@") || req.url.startsWith("/node_modules/")) {
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
const seen = await drive("chromium", () =>
  spawn(chromium, ["--headless=new", "--no-sandbox", "--disable-gpu", "--no-first-run", `--user-data-dir=${profile}`, url], { stdio: "ignore" }), 40);

// WebKitGTK: the engine the window actually ships, and the fussy one.
const MINI = "/usr/lib/webkit2gtk-4.1/MiniBrowser";
let webkit = null;
if (existsSync(MINI) && process.env.DISPLAY) {
  webkit = await drive("webkitgtk", () =>
    spawn(MINI, [url], { stdio: "ignore", env: { ...process.env } }), 40);
} else {
  console.log(`    webkitgtk: skipped — ${existsSync(MINI) ? "no DISPLAY" : "MiniBrowser is not installed"}`);
}

server.close();

for (const r of [seen, webkit].filter(Boolean)) {
  const who = r.engine;
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
  const ts = r.timestamps ?? [];
  if (ts.length >= 2 && !(ts[1] > ts[0])) {
    findings.push(`${who}: timestamps do not advance (${ts.join(", ")}) — WebCodecs orders on them`);
  }
}

const pad = (s, n) => String(s).padEnd(n);
console.log(`    ${pad("engine", 12)}${pad("read", 6)}${pad("decoded", 9)}${pad("size", 9)}codec`);
for (const r of [seen, webkit].filter(Boolean)) {
  console.log(
    `    ${pad(r.engine, 12)}${pad(r.pictures ?? 0, 6)}${pad(r.decoded ?? 0, 9)}` +
      `${pad((r.sizes ?? ["—"])[0], 9)}${(r.configurations ?? [{}])[0]?.codec ?? "—"}`,
  );
}

if (ran.length === 0) findings.push("no browser ran the page, so nothing was checked");

if (findings.length) {
  console.error("");
  for (const f of findings) console.error(`  ✗ viewer: ${f}`);
  process.exit(1);
}
console.log(
  `  ✓ viewer: ${FIXTURE.endsWith("viewer_stream.bin") ? "the committed stream" : FIXTURE} parsed by the ` +
    `page's own reader and decoded picture for picture in ` +
    `${ran.join(" and ")} — ${seen.pictures} pictures, ${seen.keyframes} keyframe(s), ${seen.sizes[0]}`,
);
