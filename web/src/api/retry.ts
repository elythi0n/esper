/**
 * How long to wait before trying something again.
 *
 * # Why this is its own module
 *
 * Because a retry that is written inline is a retry nobody measures. The one
 * thing that matters about a schedule is its shape — that it starts short
 * enough to be invisible when a control plane restarts, that it grows, and
 * that it stops growing somewhere a person would still call it "keeps
 * trying". None of those can be read off a `setTimeout` call, and all three
 * can be checked here.
 *
 * The client that uses it cannot be imported outside a bundler — it reads
 * `import.meta.env` — so the part worth checking lives where a check can
 * reach it.
 */

/** The first wait, short enough that a restart looks like nothing happened. */
export const FIRST_WAIT_MS = 1000;

/**
 * The longest wait.
 *
 * Half a minute: long enough that a window left open against a machine that
 * is never coming back is not asking all day, short enough that somebody who
 * fixes the thing sees the window notice while they are still looking at it.
 */
export const LONGEST_WAIT_MS = 30_000;

/**
 * The wait after this one.
 *
 * Doubling, and capped. A schedule that only doubled would be asking once an
 * hour by the morning, which is a window that has effectively given up
 * without saying so.
 */
export function nextWait(waited: number): number {
  if (!Number.isFinite(waited) || waited < FIRST_WAIT_MS) return FIRST_WAIT_MS;
  return Math.min(waited * 2, LONGEST_WAIT_MS);
}

/**
 * Every wait a schedule produces, up to a count. For reading and for checking.
 */
export function waits(count: number): number[] {
  const out: number[] = [];
  let wait = FIRST_WAIT_MS;
  for (let i = 0; i < count; i += 1) {
    out.push(wait);
    wait = nextWait(wait);
  }
  return out;
}
