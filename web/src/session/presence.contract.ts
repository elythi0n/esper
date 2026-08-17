/**
 * The presence indicator's contract.
 *
 * The host-side badge that says "this screen is being watched" is a product
 * surface with a security function, so it gets rules rather than styling. A
 * slogan — "verified by the engine" — is not a rule. These are the numbers,
 * and scripts/check-presence.mjs holds presence.css to them. The engine holds
 * its own X11 indicator to the same numbers; this file is the one place they
 * are written so the two cannot disagree.
 *
 * Every value here is a floor or a prohibition. None is a preference.
 */
export const presence = {
  /**
   * Smallest the indicator may ever be drawn, in CSS pixels. 28 is above the
   * 24px that WCAG 2.5.8 calls a minimum target and is large enough to be
   * seen from across a room, which is the situation it exists for: somebody
   * glancing at a machine that is not theirs.
   */
  minSizePx: 28,

  /** The ring around the fill, in CSS pixels. What keeps it visible on any picture. */
  ringPx: 2,

  /**
   * Two tones, so it contrasts with any video. The fill is near-black and the
   * ring and dot are bone; whichever the picture behind it resembles, the
   * other tone is what the eye finds. The contract is on the *pair*: against
   * black, white and a mid-grey, the better of the two must clear 3:1.
   */
  fill: "oklch(0.13 0 0)",
  ring: "oklch(0.93 0 0)",
  minContrastAgainstVideo: 3,

  /** The worst cases a video frame presents. A picture is never all one, and these are the three it is nearest. */
  videoWorstCases: ["oklch(0 0 0)", "oklch(1 0 0)", "oklch(0.6 0 0)"] as const,

  /**
   * Not themeable. Its colours are the two above, in every theme, in the
   * session and outside it. A theme that could change them could hide it.
   */
  themeable: false,

  /**
   * Never animated — not on entry, not on exit, not when reduced motion is
   * *off*. An indicator that fades in is one that is invisible for as long as
   * the fade takes, and one that pulses is one somebody learns to tune out.
   */
  animated: false,

  /** Never translucent. An opacity below 1 is a way of being themed away. */
  opacity: 1,

  /** Fixed to the viewport, top trailing corner, on the session HUD layer. */
  position: "fixed",
} as const;
