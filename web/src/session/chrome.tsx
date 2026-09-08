import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type HTMLAttributes,
  type PointerEvent,
  type ReactNode,
  type Ref,
} from "react";
import { cn } from "@/lib/cn";
import type { LinkState } from "./video";
import { durationMs } from "@/lib/theme";

/**
 * The session layer is a theme, not a set of exceptions.
 *
 * `data-session` on the root does three things through the tokens and none
 * through the components: the value layer's `:root, [data-session]` selector
 * re-declares the dark palette on it, so everything inside is dark whatever
 * the document theme is; themes.css caps `--dur-base` and `--dur-slow` at
 * `--dur-fast` and zeroes `--dur-video`; and the chrome parks itself on the
 * `--z-session-*` layers. A component inside a session is the same component
 * as outside — it reads the same tokens and gets different answers.
 */

interface ChromeState {
  hidden: boolean;
  show: () => void;
  hide: () => void;
}

const Ctx = createContext<ChromeState | null>(null);

/** How close to an edge the pointer must come to bring the chrome back, in px. */
const EDGE = 8;

export interface SessionRootProps extends HTMLAttributes<HTMLDivElement> {
  children: ReactNode;
  ref?: Ref<HTMLDivElement> | undefined;
}

/**
 * The root of a live session. Owns the auto-hide clock: the chrome retracts
 * `--session-chrome-hide` after the last interaction and comes back when the
 * pointer reaches the top or bottom edge, or focus lands inside it. Moving
 * over the video does not bring it back — that is the one gesture the person
 * makes constantly, and chrome that answered it would never be gone.
 *
 * Either edge, because chrome sits at either: the viewer's HUD is along the
 * bottom and the specimen's bar is across the top, and `hidden` is one state
 * for all of it.
 *
 * `contain: paint` makes this the containing block for the presence
 * indicator's fixed position, so in a preview it stays inside the box; in
 * the real viewer the root is the viewport and nothing changes.
 */
export function SessionRoot({ children, className, onPointerMove, onPointerLeave, ref: outerRef, ...props }: SessionRootProps) {
  const ref = useRef<HTMLDivElement>(null);
  const [hidden, setHidden] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  const arm = useCallback(() => {
    clearTimeout(timer.current);
    const ms = ref.current ? durationMs("--session-chrome-hide", ref.current) : 0;
    if (ms > 0) timer.current = setTimeout(() => setHidden(true), ms);
  }, []);

  const show = useCallback(() => {
    setHidden(false);
    arm();
  }, [arm]);

  const hide = useCallback(() => {
    clearTimeout(timer.current);
    setHidden(true);
  }, []);

  useEffect(() => {
    arm();
    return () => clearTimeout(timer.current);
  }, [arm]);

  const move = (e: PointerEvent<HTMLDivElement>) => {
    onPointerMove?.(e);
    const box = ref.current?.getBoundingClientRect();
    if (!box) return;
    if (e.clientY - box.top <= EDGE || box.bottom - e.clientY <= EDGE) show();
  };

  return (
    <Ctx.Provider value={{ hidden, show, hide }}>
      <div
        ref={(el) => {
          ref.current = el;
          if (typeof outerRef === "function") outerRef(el);
          else if (outerRef) outerRef.current = el;
        }}
        data-session=""
        onPointerMove={move}
        onPointerLeave={onPointerLeave}
        className={cn("relative overflow-hidden bg-video text-text [contain:paint]", className)}
        {...props}
      >
        {children}
      </div>
    </Ctx.Provider>
  );
}

export function useSessionChrome(): ChromeState {
  const s = useContext(Ctx);
  if (!s) throw new Error("useSessionChrome needs a <SessionRoot> above it");
  return s;
}

/**
 * A bar along an edge of a session. Retracts the way it came over
 * `--dur-base`, which the session theme has already capped at `--dur-fast`.
 * Pointer over it or focus inside it keeps it, so every control on it has a
 * keyboard path: Tab into it and it is there.
 *
 * `edge` is where it sits and therefore which way it leaves. The viewer's HUD
 * is along the bottom and the design system's specimen is across the top, and
 * both are the same component — a second one would be a second set of rules
 * about chrome over somebody's desktop.
 */
export function SessionChrome({
  children,
  className,
  edge = "top",
  ...props
}: HTMLAttributes<HTMLDivElement> & { edge?: "top" | "bottom" }) {
  const { hidden, show } = useSessionChrome();
  const top = edge === "top";
  return (
    <div
      role="toolbar"
      aria-label="Session"
      aria-hidden={hidden}
      onPointerMove={show}
      onFocusCapture={show}
      className={cn(
        "absolute inset-x-0 z-[var(--z-session-chrome)] flex items-center gap-3 bg-surface-0/90 px-4",
        top ? "top-0 h-[var(--topbar-h)] border-b border-border" : "bottom-0 border-t border-border py-2",
        "transition-[translate,opacity] duration-[var(--dur-base)] ease-[var(--ease)]",
        hidden && "pointer-events-none opacity-0",
        hidden && (top ? "-translate-y-full" : "translate-y-full"),
        className,
      )}
      {...props}
    >
      {children}
    </div>
  );
}

/**
 * The host-side presence indicator: "this screen is being watched". Its
 * styles are in presence.css and held to presence.contract.ts by a script.
 * There is nothing to configure here on purpose.
 */
export function Presence({ label = "Screen shared" }: { label?: string | undefined }) {
  return (
    <div className="presence" role="status">
      {label}
    </div>
  );
}

/**
 * The video surface. No transition, ever — not on resize, not on a monitor
 * switch, not on reconnect. `--dur-video` is 0 and this sets it anyway, so a
 * transition utility added by mistake still resolves to nothing.
 */
export function VideoSurface({
  className,
  children,
  ref,
  ...props
}: HTMLAttributes<HTMLDivElement> & { ref?: Ref<HTMLDivElement> | undefined }) {
  return (
    <div
      ref={ref}
      className={cn("absolute inset-0 grid place-items-center bg-video [transition-duration:var(--dur-video)]", className)}
      {...props}
    >
      {children}
    </div>
  );
}

/**
 * What the picture path is doing. The same three words the surface reports —
 * see `ViewerState["link"]` — because a screen and the stream feeding it
 * describing one state in two vocabularies is how they come to disagree.
 */
export type ReconnectState = LinkState;

/**
 * Reconnect states its state. No pulse, no spinner, no breathing: a sentence
 * in the middle of the frame, and a count of how long, updating in place.
 */
export function ReconnectNotice({ state, sinceMs }: { state: ReconnectState; sinceMs: number }) {
  if (state === "live") return null;
  const s = Math.floor(sinceMs / 1000);
  return (
    <div role="status" className="rounded-md border border-border bg-surface-1 px-4 py-3 text-center text-sm text-text">
      <div className="font-medium">{state === "reconnecting" ? "Connection interrupted" : "Connection lost"}</div>
      <div className="text-text-2 tabular-nums">
        {state === "reconnecting" ? `Reconnecting — ${s}s` : "Esper will reconnect if the machine comes back."}
      </div>
    </div>
  );
}
