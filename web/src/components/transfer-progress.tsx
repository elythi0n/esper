import { ArrowDown, ArrowUp, X } from "lucide-react";
import type { HTMLAttributes } from "react";
import { Button } from "./button";
import { cn } from "@/lib/cn";

export type TransferState = "running" | "done" | "failed" | "cancelled";

export interface Transfer {
  id: string;
  name: string;
  bytes: number;
  total: number;
  /** Bytes per second, recent. */
  rate?: number | undefined;
  direction: "send" | "receive";
  state: TransferState;
  /** What to do about a failure, in a sentence. */
  error?: string | undefined;
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

function eta(t: Transfer): string | undefined {
  if (!t.rate || t.state !== "running") return undefined;
  const s = Math.ceil((t.total - t.bytes) / t.rate);
  if (s < 60) return `${s}s left`;
  if (s < 3600) return `${Math.ceil(s / 60)} min left`;
  return `${(s / 3600).toFixed(1)} h left`;
}

const stateWord: Record<TransferState, string> = { running: "", done: "Done", failed: "Failed", cancelled: "Stopped" };

export interface TransferProgressProps extends HTMLAttributes<HTMLDivElement> {
  transfer: Transfer;
  onCancel?: ((t: Transfer) => void) | undefined;
}

/**
 * One file, moving. Per-file and cancellable, because a transfer a person
 * cannot stop is a transfer they did not consent to all of. The bar has no
 * transition: it is a measurement, and it is drawn where the bytes are.
 */
export function TransferProgress({ transfer: t, onCancel, className, ...props }: TransferProgressProps) {
  const pct = t.total > 0 ? Math.min(100, (t.bytes / t.total) * 100) : 0;
  const Dir = t.direction === "send" ? ArrowUp : ArrowDown;
  const left = eta(t);
  return (
    <div className={cn("flex flex-col gap-2 rounded-md border border-border bg-surface-1 p-3 text-sm", className)} {...props}>
      <div className="flex items-center gap-2">
        <Dir aria-label={t.direction === "send" ? "Sending" : "Receiving"} size={14} className="shrink-0 text-text-3" />
        <span className="min-w-0 flex-1 truncate font-medium text-text">{t.name}</span>
        <span className={cn("text-xs tabular-nums", t.state === "failed" ? "text-danger" : "text-text-2")}>
          {t.state === "running" ? `${formatBytes(t.bytes)} of ${formatBytes(t.total)}` : stateWord[t.state]}
        </span>
        {t.state === "running" && onCancel && (
          <Button variant="ghost" size="sm" aria-label={`Stop ${t.name}`} className="-my-1 px-1.5" onClick={() => onCancel(t)}>
            <X aria-hidden size={14} />
          </Button>
        )}
      </div>
      <div
        role="progressbar"
        aria-label={t.name}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(pct)}
        className="h-1 overflow-hidden rounded-full bg-surface-2"
      >
        <div
          className={cn("h-full [transition:none]", t.state === "failed" ? "bg-danger" : t.state === "cancelled" ? "bg-text-3" : "bg-accent")}
          style={{ width: `${pct}%` }}
        />
      </div>
      {(left || t.error || t.rate) && (
        <div className="flex justify-between text-xs text-text-3 tabular-nums">
          <span className={cn(t.error && "text-danger")}>{t.error ?? (t.rate && t.state === "running" ? `${formatBytes(t.rate)}/s` : "")}</span>
          <span>{left}</span>
        </div>
      )}
    </div>
  );
}
