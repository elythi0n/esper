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
