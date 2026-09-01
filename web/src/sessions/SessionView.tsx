import { Maximize2, Minimize2, Monitor, MousePointer2, MousePointerBan, Ratio, Settings2 } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/button";
import { Tooltip } from "@/components/tooltip";
import { Figure } from "@/screens/parts";
import { Presence, SessionChrome, SessionRoot } from "@/session/chrome";
import { type Fit, FIT_NAMES, FITS } from "@/session/fitting";
import { RemoteScreen } from "@/session/RemoteScreen";
import { IDLE, type ViewerState } from "@/session/video";

/** How often to ask the window to put a session back while there is no picture. */
const ASK_AGAIN_MS = 5_000;

/**
 * One session, inside its dock panel.
 *
 * The tab strip above is the chrome that gets you *between* sessions and does
 * not hide. What hides is this: the HUD and the session's own controls, which
 * sit over somebody's desktop and retract after `--session-chrome-hide`,
 * returning when the pointer comes near. The rule the design system states is
 * about chrome over the picture, and this is the chrome over the picture.
 */
export function SessionView({ fingerprint, name }: { fingerprint: string; name: string }) {
  const root = useRef<HTMLDivElement>(null);
  const [picture, setPicture] = useState<ViewerState>(IDLE);
  /** What the window said when it would not start a session. */
  const [refusal, setRefusal] = useState<string | null>(null);
  const [fit, setFit] = useState<Fit>("fit");
  const surface = useRef<HTMLCanvasElement | null>(null);
  const [captured, setCaptured] = useState(false);
  const [full, setFull] = useState(false);

  /*
    Whether this is full screen, from the browser rather than from a variable.

    Somebody can leave full screen with Escape or the window manager, and a
    button that tracked only its own clicks would then offer to leave a state
    it is no longer in. The event is the truth; the state follows it.
  */
  useEffect(() => {
    const onChange = () => setFull(document.fullscreenElement === root.current);
    document.addEventListener("fullscreenchange", onChange);
    return () => document.removeEventListener("fullscreenchange", onChange);
  }, []);

  /*
    Whether the pointer is captured, from the browser rather than a variable.

    The same reasoning as full screen: Escape releases the pointer without
    telling this component, and a control that tracked only its own clicks
    would offer to release something already released.
  */
  useEffect(() => {
    const onChange = () => setCaptured(document.pointerLockElement === surface.current);
    document.addEventListener("pointerlockchange", onChange);
    return () => document.removeEventListener("pointerlockchange", onChange);
  }, []);

  const toggleCapture = () => {
    if (document.pointerLockElement === surface.current) {
      document.exitPointerLock();
    } else {
      // Requested on the canvas and inside this click, which is what a browser
      // requires — and focused first, so what is typed next goes to the far
      // machine rather than to the button that was just pressed.
      surface.current?.focus();
      void surface.current?.requestPointerLock();
    }
  };

  const toggleFull = () => {
    if (document.fullscreenElement === root.current) {
      void document.exitFullscreen().catch(() => {});
    } else {
      void root.current?.requestFullscreen().catch(() => {});
    }
  };

  // The next mode round, so one control serves three without a menu over the
  // picture — which is the thing the design system says not to put there.
  const nextFit = () => setFit((was) => FITS[(FITS.indexOf(was) + 1) % FITS.length] ?? "fit");

  /*
    Ask the window to start the session, and to end it on the way out.

    This is what makes "one click from Devices" true: until now the screen
    showed the command and left somebody to type it. The window is what does
    it, because the window owns the socket the pictures arrive on and is the
    only thing that knows its path.

    A browser tab has no such route and gets a 404 — which is not a failure to
    report, it is a tab. `RemoteScreen` already says what to type in that case,
    which is the right answer there and the wrong one in a window.

    Ending it on unmount matters more than starting it: an engine left running
    holds a connection to somebody else's machine, and their screen goes on
    saying it is being watched.
  */
  const ask = useCallback(() => {
    let live = true;
    void fetch("/shell/session", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ device: fingerprint }),
    })
      .then((r) => {
        if (!live || r.ok || r.status === 404) return;
        return r.text().then((why) => setRefusal(why.trim() || null));
      })
      .catch(() => {
        // No window, or it went away. `RemoteScreen` says what to do.
      });
    return () => {
      live = false;
    };
  }, [fingerprint]);

  useEffect(() => {
    const stop = ask();
    return () => {
      stop();
      // `keepalive` so the request survives the page tearing this down, which
      // is exactly when it is sent.
      void fetch("/shell/session", { method: "DELETE", keepalive: true }).catch(() => {});
    };
  }, [ask]);

  /*
    And ask again while the pictures are gone.

    The surface reconnects to the socket on its own, but a socket nothing is
    writing to stays empty for ever: the engine this window started is what
    died, and starting one is the window's job. The route refuses while a
    session is already running, so asking repeatedly costs a 409 and can never
    leave two engines behind.

    Not a retry of a *request* — a retry of the session. The far end is asked
    again, because a new session is a new session and consent is per session.
  */
  useEffect(() => {
    if (picture.link === "live") return undefined;
    let stop = ask();
    const again = setInterval(() => {
      stop();
      stop = ask();
    }, ASK_AGAIN_MS);
    return () => {
      clearInterval(again);
      stop();
    };
  }, [picture.link, ask]);

  return (
    <SessionRoot ref={root} className="h-full w-full">
      <RemoteScreen device={name} onState={setPicture} fit={fit} surfaceRef={surface} />

      {/*
        The window refusing to start one — already showing another session,
        most often. Said over the picture rather than instead of it, because
        there may still be a picture from a session somebody else started.
      */}
      {refusal && (
        <p
          role="status"
          className="absolute inset-x-0 top-0 z-[var(--z-session-hud)] bg-surface-0/85 px-4 py-2 text-sm text-text-2"
        >
          {refusal}
        </p>
      )}

      {/*
        The HUD, over the picture, retracting — the design system's own chrome
        rather than a second implementation of it. It was one: the same timer,
        the same transition and the same hidden state were written again here,
        and the copy answered a pointer moving anywhere over the picture, which
        is the one gesture `SessionRoot` exists to ignore.
      */}
      <SessionChrome edge="bottom" aria-label="Session" className="gap-4">
        <Figure className="text-xs text-text-3" value={fingerprint.slice(0, 8)} />
        <span className="min-w-0 flex-1 truncate text-sm text-text">{name}</span>
        {/*
          What the window itself is doing, from the surface that is doing it.
          Not the link — there is no link here yet — so it says the two things
          it can honestly know: how often this canvas changed, and how long a
          picture took to get from the engine onto it. Tabular and still, like
          every other readout in this product.
        */}
        {picture.pictures > 0 && (
          <span className="flex shrink-0 items-center gap-3 text-xs text-text-3">
            <Figure value={picture.fps} unit="fps" className="text-xs" />
            {picture.hopMs !== null && <Figure value={Math.round(picture.hopMs)} unit="ms" className="text-xs" />}
          </span>
        )}
        <div className="flex shrink-0 items-center gap-1">
          <Tooltip label="Switch screen">
            <Button variant="ghost" size="sm" aria-label="Switch screen" className="px-1.5" disabled>
              <Monitor aria-hidden size={16} />
            </Button>
          </Tooltip>
          <Tooltip label="Quality">
            <Button variant="ghost" size="sm" aria-label="Quality" className="px-1.5" disabled>
              <Settings2 aria-hidden size={16} />
            </Button>
          </Tooltip>
          <Tooltip label={captured ? "Release the pointer (Escape)" : "Capture the pointer"}>
            <Button
              variant="ghost"
              size="sm"
              aria-label={captured ? "Release the pointer" : "Capture the pointer"}
              className="px-1.5"
              onClick={toggleCapture}
            >
              {captured ? <MousePointerBan aria-hidden size={16} /> : <MousePointer2 aria-hidden size={16} />}
            </Button>
          </Tooltip>
          <Tooltip label={`${FIT_NAMES[fit]} — change how the picture fits`}>
            <Button
              variant="ghost"
              size="sm"
              aria-label={`Change how the picture fits. Now: ${FIT_NAMES[fit].toLowerCase()}`}
              className="px-1.5"
              onClick={nextFit}
            >
              <Ratio aria-hidden size={16} />
            </Button>
          </Tooltip>
          <Tooltip label={full ? "Leave full screen" : "Full screen"}>
            <Button
              variant="ghost"
              size="sm"
              aria-label={full ? "Leave full screen" : "Full screen"}
              className="px-1.5"
              onClick={toggleFull}
            >
              {full ? <Minimize2 aria-hidden size={16} /> : <Maximize2 aria-hidden size={16} />}
            </Button>
          </Tooltip>
        </div>
      </SessionChrome>

      <Presence />
    </SessionRoot>
  );
}
