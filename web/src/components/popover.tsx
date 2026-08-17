import * as DM from "@radix-ui/react-dropdown-menu";
import type { ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * A small panel from a control, for a choice that should not take the screen.
 *
 * # What it is for
 *
 * The quality profile, mid-session, is the case the inventory named: a
 * session is somebody else's screen and a dialog over it is a dialog over the
 * thing they came to look at. The same shape serves anything with two or
 * three settings that belong to one control — never a form, which is a
 * dialog's job, and never a message, which is a `Callout`.
 *
 * # Why this is a dropdown underneath
 *
 * `@radix-ui/react-popover` exists and would be the obvious answer. It is
 * also a second floating primitive, with a second set of collision, focus and
 * portal behaviours to keep in step with the first — and the difference
 * between it and a menu, for this product's uses, is that the content is
 * arbitrary rather than a list of items. So this is the menu primitive with
 * its item semantics removed: one focus trap, one z-index, one animation, one
 * thing to get right.
 *
 * `onSelect` is stopped on the content, because a menu closes when an item is
 * chosen and a panel of controls must not close when a radio inside it is
 * clicked. That single line is the whole of the adaptation, and it is the
 * reason this is a component rather than a pattern somebody re-derives.
 */

export interface PopoverProps {
  /** The control it hangs from. */
  trigger: ReactNode;
  children: ReactNode;
  /** What it is, for the control that opens it. */
  label: string;
  side?: "top" | "right" | "bottom" | "left";
  align?: "start" | "center" | "end";
  className?: string | undefined;
}

export function Popover({ trigger, children, label, side = "bottom", align = "end", className }: PopoverProps) {
  return (
    <DM.Root>
      <DM.Trigger asChild aria-label={label}>
        {trigger}
      </DM.Trigger>
      <DM.Portal>
        <DM.Content
          side={side}
          align={align}
          sideOffset={6}
          collisionPadding={8}
          // **A panel is not a list.** A menu dismisses itself when an item is
          // chosen; a panel of controls that closed on the first click would
          // let somebody change exactly one thing.
          onSelect={(e) => e.preventDefault()}
          className={cn(
            "z-[var(--z-popover)] min-w-56 max-w-80 rounded-md border border-border bg-surface-2 p-3",
            "shadow-[var(--shadow-2)] data-[state=open]:animate-[overlay-in_var(--dur-base)_var(--ease-out)]",
            className,
          )}
        >
          {children}
        </DM.Content>
      </DM.Portal>
    </DM.Root>
  );
}
