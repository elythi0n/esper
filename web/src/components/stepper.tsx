import { Check } from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * A short sequence with a visible end.
 *
 * # What it is for, and the one rule it enforces
 *
 * First run: identity, a name, the host on or off. Three things, once, in an
 * order — which is the only case in this product that earns a stepper at all.
 * Everything else is a screen somebody returns to, and a wizard around a
 * screen you revisit is a screen you have to escape.
 *
 * The rule it enforces is that **every step is visible from every step**. A
 * sequence that shows only where you are is a sequence with an unknown end,
 * and the first thing anybody wants to know about a setup flow is how much of
 * it there is. So all of them are drawn, with the ones behind you marked done
 * and reachable — going back is how somebody checks what they just agreed to.
 *
 * # What it deliberately does not do
 *
 * It does not hold the answers. Steps are content and the caller owns its own
 * state, because a stepper that also managed a form would be a form library
 * — and this product's first run is three questions, not a form.
 *
 * It does not block on validity either. A step that cannot be left is a dead
 * end when the thing it wants is unavailable — a machine with no service
 * manager cannot turn the host on, and a first run that insisted would be a
 * first run nobody can finish.
 */

export interface Step {
  id: string;
  label: string;
  /** One line under the heading. What this step is for, not how to use it. */
  about?: ReactNode;
  content: ReactNode;
}

export interface StepperProps {
  steps: readonly Step[];
  /** Which step is showing. Controlled: the caller owns the sequence. */
  at: string;
  onGo: (id: string) => void;
  /** The last step's own action — "Finish", or whatever it really does. */
  done?: ReactNode;
  className?: string | undefined;
}

export function Stepper({ steps, at, onGo, done, className }: StepperProps) {
  const index = Math.max(
    0,
    steps.findIndex((s) => s.id === at),
  );
  const step = steps[index];
  if (!step) return null;

  return (
    <div className={cn("flex flex-col gap-8", className)}>
      {/*
        Every step, always. A rail that showed only the current one would hide
        the answer to the first question anybody has about a setup flow: how
        much of this is there.
      */}
      <ol className="flex flex-wrap items-center gap-1" aria-label="Steps">
        {steps.map((s, i) => {
          const behind = i < index;
          const here = i === index;
          return (
            <li key={s.id} className="flex items-center gap-1">
              <button
                type="button"
                // Backwards only. Forwards is what the step's own action is
                // for — a rail that skipped ahead would let somebody past a
                // question the next step depends on.
                disabled={!behind && !here}
                onClick={() => behind && onGo(s.id)}
                aria-current={here ? "step" : undefined}
                className={cn(
                  "flex h-7 items-center gap-2 rounded-sm px-2 text-sm outline-none",
                  "transition-colors duration-[var(--dur-fast)]",
                  here && "bg-surface-2 text-text",
                  behind && "cursor-pointer text-text-2 hover:text-text",
                  !behind && !here && "cursor-default text-text-disabled",
                )}
              >
                <span
                  aria-hidden
                  className={cn(
                    "flex size-4 shrink-0 items-center justify-center rounded-full text-[10px] tabular-nums",
                    behind && "bg-ok text-ok-fg",
                    here && "bg-accent text-accent-fg",
                    !behind && !here && "border border-border text-text-3",
                  )}
                >
                  {behind ? <Check size={10} strokeWidth={3} /> : i + 1}
                </span>
                {s.label}
              </button>
              {i < steps.length - 1 && (
                <span aria-hidden className="h-px w-4 shrink-0 bg-border" />
              )}
            </li>
          );
        })}
      </ol>

      <div className="flex flex-col gap-4">
        <div className="flex flex-col gap-1">
          <h2 className="text-lg font-medium leading-tight text-text">{step.label}</h2>
          {step.about && <p className="max-w-[var(--measure)] text-sm text-text-2">{step.about}</p>}
        </div>
        {step.content}
      </div>

      <div className="flex items-center gap-2">
        {index > 0 && (
          <button
            type="button"
            onClick={() => onGo(steps[index - 1]?.id ?? at)}
            className="cursor-pointer rounded-sm px-2 py-1 text-sm text-text-3 outline-none transition-colors duration-[var(--dur-fast)] hover:text-text"
          >
            Back
          </button>
        )}
        {index < steps.length - 1 ? (
          <button
            type="button"
            onClick={() => onGo(steps[index + 1]?.id ?? at)}
            className={cn(
              "cursor-pointer rounded-sm border border-transparent bg-accent px-3 py-1.5 text-sm text-accent-fg outline-none",
              "transition-[filter] duration-[var(--dur-fast)] hover:brightness-110",
            )}
          >
            Next
          </button>
        ) : (
          done
        )}
      </div>
    </div>
  );
}
