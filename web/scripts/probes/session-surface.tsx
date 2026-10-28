/**
 * The product's own session screen, mounted for `check-viewer`'s surface leg.
 *
 * # Why a gate needs a file in the product's language
 *
 * "Nothing animates over the video in a live session" is a claim about the
 * rendered document while pictures are arriving, and the two ways to check it
 * cheaply are both wrong. Reading the stylesheets misses a duration written as
 * a utility, and rendering the session with no stream measures a still — which
 * is exactly the state where nothing is animating anyway, so it passes on a
 * session that fades every frame.
 *
 * So this mounts `SessionView` itself, with the real chrome over it, and the
 * gate's server answers `/shell/video` with a live engine's stream. What is
 * measured is the document the product produces.
 *
 * It lives under `scripts/` and is imported by nothing in `src/`, so it is not
 * in the bundle; the dev server transforms it on request like any other
 * module.
 */
import { createRoot } from "react-dom/client";
import { TooltipProvider } from "@/components/tooltip";
import { SessionView } from "@/sessions/SessionView";
import "@/tokens/index.css";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** A CSS time, in milliseconds — the longest, when a property list has several. */
function longest(value: string): number {
  const each = value
    .split(",")
    .map((part) => part.trim())
    .filter(Boolean)
    .map((part) => {
      if (part.endsWith("ms")) return Number.parseFloat(part);
      if (part.endsWith("s")) return Number.parseFloat(part) * 1000;
      return 0;
    })
    .filter((n) => Number.isFinite(n));
  return each.length ? Math.max(...each) : 0;
}

/** Something a person reading a failure can find in the tree. */
function name(el: Element): string {
  const label = el.getAttribute("aria-label") ?? el.getAttribute("role") ?? "";
  const cls = (el.getAttribute("class") ?? "").split(/\s+/).slice(0, 3).join(" ");
  return `${el.tagName.toLowerCase()}${label ? `[${label}]` : ""}${cls ? ` .${cls}` : ""}`;
}

function overlaps(a: DOMRect, b: DOMRect): boolean {
  return a.left < b.right && a.right > b.left && a.top < b.bottom && a.bottom > b.top;
}

/** What each element over the picture is timed at, right now. */
function timings(root: Element, picture: DOMRect) {
  const out: { what: string; transitionMs: number; property: string; animation: string; animationMs: number }[] = [];
  for (const el of [root, ...root.querySelectorAll("*")]) {
    const box = el.getBoundingClientRect();
    if (box.width === 0 || box.height === 0) continue;
    if (!overlaps(box, picture)) continue;
    const style = getComputedStyle(el);
    const transitionMs = longest(style.transitionDuration);
    const animation = style.animationName === "none" ? "" : style.animationName;
    const animationMs = animation ? longest(style.animationDuration) : 0;
    if (transitionMs === 0 && !animation) continue;
    out.push({ what: name(el), transitionMs, property: style.transitionProperty, animation, animationMs });
  }
  return out;
}

/** Every animation the browser is actually running, whatever started it. */
function running(picture: DOMRect) {
  return document.getAnimations().map((a) => {
    const target = (a.effect as KeyframeEffect | null)?.target ?? null;
    const box = target?.getBoundingClientRect() ?? null;
    const timing = a.effect?.getComputedTiming();
    return {
      what: target ? name(target) : "(no target)",
      // A transition names the property it runs on; an animation names itself.
      cause: (a as CSSTransition).transitionProperty ?? (a as CSSAnimation).animationName ?? "?",
      ms: Number(timing?.duration ?? 0),
      overVideo: box ? overlaps(box, picture) : false,
    };
  });
}

/** The canvas, shrunk to 16x16, as a string — for telling two moments apart. */
function fingerprintOf(canvas: HTMLCanvasElement): string {
  const small = document.createElement("canvas");
  small.width = 16;
  small.height = 16;
  const ctx = small.getContext("2d");
  if (!ctx) return "";
  ctx.drawImage(canvas, 0, 0, 16, 16);
  return [...ctx.getImageData(0, 0, 16, 16).data].join(",");
}

