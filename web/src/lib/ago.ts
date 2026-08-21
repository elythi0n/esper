/**
 * How long ago, in the shortest true form, for a column that is scanned.
 *
 * # Why this is not in the component that draws it
 *
 * Because it is a rule, and a rule in a `.tsx` file beside a React import is
 * a rule nothing can hold to anything: the checks here run under node, and
 * node cannot import a module that imports React. It lived there, and so the
 * boundaries below — the ones where a number changes unit — had never been
 * read by anything.
 */

/**
 * `"42s"`, `"5h"`, `"2d"`, and `"—"` for a device that has never been seen.
 *
 * The unit changes at the point where the next one starts being the shorter
 * true thing to say: sixty seconds is a minute, sixty minutes an hour, and
 * twenty-four hours a day. A time in the future — a clock that disagrees with
 * the far machine's, which is ordinary — reads as `0s` rather than as a
 * negative age, because "seen −4s ago" is a number nobody can act on.
 */
export function shortAgo(seconds: number | null, now: number = Date.now()): string {
  if (seconds === null) return "—";
  const s = Math.max(0, Math.round(now / 1000) - seconds);
  if (s < 60) return `${s}s`;
  // The unit is chosen *after* rounding, not before it. Choosing first reads
  // 3599 seconds as "60m" and 86,399 as "24h" — both true, and both the
  // longer way to say a thing this column exists to say shortly.
  const minutes = Math.round(s / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(s / 3600);
  if (hours < 24) return `${hours}h`;
  return `${Math.round(s / 86400)}d`;
}
