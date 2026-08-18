import { Maximize2, Monitor, Settings2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/button";
import { Tooltip } from "@/components/tooltip";
import { durationMs } from "@/lib/theme";
import { Figure } from "@/screens/parts";
import { Presence } from "@/session/chrome";
import { RemoteScreen } from "@/session/RemoteScreen";
import { IDLE, type ViewerState } from "@/session/video";
import { cn } from "@/lib/cn";

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
  const [hidden, setHidden] = useState(false);
  const [picture, setPicture] = useState<ViewerState>(IDLE);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  const arm = () => {
    clearTimeout(timer.current);
    const ms = root.current ? durationMs("--session-chrome-hide", root.current) : 0;
    if (ms > 0) timer.current = setTimeout(() => setHidden(true), ms);
  };

  useEffect(() => {
    arm();
    return () => clearTimeout(timer.current);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div
      ref={root}
      className="relative h-full w-full overflow-hidden bg-video [contain:paint]"
      onPointerMove={() => {
        setHidden(false);
        arm();
      }}
    >
      <RemoteScreen device={name} onState={setPicture} />

      {/* The HUD, over the picture, retracting. */}
      <div
        aria-hidden={hidden}
        className={cn(
          "absolute inset-x-0 bottom-0 z-[var(--z-session-hud)] flex items-center gap-4 px-4 py-2",
          "bg-surface-0/85 transition-[translate,opacity] duration-[var(--dur-base)] ease-[var(--ease)]",
          hidden && "pointer-events-none translate-y-full opacity-0",
        )}
        onFocusCapture={() => setHidden(false)}
      >
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
          <Tooltip label="Full screen">
            <Button variant="ghost" size="sm" aria-label="Full screen" className="px-1.5" disabled>
              <Maximize2 aria-hidden size={16} />
            </Button>
          </Tooltip>
        </div>
      </div>

      <Presence />
    </div>
  );
}
