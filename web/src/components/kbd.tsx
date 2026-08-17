import { cn } from "@/lib/cn";

/**
 * A key, as it is printed.
 *
 * # Why one component for eleven characters of markup
 *
 * Because there were four of them, and they had already drifted: a 3px
 * radius and a 4px radius, `px-1` and `px-1.5`, one with `text-text-3` and
 * one inheriting. A key cap is a thing somebody recognises by its shape, and
 * four shapes is the shape meaning nothing.
 *
 * # The chord is split here rather than by the caller
 *
 * `Ctrl K` is two caps and `Drag a tab` is three words that are not keys at
 * all — and every caller that split on a space got the first right and the
 * second wrong, printing `Drag`, `a`, `tab` as three keys. So the split lives
 * in one place, with the rule: a part longer than four characters is a word
 * and is set as one.
 */

export interface KbdProps {
  /** One key, or a chord separated by spaces: `Ctrl K`, `⌘ ,`, `F11`. */
  keys: string;
  className?: string | undefined;
}

/** A part this long is prose, not a key. `Escape` and `Enter` are keys; `Drag` is not. */
const LONGEST_KEY = 6;

export function Kbd({ keys, className }: KbdProps) {
  const parts = keys.split(" ").filter(Boolean);
  const asWords = parts.some((p) => p.length > LONGEST_KEY);

  if (asWords) {
    // Not a chord. Printing "Drag a tab" as three caps says there is a key
    // marked `a`, which there is not.
    return <span className={cn("shrink-0 text-xs text-text-3", className)}>{keys}</span>;
  }

  return (
    <span className={cn("flex shrink-0 items-center gap-1", className)}>
      {parts.map((part, i) => (
        <kbd
          key={`${part}-${i}`}
          className="rounded-[3px] border border-border bg-surface-0 px-1.5 py-0.5 font-mono text-xs text-text-2"
        >
          {part}
        </kbd>
      ))}
    </span>
  );
}
