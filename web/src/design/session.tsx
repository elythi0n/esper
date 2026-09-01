import { Maximize2, Monitor, Settings2, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { TierBadge } from "@/components/badge";
import { Button } from "@/components/button";
import { SessionHUD } from "@/components/hud";
import { Tooltip } from "@/components/tooltip";
import { Presence, ReconnectNotice, SessionChrome, SessionRoot, useSessionChrome, VideoSurface, type ReconnectState } from "@/session/chrome";
import { RemoteScreen } from "@/session/RemoteScreen";
import { presence } from "@/session/presence.contract";
import { Demo, Section, Sub, TokenName, useTokens } from "./ui";

const durations = ["--dur-base", "--dur-slow", "--dur-video", "--session-chrome-hide"] as const;

/**
 * What `bg-surface-1` and `text-text` paint on an element, read back from the
 * browser. Custom properties are inherited already resolved, so reading
 * `--color-*` inside the session would show the document's value; what the
 * utilities paint is the honest measurement, and it is the one that matters.
 */
function usePainted(ref: React.RefObject<HTMLElement | null>): { bg: string; fg: string } {
  const [v, set] = useState({ bg: "", fg: "" });
  useEffect(() => {
    const read = () => {
      if (!ref.current) return;
      const cs = getComputedStyle(ref.current);
      set({ bg: cs.backgroundColor, fg: cs.color });
    };
    read();
    const mo = new MutationObserver(read);
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    return () => mo.disconnect();
  }, [ref]);
  return v;
}

function ChromeButtons() {
  const { show } = useSessionChrome();
  return (
    <div className="absolute bottom-3 right-3">
      <Button size="sm" onClick={show}>Bring the chrome back</Button>
    </div>
  );
}

export function SessionSection() {
  const outside = useTokens(durations);
  const sessionRef = useRef<HTMLDivElement>(null);
  const inside = useTokens(durations, sessionRef);
  const outerProbe = useRef<HTMLSpanElement>(null);
  const innerProbe = useRef<HTMLSpanElement>(null);
  const outerPaint = usePainted(outerProbe);
  const innerPaint = usePainted(innerProbe);
  const [reconnect, setReconnect] = useState<ReconnectState>("live");

  return (
    <Section
      id="session"
      title="The session layer"
      lede="A theme, not a set of exceptions. data-session on the root re-declares the dark values, caps every duration at --dur-fast, zeroes the video surface's transition and parks the chrome above everything else. Switch the theme above: the box below does not change."
    >
      <Sub
        title="The picture"
        about="Where the remote screen is drawn: the engine's own encoded frames over a loopback socket, decoded by the window's own VideoDecoder and painted on a canvas. No WebRTC and no new crates — the measurement that settled it found 151 frames a second at 1920×1080 with every frame drawn, on software rendering. Here there is no session, so it shows the other half of its job: the command that starts one, with this window's socket already in it."
      >
        <div className="relative h-[14rem] overflow-hidden rounded-lg border border-border" data-session="">
          {/* Not drivable: a specimen that took the keyboard would be a
              gate page that swallows Tab. */}
          <RemoteScreen device="workstation" drivable={false} />
        </div>
      </Sub>

      <Sub title="Live" about="Auto-hiding chrome, the HUD, the presence indicator, and a video surface that swaps frames. The chrome retracts after --session-chrome-hide and comes back at the top edge or on focus, so every control on it has a keyboard path.">
        <SessionRoot ref={sessionRef} className="h-[28rem] rounded-lg border border-border">
          <VideoSurface>
            {reconnect === "live" ? (
              <span className="font-mono text-xs text-text-3">2560 × 1440 · 60 fps · the picture goes here</span>
            ) : (
              <ReconnectNotice state={reconnect} sinceMs={7000} />
            )}
          </VideoSurface>
          <SessionChrome>
            <span className="text-sm font-medium">workstation</span>
            <TierBadge tier="lan" />
            <div className="ml-auto flex items-center gap-1">
              <Tooltip label="Switch screen"><Button variant="ghost" size="sm" aria-label="Switch screen" className="px-1.5"><Monitor aria-hidden size={16} /></Button></Tooltip>
              <Tooltip label="Quality"><Button variant="ghost" size="sm" aria-label="Quality" className="px-1.5"><Settings2 aria-hidden size={16} /></Button></Tooltip>
              <Tooltip label="Full screen"><Button variant="ghost" size="sm" aria-label="Full screen" className="px-1.5"><Maximize2 aria-hidden size={16} /></Button></Tooltip>
              <Tooltip label="End session"><Button variant="ghost" size="sm" aria-label="End session" className="px-1.5"><X aria-hidden size={16} /></Button></Tooltip>
            </div>
          </SessionChrome>
          <div className="absolute bottom-3 left-3 z-[var(--z-session-hud)]">
            <SessionHUD stats={{ rttMs: 18, bitrateKbps: 4200, fps: 60, hopMs: 2, encoder: "h264 sw", tier: "lan" }} className="bg-surface-0/90" />
          </div>
          <Presence />
          <ChromeButtons />
          <span ref={innerProbe} aria-hidden className="absolute bg-surface-1 text-text opacity-0" />
        </SessionRoot>
        <span ref={outerProbe} aria-hidden className="absolute bg-surface-1 text-text opacity-0" />
        <Demo className="justify-between">
          <span className="text-sm text-text-2">Reconnect states its state — no pulse, no spinner, no breathing.</span>
          <div className="flex gap-2">
            {(["live", "reconnecting", "lost"] as const).map((s) => (
              <Button key={s} size="sm" variant={reconnect === s ? "primary" : "secondary"} onClick={() => setReconnect(s)}>{s}</Button>
            ))}
          </div>
        </Demo>
      </Sub>

      <Sub title="The same utilities, different answers" about="Measured from the browser, outside and inside the session root above, right now. A component inside a session is the same component as outside — it reads the same tokens and gets different answers.">
        <table className="w-full border-collapse text-sm">
          <thead className="text-left text-xs text-text-3">
            <tr className="h-[var(--row-h)]"><th className="pr-4 font-medium">Utility or token</th><th className="pr-4 font-medium">Outside</th><th className="font-medium">Inside a session</th></tr>
          </thead>
          <tbody className="[&_tr]:border-t [&_tr]:border-border">
            <tr className="h-[var(--row-h)]">
              <td className="pr-4"><TokenName>bg-surface-1</TokenName></td>
              <td className="pr-4 font-mono text-xs text-text-2">{outerPaint.bg}</td>
              <td className="font-mono text-xs text-text">{innerPaint.bg}</td>
            </tr>
            <tr className="h-[var(--row-h)]">
              <td className="pr-4"><TokenName>text-text</TokenName></td>
              <td className="pr-4 font-mono text-xs text-text-2">{outerPaint.fg}</td>
              <td className="font-mono text-xs text-text">{innerPaint.fg}</td>
            </tr>
            {durations.map((t) => (
              <tr key={t} className="h-[var(--row-h)]">
                <td className="pr-4"><TokenName>{t}</TokenName></td>
                <td className="pr-4 font-mono text-xs tabular-nums text-text-2">{outside[t]}</td>
                <td className="font-mono text-xs tabular-nums text-text">{inside[t]}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </Sub>

      <Sub title="The presence indicator's contract" about="The host-side badge that says this screen is being watched gets rules, not styling. These are read from presence.contract.ts; scripts/check-presence.mjs holds presence.css to them and the engine's own indicator carries the same numbers.">
        <Demo className="items-stretch">
          <div className="relative grid h-24 min-w-48 flex-1 place-items-center rounded-sm bg-video [contain:paint]"><Presence /></div>
          <div className="relative grid h-24 min-w-48 flex-1 place-items-center rounded-sm bg-surface-2 [contain:paint]"><Presence /></div>
          <div className="relative grid h-24 min-w-48 flex-1 place-items-center rounded-sm bg-text [contain:paint]"><Presence /></div>
        </Demo>
        <ul className="grid gap-x-8 gap-y-1 text-sm sm:grid-cols-2">
          <li className="flex justify-between border-b border-border py-1.5"><span className="text-text-2">Minimum size</span><span className="font-mono text-xs">{presence.minSizePx}px</span></li>
          <li className="flex justify-between border-b border-border py-1.5"><span className="text-text-2">Ring</span><span className="font-mono text-xs">{presence.ringPx}px · {presence.ring}</span></li>
          <li className="flex justify-between border-b border-border py-1.5"><span className="text-text-2">Fill</span><span className="font-mono text-xs">{presence.fill}</span></li>
          <li className="flex justify-between border-b border-border py-1.5"><span className="text-text-2">Contrast against video</span><span className="font-mono text-xs">≥ {presence.minContrastAgainstVideo}:1 over {presence.videoWorstCases.length} worst cases</span></li>
          <li className="flex justify-between border-b border-border py-1.5"><span className="text-text-2">Themeable</span><span className="font-mono text-xs">{String(presence.themeable)}</span></li>
          <li className="flex justify-between border-b border-border py-1.5"><span className="text-text-2">Animated</span><span className="font-mono text-xs">{String(presence.animated)} — even with reduced motion off</span></li>
          <li className="flex justify-between border-b border-border py-1.5"><span className="text-text-2">Opacity</span><span className="font-mono text-xs">{presence.opacity}</span></li>
          <li className="flex justify-between border-b border-border py-1.5"><span className="text-text-2">Position</span><span className="font-mono text-xs">{presence.position}, top trailing corner</span></li>
        </ul>
      </Sub>
    </Section>
  );
}
