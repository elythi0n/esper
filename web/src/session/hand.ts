/**
 * What the person does in the window, on its way to the far machine.
 *
 * # Why it batches
 *
 * Because a mouse moving across a canvas produces an event per pixel of the
 * path, and a request per pixel would be absurd. One batch per animation
 * frame is one request every sixteen milliseconds, which is also the rate at
 * which anything can be *seen*, so nothing is lost by waiting that long.
 *
 * A streaming request body would be better still and is not available: a
 * duplex `fetch` needs HTTP/2 in Chromium and does not exist in the webview
 * this product ships, so a batch per frame is the shape that works in both
 * engines and in a browser tab.
 *
 * # What it does not send
 *
 * Characters. The wire carries physical positions — `event.code`, mapped to a
 * HID usage — because a key left of Enter must be *that key* on the far
 * machine whatever either layout calls it. `event.key` never appears here.
 *
 * And it does not send everything: a pointer move outside the picture, or a
 * key this build has no usage for, is dropped in the page rather than sent to
 * be dropped at the other end.
 */
import { type Fit, inPicture, placement } from "./fitting";
import { buttonFor, usageFor } from "./keys";

const MAGIC = "ESPRINP\0";
const VERSION = 1;

const KEY = 1;
const POINTER = 2;
const BUTTON = 3;
const SCROLL = 4;
const LET_GO = 5;
const SCREENS = 6;
const QUALITY = 7;

/** One thing the person did, before it is bytes. */
type Event =
  | { kind: typeof KEY; usage: number; pressed: boolean }
  | { kind: typeof POINTER; x: number; y: number }
  | { kind: typeof BUTTON; button: number; pressed: boolean }
  | { kind: typeof SCROLL; dx: number; dy: number }
  | { kind: typeof LET_GO }
  | Ask;

/**
 * Something the window asks *of the session*, rather than does to the machine.
 *
 * It travels on the input stream because that is the window's only way to
 * speak, and a second socket for two messages would be a second thing to
 * connect, to fail and to authorise. The engine keeps them apart at its end:
 * nothing is held down by choosing a screen.
 */
export type Ask =
  /** Show these of the host's screens, by the ids it gave them. */
  | { kind: typeof SCREENS; screens: number[] }
  /** What this window is for, and what it will settle for. */
  | { kind: typeof QUALITY; profile: number; sharpness: number };

function encode(events: readonly Event[], first: boolean): Uint8Array {
  const parts: number[] = [];
  if (first) {
    for (const ch of MAGIC) parts.push(ch.charCodeAt(0));
    parts.push(VERSION);
  }
  // Big-endian, like the rest of this protocol, and written once so a field
  // cannot be put on the wire the other way round.
  const be16 = (into: number[], v: number) => {
    into.push((v >>> 8) & 0xff, v & 0xff);
  };
  const be32 = (into: number[], v: number) => {
    into.push((v >>> 24) & 0xff, (v >>> 16) & 0xff, (v >>> 8) & 0xff, v & 0xff);
  };
  for (const e of events) {
    const body: number[] = [];
    switch (e.kind) {
      case KEY:
        be16(body, e.usage);
        body.push(e.pressed ? 1 : 0);
        break;
      case POINTER:
        // Signed, because a pointer just outside the picture is a real
        // position and the engine is the one that decides to drop it.
        be32(body, e.x | 0);
        be32(body, e.y | 0);
        break;
      case BUTTON:
        body.push(e.button, e.pressed ? 1 : 0);
        break;
      case SCROLL:
        be16(body, e.dx | 0);
        be16(body, e.dy | 0);
        break;
      case SCREENS:
        body.push(e.screens.length);
        for (const id of e.screens) be32(body, id);
        break;
      case QUALITY:
        body.push(e.profile, e.sharpness);
        break;
      default:
        break;
    }
    parts.push(e.kind);
    be32(parts, body.length);
    for (const b of body) parts.push(b);
  }
  // Built on its own buffer, which is what a request body has to be: a view
  // over a shared buffer is not one, and the types are right to insist.
  const out = new Uint8Array(new ArrayBuffer(parts.length));
  out.set(parts);
  return out;
}

/** Ask the session to show these of the host's screens. */
export function showScreens(screens: number[]): Ask {
  return { kind: SCREENS, screens };
}

/** Ask the session for a quality. The numbers are the stream's; see `stream.ts`. */
export function wantQuality(profile: number, sharpness: number): Ask {
  return { kind: QUALITY, profile, sharpness };
}

