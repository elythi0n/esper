import { useEffect, useState } from "react";

/**
 * "3 minutes ago", updating in place.
 *
 * In place, with no transition: a time is content on the reading path, and
 * the never-animate list is explicit. The element re-renders on a schedule
 * that matches how fast the words can change — every second under a minute,
 * every minute under an hour, and then every hour — so it neither drifts nor
 * wakes the tab up for nothing.
 */
export function timeAgo(then: Date | number, now = Date.now()): string {
  const t = typeof then === "number" ? then : then.getTime();
  const s = Math.max(0, Math.round((now - t) / 1000));
  if (s < 5) return "just now";
  if (s < 60) return `${s} seconds ago`;
  const m = Math.round(s / 60);
  if (m < 60) return m === 1 ? "a minute ago" : `${m} minutes ago`;
  const h = Math.round(m / 60);
  if (h < 24) return h === 1 ? "an hour ago" : `${h} hours ago`;
  const d = Math.round(h / 24);
  return d === 1 ? "yesterday" : `${d} days ago`;
}

function nextTick(then: number, now: number): number {
  const s = (now - then) / 1000;
  if (s < 60) return 1000;
  if (s < 3600) return 60_000;
  return 3_600_000;
}

export function TimeAgo({ date, className }: { date: Date | number; className?: string }) {
  const then = typeof date === "number" ? date : date.getTime();
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const id = setTimeout(() => setNow(Date.now()), nextTick(then, now));
    return () => clearTimeout(id);
  }, [then, now]);
  return (
    <time dateTime={new Date(then).toISOString()} title={new Date(then).toLocaleString()} className={className}>
      {timeAgo(then, now)}
    </time>
  );
}
