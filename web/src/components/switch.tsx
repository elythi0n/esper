import * as RC from "@radix-ui/react-checkbox";
import * as RSw from "@radix-ui/react-switch";
import { Check, Minus } from "lucide-react";
import { cn } from "@/lib/cn";

/**
 * Switch and checkbox. Both are found by their strong border, both are the
 * inverse fill when on — the accent is the text colour — and both carry the
 * 44px tap minimum however small they are drawn.
 */
export function Switch({ className, ...props }: RSw.SwitchProps) {
  return (
    <RSw.Root
      data-tap-min=""
      className={cn(
        "relative inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full border border-border-strong bg-surface-0",
        "transition-[background-color,border-color] duration-[var(--dur-fast)] ease-[var(--ease)]",
        "data-[state=checked]:border-accent data-[state=checked]:bg-accent",
        "disabled:border-border disabled:bg-surface-1",
        className,
      )}
      {...props}
    >
      <RSw.Thumb
        className={cn(
          "block size-3.5 translate-x-0.5 rounded-full bg-text-2",
          "transition-[translate,background-color] duration-[var(--dur-fast)] ease-[var(--ease)]",
          "data-[state=checked]:translate-x-[18px] data-[state=checked]:bg-accent-fg",
        )}
      />
    </RSw.Root>
  );
}

export function Checkbox({ className, ...props }: RC.CheckboxProps) {
  return (
    <RC.Root
      data-tap-min=""
      className={cn(
        "inline-flex size-4 shrink-0 cursor-pointer items-center justify-center rounded-[3px] border border-border-strong bg-surface-0",
        "transition-[background-color,border-color] duration-[var(--dur-fast)] ease-[var(--ease)]",
        "data-[state=checked]:border-accent data-[state=checked]:bg-accent data-[state=indeterminate]:border-accent data-[state=indeterminate]:bg-accent",
        "disabled:border-border disabled:bg-surface-1",
        className,
      )}
      {...props}
    >
      <RC.Indicator className="text-accent-fg">
        {props.checked === "indeterminate" ? <Minus aria-hidden size={12} strokeWidth={3} /> : <Check aria-hidden size={12} strokeWidth={3} />}
      </RC.Indicator>
    </RC.Root>
  );
}
