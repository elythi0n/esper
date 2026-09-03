import { Maximize2, Minimize2, Monitor, MousePointer2, MousePointerBan, Ratio, Settings2, TrendingDown } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/button";
import { MenuUnder, type MenuAction } from "@/components/menu";
import { Tooltip } from "@/components/tooltip";
import { Figure } from "@/screens/parts";
import { Presence, SessionChrome, SessionRoot } from "@/session/chrome";
import { type Fit, FIT_NAMES, FITS } from "@/session/fitting";
import { showScreens, wantQuality, type Hand } from "@/session/hand";
import { RemoteScreen } from "@/session/RemoteScreen";
import { askState, narrowing } from "@/session/narrowing";
import { IDLE, type ViewerState } from "@/session/video";

/** How often to ask the window to put a session back while there is no picture. */
const ASK_AGAIN_MS = 5_000;

/**
 * How long an ask may go unanswered before it stops being a pending one.
 *
 * A switch costs a round trip, a decision at the host and then a keyframe, so
 * this is generous on purpose: an ask reported as refused while it was merely
 * in flight would be worse than one that took a moment.
 */
const ASK_IS_LATE_MS = 4_000;

/**
 * What a person can ask a session to be.
 *
 * The numbers are the viewer stream's own — see `stream.ts` — and the pairing
 * of a profile with a sharpness is the same statement said twice, once as
 * content and once as a consequence, which is how `esper connect` says it
 * too. "No preference" is a real answer and not the middle rung: it leaves
 * the host's own judgement alone.
 */
const PROFILES: readonly { label: string; profile: number; sharpness: number }[] = [
  { label: "Reading and typing — sharp", profile: 1, sharpness: 1 },
  { label: "Balanced", profile: 0, sharpness: 0 },
  { label: "Video and motion — smooth", profile: 2, sharpness: 3 },
];

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
    What asks the session for a screen or a quality.

    A ref rather than state because it is the same object for the life of a
    connection and nothing renders differently for *which* hand it is; what
    the render does need is whether there is one, which is `drivable`.
  */
  const hand = useRef<Hand | null>(null);
  const [drivable, setDrivable] = useState(false);

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
    One screen at a time from this menu.

    The protocol carries a set and the host can serve several at once, which
    is what `--monitor 0,1` asks for. A menu of single screens is what a
    person picking one from a list means, and a multi-select over somebody
    else's desktop is a control for a later day rather than one to guess at
    now.
  */
  const screens: MenuAction[] = picture.screens.map((screen) => ({
    label:
      `${screen.name}${screen.primary ? " (primary)" : ""} — ${screen.width}x${screen.height}` +
      (picture.showing.includes(screen.id) ? " · showing" : ""),
    icon: Monitor,
    disabled: !drivable,
    onSelect: () => {
      setAsked(screen.id);
      hand.current?.ask(showScreens([screen.id]));
    },
  }));

  const narrowed = narrowing(picture.askedFps, picture.askedLongEdge);

  /*
    **What was asked for, said here before the far machine says anything.**

    A monitor switch is a round trip and then a keyframe, and a person who
    clicks and sees nothing clicks again. So the ask is acknowledged where it
    was made: the control says what it is asking for the moment it is asked,
    and stops saying it when the session shows that thing — or when it has
    waited long enough that the honest answer is that the host kept what it
    had.

    Cleared by *agreement*, not by a timer alone: the timer only turns a
    pending ask into a plain one, so a slow host is never reported as a
    refusal it did not give.
  */
  const [asked, setAsked] = useState<number | null>(null);
  const [late, setLate] = useState(false);
  const stands = askState(asked, picture.showing, late);
  useEffect(() => {
    if (asked === null) return undefined;
    setLate(false);
    const timer = setTimeout(() => setLate(true), ASK_IS_LATE_MS);
    return () => clearTimeout(timer);
  }, [asked]);

  const qualities: MenuAction[] = PROFILES.map(({ label, profile, sharpness }) => ({
    label,
    icon: Settings2,
    disabled: !drivable,
    onSelect: () => hand.current?.ask(wantQuality(profile, sharpness)),
  }));

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
      <RemoteScreen
        device={name}
        onState={setPicture}
        fit={fit}
        surfaceRef={surface}
        onHand={(h) => {
          hand.current = h;
          // The controls are disabled while there is nothing to ask with, so
          // the hand arriving has to reach the render.
          setDrivable(h !== null);
        }}
      />

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
        {/*
          What has been given up, and why.

          Degradation is otherwise invisible: the only fact on screen is a
          picture that has gone soft, and somebody looking at that cannot tell
          a link that is struggling from a host that is — or from a quality
          somebody chose. It says the number in force and the reason in one
          line, and it is not there at all while nothing has been given up.
        */}
        {/*
          The ask, acknowledged where it was made. It names the screen rather
          than saying "working", because a person who asked for one of three
          screens wants to know which one is coming.
        */}
        {stands !== "settled" && (
          <span role="status" data-asking="" className="shrink-0 text-xs text-text-3">
            {stands === "kept"
              ? "The host kept the screen it had"
              : `Asking for ${picture.screens.find((s) => s.id === asked)?.name ?? "another screen"}`}
          </span>
        )}
        {narrowed && (
          <Tooltip label="The link could not carry what the host was sending, so this session asked for less">
            <span
              role="status"
              data-narrowed=""
              className="flex shrink-0 items-center gap-1.5 text-xs text-text-3"
            >
              <TrendingDown aria-hidden size={14} />
              <span>{narrowed}</span>
            </span>
          </Tooltip>
        )}
        <div className="flex shrink-0 items-center gap-1">
          {/*
            The host's screens, from the host.

            Disabled until the engine has said what there is — and it says so
            on the picture stream, which is the only place a window learns it.
            A switcher built from anything else would be a second idea of
            what is plugged in to somebody else's machine.
          */}
          <MenuUnder actions={screens}>
            <Button
              variant="ghost"
              size="sm"
              aria-label="Switch screen"
              className="px-1.5"
              disabled={screens.length === 0}
            >
              <Monitor aria-hidden size={16} />
            </Button>
          </MenuUnder>
          <MenuUnder actions={qualities}>
            <Button
              variant="ghost"
              size="sm"
              aria-label="Quality"
              className="px-1.5"
              disabled={hand.current === null}
            >
              <Settings2 aria-hidden size={16} />
            </Button>
          </MenuUnder>
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
