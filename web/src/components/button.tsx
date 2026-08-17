import { Slot } from "@radix-ui/react-slot";
import type { ButtonHTMLAttributes } from "react";
import { cn } from "@/lib/cn";

/**
 * The button.
 *
 * `primary` is an inverse fill — the text colour as a background, the surface
 * as text — because the accent is not a colour. There is no coloured variant
 * for emphasis; the one thing a view is about is simply the brightest thing on
 * it. `danger` is the only tinted button, and it is tinted because destroying
 * something deserves a warning, not a highlight.
 *
 * Feedback is `--dur-fast` on background and border only. Nothing scales,
 * nothing moves: press feedback must be felt, not watched, and inside a
 * session `--dur-fast` is the ceiling anyway.
 */
export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";
export type ButtonSize = "sm" | "md" | "lg";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  /** Render the child as the button (an anchor, a router link). */
  asChild?: boolean;
}

const variants: Record<ButtonVariant, string> = {
  primary: "bg-accent text-accent-fg hover:opacity-90 active:opacity-80",
  secondary:
    "bg-surface-2 text-text border border-border-strong hover:bg-surface-1 active:bg-surface-0",
  ghost: "bg-transparent text-text-2 hover:bg-surface-2 hover:text-text active:bg-surface-1",
  danger: "bg-danger text-danger-fg hover:opacity-90 active:opacity-80",
};

const sizes: Record<ButtonSize, string> = {
  sm: "h-[var(--row-h)] px-2 text-sm rounded-sm gap-1.5",
  md: "h-[var(--control-h)] px-3 text-sm rounded-sm gap-2",
  lg: "h-9 px-4 text-base rounded-md gap-2",
};

export function Button({
  variant = "secondary",
  size = "md",
  asChild = false,
  className,
  type,
  ...props
}: ButtonProps) {
  const Comp = asChild ? Slot : "button";
  return (
    <Comp
      data-tap-min=""
      type={asChild ? undefined : type ?? "button"}
      className={cn(
        "inline-flex cursor-pointer select-none items-center justify-center whitespace-nowrap font-medium",
        "transition-[background-color,border-color,opacity] duration-[var(--dur-fast)] ease-[var(--ease)]",
        "disabled:cursor-default disabled:pointer-events-none disabled:text-text-disabled disabled:bg-surface-1 disabled:border-border",
        variants[variant],
        sizes[size],
        className,
      )}
      {...props}
    />
  );
}
