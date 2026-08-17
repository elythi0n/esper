#!/usr/bin/env node
// The keyboard: where focus goes, and whether it comes back.
//
// # Why this is worth a check of its own
//
// Radix does it, and that is exactly why it needs checking: a promise kept by
// a library is a promise nobody in this repository is thinking about, and the
// way it breaks is not the library — it is **a trigger that unmounts.** Open a
// menu, choose the thing that replaces the row the menu hung from, and focus
// has nowhere to return to: it lands on `<body>`, the next Tab starts from the
// top of the document, and somebody navigating by keyboard is thrown back to
// the beginning of the window with no idea why.
//
// That failure is invisible to every other gate here. It leaves no wrong
// pixel, breaks no contrast rule, drops no frame, and reads perfectly in the
// source.
//
// # What it measures
//
// For each dialog on the gate: what was focused before, that focus moved
// *inside* the dialog when it opened, that Tab stays inside it, and that on
// close focus is back where it started. Four facts, and the last is the one
// that regresses.
//
// # And the order it walks in
//
// The second pass tabs through the real shell and reads the order out. A
// window whose tab order is not its reading order is a window somebody using
// a keyboard has to explore rather than read — and the way that happens is
// never deliberate: a portal that renders at the end of the document, an
// absolutely-positioned control that keeps its DOM position, a header action
// mounted after the body. None of it is visible on screen.
//
// What is asserted is weak on purpose: the *bands* of the shell must come in
// the order they are drawn — chrome, then the rail, then the page. Within a
// band the order is the DOM's and this has no opinion. A stricter check would
// be a check that fails every time somebody adds a button.
//
// Usage: node scripts/check-keyboard.mjs [--url URL]
import { spawn } from "node:child_process";

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : fallback;
};
const base = flag("--url", "http://127.0.1.1:5178/");

/** The controls on the gate that open something with a focus trap. */
const OPENERS = ["Open a dialog", "Open a drawer", "Show the sheet", "Open the palette"];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const port = 9700 + (process.pid % 90);
const chrome = spawn(
  process.env.CHROME ?? "/usr/bin/chromium",
  [
    "--headless=new", `--remote-debugging-port=${port}`, "--no-first-run", "--no-sandbox",
    "--disable-gpu", "--window-size=1400,1000", "--hide-scrollbars",
    `--user-data-dir=/tmp/esper-focus-${process.pid}`, "about:blank",
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
  console.error("  ✗ keyboard: chromium did not start. Set CHROME to a browser binary.");
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
  (await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true })).result?.result?.value;

const key = async (opts) => {
  await send("Input.dispatchKeyEvent", { type: "keyDown", ...opts });
  await send("Input.dispatchKeyEvent", { type: "keyUp", ...opts });
  await sleep(160);
};

const findings = [];
const rows = [];
let order = [];