export interface HandOptions {
  /** Where the window takes input. */
  url?: string;
  /** The picture's size, so a position can be a position in it. */
  picture: () => { width: number; height: number } | null;
  /**
   * How the picture is sitting, read at the moment of each event.
   *
   * A function rather than a value because somebody can change it mid-session,
   * and the very next click has to land in the right place.
   */
  fit?: () => Fit;
}

/** A hand on a surface: what it can say, and how to take it off. */
export interface Hand {
  /**
   * Stop watching, and let go of everything held.
   *
   * The important half: a key still down when a session view unmounts would
   * be a key held on somebody else's machine with nothing left here to
   * release it.
   */
  stop: () => void;
  /**
   * Ask something of the session — a screen, a quality.
   *
   * Here rather than in a function of its own because the stream has a
   * beginning: its magic and version go down the socket once, and a second
   * writer would send them again in the middle of somebody's typing, where
   * the engine reads them as an event kind with a preposterous length and
   * refuses the whole stream.
   */
  ask: (what: Ask) => void;
}

/**
 * Watch an element, and send what happens on it to the session.
 *
 * One of these per *connection* to the engine: see `Hand.ask` for why, and
 * `ViewerState.connection` for what tells a caller the engine has changed.
 */
export function driveWith(
  surface: HTMLElement,
  { url = "/shell/video/input", picture, fit = () => "fit" }: HandOptions,
): Hand {
  let queue: Event[] = [];
  let first = true;
  let stopped = false;
  let frame: number | null = null;
  const held = new Set<number>();

  /*
    One request at a time, in the order they were made.

    Two POSTs in flight can arrive in either order — a browser keeps several
    connections to an origin and uses them — and input out of order is a key
    released before it was pressed, which is a key held down on somebody
    else's machine with nothing left to lift it. Chaining costs nothing that
    matters here: the far end is a process this window started, on a socket
    on this machine, and a batch is at most a frame old when it goes.
  */
  let sending: Promise<void> = Promise.resolve();
  const done = () => {
    // A window with no session takes no input, which is an ordinary state
    // and not one worth a line per frame.
  };

  const flush = () => {
    frame = null;
    if (stopped || queue.length === 0) return;
    const batch = queue;
    queue = [];
    const bytes = encode(batch, first);
    first = false;
    sending = sending
      .then(() =>
        fetch(url, {
          method: "POST",
          body: bytes.buffer as ArrayBuffer,
          headers: { "Content-Type": "application/octet-stream" },
        }),
      )
      .then(done, done);
  };

  const send = (e: Event) => {
    queue.push(e);
    frame ??= requestAnimationFrame(flush);
  };

  /*
    Where a pointer event is, in the picture rather than on the screen.

    The geometry is `fitting.ts` and not this file, because the canvas is told
    how to draw by the same answer. If the two disagreed — the canvas
    contained and the mapping assuming stretched, or the reverse — every click
    would be offset along one axis and scaled along the other, worse the
    further the window is from the session's shape. That is the kind of wrong
    which looks like a calibration problem rather than a bug, and this project
    has had it once already.
  */
  const where = (e: PointerEvent | WheelEvent): { x: number; y: number } | null => {
    const size = picture();
    if (!size) return null;
    // The mode is read at the moment of the event, not captured when the
    // surface was wired up: somebody who changes how the picture sits expects
    // the very next click to land in the right place.
    return inPicture(fit(), surface.getBoundingClientRect(), size, e.clientX, e.clientY);
  };

  /*
    Where the far machine's pointer is, while this one is locked.

    Pointer lock hides the local cursor and reports **movement** rather than a
    position — which is what a game wants and the opposite of what this
    protocol carries, since Esper sends where the pointer *is*. So a position
    is kept here and moved by each event, clamped to the picture.

    Clamped rather than left to run: an unclamped position drifts off the
    picture and stays there, and the pointer then appears stuck against an
    edge with no way back — the mouse is locked, so there is no absolute
    position to recover from. Clamping means pushing against an edge and
    coming back works the way it does on a real desk.
  */
  let locked: { x: number; y: number } | null = null;

  const onPointerMove = (e: PointerEvent) => {
    const size = picture();
    if (document.pointerLockElement === surface && size) {
      const from = locked ?? { x: Math.round(size.width / 2), y: Math.round(size.height / 2) };
      // The scale the picture is drawn at, so a pixel of hand movement is a
      // pixel of picture movement rather than a pixel of screen.
      const at = placement(fit(), surface.getBoundingClientRect(), size);
      const scaleX = at?.scaleX || 1;
      const scaleY = at?.scaleY || 1;
      locked = {
        x: Math.min(size.width, Math.max(0, Math.round(from.x + e.movementX / scaleX))),
        y: Math.min(size.height, Math.max(0, Math.round(from.y + e.movementY / scaleY))),
      };
      send({ kind: POINTER, x: locked.x, y: locked.y });
      return;
    }
    locked = null;
    const at = where(e);
    if (at) send({ kind: POINTER, x: at.x, y: at.y });
  };
  const onPointerDown = (e: PointerEvent) => {
    const button = buttonFor(e.button);
    if (button === null) return;
    // The surface takes the keyboard on a press, so what is typed next goes
    // to the far machine rather than to whatever was focused before.
    surface.focus();
    const at = where(e);
    if (at) send({ kind: POINTER, x: at.x, y: at.y });
    send({ kind: BUTTON, button, pressed: true });
  };
  const onPointerUp = (e: PointerEvent) => {
    const button = buttonFor(e.button);
    if (button !== null) send({ kind: BUTTON, button, pressed: false });
  };
  const onWheel = (e: WheelEvent) => {
    e.preventDefault();
    // Notches, not pixels: a webview's pixel deltas are its own smoothing,
    // and forwarding them would send this machine's scrolling preferences to
    // somebody else's.
    //
    // Neither axis is inverted. A wheel event is positive down and positive
    // right, and so is the protocol — see `esper-input`'s `wheel` module,
    // which inverts for Windows in the one place that has to. Flipping a sign
    // here as well is how scrolling ends up backwards on one platform and
    // right on the other.
    const dx = Math.sign(e.deltaX);
    const dy = Math.sign(e.deltaY);
    if (dx !== 0 || dy !== 0) send({ kind: SCROLL, dx, dy });
  };
  /*
    Escape twice, quickly, gives the keyboard back.

    A surface that forwards Tab cannot be left by the ordinary route, and it
    has to forward Tab — a remote desktop that cannot Tab is not one. So there
    is a way out that is not a key: two presses of Escape inside
    `theSameThought` milliseconds, which is a pattern rather than a chord, so
    both presses still reach the far machine and nothing typed is lost.

    Escape on its own is left alone, because it is a key somebody needs.
  */
  const theSameThought = 500;
  let lastEscape = 0;

  const onKey = (e: KeyboardEvent) => {
    const usage = usageFor(e.code);
    if (usage === null) return;
    // The browser's own shortcuts stay the browser's; everything this table
    // knows belongs to the far machine while the surface has the keyboard.
    e.preventDefault();
    const pressed = e.type === "keydown";
    // A key held down repeats; the far machine has its own repeat and does
    // not need this one's on top of it.
    if (pressed && held.has(usage)) return;
    if (pressed) held.add(usage);
    else held.delete(usage);
    send({ kind: KEY, usage, pressed });
    if (pressed && e.code === "Escape") {
      const now = Date.now();
      const again = now - lastEscape < theSameThought;
      lastEscape = again ? 0 : now;
      // Blurring is what lets go: the release goes out from the blur handler,
      // so the far machine never keeps a key this window has stopped holding.
      if (again) surface.blur();
    }
  };
  const letGo = () => {
    held.clear();
    send({ kind: LET_GO });
    flush();
  };

  surface.addEventListener("pointermove", onPointerMove);
  surface.addEventListener("pointerdown", onPointerDown);
  surface.addEventListener("pointerup", onPointerUp);
  surface.addEventListener("wheel", onWheel, { passive: false });
  surface.addEventListener("keydown", onKey);
  surface.addEventListener("keyup", onKey);
  surface.addEventListener("blur", letGo);
  window.addEventListener("blur", letGo);

  const stop = () => {
    if (frame !== null) cancelAnimationFrame(frame);
    /*
      Let go, always — not only when this side thinks something is held.

      The release exists precisely for the case where this side's bookkeeping
      is wrong: a key down that never got its up, a browser that swallowed an
      event, a surface torn down mid-keystroke. Trusting `held` here would
      mean trusting the thing the release is insurance against, and the cost
      of being wrong is a key stuck down on somebody else's machine with
      nothing left on this side to lift it.
    */
    letGo();
    stopped = true;
    surface.removeEventListener("pointermove", onPointerMove);
    surface.removeEventListener("pointerdown", onPointerDown);
    surface.removeEventListener("pointerup", onPointerUp);
    surface.removeEventListener("wheel", onWheel);
    surface.removeEventListener("keydown", onKey);
    surface.removeEventListener("keyup", onKey);
    surface.removeEventListener("blur", letGo);
    window.removeEventListener("blur", letGo);
  };

  return {
    stop,
    ask: (what: Ask) => {
      if (stopped) return;
      send(what);
      // Sent now rather than on the next frame. A frame's wait is right for
      // input, which arrives in floods; somebody who picked a monitor has
      // made one decision and is watching for it to happen.
      flush();
    },
  };
}
