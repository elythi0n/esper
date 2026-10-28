import { Globe, Lock, Route, TriangleAlert, Wifi } from "lucide-react";
import type { HTMLAttributes } from "react";
import { cn } from "@/lib/cn";

export type Tone = "neutral" | "ok" | "warn" | "danger" | "info" | "signal";

const tones: Record<Tone, string> = {
  neutral: "bg-text/5 text-text-2 border-border",
  ok: "bg-ok-soft text-ok border-ok/0 [border-width:var(--soft-border-width)] [border-color:currentColor]",
  warn: "bg-warn-soft text-warn [border-width:var(--soft-border-width)] [border-color:currentColor]",
  danger: "bg-danger-soft text-danger [border-width:var(--soft-border-width)] [border-color:currentColor]",
  info: "bg-info-soft text-info [border-width:var(--soft-border-width)] [border-color:currentColor]",
  // The signal is a *fill*, never a tint: it is the one element on a view that
  // deserves attention, and a tint would make it one of five quiet things.
  signal: "bg-signal text-signal-fg border-transparent font-medium",
};

export interface BadgeProps extends HTMLAttributes<HTMLSpanElement> {
  tone?: Tone;
}

/** A small label. Tones are tints with the tone's own text on them; in high contrast the tint is a border. */
export function Badge({ tone = "neutral", className, ...props }: BadgeProps) {
  return (
    <span
      className={cn(
        "inline-flex h-5 items-center gap-1 rounded-full border px-2 text-xs",
        tones[tone],
        className,
      )}
      {...props}
    />
  );
}

/**
 * How a session reaches the other machine.
 *
 * Icon and word first; colour drawn from tokens that already exist. LAN,
 * private and direct are not different *statuses* — all three are fine — so
 * they carry the same weight and are told apart by icon and label. Relayed is
 * the one state where bytes cross a machine that is not an endpoint, and the
 * person is entitled to see that without looking for it: it takes the signal.
 * Degraded takes `--warn`. Nothing here invents a hue.
 *
 * Never colour alone. The word is always present, because the one piece of
 * information with a privacy consequence must not depend on telling two shades
 * apart.
 */
export type Tier = "lan" | "private" | "direct" | "relayed";

const tierWord: Record<Tier, string> = {
  lan: "On this network",
  private: "Private network",
  direct: "Direct",
  relayed: "Relayed",
};

const tierIcon: Record<Tier, typeof Wifi> = {
  lan: Wifi,
  private: Lock,
  direct: Globe,
  relayed: Route,
};

export interface TierBadgeProps extends HTMLAttributes<HTMLSpanElement> {
  tier: Tier;
  /** Software encode, heavy loss or buffering. Shown beside the tier, in `--warn`. */
  degraded?: boolean;
}

export function TierBadge({ tier, degraded = false, className, ...props }: TierBadgeProps) {
  const Icon = tierIcon[tier];
  return (
    <span className={cn("inline-flex items-center gap-2", className)} {...props}>
      <Badge tone={tier === "relayed" ? "signal" : "neutral"}>
        <Icon aria-hidden size={12} strokeWidth={1.75} />
        {tierWord[tier]}
      </Badge>
      {degraded && (
        // Relayed outranks degraded for the signal — one signal per view — so
        // this is text-level warn and never a second fill.
        <span className="inline-flex items-center gap-1 text-xs text-warn">
          <TriangleAlert aria-hidden size={12} strokeWidth={1.75} />
          Degraded
        </span>
      )}
    </span>
  );
}
