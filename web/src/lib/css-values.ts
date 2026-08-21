/**
 * Reading a CSS token's value as a number.
 *
 * # Why these are apart from the lookup
 *
 * `getComputedStyle` needs a document, so a parser welded to it can only be
 * checked in a browser — and what a browser check cannot see is the *order of
 * the tests below*, which is the whole of the risk here. Both of these are
 * four lines and both have one way to be badly wrong.
 */

/**
 * A CSS duration, in milliseconds. Zero for anything this cannot read.
 *
 * **`ms` is tested before `s`, and that is not a style choice**: `"200ms"`
 * ends with `"s"`, so a version that asked the shorter question first would
 * read every duration in the product as a thousand times itself — a 200ms
 * transition becoming 200 seconds, in timers that have to agree with the CSS.
 * Nothing about the result would look like a parse error.
 *
 * Zero for a token that is not set, which a caller treats as "use my own
 * default" rather than as an instant.
 */
export function millisecondsIn(raw: string): number {
  const value = raw.trim();
  if (value.endsWith("ms")) return Number.parseFloat(value) || 0;
  if (value.endsWith("s")) return (Number.parseFloat(value) || 0) * 1000;
  return 0;
}

/**
 * A CSS length, in pixels. Zero for anything this cannot read.
 *
 * A token given in any other unit is a token this cannot use, and guessing at
 * a conversion — `rem` against a root size that may itself be a token — would
 * be worse than saying so.
 */
export function pixelsIn(raw: string): number {
  const value = raw.trim();
  if (value.endsWith("px")) return Number.parseFloat(value) || 0;
  return 0;
}
