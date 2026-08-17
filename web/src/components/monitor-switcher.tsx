import { useRef, type KeyboardEvent } from "react";
import { cn } from "@/lib/cn";

export interface MonitorInfo {
  id: string;
  label: string;
  width: number;
  height: number;
  primary?: boolean | undefined;
}

export const ALL_SCREENS = "all";

export interface MonitorSwitcherProps {
  monitors: readonly MonitorInfo[];
  value: string;
  onChange: (id: string) => void;
  /** Offer every screen at once as a tiled picture. */
  allowAll?: boolean | undefined;
  className?: string | undefined;
}

/**
 * Which screen of a many-screen host to show. Each option is drawn at its own
 * aspect ratio, so a portrait screen looks like one, and each names its size,
 * because "Monitor 2" tells nobody which one that is.
 *
 * A radio group with arrow keys — every session control has a keyboard path.
 * Switching does not animate: the video surface swaps frames, and nothing
 * about this control should suggest otherwise.
 */
export function MonitorSwitcher({ monitors, value, onChange, allowAll = false, className }: MonitorSwitcherProps) {
  const options = allowAll
    ? [{ id: ALL_SCREENS, label: "All screens", width: monitors.reduce((w, m) => w + m.width, 0), height: Math.max(...monitors.map((m) => m.height)) }, ...monitors]
    : [...monitors];
  const refs = useRef<Array<HTMLButtonElement | null>>([]);

  const onKey = (e: KeyboardEvent, i: number) => {
    const delta = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
    if (!delta) return;
    e.preventDefault();
    const next = options[(i + delta + options.length) % options.length];
    if (!next) return;
    onChange(next.id);
    refs.current[options.indexOf(next)]?.focus();
  };

  return (
    <div role="radiogroup" aria-label="Screen" className={cn("flex flex-wrap gap-2", className)}>
      {options.map((m, i) => {
        const on = m.id === value;
        return (
          <button
            key={m.id}
            ref={(el) => {
              refs.current[i] = el;
            }}
            type="button"
            role="radio"
            aria-checked={on}
            tabIndex={on ? 0 : -1}
            onClick={() => onChange(m.id)}
            onKeyDown={(e) => onKey(e, i)}
            className={cn(
              "flex cursor-pointer flex-col items-start gap-1.5 rounded-sm border p-2 text-left",
              "transition-[border-color,background-color] duration-[var(--dur-fast)] ease-[var(--ease)]",
              on ? "border-accent bg-surface-2" : "border-border-strong bg-surface-0 hover:bg-surface-1",
            )}
          >
            <span
              aria-hidden
              style={{ aspectRatio: `${m.width} / ${m.height}` }}
              className={cn("h-7 rounded-[2px]", on ? "bg-accent" : "bg-surface-2 border border-border-strong")}
            />
            <span className="text-xs text-text">
              {m.label}
              {"primary" in m && m.primary && <span className="text-text-3"> · primary</span>}
            </span>
            <span className="font-mono text-xs tabular-nums text-text-3">
              {m.width} × {m.height}
            </span>
          </button>
        );
      })}
    </div>
  );
}
