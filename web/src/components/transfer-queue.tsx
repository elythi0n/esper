import { CircleAlert, Check } from "lucide-react";
import { useMemo } from "react";
import { Button } from "./button";
import { formatBytes, TransferProgress, type Transfer } from "./transfer-progress";
import { cn } from "@/lib/cn";

/**
 * Several files at once, each with its own progress and its own failure.
 *
 * # Why a queue is not a list of bars
 *
 * Because the questions change. One file asks "how far along is it"; six ask
 * **"is it going to finish, and did anything fail"** — and a column of six
 * bars answers neither without the reader adding up. So there is a line at
 * the top that says the whole thing in one sentence, and the bars are the
 * detail under it.
 *
 * # The order is not the order they were started
 *
 * What is running comes first, then what failed, then what finished. A
 * failure that scrolled away under nine successes is a failure nobody sees,
 * and a queue that keeps the arrival order makes exactly that happen — the
 * thing that needs a person is the thing that has been there longest.
 *
 * # What it does not do
 *
 * It does not retry. A retry is a decision about a file that may have changed
 * on either side, and a queue that quietly re-sent things would be a queue
 * that surprises somebody. The failure carries its sentence and the screen
 * that owns the transfer offers what to do about it.
 *
 * It does not virtualise either: a transfer queue with a thousand entries is
 * a design problem, not a scrolling problem, and `LogView` is there when a
 * screen genuinely has a record that long.
 */

export interface TransferQueueProps {
  transfers: readonly Transfer[];
  onCancel?: ((t: Transfer) => void) | undefined;
  /** Clears what has finished. Absent where nothing owns that decision. */
  onClearFinished?: (() => void) | undefined;
  className?: string | undefined;
}

/** Running first, then failed, then finished — see the note above. */
const ORDER: Record<Transfer["state"], number> = { running: 0, failed: 1, cancelled: 2, done: 3 };

export function TransferQueue({ transfers, onCancel, onClearFinished, className }: TransferQueueProps) {
  const sorted = useMemo(
    // A copy: sorting the caller's array would reorder whatever else holds it.
    () => [...transfers].sort((a, b) => (ORDER[a.state] ?? 9) - (ORDER[b.state] ?? 9)),
    [transfers],
  );

  const summary = useMemo(() => {
    const running = transfers.filter((t) => t.state === "running");
    const failed = transfers.filter((t) => t.state === "failed");
    const done = transfers.filter((t) => t.state === "done");
    const left = running.reduce((n, t) => n + Math.max(0, t.total - t.bytes), 0);
    // Rates add: two files over one link share it, and what somebody wants to
    // know is when *all* of it will be done.
    const rate = running.reduce((n, t) => n + (t.rate ?? 0), 0);
    return { running, failed, done, left, rate };
  }, [transfers]);

  if (transfers.length === 0) return null;

  const { running, failed, done, left, rate } = summary;
  const seconds = rate > 0 ? Math.ceil(left / rate) : null;

  return (
    <div className={cn("flex min-w-0 flex-col gap-3", className)}>
      {/*
        The whole queue in one line, because six bars do not answer "is this
        going to finish" without the reader adding up.
      */}
      <div className="flex min-w-0 items-baseline gap-3">
        <p className="min-w-0 flex-1 text-sm text-text">
          {running.length > 0 ? (
            <>
              {running.length} of {transfers.length} still moving
              {left > 0 && <span className="text-text-2"> · {formatBytes(left)} to go</span>}
              {seconds !== null && (
                <span className="text-text-2">
                  {" · "}
                  {seconds < 60 ? `${seconds}s left` : `${Math.ceil(seconds / 60)} min left`}
                </span>
              )}
            </>
          ) : failed.length > 0 ? (
            <span className="flex items-center gap-2">
              <CircleAlert aria-hidden size={14} className="shrink-0 text-danger" />
              {failed.length} of {transfers.length} did not finish
            </span>
          ) : (
            <span className="flex items-center gap-2">
              <Check aria-hidden size={14} className="shrink-0 text-ok" />
              {done.length === 1 ? "One file arrived" : `All ${done.length} files arrived`}
            </span>
          )}
        </p>
        {onClearFinished && done.length > 0 && (
          <Button variant="ghost" size="sm" onClick={onClearFinished}>
            Clear finished
          </Button>
        )}
      </div>

      <div className="flex flex-col gap-2">
        {sorted.map((t) => (
          <TransferProgress key={t.id} transfer={t} onCancel={onCancel} />
        ))}
      </div>
    </div>
  );
}
