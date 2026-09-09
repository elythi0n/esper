import { Maximize2, Minimize2, Monitor, MousePointer2, MousePointerBan, Ratio, Settings2, TrendingDown } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/button";
import { TierBadge } from "@/components/badge";
import { SessionHUD } from "@/components/hud";
import { MenuUnder, type MenuAction } from "@/components/menu";
import { MonitorSwitcher } from "@/components/monitor-switcher";
import { Popover } from "@/components/popover";
import { Tooltip } from "@/components/tooltip";
import { Figure } from "@/screens/parts";
import { SessionChrome, SessionRoot } from "@/session/chrome";
import { type Fit, FIT_NAMES, FITS } from "@/session/fitting";
import { showScreens, wantQuality, type Hand } from "@/session/hand";
import { RemoteScreen } from "@/session/RemoteScreen";
import { askState, narrowing } from "@/session/narrowing";
import { IDLE, type ViewerState } from "@/session/video";

/*
    **The presence badge is not drawn here, and that is the point of it.**

    It is the *host-side* session badge — a product surface with a security
    function, says the design system, and the function is that the person whose
    screen is being shared can see that it is. This is the other end: the
    window of the person doing the watching, over a picture of somebody else's
    machine. A badge here said "Screen shared" about a machine that was sharing
    nothing, in the exact shape and place the host's own indicator takes, which
    weakens the one badge that has to mean something.

    The real one is drawn by the engine on the host, natively — see
    `esper-presence-x11` and `esper-presence-wlr` — because a badge that a
    webview draws is a badge that closing a webview removes.

    What a controller needs is to know which machine it is looking at, and the
    chrome says that by name.
*/

/** How often to ask the window to put a session back while there is no picture. */
const ASK_AGAIN_MS = 5_000;

/** How often to ask the window what it is sending. */
const TRANSFERS_EVERY_MS = 1_000;

/** A file dropped on this session, as the window reports it. */
interface Transfer {
  id: number;
  name: string;
  state: "sending" | "sent" | "failed";
  trouble?: string;
}

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
    One screen at a time.

    The protocol carries a set and the host can serve several at once, which
    is what `--monitor 0,1` asks for. One screen is what a person picking from
    a list of screens means, and a multi-select over somebody else's desktop is
    a control for a later day rather than one to guess at now.
  */
  const screens = picture.screens.map((screen) => ({
    id: String(screen.id),
    label: screen.name,
    width: screen.width,
    height: screen.height,
    primary: screen.primary,
  }));

  const pickScreen = (id: string) => {
    const chosen = Number(id);
    if (!Number.isInteger(chosen)) return;
    setAsked(chosen);
    hand.current?.ask(showScreens([chosen]));
  };

  const narrowed = narrowing(picture.askedFps, picture.askedLongEdge);

  /*
    What has been dropped on this session and sent to the far machine.

    Polled rather than pushed: a transfer is a rare thing a person does
    deliberately, and a second channel from the window to the page — an event
    stream for a list that is empty almost always — would be a second thing to
    connect and a second thing to fail. Polled *only while a session is on
    screen*, and slowly.
  */
  const [sending, setSending] = useState<Transfer[]>([]);
  useEffect(() => {
    let live = true;
    const look = () => {
      fetch("/shell/transfers")
        .then((r) => (r.ok ? r.json() : null))
        .then((body: { transfers?: Transfer[] } | null) => {
          if (live && body?.transfers) setSending(body.transfers);
        })
        .catch(() => {
          // A tab, or a window without the route. Nothing to add.
        });
    };
    look();
    const timer = setInterval(look, TRANSFERS_EVERY_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, []);

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
      {/*
        **The surface is what a file may be dropped on, and it says so in
        CSS.** The window reads this property to decide whether a drop is for
        the product or for the webview — see `drop.go` — so the declaration is
        the whole of the page's part in it. Nothing else in the window is a
        target, because nothing else has a machine on the other end of it.
      */}
      <style>{`[data-session] canvas { --wails-drop-target: drop; }`}</style>
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
        **The readouts, in the corner, as the design system drew them.**

        A panel rather than more numbers on the bar: `SessionHUD` was built in
        Phase 10 for exactly this and reached no screen, so this window grew a
        smaller readout of its own and the design ended up with two versions of
        one thing. It goes with the chrome, because it is over somebody's
        desktop and the rule about chrome over a desktop is not about which
        element it is.

        Only where there is something to say: a loopback reports no path, and a
        panel of zeroes teaches people to stop reading it.
      */}
      {picture.path?.real && (
        <div
          data-stats=""
          className="absolute bottom-[calc(var(--topbar-h)+0.75rem)] left-3 z-[var(--z-session-hud)]"
        >
          <SessionHUD
            className="bg-surface-0/90"
            stats={{
              rttMs: picture.path.rttMs,
              bitrateKbps: picture.path.kbps,
              fps: picture.fps,
              encoder: picture.codec ?? "—",
              tier: picture.path.tier === "unknown" ? "direct" : picture.path.tier,
              degraded: picture.path.degraded,
              ...(picture.hopMs === null ? {} : { hopMs: picture.hopMs }),
            }}
          />
        </div>
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
        {/*
          How the far machine was reached, which is the one fact on this bar
          with a privacy consequence: `relayed` means the bytes cross a machine
          that is not an endpoint, and a person is entitled to see that without
          looking for it. Not drawn until the engine has said — an unknown
          badge on every session would teach people to ignore it.
        */}
        {picture.path?.real && picture.path.tier !== "unknown" && (
          <TierBadge tier={picture.path.tier} degraded={picture.path.degraded} className="shrink-0" />
        )}
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
        {/*
          What is on its way, and what happened to it. Said in the HUD rather
          than over the picture: a transfer is not about the screen, and a
          panel over somebody's desktop for a file that is already moving
          would be chrome in the way of the thing it is reporting.
        */}
        {sending.length > 0 && (
          <span role="status" data-transfers="" className="min-w-0 shrink-0 truncate text-xs text-text-3">
            {sending
              .map((t) =>
                t.state === "sending" ? `Sending ${t.name}` : t.state === "sent" ? `Sent ${t.name}` : `${t.name}: ${t.trouble}`,
              )
              .join(" · ")}
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
            The host's screens, from the host, drawn the way the design system
            draws them: each option at its **own aspect ratio** with its size
            beside it, because "Monitor 2" tells nobody which one that is. A
            text menu here was a second version of a control that already
            existed — `MonitorSwitcher`, built in Phase 10, reaching no screen.

            In a popover rather than a menu, because it is a panel: a menu
            dismisses itself on the first click, and a radio group inside one
            would let somebody change exactly one thing.

            Disabled until the engine has said what there is; a switcher built
            from anything else would be a second idea of what is plugged in to
            somebody else's machine.
          */}
          <Popover
            label="Switch screen"
            side="top"
            trigger={
              <Button
                variant="ghost"
                size="sm"
                aria-label="Switch screen"
                className="px-1.5"
                disabled={screens.length === 0 || !drivable}
              >
                <Monitor aria-hidden size={16} />
              </Button>
            }
          >
            <MonitorSwitcher monitors={screens} value={String(picture.showing[0] ?? "")} onChange={pickScreen} />
          </Popover>
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
    </SessionRoot>
  );
}
