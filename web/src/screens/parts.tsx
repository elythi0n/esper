import type { HTMLAttributes, ReactNode } from "react";
import { cn } from "@/lib/cn";

/*
  The instrument-panel vocabulary the product screens are built from.

  Three rules, and they are why these exist rather than each screen deciding:
  a **key** is mono, uppercase, widely tracked and small; a **figure** is mono
  and tabular and never animates; and a **row** is a hairline, not a box. Boxes
  around everything is how a dense screen becomes a scrapbook.
*/

/** A field name. Small, mono, tracked — read as a label, never as content. */
export function Key({ children, className }: { children: ReactNode; className?: string | undefined }) {
  return (
    <span className={cn("font-mono text-xs uppercase tracking-[0.14em] text-text-3", className)}>{children}</span>
  );
}

/** A number and its unit. Tabular, so a column of them lines up, and still. */
export function Figure({
  value,
  unit,
  className,
}: {
  value: ReactNode;
  unit?: string | undefined;
  className?: string | undefined;
}) {
  return (
    <span className={cn("font-mono tabular-nums text-text [transition:none]", className)}>
      {value}
      {unit && <span className="ml-0.5 text-text-3">{unit}</span>}
    </span>
  );
}

/** A key above its value, for a panel of facts. */
export function Fact({ label, children, className }: { label: string; children: ReactNode; className?: string | undefined }) {
  return (
    <div className={cn("flex min-w-0 flex-col gap-1", className)}>
      <Key>{label}</Key>
      <div className="min-w-0 break-words text-sm text-text">{children}</div>
    </div>
  );
}

/** A hairline row. The list primitive: no border, no radius, no fill at rest. */
export function Row({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn(
        "group relative flex items-center gap-4 border-b border-border py-3",
        "transition-[background-color] duration-[var(--dur-fast)] ease-[var(--ease)]",
        className,
      )}
      {...props}
    />
  );
}

/**
 * A fingerprint, in groups of four.
 *
 * The one string in this product two people read to each other, so it is set
 * the way it is spoken and never wrapped mid-group.
 */
export function Fingerprint({ hex, className }: { hex: string; className?: string | undefined }) {
  const groups = hex.replace(/\s+/g, "").match(/.{1,4}/g) ?? [hex];
  return (
    <span className={cn("inline-flex flex-wrap gap-x-2 font-mono text-xs tabular-nums text-text-2", className)}>
      {groups.map((g, i) => (
        <span key={`${g}-${i}`}>{g}</span>
      ))}
    </span>
  );
}

/** "42s", "5h", "2d" — the shortest true thing, for a column that is scanned. */
export function shortAgo(seconds: number | null): string {
  if (seconds === null) return "—";
  const s = Math.max(0, Math.round(Date.now() / 1000) - seconds);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) return `${Math.round(s / 3600)}h`;
  return `${Math.round(s / 86400)}d`;
}

/**
 * A section heading inside a screen. A key and a count, and nothing else —
 * a screen that explains itself in a paragraph is a screen that needs fewer
 * things on it.
 */
export function Heading({ children, count }: { children: ReactNode; count?: ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-4 pb-2">
      <Key>{children}</Key>
      {count !== undefined && <Figure className="text-xs text-text-3" value={count} />}
    </div>
  );
}

/**
 * A state that is not the happy one, said in one line.
 *
 * Errors say what to do and nothing else. The API's own sentence goes
 * underneath in mono, because it names the file or the address and that is
 * what somebody acts on.
 */
export function Trouble({ children, detail, action }: { children: ReactNode; detail?: string | undefined; action?: ReactNode }) {
  return (
    <div className="flex flex-col items-start gap-3 border-l-2 border-warn py-2 pl-4">
      <p className="text-sm text-text">{children}</p>
      {detail && <p className="max-w-[var(--measure)] font-mono text-xs leading-relaxed text-text-3">{detail}</p>}
      {action}
    </div>
  );
}