try {
  await send("Page.enable");
  await send("Runtime.enable");
  // **Tell the page it is in a frameless window**, exactly as the shell does
  // by rewriting the document it serves. Without this there is no title bar
  // and the first band of the shell is simply absent — which the check would
  // report as unreachable rather than as not drawn.
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

  // A description of where focus is, stable enough to compare before and after.
  const focused = `(() => {
    const el = document.activeElement;
    if (!el || el === document.body) return "body";
    const label = (el.getAttribute("aria-label") || el.textContent || "").trim().slice(0, 40);
    return el.tagName.toLowerCase() + (label ? ":" + label : "");
  })()`;

  for (const opener of OPENERS) {
    const found = await evaluate(`(() => {
      const el = [...document.querySelectorAll("button")]
        .find((b) => (b.textContent || "").trim().toLowerCase().includes(${JSON.stringify(opener.toLowerCase())}));
      if (!el) return null;
      el.scrollIntoView({ block: "center" });
      el.focus();
      return "ok";
    })()`);
    if (!found) {
      findings.push(`no control labelled "${opener}" — the gate no longer offers this to open`);
      continue;
    }
    await sleep(300);
    const before = await evaluate(focused);

    await evaluate(`(() => {
      const el = [...document.querySelectorAll("button")]
        .find((b) => (b.textContent || "").trim().toLowerCase().includes(${JSON.stringify(opener.toLowerCase())}));
      el.click();
      return 1;
    })()`);
    await sleep(500);

    const inside = await evaluate(`(() => {
      const dialog = document.querySelector('[role="dialog"]');
      if (!dialog) return "no dialog opened";
      return dialog.contains(document.activeElement) ? "inside" : "outside: " + ${focused};
    })()`);

    // Tab a few times: it must not walk out of the dialog.
    let escaped = false;
    for (let i = 0; i < 6 && !escaped; i++) {
      await key({ key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
      const still = await evaluate(`(() => {
        const dialog = document.querySelector('[role="dialog"]');
        return dialog ? dialog.contains(document.activeElement) : false;
      })()`);
      if (!still) escaped = true;
    }

    await key({ key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
    await sleep(400);
    const after = await evaluate(focused);
    const closed = await evaluate(`document.querySelectorAll('[role="dialog"]').length === 0`);

    rows.push({ opener, before, inside, escaped, after, closed });

    if (inside !== "inside") findings.push(`${opener}: focus did not move into the dialog (${inside})`);
    if (escaped) findings.push(`${opener}: Tab walked out of the dialog — the trap is not holding`);
    if (!closed) findings.push(`${opener}: Escape did not close it`);
    if (after !== before) {
      findings.push(
        `${opener}: focus did not come back. It was on ${before} and is now on ${after}. ` +
          `A keyboard now starts from the top of the document.`,
      );
    }
  }
  // ── The order the shell is walked in ───────────────────────────────────
  //
  // On a real screen rather than the gate: the gate is one long document and
  // its order says nothing about the product's.
  await evaluate(`location.hash = "#/devices"`);
  await sleep(2500);

  // Start from the very top of the document, the way a fresh Tab does.
  await evaluate(`(() => { document.activeElement?.blur?.(); return 1; })()`);
  const band = `(() => {
    const el = document.activeElement;
    if (!el || el === document.body) return "body";
    if (el.closest(".titlebar")) return "chrome";
    // Only there when something is wrong with the machine, and then it is
    // the first thing after the chrome — which is where somebody looking for
    // it would tab to.
    if (el.closest(".alarm")) return "alarm";
    if (el.closest(".sidebar")) return "rail";
    if (el.closest(".topbar")) return "topbar";
    if (el.closest(".page-body")) return "page";
    if (el.closest(".statusbar")) return "status";
    return "elsewhere";
  })()`;

  const walked = [];
  for (let i = 0; i < 30; i++) {
    await key({ key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
    const where = await evaluate(band);
    if (where === "body" && walked.length > 0) break;
    if (walked[walked.length - 1] !== where) walked.push(where);
  }
  order = walked;

  // Every band the shell draws has to be reachable at all.
  // The alarm is not required: it is only drawn when the machine has
  // something wrong with it, which is not a state a check should arrange.
  for (const wanted of ["chrome", "rail", "topbar", "page"]) {
    if (!walked.includes(wanted)) {
      findings.push(`Tab never reaches the ${wanted}: the order is ${walked.join(" → ") || "nothing"}`);
    }
  }
  // And in the order they are drawn. Only the first visit of each band is
  // compared, because a band may legitimately be returned to.
  const first = (name) => walked.indexOf(name);
  const drawn = ["chrome", "alarm", "rail", "topbar", "page"].filter((b) => first(b) >= 0);
  const sorted = [...drawn].sort((a, b) => first(a) - first(b));
  if (drawn.join(",") !== sorted.join(",")) {
    findings.push(
      `the tab order is not the reading order: ${walked.join(" → ")}. ` +
        `A window whose tab order is not its reading order has to be explored rather than read.`,
    );
  }
} finally {
  ws.close();
  chrome.kill();
}

const pad = (s, n) => String(s).padEnd(n);
console.log(`    ${pad("opens", 20)}${pad("focus went", 12)}${pad("trapped", 9)}${pad("closed", 8)}came back`);
for (const r of rows) {
  console.log(
    `    ${pad(r.opener, 20)}${pad(r.inside === "inside" ? "inside" : "out", 12)}` +
      `${pad(r.escaped ? "no" : "yes", 9)}${pad(r.closed ? "yes" : "no", 8)}` +
      `${r.after === r.before ? "yes" : `no (${r.after})`}`,
  );
}

if (findings.length) {
  console.error("");
  for (const f of findings) console.error(`  ✗ keyboard: ${f}`);
  process.exit(1);
}
console.log(
  `  ✓ keyboard: ${rows.length} overlays take focus, hold it against Tab and give it back; ` +
    `the shell walks ${order.join(" → ")}`,
);