/**
 * Whether the canvas is one flat colour — which is what a cleared one is.
 *
 * Not "all zeroes": the context is opaque, so clearing it gives black at full
 * alpha and a test for zero bytes passes over exactly the failure it was
 * written for. A picture of somebody's screen is never one colour.
 */
function flat(fingerprint: string): boolean {
  const bytes = fingerprint.split(",");
  if (bytes.length < 8) return true;
  const first = bytes.slice(0, 4).join(",");
  for (let at = 4; at + 4 <= bytes.length; at += 4) {
    if (bytes.slice(at, at + 4).join(",") !== first) return false;
  }
  return true;
}

const box = document.getElementById("surface");
const report = (r: unknown) =>
  fetch("/report", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(r) });

(async () => {
  const seen: Record<string, unknown> = { mounted: false, playing: false };
  try {
    if (!box) throw new Error("the page has no surface to mount into");
    // The one thing the app supplies that a session screen needs: the same
    // provider `App` wraps everything in, because the HUD's controls carry
    // tooltips and a tooltip without it throws.
    createRoot(box).render(
      <TooltipProvider>
        <SessionView fingerprint="827aabec450ddf13" name="Workshop" />
      </TooltipProvider>,
    );
    seen.mounted = true;

    /*
      Wait for a picture rather than for a time.

      The canvas is `sr-only` until the first one arrives, so its box is the
      product's own answer to "is this a session yet" — and waiting on it means
      everything below is measured on a session that has one, which is the
      whole difference between this and a still.
    */
    let canvas: HTMLCanvasElement | null = null;
    for (let waited = 0; waited < 20000; waited += 100) {
      canvas = box.querySelector("canvas");
      if (canvas && canvas.getBoundingClientRect().width > 32) break;
      await sleep(100);
    }
    if (!canvas || canvas.getBoundingClientRect().width <= 32) {
      // What the screen itself says. `RemoteScreen` puts the reason on the
      // page, so a gate that threw without reading it would be hiding the
      // answer it was standing next to.
      seen.said = (box.textContent ?? "").replace(/\s+/g, " ").trim().slice(0, 300);
      throw new Error("no picture arrived, so nothing here would be measured on a live session");
    }
    seen.playing = true;
    seen.size = `${canvas.width}x${canvas.height}`;

    const root = box.firstElementChild as HTMLElement;
    const style = getComputedStyle(root);
    seen.ceilingMs = longest(style.getPropertyValue("--dur-fast"));
    seen.baseMs = longest(style.getPropertyValue("--dur-base"));
    seen.slowMs = longest(style.getPropertyValue("--dur-slow"));
    seen.videoMs = longest(style.getPropertyValue("--dur-video"));
    seen.session = root.hasAttribute("data-session");

    // The surface itself: the canvas and the box it is drawn in.
    const surface = canvas.parentElement as HTMLElement;
    const onSurface = [canvas, surface].map((el) => ({
      what: name(el),
      transitionMs: longest(getComputedStyle(el).transitionDuration),
      animation: getComputedStyle(el).animationName,
    }));
    seen.surface = onSurface;

    const hud = (box.querySelector('[role="toolbar"]') ?? box.querySelector("[aria-hidden]")) as HTMLElement | null;
    const isHidden = () => hud?.getAttribute("aria-hidden") === "true";
    const at = (x: number, y: number) =>
      root.dispatchEvent(new PointerEvent("pointermove", { clientX: x, clientY: y, bubbles: true }));
    const bounds = () => root.getBoundingClientRect();

    // ── Frames arriving, chrome up ─────────────────────────────────────────
    const edge = bounds();
    at(edge.left + edge.width / 2, edge.bottom - 2);
    await sleep(80);

    /*
      The HUD's own numbers, while they are live.

      "Updates in place" is a claim about the same node showing a new number:
      a readout that is replaced, transitioned or eased is one somebody has to
      read twice. So the node is held onto across the samples and compared by
      identity, not by what it says.
    */
    const readout = () =>
      [...box.querySelectorAll("span")].find((el) => /^\d+\s*fps$/.test((el.textContent ?? "").trim())) ?? null;
    const firstReadout = readout();

    const before = fingerprintOf(canvas);
    const sampled: ReturnType<typeof running> = [];
    const said = new Set<string>();
    let sameNode = firstReadout !== null;
    for (let i = 0; i < 12; i++) {
      sampled.push(...running(canvas.getBoundingClientRect()));
      const now = readout();
      if (now) {
        said.add((now.textContent ?? "").trim());
        if (now !== firstReadout) sameNode = false;
      }
      await sleep(100);
    }
    const hudStyle = firstReadout ? getComputedStyle(firstReadout) : null;
    seen.hud = {
      found: firstReadout !== null,
      said: [...said],
      sameNode,
      transitionMs: hudStyle ? longest(hudStyle.transitionDuration) : null,
      animation: hudStyle && hudStyle.animationName !== "none" ? hudStyle.animationName : "",
      // Digits of one width, or a number changing in place moves the ones
      // beside it — which is the same jitter an animation would be.
      numerals: hudStyle ? hudStyle.fontVariantNumeric : "",
    };
    seen.arriving = fingerprintOf(canvas) !== before;
    seen.running = sampled;
    seen.timed = timings(root, canvas.getBoundingClientRect());

    // ── The same, through a resize ─────────────────────────────────────────
    //
    // A resize is one of the three things the item names, and it is the one
    // that can be driven here: the box changes shape while pictures are
    // arriving, and the picture has to swap rather than ease.
    const wasBox = fingerprintOf(canvas);
    box.style.width = "660px";
    box.style.height = "380px";
    await sleep(60);
    const duringResize: ReturnType<typeof running> = [];
    for (let i = 0; i < 6; i++) {
      duringResize.push(...running(canvas.getBoundingClientRect()));
      await sleep(100);
    }
    seen.resized = fingerprintOf(canvas) !== wasBox;
    seen.runningOnResize = duringResize;
    seen.timedOnResize = timings(root, canvas.getBoundingClientRect());

    // ── And the chrome's own clock ─────────────────────────────────────────
    //
    // Phase 10 says the chrome retracts after `--session-chrome-hide` and
    // comes back at the edge — and that moving *over the video* does not bring
    // it back, because that is the gesture somebody makes constantly.
    const beforeChrome = fingerprintOf(canvas);
    const hideMs = longest(style.getPropertyValue("--session-chrome-hide"));
    seen.hideMs = hideMs;
    at(bounds().left + bounds().width / 2, bounds().bottom - 2);
    const armed = Date.now();
    seen.hiddenWhileUsed = isHidden();
    for (let waited = 0; waited < hideMs + 1500; waited += 100) {
      if (isHidden()) break;
      await sleep(100);
    }
    seen.retractedAfterMs = Date.now() - armed;
    seen.retracted = isHidden();

    // Over the picture: it stays gone.
    const mid = bounds();
    at(mid.left + mid.width / 2, mid.top + mid.height / 2);
    await sleep(200);
    seen.stayedGoneOverVideo = isHidden();

    // At the edge: it comes back.
    at(mid.left + mid.width / 2, mid.bottom - 2);
    await sleep(200);
    seen.cameBackAtEdge = !isHidden();
    seen.stillArriving = fingerprintOf(canvas) !== beforeChrome;
    /*
      And the reconnect: the pictures actually taken away.

      Nothing here is staged in the page. The gate kills the engine when this
      asks it to and starts another a couple of seconds later, so what is
      measured is the window's own answer to a session that stopped — the last
      picture kept, a notice that counts, and the picture coming back without
      anybody touching anything.
    */
    const notice = () =>
      [...box.querySelectorAll('[role="status"]')].find((el) => /Connection/.test(el.textContent ?? "")) ?? null;

    /*
      **The host's pointer**, worn by the surface and drawn over it.

      Worn is the CSS cursor, which is how a browser draws a pointer at the
      person's own hand with no network in the path; drawn is an element over
      the picture, which is what a session that is only watching needs. The
      engine decides which, so the window is checked for doing what it was
      told rather than for deciding again.
    */
    // A file may be dropped on the picture: the window reads this property to
    // tell a drop meant for the product from one meant for the webview.
    seen.dropTarget = getComputedStyle(canvas).getPropertyValue("--wails-drop-target").trim();
    const onItsWay = box.querySelector("[data-transfers]") as HTMLElement | null;
    seen.transfers = onItsWay ? (onItsWay.textContent ?? "").replace(/\s+/g, " ").trim() : "";

    /*
      **How far along, not just what.**

      A drop used to be reported as a name and a word — "Sending big.iso" —
      for as long as the file took, which for a large one is the whole time
      somebody is looking at it. The window now reads the engine's own figures
      and the queue behind the readout turns them into a bar, a size and an
      estimate. The fixture behind this moves, so a page that displayed a
      total it never updated could not reach a second percentage.
    */
    const queueText = () => {
      const panel = [...document.querySelectorAll('[role="menu"]')]
        .map((el) => (el.textContent ?? "").replace(/\s+/g, " ").trim())
        .find((t) => t.includes("still sending") || t.includes("did not finish") || t.includes("arrived"));
      return panel ?? "";
    };
    // A pointer, not a click: the panel hangs from the menu primitive, which
    // opens on pointerdown — and a probe that only clicked would report the
    // panel missing on a page where it works for everybody with a mouse.
    const poke = (el: HTMLElement) => {
      el.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0, pointerType: "mouse" }));
      el.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, button: 0, pointerType: "mouse" }));
      el.click();
    };
    if (onItsWay) {
      poke(onItsWay);
      for (let waited = 0; waited < 3000 && queueText() === ""; waited += 100) await sleep(100);
    }
    /*
      The sizes, read out of the queue rather than the whole blob compared.

      Comparing the panel's text with itself is not enough: a rate ticking
      between 390 and 391 KB/s changes the text while the file it is about
      sits at `0 B of 0 B` — which is what a page that dropped the engine's
      figures looks like, and it passed. So what is read is the pair of sizes
      the queue is showing for the file that is moving.
    */
    const SIZE = /([\d.]+)\s*(B|KB|MB|GB|TB) of ([\d.]+)\s*(B|KB|MB|GB|TB)/;
    const SCALE: Record<string, number> = { B: 1, KB: 1024, MB: 1024 ** 2, GB: 1024 ** 3, TB: 1024 ** 4 };
    const sizes = (text: string) => {
      const m = SIZE.exec(text);
      if (!m) return null;
      return {
        sent: Number(m[1]) * (SCALE[m[2] ?? "B"] ?? 1),
        total: Number(m[3]) * (SCALE[m[4] ?? "B"] ?? 1),
      };
    };
    // The estimate in seconds, so "0s left" cannot pass for an estimate.
    const secondsLeft = (text: string) => {
      const m = /(\d+)\s*(s|min) left/.exec(text);
      if (!m) return null;
      return Number(m[1]) * (m[2] === "min" ? 60 : 1);
    };

    const first = queueText();
    // Two readings apart, because the claim is that the figures move. One
    // reading cannot tell a live percentage from a fixed one.
    await sleep(1400);
    const second = queueText();
    const wasAt = sizes(first);
    const isAt = sizes(second);
    seen.queue = {
      opened: first !== "",
      first,
      second,
      sizes: [wasAt, isAt],
      // A size at all: a queue showing "0 B of 0 B" is a queue that was handed
      // a name and a state and made the rest up.
      sized: (isAt?.total ?? 0) > 0,
      moved: wasAt !== null && isAt !== null && isAt.sent > wasAt.sent,
      // The estimate, which exists only if the page worked out a rate from two
      // readings of its own — and is an actual duration, not "0s".
      estimated: (secondsLeft(second) ?? 0) > 0,
      // A failure is named in the queue as well as summarised.
      names: /big\.iso/.test(second),
    };
    if (onItsWay) {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
      await sleep(150);
    }

    // The readouts panel, which on a loopback is *not there*: a session with
    // no peer has no round trip, no cost and no path, and a panel of zeroes is
    // a panel that teaches people to stop reading it.
    seen.stats = box.querySelector("[data-stats]") !== null;
    // What the engine actually said about the path, so a check that finds no
    // panel can tell "the window did not draw one" from "there was nothing to
    // draw".
    seen.pathSaid = (window as unknown as { __esperPath?: unknown }).__esperPath ?? null;
    seen.tierBadge = (box.textContent ?? "").includes("On this network") ||
      (box.textContent ?? "").includes("Relayed") ||
      (box.textContent ?? "").includes("Direct");

    // What the HUD says was given up, which on a loopback is nothing at all.
    const gave = box.querySelector("[data-narrowed]");
    seen.narrowed = gave ? (gave.textContent ?? "").trim() : null;

    const drawnPointer = () => box.querySelector("[data-remote-pointer]") as HTMLElement | null;
    for (let waited = 0; waited < 4000; waited += 100) {
      if (drawnPointer() && /url\(/.test(getComputedStyle(canvas).cursor)) break;
      await sleep(100);
    }
    const pointerStyle = getComputedStyle(canvas).cursor;
    const composited = drawnPointer();
    const compositedBox = composited?.getBoundingClientRect() ?? null;
    const pictureBox = canvas.getBoundingClientRect();
    seen.pointer = {
      worn: /^url\(/.test(pointerStyle),
      // The hotspot travels with it: `url(...) 4 3, auto` and not `url(...), auto`.
      hotspot: /url\([^)]*\)\s+-?\d+\s+-?\d+/.test(pointerStyle),
      drawn: composited !== null,
      // Where it ended up, so "it is on the screen somewhere" cannot pass for
      // "it is where the engine said".
      inPicture:
        compositedBox !== null &&
        compositedBox.left >= pictureBox.left - 32 &&
        compositedBox.right <= pictureBox.right + 32 &&
        compositedBox.top >= pictureBox.top - 32 &&
        compositedBox.bottom <= pictureBox.bottom + 32,
      moves: false,
    };
    if (compositedBox) {
      const was = `${compositedBox.left},${compositedBox.top}`;
      for (let waited = 0; waited < 2000; waited += 100) {
        const now = drawnPointer()?.getBoundingClientRect();
        if (now && `${now.left},${now.top}` !== was) {
          (seen.pointer as { moves: boolean }).moves = true;
          break;
        }
        await sleep(100);
      }
    }

    /*
      **The switcher, which was a button that could not be pressed.**

      The host's screens reach a window on the picture stream and nowhere
      else, so this is also the check that the engine sent them: the menu is
      built from what arrived, and picking one has to leave as bytes on the
      window's own input route.
    */
    const named = (label: RegExp) =>
      [...box.querySelectorAll("button")].find((b) => label.test(b.getAttribute("aria-label") ?? "")) ?? null;
    const switcher = named(/switch screen/i);
    seen.switcher = {
      found: switcher !== null,
      offered: switcher?.hasAttribute("disabled") === false,
    };
    // Opened the way a person opens it. A dropdown of this kind opens on
    // *pointerdown* — `click()` alone leaves it shut, and a check that only
    // clicked would report an empty menu on a menu that works.
    const open = (el: HTMLElement) => {
      el.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0, pointerType: "mouse" }));
      el.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, button: 0, pointerType: "mouse" }));
      el.click();
    };
    const choose = (el: HTMLElement) => {
      el.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerType: "mouse" }));
      el.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0, pointerType: "mouse" }));
      el.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, button: 0, pointerType: "mouse" }));
      el.click();
    };

    if (switcher && !switcher.hasAttribute("disabled")) {
      open(switcher);
      await sleep(400);
      // A radio group, not a menu: each screen is drawn at its own aspect
      // ratio, which is a panel rather than a list of words. See
      // MonitorSwitcher.
      const items = [...document.querySelectorAll('[role="radio"]')];
      const surfaceScreens = items.map((el) => (el.textContent ?? "").trim());
      seen.screensOffered = surfaceScreens;
      const first = items[0] as HTMLElement | undefined;
      if (first) choose(first);
      await sleep(400);
      // Asking for the screen already on show has nothing to acknowledge, so
      // the chrome says nothing. An acknowledgement that appeared here would
      // be reporting progress towards where the session already is.
      seen.acknowledged = box.querySelector("[data-asking]") !== null;
      // Which one the session is on: `aria-checked`, which is how a radio
      // group says it and what a screen reader reads. A word in the label
      // would be a second way of saying the same thing, and only one of the
      // two reaches somebody who cannot see the border.
      seen.saysWhichIsShowing = items.some((el) => el.getAttribute("aria-checked") === "true");
    }

    const quality = named(/^quality$/i);
    seen.quality = { found: quality !== null, offered: quality?.hasAttribute("disabled") === false };
    if (quality && !quality.hasAttribute("disabled")) {
      open(quality);
      await sleep(400);
      const items = [...document.querySelectorAll('[role="menuitem"]')];
      seen.qualitiesOffered = items.map((el) => (el.textContent ?? "").trim());
      // The last one, so what arrives cannot be mistaken for a default.
      const last = items[items.length - 1] as HTMLElement | undefined;
      if (last) choose(last);
      await sleep(400);
    }

    // The oracle, checked against a canvas that *is* cleared, so "the picture
    // was kept" cannot be a sentence that is true of everything.
    seen.flatIsFlat = flat([...new Array(16 * 16 * 4)].map((_, i) => (i % 4 === 3 ? 255 : 0)).join(","));

    await fetch("/break-the-link", { method: "GET" });
    const readings: string[] = [];
    for (let waited = 0; waited < 8000; waited += 100) {
      const el = notice();
      if (el) {
        const text = (el.textContent ?? "").replace(/\s+/g, " ").trim();
        if (readings[readings.length - 1] !== text) readings.push(text);
        // Two different readings is the count moving, which is the difference
        // between a notice and a window that has simply stopped.
        if (readings.length >= 2) break;
      }
      await sleep(100);
    }
    seen.noticed = readings;
    // The last picture is still there under it, rather than a black canvas.
    seen.keptThePicture = !flat(fingerprintOf(canvas));

    /*
      Watched every frame rather than every fifth of a second.

      A reconnect that clears the canvas is black for the gap between the new
      stream's configuration and its first decoded picture — a flash, and a
      poll slow enough to miss it would pass on a session that blinks every
      time the link hiccups.
    */
    /*
      Whether the canvas is *resized* on the way back, which is the thing that
      clears it.

      Watched rather than sampled. A cleared canvas is black for the gap
      between the new stream's configuration and its first decoded picture,
      and sampling for that flash caught it in one run out of two — a detector
      that finds a real defect half the time is worse than one that says it
      cannot see it. Writing `canvas.width` queues a mutation record even when
      the value is the one already there, so the observer sees every write and
      the reconnect is expected to make none: the size did not change.
    */
    let resizes = 0;
    const watcher = new MutationObserver((records) => {
      resizes += records.length;
    });
    watcher.observe(canvas, { attributes: true, attributeFilter: ["width", "height"] });

    const frozen = fingerprintOf(canvas);
    let cameBackMs: number | null = null;
    let wentBlank = false;
    const broke = Date.now();
    await new Promise<void>((done) => {
      const look = () => {
        const now = fingerprintOf(canvas);
        if (flat(now)) wentBlank = true;
        if (now !== frozen && !flat(now) && notice() === null) {
          cameBackMs = Date.now() - broke;
          done();
          return;
        }
        if (Date.now() - broke > 20000) {
          done();
          return;
        }
        requestAnimationFrame(look);
      };
      requestAnimationFrame(look);
    });
    watcher.disconnect();
    seen.cameBackMs = cameBackMs;
    seen.wentBlank = wentBlank;
    seen.resizedOnReturn = resizes;
  } catch (e) {
    seen.threw = e instanceof Error ? `${e.name}: ${e.message}` : String(e);
  }
  await report(seen);
})();
