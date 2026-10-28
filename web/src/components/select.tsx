import * as RS from "@radix-ui/react-select";
import { Check, ChevronDown } from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * Select. Selects and menus are one family: 32px rows, and the check in a
 * fixed left slot so labels do not shift when the selection changes. The
 * content floats on `--z-popover`, above a modal, because a select inside a
 * dialog is the common case and must not be clipped.
 */
export const Select = RS.Root;
export const SelectGroup = RS.Group;
export const SelectValue = RS.Value;

export function SelectTrigger({ className, children, ...props }: RS.SelectTriggerProps) {
  return (
    <RS.Trigger
      data-tap-min=""
      className={cn(
        "inline-flex h-[var(--control-h)] w-full cursor-pointer items-center justify-between gap-2 rounded-sm border border-border-strong bg-surface-0 px-2 text-sm text-text",
        "transition-[border-color,background-color] duration-[var(--dur-fast)] ease-[var(--ease)]",
        "data-[placeholder]:text-text-3 disabled:text-text-disabled disabled:border-border",
        className,
      )}
      {...props}
    >
      {children}
      <RS.Icon asChild>
        <ChevronDown aria-hidden size={14} className="text-text-3" />
      </RS.Icon>
    </RS.Trigger>
  );
}

export function SelectContent({ className, children, ...props }: RS.SelectContentProps) {
  return (
    <RS.Portal>
      <RS.Content
        position="popper"
        sideOffset={4}
        className={cn(
          "z-[var(--z-popover)] min-w-[var(--radix-select-trigger-width)] overflow-hidden rounded-lg border border-border bg-surface-2 p-1 shadow-[var(--shadow-2)]",
          "data-[state=open]:animate-[overlay-in_var(--dur-base)_var(--ease-out)]",
          className,
        )}
        {...props}
      >
        <RS.Viewport>{children}</RS.Viewport>
      </RS.Content>
    </RS.Portal>
  );
}

export function SelectItem({ className, children, ...props }: RS.SelectItemProps & { children: ReactNode }) {
  return (
    <RS.Item
      data-focus-exempt="a listbox moves a highlight rather than focus; data-[highlighted] is the indicator"
      className={cn(
        // 32px rows. The check slot is 20px wide and always present, so a
        // selected label sits exactly where an unselected one did.
        "relative flex h-8 cursor-pointer select-none items-center rounded-sm pl-7 pr-2 text-sm text-text outline-none",
        "data-[highlighted]:bg-surface-1 data-[disabled]:text-text-disabled",
        className,
      )}
      {...props}
    >
      <span className="absolute left-1.5 inline-flex w-4 items-center justify-center">
        <RS.ItemIndicator>
          <Check aria-hidden size={14} />
        </RS.ItemIndicator>
      </span>
      <RS.ItemText>{children}</RS.ItemText>
    </RS.Item>
  );
}

export function SelectLabel({ className, ...props }: RS.SelectLabelProps) {
  return <RS.Label className={cn("px-2 py-1.5 text-xs text-text-3", className)} {...props} />;
}
