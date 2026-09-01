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
