import { useEffect, useRef, useState } from "react";
import { CopyField } from "@/components/copy-field";
import { Key } from "@/screens/parts";
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
}: {
  device?: string | undefined;
  /** What the surface knows, for a HUD that wants to say it. */
  onState?: ((state: ViewerState) => void) | undefined;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [state, setState] = useState<ViewerState>(IDLE);
  const [socket, setSocket] = useState<string | null>(null);

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

  useEffect(() => {
    if (!canvas.current) return undefined;
    return showTheScreen(canvas.current, {
      onState: (next) => {
        setState(next);
        report.current?.(next);
      },
    });
  }, []);

  const playing = state.status === "playing";
  /*
    A session that ended keeps its last picture and says so over it.

    Not a blank canvas: the last frame is the most recent true thing about
    that machine, and clearing it throws away information to make room for a
    message. Not silence either — a still picture with no explanation is
    exactly what a frozen session looks like, which is the failure this
    product's reconnect rules are written against.
  */
  const ended = state.status === "ended" && state.pictures > 0;
  return (
    <div className="absolute inset-0 grid place-items-center bg-video [transition-duration:var(--dur-video)]">
      {/*
        Always mounted, and hidden rather than unmounted while it waits: a
        canvas that appears when the first picture arrives is a canvas that
        gets its first picture drawn into a node the browser has not laid out
        yet, and the frame is lost.
      */}
      <canvas
        ref={canvas}
        aria-label={device ? `The screen of ${device}` : "The remote screen"}
        className={playing || ended ? "h-full w-full object-contain" : "sr-only"}
      />
      {ended && (
        <div
          role="status"
          className="absolute inset-x-0 bottom-0 flex justify-center p-4"
        >
          <p className="rounded-md border border-border bg-surface-1/95 px-4 py-2 text-sm text-text">
            The session ended. This is the last picture it sent.
          </p>
        </div>
      )}
      {!playing && !ended && (
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
