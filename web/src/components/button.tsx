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
  primary: "bg-accent font-semibold text-accent-fg hover:opacity-90 active:opacity-80",
  secondary:
    "bg-transparent text-text border border-border-strong hover:bg-text/5 active:bg-text/10",
  ghost: "bg-transparent text-text-2 hover:bg-text/5 hover:text-text active:bg-text/10",
  danger: "bg-danger font-semibold text-danger-fg hover:opacity-90 active:opacity-80",
};

const sizes: Record<ButtonSize, string> = {
  sm: "h-[var(--row-h)] px-2.5 text-sm rounded-sm gap-1.5",
  md: "h-8 px-3.5 text-sm rounded-md gap-2",
  lg: "h-10 px-5 text-base rounded-md gap-2",
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
        "disabled:cursor-default disabled:pointer-events-none disabled:text-text-disabled disabled:bg-text/5 disabled:border-transparent",
        variants[variant],
        sizes[size],
        className,
      )}
      {...props}
    />
  );
}
