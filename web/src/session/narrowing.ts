/**
 * What a session has given up, said in the fewest words that are true.
 *
 * # Why this is not in `video.ts`
 *
 * Because it is the one part of the surface's state that is a *sentence*, and
 * a sentence is worth testing on its own. The rest of that module needs a
 * browser — a decoder, a canvas, a fetch — and this needs nothing, so it sits
 * where the logic gate can reach it.
 */

/**
 * The ceilings a ladder has asked the host for, as a person would read them.
 *
 * Zero is not a ceiling: it is the ordinary state of a link that is keeping
 * up, and a readout saying "0 fps" would report a stalled session on a perfect
 * one. Both numbers are asks rather than settings — a session narrowed to fit
 * a link that could not carry it looks exactly like one somebody chose to
 * soften, and this is the only thing that tells them apart.
 *
 * An empty string means nothing was taken away.
 */
export function narrowing(askedFps: number, askedLongEdge: number): string {
  return [askedFps > 0 ? `${askedFps} fps` : null, askedLongEdge > 0 ? `${askedLongEdge}px` : null]
    .filter(Boolean)
    .join(", ");
}

/** What an ask for a screen is doing, as the chrome has to say it. */
export type AskState =
  /** Nothing was asked, or what was asked for is what is on screen. */
  | "settled"
  /** Asked, and the session has not changed to it yet. */
  | "asking"
  /** Asked long enough ago that the honest answer is the host kept its own. */
  | "kept";

/**
 * Where an ask for a screen stands.
 *
 * A switch is a round trip, a decision at the host and then a keyframe, so a
 * person who clicks and sees nothing clicks again — which is why an ask is
 * acknowledged where it was made rather than when it comes back.
 *
 * Settled by **agreement**, never by the clock alone: `late` only turns a
 * pending ask into a plain statement that the host kept what it had, and a
 * session that has arrived at the screen asked for is settled however late it
 * was. A slow host must never be reported as a refusal it did not give.
 *
 * Asking for the screen already on show is settled from the start. There is
 * nothing to wait for, and chrome that said otherwise would be reporting
 * progress towards where it already is.
 */
export function askState(asked: number | null, showing: readonly number[], late: boolean): AskState {
  if (asked === null) return "settled";
  if (showing.length === 1 && showing[0] === asked) return "settled";
  return late ? "kept" : "asking";
}
