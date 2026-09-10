#!/usr/bin/env node
// Every control on the gate has a name, and nothing small is left small.
//
// # Why this exists
//
// The design system's baseline makes seven promises and a script checked five
// of them. Two were *asserted*: that every control is keyboard reachable with
// a text label somewhere, and that anything drawn under the tap minimum
// carries `data-tap-min`. An asserted promise about accessibility is the kind
// that is true on the day it is written and false three components later,
// because the way it breaks is an icon button somebody added in a hurry —
// which looks completely fine.
//
// # The oracle is the browser, not a regex over the source
//
// An accessible name is not "the aria-label or the text": it comes from a
// specified algorithm over labels, `aria-labelledby`, `title`, alt text,
// and the content of the element and its children. Reimplementing that would
// be reimplementing it *wrongly*, and then this check would have opinions
// rather than findings.
//
// So it asks Chromium. `Accessibility.getFullAXTree` is the name the browser
// computed and therefore the name a screen reader will read — and every
// control on `/design` goes through it, which is every component the product
// has, in its real states, rendered by the same code the window runs.
//
// Usage: node scripts/check-a11y.mjs [--url URL]
// With no --url it expects a dev server or a built preview at :5178.
import { spawn } from "node:child_process";

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : fallback;
};
const base = flag("--url", "http://127.0.1.1:5178/");

/** AX roles that are controls: something a person operates. */
const CONTROLS = new Set([
  "button",
  "link",
  "textbox",
  "checkbox",
  "switch",
  "radio",
  "tab",
  "combobox",
  "menuitem",
  "menuitemcheckbox",
  "menuitemradio",
  "slider",
  "spinbutton",
  "searchbox",
  "listbox",
  "option",
]);

/**
 * The tap minimum, and what "smaller than this" honestly means.
 *
 * The first version of this check required **both** dimensions to reach 44
 * and reported 139 findings, every one of them the check being wrong: a
 * navigation link is 176×28 and a table row is 28px tall *because the product
 * is dense* — `--row-h` is 28px on purpose — and demanding 44px of vertical
 * from every row is demanding a different product.
 *
 * What `data-tap-min` actually rescues is a control with **no long axis to
 * grab**: a 16px close button, an icon in a corner. Its `::after` expands
 * both dimensions, which is the giveaway — it is for things that are small in
 * both. So that is what is required here, and the short-but-wide controls are
 * *counted and reported* rather than hidden, so the number is visible if
 * somebody wants to argue about the density.
 */
const TAP_MIN = 44;

