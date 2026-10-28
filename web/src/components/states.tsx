import type { LucideIcon } from "lucide-react";
import type { HTMLAttributes, ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * A placeholder the shape of what is coming, so nothing pops.
 *
 * It does not shimmer. A shimmer is an animation on the reading critical path
 * — the person is waiting to read this — and the never-animate list is
 * explicit about that. A still block in the muted tone says "loading" well
 * enough, and says it without asking the eye to track a highlight.
 */
export function Skeleton({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div aria-hidden className={cn("rounded-sm bg-surface-2", className)} {...props} />;
}

export interface EmptyStateProps extends Omit<HTMLAttributes<HTMLDivElement>, "title"> {
  /** Drawn at a hairline weight, so it reads as a mark rather than a control. */
  icon?: LucideIcon | undefined;
  /** What is missing. Often unnecessary: the sentence usually says it. */
  title?: ReactNode;
  /** What to do about it, in one sentence. */
  children?: ReactNode;
  action?: ReactNode;
}

/**
 * A list with nothing in it, saying so and saying what to do next.
 *
 * # There were two of these
 *
 * This component and a `Nothing` in the screens' own vocabulary, and **every
 * real screen used the other one** — the three empty lists in the product
 * imported `Nothing` while the design system's `EmptyState` was rendered
 * nowhere but the gate. That is the exact failure the inventory's
 * not-building list argues against, arrived at from the other direction: not
 * a second component somebody asked for, but a first one that quietly stopped
 * being the answer. The two even disagreed about what an empty state looks
 * like — a dashed box against nothing at all — and the doc had already
 * written down the screens' version.
 *
 * So: one component, the treatment the screens proved they wanted, under the
 * name the inventory and the gate already use.
 *
 * # Centred, and no box
 *
 * Left-aligned text under a heading reads as a paragraph somebody forgot to
 * finish. A screen with nothing on it should look composed, because it is a
 * state this product is often in — a fresh install has no devices and nothing
 * has happened on it. The dashed border went with the box: a row list draws
 * hairlines, and a dashed rectangle in the middle of one is a placeholder
 * graphic, not a state.
 */
export function EmptyState({ icon: Icon, title, children, action, className, ...props }: EmptyStateProps) {
  return (
    <div
      className={cn("flex flex-1 flex-col items-center justify-center gap-5 py-20 text-center", className)}
      {...props}
    >
      {/* The mark sits in the tile a device row's icon sits in, so an empty list looks like the list it will be. */}
      {Icon && (
        <span
          aria-hidden
          className="flex size-14 items-center justify-center rounded-xl border border-border bg-text/[0.04] text-text-3"
        >
          <Icon size={22} strokeWidth={1.5} />
        </span>
      )}
      <div className="flex max-w-[var(--measure-short)] flex-col gap-1.5">
        {title && <p className="font-display text-lg font-medium text-text">{title}</p>}
        {children && <p className="text-sm text-text-2">{children}</p>}
      </div>
      {action}
    </div>
  );
}
