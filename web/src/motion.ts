// GENERATED from src/tokens/motion.css and src/tokens/semantic.css by
// scripts/gen-motion.mjs. Do not edit: the CSS is the source of truth, and CI
// regenerates this file and fails if it has drifted.
//
// Seconds and bezier arrays, which is what the animation library takes. Use
// these for animations the library drives; CSS transitions read the tokens
// directly and never need this module.

/** Durations, in seconds, by purpose. */
export const duration = {
  instant: 0,
  fast: 0.12,
  base: 0.2,
  slow: 0.32,
  graph: 0.6,
  video: 0,
} as const;

/** Easing curves as cubic-bezier control points. */
export const ease = {
  out: [0.16,1,0.3,1],
  default: [0.25,0.1,0.25,1],
  spring: [0.34,1.4,0.64,1],
} as const;

/** The `arrive` motion: rise on enter, leave upward, in pixels. */
export const arrive = {
  rise: 6,
  leave: 4,
} as const;

/** The `overlay` motion opens from this scale. */
export const overlayFrom = 0.97 as const;

/** How long the session chrome waits before retracting, in seconds. */
export const sessionChromeHide = 3 as const;