/*
  All three themes, because that is what Phase 10's exit criteria ask for.

  "`/design` is complete, reviewed, and **passes the a11y baseline in all three
  themes**" — and this gate ran at whichever theme the page loaded with, which
  is dark. `check-ink` and `check-overflow` sweep themes; the one gate the
  criterion names by its own name did not, so the criterion was asserted rather
  than measured.

  It is not a formality. High contrast is a different theme rather than a
  filter over one: the focus ring is 3px instead of 2, soft tints are gone and
  a callout takes a border in their place, and elevation becomes a ring. Any of
  those can change what a control's box is — which is the tap minimum — and a
  component that branches on theme can drop a label in one and not another.
  Neither shows up in a sweep of dark.
*/
const THEMES = ["dark", "light", "hc"];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const port = 9600 + (process.pid % 90);
const chrome = spawn(
  process.env.CHROME ?? "/usr/bin/chromium",
  [
    "--headless=new", `--remote-debugging-port=${port}`, "--no-first-run", "--no-sandbox",
    "--disable-gpu", "--window-size=1400,1000", "--hide-scrollbars",
    `--user-data-dir=/tmp/esper-a11y-${process.pid}`, "about:blank",
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
  console.error("  ✗ a11y: chromium did not start. Set CHROME to a browser binary.");
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
let controls = 0;
let named = 0;
let small = 0;
let dense = 0;
let exempted = new Set();
let exemptFocus = new Set();
let ringless = 0;
let focusAsked = 0;
let swept = 0;

try {
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Accessibility.enable");
  await send("DOM.enable");
  await send("Page.navigate", { url: base.includes("#") ? base : `${base.replace(/\/$/, "")}/#/design` });
  await sleep(5000);

  for (const theme of THEMES) {
  // Set on the document root, which is where a theme lives, and given a beat
  // to repaint before anything is measured.
  const applied = await evaluate(
    `(() => { document.documentElement.dataset.theme = ${JSON.stringify(theme)};
      return document.documentElement.dataset.theme; })()`,
  );
  if (applied !== theme) {
    findings.push(`the ${theme} theme did not apply — the page reports ${applied}, so this pass measured another theme`);
    continue;
  }
  await sleep(400);

  // Everything on the page, as the browser understands it.
  const tree = await send("Accessibility.getFullAXTree");
  const nodes = tree.result?.nodes ?? [];
  if (nodes.length < 50) {
    findings.push(`in ${theme}, the accessibility tree has ${nodes.length} nodes, so the page did not render`);
  }

  for (const node of nodes) {
    const role = node.role?.value;
    if (!role || !CONTROLS.has(role)) continue;
    // An ignored node is one the browser has decided is not exposed — a
    // decorative wrapper, or something `aria-hidden`. Not a finding.
    if (node.ignored) continue;
    controls++;
    const name = (node.name?.value ?? "").trim();
    if (name) {
      named++;
      continue;
    }
    // What it is, so a finding can be found. The backend id is useless to a
    // person; the markup is not.
    let where = "";
    if (node.backendDOMNodeId) {
      try {
        const described = await send("DOM.describeNode", { backendNodeId: node.backendDOMNodeId });
        const n = described.result?.node;
        where = n ? `<${(n.localName ?? "?").toLowerCase()}${(n.attributes ?? [])
          .reduce((out, v, i, all) => (i % 2 === 0 ? `${out} ${v}="${all[i + 1]}"` : out), "")
          .slice(0, 160)}>` : "";
      } catch {
        where = "";
      }
    }
    findings.push(`in ${theme}, a ${role} has no accessible name: ${where || "(could not describe it)"}`);
  }

  // And the tap minimum. Measured from the DOM rather than the AX tree,
  // because it is a fact about pixels.
  const tapped = await evaluate(`(() => {
    const selector = 'button, a[href], input, select, textarea, [role="button"], [role="radio"], [role="switch"], [role="tab"], [role="menuitem"], [role="option"]';
    const out = [];
    const exemptions = [];
    let narrow = 0;
    for (const el of document.querySelectorAll(selector)) {
      const r = el.getBoundingClientRect();
      // Nothing that is not drawn: a control inside a closed menu has no box
      // and no tap target to be wrong about.
      if (r.width === 0 || r.height === 0) continue;
      // Small in *both* directions is the case a hit area exists for.
      const tiny = r.width < ${TAP_MIN} && r.height < ${TAP_MIN};
      if (!tiny) {
        if (r.width < ${TAP_MIN} || r.height < ${TAP_MIN}) narrow++;
        continue;
      }
      // The hit area comes from data-tap-min, on the control or an ancestor
      // that carries it for a group.
      if (el.closest("[data-tap-min]")) continue;
      // **An exemption, with its reason in the markup.** Some chrome cannot
      // hold a 44px hit area — a title bar is 32px because the OS draws one
      // at 32px, and an ::after that big would swallow clicks meant for the
      // screen below. Counted and reported rather than skipped silently, so
      // it is a decision somebody can argue with.
      const exempt = el.closest("[data-tap-exempt]");
      if (exempt) {
        exemptions.push(exempt.getAttribute("data-tap-exempt") || "no reason given");
        continue;
      }
      out.push({
        w: Math.round(r.width),
        h: Math.round(r.height),
        what: el.outerHTML.slice(0, 120).replace(/\\s+/g, " "),
      });
    }
    return JSON.stringify({ out, narrow, exemptions });
  })()`);
  const measured = JSON.parse(tapped ?? '{"out":[],"narrow":0,"exemptions":[]}');
  const undersized = measured.out;
  // **Accumulated, not overwritten.** Each theme is its own measurement and a
  // control that is right in dark and wrong in high contrast is a finding in
  // high contrast, not a number the last pass replaces.
  small += undersized.length;
  dense = Math.max(dense, measured.narrow);
  for (const why of measured.exemptions ?? []) exempted.add(why);
  for (const why of new Set(measured.exemptions ?? [])) {
    if (why === "no reason given") {
      findings.push(`in ${theme}, a control is exempt from the tap minimum and gives no reason for it`);
    }
  }
  for (const u of undersized.slice(0, 12)) {
    findings.push(`in ${theme}, a control is drawn ${u.w}×${u.h} and carries no data-tap-min: ${u.what}`);
  }
  if (undersized.length > 12) {
    findings.push(`…and ${undersized.length - 12} more under ${TAP_MIN}px with no hit area in ${theme}`);
  }

  /*
    And the focus ring, on every control, measured rather than assumed.

    This is the promise the design system states most plainly — one focus
    treatment, everywhere, never removed — and it was the one nothing checked.
    It broke the way that promise always breaks: `outline-none` is a Tailwind
    *utility*, the rule that paints the ring lives in `@layer base`, and a
    utility beats a layer. Seventeen places had copied it out of a component
    library's examples. Fifty-eight of three hundred and fifty-five controls
    painted nothing at all, and every one of them looked completely fine.

    # Why a real Tab first

    `:focus-visible` is the browser's judgement about whether the person is
    using a keyboard, so `el.focus()` from a script does not match it — the
    first version of this check read an outline of `none` everywhere and the
    finding was the check. One real key event puts the browser in keyboard
    modality; after that, scripted focus does match, which is what makes it
    possible to ask all 355 rather than only those Tab happens to reach.
  */
  await send("Input.dispatchKeyEvent", { type: "rawKeyDown", windowsVirtualKeyCode: 9, key: "Tab", code: "Tab" });
  await send("Input.dispatchKeyEvent", { type: "keyUp", windowsVirtualKeyCode: 9, key: "Tab", code: "Tab" });
  await sleep(150);
  const rings = await evaluate(`(() => {
    const selector = 'button, a[href], input, select, textarea, [tabindex]:not([tabindex="-1"]), [role="button"], [role="tab"], [role="switch"], [role="checkbox"]';
    const dark = [];
    const excused = [];
    let asked = 0;
    let notKeyboard = 0;
    for (const el of document.querySelectorAll(selector)) {
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) continue;
      if (el.disabled) continue;
      el.focus();
      if (document.activeElement !== el) continue;
      // If the browser does not consider this keyboard focus, the reading
      // would be about modality rather than about the ring.
      if (!el.matches(":focus-visible")) { notKeyboard++; continue; }
      asked++;
      const s = getComputedStyle(el);
      if (s.outlineStyle !== "none" && parseFloat(s.outlineWidth) >= 1) continue;
      // A suppression with its reason in the markup, the way the tap minimum
      // does it: some things are not controls, and a menu moves a highlight
      // instead of moving focus.
      const excuse = el.closest("[data-focus-exempt]");
      if (excuse) { excused.push(excuse.getAttribute("data-focus-exempt") || "no reason given"); continue; }
      dark.push({
        what: el.outerHTML.slice(0, 120).replace(/\\s+/g, " "),
        outline: s.outlineStyle + " " + s.outlineWidth,
      });
    }
    return JSON.stringify({ dark, excused, asked, notKeyboard });
  })()`);
  const ring = JSON.parse(rings ?? '{"dark":[],"excused":[],"asked":0,"notKeyboard":0}');
  // A floor. A page that rendered nothing focusable would pass every claim
  // below by having nothing to be wrong about.
  if (ring.asked < 40) {
    findings.push(`in ${theme}, only ${ring.asked} control(s) answered about focus, so the sweep did not run`);
  }
  if (ring.notKeyboard > 0) {
    findings.push(
      `in ${theme}, ${ring.notKeyboard} control(s) did not register as keyboard focus — ` +
        `the modality tap did not take, so their rings went unmeasured`,
    );
  }
  ringless += ring.dark.length;
  for (const why of ring.excused ?? []) {
    exemptFocus.add(why);
    if (why === "no reason given") {
      findings.push(`in ${theme}, a control drops the focus ring and gives no reason for it`);
    }
  }
  for (const d of ring.dark.slice(0, 8)) {
    findings.push(`in ${theme}, a focused control paints an outline of ${d.outline}: ${d.what}`);
  }
  if (ring.dark.length > 8) {
    findings.push(`…and ${ring.dark.length - 8} more with no focus ring in ${theme}`);
  }
  focusAsked = Math.max(focusAsked, ring.asked);

  swept++;
  }

  // A floor: a run that swept one theme would pass every claim above while
  // measuring a third of what the exit criteria ask for.
  if (swept !== THEMES.length) {
    findings.push(`swept ${swept} theme(s) of ${THEMES.length} — the sweep did not run`);
  }
} finally {
  ws.close();
  chrome.kill();
}

if (findings.length) {
  console.error("");
  for (const f of findings.slice(0, 20)) console.error(`  ✗ a11y: ${f}`);
  if (findings.length > 20) console.error(`  ✗ a11y: …and ${findings.length - 20} more`);
  process.exit(1);
}
console.log(
  `  ✓ a11y: ${controls} controls across ${swept} themes, all ${named} named by the browser; ` +
    `${focusAsked} of them keyboard-focused in each theme and every one painting a ring; ` +
    `nothing small in both directions without a hit area ` +
    `(${dense} are short or narrow by density, which is the product's own choice)`,
);
for (const why of exempted) console.log(`    exempt from the tap minimum: ${why}`);
for (const why of exemptFocus) console.log(`    focus shown another way: ${why}`);
if (small !== 0 || ringless !== 0) throw new Error("unreachable: findings above would have exited");
