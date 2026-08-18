import type { HTMLAttributes, ReactNode } from "react";
import { TierBadge, type Tier } from "./badge";
import { cn } from "@/lib/cn";

/**
 * The HUD never animates its numbers. RTT, bitrate and fps are read at a
 * glance while somebody is doing something else, and a figure counting up to
 * its value is, for the whole count, a figure that is wrong. Everything here
 * updates in place and declares no transition.
 */

export type StatTone = "neutral" | "warn" | "danger";

export interface StatTileProps extends HTMLAttributes<HTMLDivElement> {
  label: string;
  value: ReactNode;
  unit?: string | undefined;
  tone?: StatTone | undefined;
  /** A word beside a toned value, so the tone is never the only signal. */
  note?: string | undefined;
}

const toneText: Record<StatTone, string> = { neutral: "text-text", warn: "text-warn", danger: "text-danger" };

export function StatTile({ label, value, unit, tone = "neutral", note, className, ...props }: StatTileProps) {
  return (
    <div className={cn("flex min-w-16 flex-col gap-0.5 [transition:none]", className)} {...props}>
      <span className="font-mono text-xs uppercase tracking-[0.06em] text-text-3">{label}</span>
      <span className={cn("font-mono text-base tabular-nums [transition:none]", toneText[tone])}>
        {value}
        {unit && <span className="ml-0.5 text-xs text-text-3">{unit}</span>}
      </span>
      {note && <span className={cn("text-xs", toneText[tone])}>{note}</span>}
    </div>
  );
}

export interface SparklineProps {
  values: readonly number[];
  /** The top of the scale. Defaults to the largest value present. */
  max?: number | undefined;
  label: string;
  className?: string | undefined;
}

/** A line of recent values. Draws the current data and nothing between frames. */
export function Sparkline({ values, max, label, className }: SparklineProps) {
  const w = 100;
  const h = 24;
  const top = max ?? Math.max(1, ...values);
  const step = values.length > 1 ? w / (values.length - 1) : w;
  const points = values
    .map((v, i) => `${(i * step).toFixed(1)},${(h - 1 - (Math.min(v, top) / top) * (h - 2)).toFixed(1)}`)
    .join(" ");
  return (
    <svg role="img" aria-label={label} viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" className={cn("h-6 w-24 text-text-2", className)}>
      <polyline fill="none" stroke="currentColor" strokeWidth={1.5} vectorEffect="non-scaling-stroke" points={points} />
    </svg>
  );
}

export interface SessionStats {
  rttMs: number;
  bitrateKbps: number;
  fps: number;
  encoder: string;
  tier: Tier;
  degraded?: boolean | undefined;
  rttHistory?: readonly number[] | undefined;
  fpsHistory?: readonly number[] | undefined;
  /**
   * The hop from the engine to this canvas, in milliseconds.
   *
   * Distinct from `rttMs`, which is the link. This is what the *window* adds:
   * the socket, the decoder and the paint. Shown beside the round trip rather
   * than added to it, because two numbers that are measured differently
   * should not be summed into a third that is measured neither way.
   */
  hopMs?: number | undefined;
}

export function formatBitrate(kbps: number): string {
  return kbps >= 1000 ? `${(kbps / 1000).toFixed(1)} Mbit/s` : `${Math.round(kbps)} kbit/s`;
}

/**
 * How the window's own hop reads.
 *
 * The bars are not the link's. A hop is a socket, a decode and a paint on one
 * machine: a hundred milliseconds of *that* is a window that is struggling,
 * where a hundred milliseconds of round trip is an ordinary continent.
 */
function hopTone(ms: number): { tone: StatTone; note?: string } {
  if (ms >= 100) return { tone: "danger", note: "the window is behind" };
  if (ms >= 50) return { tone: "warn", note: "slow" };
  return { tone: "neutral" };
}

function rttTone(ms: number): { tone: StatTone; note?: string } {
  if (ms >= 400) return { tone: "danger", note: "very high" };
  if (ms >= 150) return { tone: "warn", note: "high" };
  return { tone: "neutral" };
}

/** RTT, bitrate, fps, encoder and tier, in one row, for the session chrome. */
export function SessionHUD({ stats, className, ...props }: { stats: SessionStats } & HTMLAttributes<HTMLDivElement>) {
  const rtt = rttTone(stats.rttMs);
  return (
    <div
      role="group"
      aria-label="Session statistics"
      className={cn("flex flex-wrap items-end gap-x-6 gap-y-3 rounded-md border border-border bg-surface-1 px-4 py-3", className)}
      {...props}
    >
      <StatTile label="rtt" value={Math.round(stats.rttMs)} unit="ms" tone={rtt.tone} note={rtt.note} />
      {stats.rttHistory && <Sparkline values={stats.rttHistory} label="Round-trip time, recent" className="mb-1" />}
      <StatTile label="rate" value={formatBitrate(stats.bitrateKbps)} />
      <StatTile label="fps" value={Math.round(stats.fps)} />
      {stats.hopMs !== undefined && (
        <StatTile label="hop" value={Math.round(stats.hopMs)} unit="ms" tone={hopTone(stats.hopMs).tone} note={hopTone(stats.hopMs).note} />
      )}
      {stats.fpsHistory && <Sparkline values={stats.fpsHistory} max={60} label="Frames per second, recent" className="mb-1" />}
      <StatTile label="enc" value={stats.encoder} />
      <div className="mb-0.5 ml-auto">
        <TierBadge tier={stats.tier} degraded={stats.degraded ?? false} />
      </div>
    </div>
  );
}
