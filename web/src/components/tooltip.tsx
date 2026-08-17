import * as RTt from "@radix-ui/react-tooltip";
import type { ReactNode } from "react";
import { cn } from "@/lib/cn";

export const TooltipProvider = RTt.Provider;

/**
 * A tooltip is *the text label an icon owes*. Every icon has a text label
 * somewhere; where there is no room beside it, this is where the label lives,
 * and it is also the `aria-label`.
 */
export function Tooltip({ label, children, side = "top" }: { label: ReactNode; children: ReactNode; side?: RTt.TooltipContentProps["side"] }) {
  return (
    <RTt.Root delayDuration={300}>
      <RTt.Trigger asChild>{children}</RTt.Trigger>
      <RTt.Portal>
        <RTt.Content
          side={side}
          sideOffset={6}
          className={cn(
            "z-[var(--z-popover)] rounded-sm border border-border bg-surface-2 px-2 py-1 text-xs text-text shadow-[var(--shadow-1)]",
            "data-[state=delayed-open]:animate-[fade-in_var(--dur-fast)_var(--ease-out)]",
          )}
        >
          {label}
        </RTt.Content>
      </RTt.Portal>
    </RTt.Root>
  );
}
