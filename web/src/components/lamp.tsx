import type { ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * A lamp: a small dot, and the word it is about.
 *
 * # Why the word is not optional
 *
 * "No colour-only meaning" is a rule the baseline states and nothing enforced
 * — and it was already broken in five places by the same six-pixel dot,
 * written out five times: the status strip's three lamps, a capability row, a
 * permission, a device that is on this network. Each one happened to have a
 * word beside it, by the care of whoever wrote it.
 *
 * So the dot and the word are one component and `children` is required. A
 * lamp on its own does not typecheck, which is the only kind of rule that
 * survives somebody in a hurry.
 *
 * # Three states, and `off` is not a colour
 *
 * `ok` and `warn` are the semantic tones. `off` is a **hollow ring** rather
 * than a grey fill, because "this is not on" is different in kind from "this
 * is on and fine" — a filled dot in a muted colour is read as a third status
 * whose meaning has to be learned, and at 6px a muted fill and a warn fill
 * are the same dot to anybody not looking for the difference.
 */
export type LampState = "ok" | "warn" | "off";

export interface LampProps {
  state: LampState;
  /** The word. Required: a lamp with no word is colour alone. */
  children: ReactNode;
  className?: string | undefined;
  /** Where the word beside it is a label and the sentence does not fit. */
  title?: string | undefined;
}

const FILL: Record<LampState, string> = {
  ok: "bg-ok",
  warn: "bg-warn",
  off: "border border-border-strong",
};

export function Lamp({ state, children, className, title }: LampProps) {
  return (
    <span title={title} className={cn("inline-flex min-w-0 items-center gap-1.5", className)}>
      <span aria-hidden className={cn("size-1.5 shrink-0 rounded-full", FILL[state])} />
      {children}
    </span>
  );
}

/** The common case: a capability either is or is not. */
export function lampFor(available: boolean): LampState {
  return available ? "ok" : "warn";
}
