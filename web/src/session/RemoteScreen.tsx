import { useEffect, useId, useRef, useState } from "react";
import { CopyField } from "@/components/copy-field";
import { Key } from "@/screens/parts";
import { ReconnectNotice } from "./chrome";
import { type Fit, FIT_CLASS, placement } from "./fitting";
import { driveWith, type Hand } from "./hand";
import { IDLE, showTheScreen, type ViewerState } from "./video";

/**
 * The remote screen, or the reason there is not one.
 *
 * # Why it says how to start a session rather than only that there is none
 *
 * Because the way a session starts today is a person running a command, and
 * the window is the only thing that knows the path to give it. "No picture"
 * is a state; "no picture, and here is the command that makes one" is the
 * same state with the next step in it, which is what the content rules ask
 * of every failure in this product.
 */
export function RemoteScreen({
  device,
  onState,
  drivable = true,
  fit = "fit",
  surfaceRef,
  onHand,
}: {
  device?: string | undefined;
  /** What the surface knows, for a HUD that wants to say it. */
  onState?: ((state: ViewerState) => void) | undefined;
  /**
   * Whether what happens here goes to the far machine.
   *
   * Off for a specimen and for a session nobody asked to drive. The engine
   * refuses input a session does not carry either — this is not the
   * enforcement, it is not *pretending* to drive something that will not
   * move.
   */
  drivable?: boolean;
  /** How the picture sits in the surface. See `fitting.ts`. */
  fit?: Fit;
  /**
   * The hand on the surface, while there is one, so chrome outside can ask
   * something of the session — a screen, a quality.
   *
   * Handed over rather than rebuilt outside, because the input stream has one
   * beginning and therefore one writer. `null` when the surface is not being
   * driven, which is also when there is nothing to ask with.
   */
  onHand?: ((hand: Hand | null) => void) | undefined;
  /**
   * Filled with the canvas, so chrome outside this component can act on it.
   *
   * Pointer lock is the reason: it must be requested from the element the
   * pointer will be locked to, inside a user gesture, and the control that
   * asks for it lives in the HUD rather than here.
   */
  surfaceRef?: React.RefObject<HTMLCanvasElement | null>;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const id = useId();
  /*
    How big the surface is, watched rather than measured.

    The drawn pointer needs it and the pointer moves at the speed of a hand —
    a hundred times a second — so reading a bounding box in the render would
    be a hundred forced layouts a second for a number that changes when
    somebody resizes a window.
  */
  const box = useRef<HTMLDivElement>(null);
  const [surface, setSurface] = useState<{ width: number; height: number } | null>(null);
  useEffect(() => {
    const node = box.current;
    if (!node) return undefined;
    const watch = new ResizeObserver(([entry]) => {
      const rect = entry?.contentRect;
      if (rect) setSurface({ width: rect.width, height: rect.height });
    });
    watch.observe(node);
    return () => watch.disconnect();
  }, []);
  const [state, setState] = useState<ViewerState>(IDLE);
  const [socket, setSocket] = useState<string | null>(null);
  const fitRef = useRef<Fit>(fit);
  fitRef.current = fit;

  useEffect(() => {
    // Where to point a session, asked once. A window without the route — a
    // browser tab — simply has no socket to name, and says so by not saying.
    let live = true;
    fetch("/shell/video/socket")
      .then((r) => (r.ok ? r.json() : null))
      .then((body: { path?: string } | null) => {
        if (live && body?.path) setSocket(body.path);
      })
      .catch(() => {
        // A tab, or a window without the route. Nothing to add.
      });
    return () => {
      live = false;
    };
  }, []);

  // The callback is held in a ref so a parent that passes an inline function
  // does not tear the session down and start it again on every render — which
  // would mean a new decoder, a new keyframe wait, and a picture that flickers
  // whenever anything above it changed.
  const report = useRef(onState);
  report.current = onState;
  // The picture's size, read when a pointer moves rather than captured when
  // the hand was attached — a session that changes size mid-flight would
  // otherwise send positions in the size it used to be.
  const stateRef = useRef(state);
  stateRef.current = state;

  useEffect(() => {
    if (!canvas.current) return undefined;
    return showTheScreen(canvas.current, {
      onState: (next) => {
        setState(next);
        report.current?.(next);
      },
    });
  }, []);

  /*
    The hand, attached to the canvas and only while there is a picture.

    Before the first picture there is nothing to point *at*: the canvas has no
    size, so a position in it would be a fraction of zero. And when the
    session ends the listeners come off, which is what sends the release —
    a key still down with nothing left here to let it go is the failure this
    whole path is most careful about.
  */
  useEffect(() => {
    if (!drivable || !canvas.current || state.status !== "playing") return undefined;
    const hand = driveWith(canvas.current, {
      picture: () => stateRef.current.size,
      // Read through a ref, so changing how the picture sits does not tear the
      // input wiring down and put it back — which would drop whatever is held.
      fit: () => fitRef.current,
    });
    onHand?.(hand);
    return () => {
      onHand?.(null);
      hand.stop();
    };
    // `connection` is in here on purpose: the input stream writes its magic
    // once, so a new engine on the socket needs a new hand. Without it the
    // window would go on typing into a reader that never saw the beginning of
    // the stream, and every keystroke after a reconnect would be refused.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [drivable, state.status, state.connection]);

  const playing = state.status === "playing";

  /*
    The host's pointer, worn.

    Worn rather than composited whenever it is honest: the browser draws it at
    the person's own hand, so it moves at the speed of the hand rather than the
    speed of the link, and nothing has to be repainted behind it. Phase 5
    measured the same choice for X11 — 22.5µs worn against 96.8µs composited,
    and a round trip of latency either way.

    `none` when the host says its pointer is hidden, because on a real desktop
    an invisible cursor is how an application hides one.
  */
  const worn = state.cursor
    ? `url(${state.cursor.url}) ${state.cursor.hotX} ${state.cursor.hotY}, auto`
    : state.pointer && !state.pointer.inPicture
      ? "none"
      : undefined;

  /*
    And composited, when wearing it would be a lie.

    A session that is only watching has no local pointer to speak of, and a
    host whose pointer is not following this one has a pointer somewhere else.
    The engine decides which it is — see `stream.ts` — and this draws it where
    the picture is drawn, which is the same geometry the canvas was told.
  */
  const drawn =
    state.cursor && state.pointer?.draw && state.pointer.inPicture && state.size && surface
      ? // A box at the origin, because the pointer is drawn *inside* this
        // container and the canvas fills it: what comes back is then already
        // an offset within it, with no page coordinates to subtract and no
        // layout to read on every move.
        placement(fit, { left: 0, top: 0, width: surface.width, height: surface.height }, state.size)
      : null;
  /*
    A session whose pictures stopped keeps the last one and says so over it.

    Not a blank canvas: the last frame is the most recent true thing about
    that machine, and clearing it throws away information to make room for a
    message. Not silence either — a still picture with no explanation is
    exactly what a frozen session looks like, which is the failure this
    product's reconnect rules are written against. The window goes on asking
    for the picture underneath; see `showTheScreen`.
  */
  const away = state.link !== "live" && state.pictures > 0;
  const wayOut = `${id}-way-out`;
  return (
    <div ref={box} className="absolute inset-0 grid place-items-center bg-video [transition-duration:var(--dur-video)]">
      {/*
        Always mounted, and hidden rather than unmounted while it waits: a
        canvas that appears when the first picture arrives is a canvas that
        gets its first picture drawn into a node the browser has not laid out
        yet, and the frame is lost.
      */}
      <canvas
        ref={(node) => {
          canvas.current = node;
          if (surfaceRef) surfaceRef.current = node;
        }}
        // A canvas cannot take the keyboard without one, and a remote desktop
        // that cannot be typed into is a television. Focusable by click and by
        // Tab, because a session somebody reached with the keyboard has to be
        // drivable with it.
        tabIndex={drivable ? 0 : -1}
        aria-label={device ? `The screen of ${device}` : "The remote screen"}
        // The way back out, said where somebody who cannot see the screen will
        // hear it. Tab goes to the far machine while this has the keyboard, so
        // the ordinary way out of a control is the one thing that does not
        // work here.
        aria-describedby={drivable ? wayOut : undefined}
        // The class and the pointer mapping come from the same module, so a
        // picture drawn one way cannot be clicked as though it sat another.
        className={playing || away ? `h-full w-full ${FIT_CLASS[fit]}` : "sr-only"}
        style={worn ? { cursor: worn } : undefined}
      />
      {drivable && (
        <p id={wayOut} className="sr-only">
          While this screen has the keyboard, what you type goes to the far machine. Press Escape twice to bring it
          back.
        </p>
      )}
      {drawn && state.cursor && state.pointer && (
        // Nothing to click on and nothing to read: it is a picture of where
        // somebody else's pointer is.
        <img
          src={state.cursor.url}
          alt=""
          aria-hidden
          data-remote-pointer=""
          className="pointer-events-none absolute left-0 top-0 z-[var(--z-session-hud)] origin-top-left"
          style={{
            translate: `${drawn.left + (state.pointer.x - state.cursor.hotX) * drawn.scaleX}px ${
              drawn.top + (state.pointer.y - state.cursor.hotY) * drawn.scaleY
            }px`,
          }}
        />
      )}

      {away && (
        <div className="absolute inset-0 grid place-items-center">
          <ReconnectNotice state={state.link} sinceMs={state.awayMs ?? 0} />
        </div>
      )}
      {!playing && !away && (
        <div className="flex max-w-[var(--measure)] flex-col items-center gap-3 px-8 text-center">
          <Key>{state.status === "failed" ? "No picture" : "Waiting for a session"}</Key>
          <p className="text-sm text-text-2">
            {state.trouble ??
              "This window is listening. A session started against it will draw here — the pictures are decoded in the window itself."}
          </p>
          {socket && (
            <div className="flex w-full flex-col items-center gap-1.5">
              <Key>Point a session at</Key>
              <CopyField value={`esper connect ${device ?? "<device>"} --viewer ${socket}`} wrap />
            </div>
          )}
        </div>
      )}
    </div>
  );
}
