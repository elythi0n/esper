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
export function RemoteScreen({ device }: { device?: string | undefined }) {
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

  useEffect(() => {
    if (!canvas.current) return undefined;
    return showTheScreen(canvas.current, { onState: setState });
  }, []);

  const playing = state.status === "playing";
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
        className={playing ? "h-full w-full object-contain" : "sr-only"}
      />
      {!playing && (
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
