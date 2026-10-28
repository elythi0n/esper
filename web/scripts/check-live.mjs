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
    // Back to the list, whatever an earlier check left open on this screen.
    location.hash = "#/";
    await new Promise((r) => setTimeout(r, 200));
    location.hash = "#/devices";
    await new Promise((r) => setTimeout(r, 700));
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
      (el) => el.children.length === 0 && /Connect without asking/.test(el.textContent || ""),
    );
    /*
      What a session with this device leaves the screen as.

      The store says "always" for this one. A control that read nothing and
      showed its own default would look exactly like a working one until
      somebody changed the wrong machine's setting, so what is read is the
      word on screen and not merely that a control is there.
    */
    const lock = document.querySelector("[data-lock-after]");
    // Read now, not in the answer below: the panel is one element that
    // re-renders for whichever device is selected, so anything still holding a
    // node after the next click is holding the other device's words.
    const lockSaid = lock ? (lock.textContent || "").replace(/\\s+/g, " ").trim() : null;
    /*
      And whether this one may be woken.

      Offered only for a device this machine has learned a hardware address
      from, because a magic packet is that address and nothing else. The
      fixture has one such device and six without, so a button that is always
      there is a button that fails for six people out of seven — which is why
      the second device is looked at too.
    */
    const boundSaid = bound ? (bound.textContent || "").trim() : null;
    const hadUnattended = Boolean(unattended);
    const canWake = () => Boolean(document.querySelector("[data-wake]"));
    /*
      And the three things this machine can do *to* that one.

      Locking, logging off and restarting have been permissions since the
      store existed, and a terminal was the only thing that could send one —
      so a window could list a machine and not lock it. Whether any of them is
      allowed is decided at the far end, so what is checked here is that they
      are offered and that the two which outlive the session ask first.
    */
    const reaching = document.querySelector("[data-reaching]");
    const offered = reaching
      ? [...reaching.querySelectorAll("button")].map((b) => (b.textContent || "").trim())
      : [];
    const wakesTheOneWithAnAddress = canWake();
    let wakesTheOneWithout = null;
    const other = rowFor("elythion");
    if (other) {
      other.click();
      await new Promise((r) => setTimeout(r, 700));
      wakesTheOneWithout = canWake();
    }
    /*
      **A dialog's "no" actually closes it.**

      A button inside a Radix dialog is an ordinary button: it runs its
      handler and the dialog stays exactly where it was. Both of this panel's
      dialogs had a cancel that did nothing at all — it looked like one, and
      only the X in the corner and Escape worked. Nothing could see it: the
      markup is right, the name is right, the focus trap is right, and the
      keyboard gate checks that an overlay gives focus back when it closes
      rather than that a button closes it.

      So: open one, press the button that means no, and look for the dialog.
    */
    let cancelWorks = null;
    const forget = [...document.querySelectorAll("button")].find((b) =>
      /forget this device/i.test(b.textContent || ""),
    );
    if (forget) {
      forget.click();
      await new Promise((r) => setTimeout(r, 600));
      const no = [...document.querySelectorAll('[role="dialog"], [role="alertdialog"]')]
        .flatMap((d) => [...d.querySelectorAll("button")])
        .find((b) => /^(keep|leave it|cancel)$/i.test((b.textContent || "").trim()));
      if (no) {
        no.click();
        await new Promise((r) => setTimeout(r, 700));
        cancelWorks = document.querySelector('[role="dialog"], [role="alertdialog"]') === null;
      }
      // Whatever happened, leave nothing open behind this.
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
      await new Promise((r) => setTimeout(r, 300));
    }

    // Restarting is the one that cannot be undone from here, so it must not
    // happen on one click. Opened and dismissed, never confirmed.
    let asksFirst = null;
    const restart = [...(reaching ? reaching.querySelectorAll("button") : [])].find((b) =>
      /restart/i.test(b.textContent || ""),
    );
    if (restart) {
      restart.click();
      await new Promise((r) => setTimeout(r, 600));
      asksFirst = [...document.querySelectorAll('[role="dialog"], [role="alertdialog"]')].some((d) =>
        /restart/i.test(d.textContent || ""),
      );
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
      await new Promise((r) => setTimeout(r, 400));
    }

    return JSON.stringify({
      said: boundSaid,
      hasUnattended: hadUnattended,
      offered,
      asksFirst,
      cancelWorks,
      lockAfter: lockSaid,
      wakesTheOneWithAnAddress,
      wakesTheOneWithout,
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
    /*
      **And what a session leaves the screen as.** The engine has had the verb
      since the setting existed and nothing on this side reached it, so the
      answer was visible only in `esper devices` and changeable only in a
      terminal. The store says `always` for this device, so a panel showing
      "Follow this machine" is a panel that read nothing.
    */
    /*
      **And a device asleep on the network is one the window can reach.** The
      engine has had `wake` since Phase 12 and only a terminal could send one.
      The offer follows the hardware address rather than being always there:
      six of the fixture's seven devices have none, and there is nothing to
      send to for those.
    */
    if (!seen.wakesTheOneWithAnAddress) {
      findings.push(
        "the panel for a device this machine has a hardware address for offers no way to wake it — the " +
          "engine has had the verb since Phase 12 and a terminal was the only thing that could send one",
      );
    }
    if (seen.wakesTheOneWithout === null) {
      findings.push("no second device was opened, so nothing checked that the wake offer follows the address");
    } else if (seen.wakesTheOneWithout) {
      findings.push(
        "a device this machine has never learned a hardware address for is offered a wake — there is " +
          "nothing to send to, so it is a button that fails for six of the seven devices here",
      );
    }
    /*
      **And the actions that reach the far machine.** The window could show a
      device and not lock it: the permission had existed since the store did
      and a terminal was the only thing that could send one.
    */
    const doing = (seen.offered ?? []).join(" ").toLowerCase();
    for (const wanted of ["lock", "log", "restart"]) {
      if (!doing.includes(wanted)) {
        findings.push(
          `the device panel does not offer to ${wanted} that machine — it offers ` +
            `${JSON.stringify(seen.offered ?? [])}`,
        );
      }
    }
    if (seen.cancelWorks === null) {
      findings.push(
        "no dialog with a cancel was found on the device panel, so nothing checked that the button " +
          "meaning no does anything",
      );
    } else if (!seen.cancelWorks) {
      findings.push(
        "a dialog's cancel button leaves it open — it looks like a way out and is not one, and the " +
          "only ways out are the X in the corner and Escape",
      );
    }
    if (seen.asksFirst === null) {
      findings.push("no restart was offered, so nothing checked that it asks before it happens");
    } else if (!seen.asksFirst) {
      findings.push(
        "restarting a paired machine happens on one click — it is the one action whose effect " +
          "outlives the session that asked for it, and it cannot be undone from here",
      );
    }
    if (seen.lockAfter === null) {
      findings.push(
        "the device panel says nothing about what a session leaves the screen as — the host-wide setting " +
          "is the only thing a person can see, and this device overrides it",
      );
    } else if (!/always lock/i.test(seen.lockAfter)) {
      findings.push(
        `the device panel says ${JSON.stringify(seen.lockAfter)} about what a session leaves the screen ` +
          `as, and the store says "always" for this device`,
      );
    }
  }

  // ── 3. A list left open does not freeze the clock ──────────────────────
  //
  // An age worked out at render time is an age that stays put — a device seen
  // fourteen seconds ago reads "14s" for as long as somebody leaves the
  // window open, which on a list nobody is touching is the ordinary case and
  // which makes the column say the opposite of what it is for.
  //
  // Nothing in the source can catch that. The function is right, the call is
  // right, and the defect is entirely that the call happens once. So: read
  // the column, wait, and read it again.
  //
  // **On the gallery, not on the product's own Devices screen.** That screen
  // reloads its list whenever the control plane says something happened, and
  // a reload redraws every row — so a gate watching it cannot tell an element
  // that keeps its own clock from one redrawn for an unrelated reason, and a
  // static age passes. The gallery draws the same row from a fixed array with
  // nothing behind it, so the only thing that can change a figure there is
  // the figure itself.
  //
  // Loaded afresh, because the gallery's ages are worked out when its module
  // is first evaluated: a page open for a minute has no device left on the
  // seconds scale, and this claim is about seconds.
  await send("Page.navigate", { url: `${base.replace(/\/$/, "")}/#/design` });
  await sleep(3000);
  const ticking = await evaluate(`(async () => {
    // Scoped to the list's own column. Other things on the page keep their
    // own clocks — a pairing word counts itself down — and a probe that read
    // any <time> would be measuring one of those instead.
    const ages = () => [...document.querySelectorAll("[data-last-seen] time")]
      .map((el) => ({ said: (el.textContent || "").trim(), at: el.getAttribute("datetime") }));
    const all = ages();
    // Only the ones counting in seconds: a device seen three days ago will not
    // change in the second and a half this waits, and is no evidence either
    // way.
    const before = all.filter((a) => /^\\d+s$/.test(a.said));
    await new Promise((r) => setTimeout(r, 1500));
    // Matched by the instant each row carries rather than by position: a row
    // whose words move out of the range this looks at must not shift the
    // others along and make an unmoved figure read as a moved one.
    const now = new Map(ages().map((a) => [a.at, a.said]));
    return JSON.stringify({
      before,
      after: before.map((b) => now.get(b.at) ?? null),
      // How many of the same rows now say something else.
      moved: before.filter((b) => now.has(b.at) && now.get(b.at) !== b.said).length,
      // An instant a machine can read, which "14s" on its own is not.
      stamped: all.every((a) => a.at && !Number.isNaN(Date.parse(a.at))),
    });
  })()`);
  {
    const clock = JSON.parse(ticking ?? "{}");
    if (!clock.before || clock.before.length === 0) {
      findings.push(
        "no device row reports an age in seconds, so nothing checked that the column is current — the " +
          "gallery's fixture is meant to hold one seen moments ago",
      );
    } else if (!clock.moved) {
      findings.push(
        `a device row said ${JSON.stringify(clock.before.map((a) => a.said))} and still said it 1.5s ` +
          `later — nothing redraws a list somebody is only looking at, so an age drawn once is an age ` +
          `that is wrong from the second after it is drawn`,
      );
    } else if (!clock.stamped) {
      findings.push(
        "an age in a device row carries no machine-readable instant — a screen reader and a test both " +
          "get \"14s\" with nothing saying 14s before what",
      );
    }
  }

  // ── 4. The wordmark's letters are in five places ───────────────────────
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

  // ── 5. Every setting this machine has, and the ones it may not change ──
  //
  // Until this screen existed, all thirty-two of them were a file somebody
  // edited by hand — and nothing creates that file, so the first change meant
  // knowing the path, the syntax and the key.
  //
  // What is checked is the part no unit test can see: that the registry the
  // engine owns reaches a screen whole, that the six settings which need
  // somebody at the machine are marked as such rather than offered, and that
  // a change made here reaches the file. The last one is the whole path — a
  // control, a request, the engine, the file — and it is measured by the
  // *source* changing from "default" to the file, which only a written file
  // can do.
  await send("Page.navigate", { url: `${base.replace(/\/$/, "")}/#/settings` });
  await sleep(3500);
  const configured = await evaluate(`(async () => {
    const rows = () => [...document.querySelectorAll("[data-setting]")];
    for (let waited = 0; waited < 8000 && rows().length === 0; waited += 200) {
      await new Promise((r) => setTimeout(r, 200));
    }
    const said = (key) => {
      const row = rows().find((el) => el.getAttribute("data-setting") === key);
      return row ? (row.textContent || "").replace(/\\s+/g, " ").trim() : null;
    };
    // The control belongs to the row's setting; the row is the words beside
    // it, so the control is found from the whole block they share.
    const blockFor = (key) => {
      const row = rows().find((el) => el.getAttribute("data-setting") === key);
      let up = row;
      for (let i = 0; i < 5 && up; i += 1) {
        if (up.querySelector("button, input, [role='switch'], [role='combobox']")) return up;
        up = up.parentElement;
      }
      return null;
    };
    const controlFor = (key) => {
      const block = blockFor(key);
      return block ? block.querySelector("button, input, [role='switch'], [role='combobox']") : null;
    };
    const off = (el) =>
      el !== null && (el.disabled === true || el.getAttribute("aria-disabled") === "true" ||
        el.getAttribute("data-disabled") !== null);

    // The screen is in sections now, so the registry is read across all of
    // them: every section visited, every key it shows counted once. A setting
    // that no section shows is a setting missing from the screen.
    const settle = async () => {
      await new Promise((r) => setTimeout(r, 400));
      for (let waited = 0; waited < 6000 && rows().length === 0; waited += 200) {
        await new Promise((r) => setTimeout(r, 200));
      }
    };
    const open = async (section) => {
      window.location.hash = "/settings/" + section;
      await settle();
    };
    const sections = [...document.querySelectorAll(".settings-nav-item")].length;
    const everyKey = new Set();
    for (const id of ["general", "machine", "connections", "privacy", "files", "activity"]) {
      await open(id);
      for (const el of rows()) everyKey.add(el.getAttribute("data-setting"));
    }

    await open("connections");
    const file = (document.querySelector("[data-config-file]") || {}).textContent || null;
    const machineOnly = controlFor("host.serve_without_indicator");
    const ordinary = controlFor("host.fps");
    const machineOnlyDisabled = off(machineOnly);
    const machineOnlySaysWhy = (said("host.serve_without_indicator") || "").length > 0 &&
      [...document.querySelectorAll("[data-not-here]")].some((el) =>
        /esper config set host.serve_without_indicator/.test(el.textContent || ""));
    const ordinaryOffered = ordinary !== null && !off(ordinary);

    await open("privacy");

    // And a change, all the way to the file. A flag, so it is one click; one
    // nothing else in this suite reads; and put back afterwards so the rig is
    // as it was found.
    const flag = "policy.deny_when_locked";
    const before = said(flag);
    let after = null;
    const toggle = controlFor(flag);
    if (toggle) {
      toggle.click();
      for (let waited = 0; waited < 6000; waited += 200) {
        await new Promise((r) => setTimeout(r, 200));
        after = said(flag);
        if (after && after !== before) break;
      }
      const back = controlFor(flag);
      if (back) back.click();
      await new Promise((r) => setTimeout(r, 1500));
    }

    return JSON.stringify({
      shown: everyKey.size,
      sections,
      file,
      machineOnlyDisabled,
      machineOnlySaysWhy,
      ordinaryOffered,
      before,
      after,
    });
  })()`);
  {
    const settings = JSON.parse(configured ?? "{}");
    // A floor: a screen showing one row would otherwise pass every claim
    // below, and the registry has thirty-two.
    if (!settings.shown || settings.shown < 20) {
      findings.push(
        `the settings screen shows ${settings.shown ?? 0} setting(s), and this build has over thirty — ` +
          `a screen with a handful of them is a partial answer to "what is this machine set to"`,
      );
    } else {
      if (!settings.file) {
        findings.push(
          "the settings screen does not say where the values are written, so somebody who wants to " +
            "read or back up the file has nowhere to look",
        );
      }
      if (!settings.machineOnlyDisabled) {
        findings.push(
          "host.serve_without_indicator is offered as changeable from this window — it is what makes " +
            "being watched visible, and turning it off from anywhere but the machine is the one change " +
            "nobody in front of the screen would know about",
        );
      }
      if (!settings.machineOnlySaysWhy) {
        findings.push(
          "a setting that may only be changed at the machine is shown without the reason or the " +
            "command — a disabled control with no sentence is the screen refusing and not saying so",
        );
      }
      if (!settings.ordinaryOffered) {
        findings.push(
          "host.fps is not offered either, so the screen refuses everything — which would make every " +
            "claim above true and the screen useless",
        );
      }
      if (!settings.after || settings.after === settings.before) {
        findings.push(
          `changing a setting on the screen left it reading ${JSON.stringify(settings.before)} — ` +
            `the control, the request, the engine or the file is not connected`,
        );
      } else if (!/config file/i.test(settings.after)) {
        findings.push(
          `after a change the setting reads ${JSON.stringify(settings.after)}, and a change that ` +
            `reached the file would say so as its source`,
        );
      }
    }
  }

  // ── 6. A filter that hides everything says so ──────────────────────────
  //
  // The Activity screen draws its table only when there are rows, so filters
  // that match nothing left the filter row, a count of nought, and blank
  // space. That is the same screen a machine where nothing has happened
  // shows — and a person cannot tell a filter they got wrong from a quiet
  // machine, which is the moment they need the way back most.
  //
  // Easier to reach since the screen grew a filter for what happened:
  // twenty-four kinds of line and fourteen permissions is a lot of pairs that
  // never occurred together.
  await send("Page.navigate", { url: `${base.replace(/\/$/, "")}/#/activity` });
  await sleep(3000);
  const filtered = await evaluate(`(async () => {
    const rows = () => document.querySelectorAll("[data-line]").length;
    for (let waited = 0; waited < 8000 && rows() < 1; waited += 200) {
      await new Promise((r) => setTimeout(r, 200));
    }
    const before = rows();
    // Two filters that cannot both be true of one line: a pairing is
    // authorised by no permission at all.
    const pick = async (label, value) => {
      const trigger = [...document.querySelectorAll("[role='combobox']")].find(
        (b) => (b.getAttribute("aria-label") || "").toLowerCase().includes(label),
      );
      if (!trigger) return false;
      trigger.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0, pointerType: "mouse" }));
      await new Promise((r) => setTimeout(r, 500));
      const item = [...document.querySelectorAll("[role='option']")].find(
        (o) => (o.textContent || "").trim().toLowerCase() === value,
      );
      if (!item) {
        document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
        return false;
      }
      item.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0, pointerType: "mouse" }));
      item.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, button: 0, pointerType: "mouse" }));
      item.click();
      await new Promise((r) => setTimeout(r, 600));
      return true;
    };

    // The first option that is not the "any" one, whatever this record holds.
    const pickFirstReal = async (label) => {
      const trigger = [...document.querySelectorAll("[role='combobox']")].find(
        (b) => (b.getAttribute("aria-label") || "").toLowerCase().includes(label),
      );
      if (!trigger) return false;
      trigger.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0, pointerType: "mouse" }));
      await new Promise((r) => setTimeout(r, 500));
      const item = [...document.querySelectorAll("[role='option']")].find(
        (o) => !/^any/i.test((o.textContent || "").trim()),
      );
      if (!item) {
        document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
        return false;
      }
      item.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0, pointerType: "mouse" }));
      item.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, button: 0, pointerType: "mouse" }));
      item.click();
      await new Promise((r) => setTimeout(r, 600));
      return true;
    };

    // **A pairing, and any permission at all.** Pairing is authorised by no
    // permission, so no line can satisfy both — whichever permission the
    // record happens to offer.
    const kind = await pick("event", "paired");
    const perm = await pickFirstReal("permission");
    const empty = document.querySelector("[data-empty='filtered']");
    const said = empty ? (empty.textContent || "").replace(/\\s+/g, " ").trim() : null;
    const away = empty ? [...empty.querySelectorAll("button")].map((b) => (b.textContent || "").trim()) : [];
    return JSON.stringify({ before, kind, perm, said, away });
  })()`);
  {
    const seen = JSON.parse(filtered ?? "{}");
    if (!seen.before || seen.before < 1) {
      findings.push(
        "the Activity screen drew no rows at all, so nothing checked what it says when a filter " +
          "hides them — the rig's log is meant to hold some",
      );
    } else if (!seen.kind || !seen.perm) {
      findings.push(
        "one of the Activity screen's filters could not be set, so nothing checked what it says " +
          "when they match nothing",
      );
    } else if (!seen.said) {
      findings.push(
        "filters that match nothing leave the Activity screen blank — which is the same screen a " +
          "machine where nothing has happened shows, and neither says which it is",
      );
    } else {
      // It says how many there are, so "my filter is wrong" and "this machine
      // is quiet" are different sentences.
      if (!/has \d+ entr(y|ies)/.test(seen.said)) {
        findings.push(
          `a filter that hides everything says ${JSON.stringify(seen.said)} — without the count, it ` +
            `reads the same as a machine with nothing on it`,
        );
      }
      // And the way back is on it, because that is the one thing to do next.
      if (!seen.away.some((b) => /clear/i.test(b))) {
        findings.push(
          `a filter that hides everything offers ${JSON.stringify(seen.away)} — the way back has to ` +
            `be where the sentence is`,
        );
      }
    }
  }

  // ── 7. A setting changed here is in the record of what happened ────────
  //
  // Section 5 changed `policy.deny_when_locked` and changed it back, which is
  // two changes to a setting that decides whether a device may connect while
  // this screen is locked. The settings are the part of this product that
  // decides who can reach the machine, so "somebody changed one" belongs in
  // the record — and this is the only check that walks the whole of it: a
  // control on a screen, the local API, the engine, the file it writes, the
  // trail beside it, the API again, and the screen that shows it.
  //
  // Written as a wait rather than a read: the trail is appended by a separate
  // process, and the Activity screen polls.
  //
  // The filters section 6 set are still on — this is a hash route on the same
  // page, so nothing reloads — and they are deliberately a pair no line can
  // satisfy. Cleared with the button that section checked is there, which
  // also proves the way back leads somewhere.
  await send("Page.navigate", { url: `${base.replace(/\/$/, "")}/#/activity` });
  await sleep(2000);
  const recorded = await evaluate(`(async () => {
    const clear = [...document.querySelectorAll("button")].find(
      (b) => /clear filters/i.test(b.textContent || ""),
    );
    if (clear) {
      clear.click();
      await new Promise((r) => setTimeout(r, 800));
    }
    // The event's tag is an attribute now, not a word on screen.
    const lines = () => [...document.querySelectorAll("[data-line]")].map(
      (el) => ((el.getAttribute("data-event") || "") + " " + (el.textContent || "")).replace(/\\s+/g, " ").trim(),
    );
    let found = [];
    for (let waited = 0; waited < 15000; waited += 500) {
      found = lines().filter((t) => /setting_changed/.test(t));
      if (found.length > 0) break;
      await new Promise((r) => setTimeout(r, 500));
    }
    return JSON.stringify({ rows: lines().length, found });
  })()`);
  {
    const trail = JSON.parse(recorded ?? '{"rows":0,"found":[]}');
    if (trail.rows < 1) {
      findings.push(
        "the Activity screen shows no lines at all, so nothing below it measures anything",
      );
    } else if (trail.found.length === 0) {
      findings.push(
        "a setting was changed from this window and the record of what happened says nothing about " +
          "it — the settings decide who can reach this machine, and a change nobody can find " +
          "afterwards is the one an investigation asks about",
      );
    } else {
      const about = trail.found.find((t) => /policy\.deny_when_locked/.test(t));
      if (!about) {
        findings.push(
          `the record has ${trail.found.length} setting change(s) and none names the setting that ` +
            `was changed: ${JSON.stringify(trail.found.slice(0, 3))}`,
        );
      } else if (!/this machine/i.test(about)) {
        findings.push(
          `the line reads ${JSON.stringify(about)} — nobody connected to change a setting, so the ` +
            `device column has to say so rather than leave a reader looking for a peer`,
        );
      } else if (!/Setting changed/.test(about)) {
        findings.push(
          `the line reads ${JSON.stringify(about)} — the What column is showing the tag, which is ` +
            `the state that column used to be in`,
        );
      }
    }
  }

  // ── 8. Three fonts, all loaded, and nothing uses a fourth ──────────────
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
    `contrast, a popover outranks a modal, a device's age keeps itself current, the panel says what a ` +
    `session leaves the screen as, offers to wake the one it can and to lock, log off or restart it — ` +
    `the last asking first and every dialog's no closing it — a filter that hides the record says so ` +
    `and offers the way back — every setting is on a screen with ` +
    `the six that need somebody at the machine marked and a change reaching the file and, from there, ` +
    `the record of what happened — the wordmark's ` +
    `letters are in five places, and all three faces are loaded with nothing asking for a fourth`,
);
