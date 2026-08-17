import { useMemo } from "react";
import { cn } from "@/lib/cn";
import { ACROSS, cellsOf } from "@/lib/identicon";

/**
 * A fingerprint you can recognise before you can read it.
 *
 * # What this is for, and what it is emphatically not
 *
 * A device is a public key, and the honest way to say which key is sixteen
 * hex characters or eight words. Neither is *recognisable*: a person who
 * sees `7503856abe9926c3` twice cannot tell you whether it was the same one.
 * So this draws the key, and what it buys is that a device which has quietly
 * become a different device **looks** different, at a glance, from across a
 * desk.
 *
 * **It is not a security check, and must never be presented as one.** Two
 * keys can be made to draw the same picture by anybody willing to grind for
 * it — the drawing is a few dozen bits of the digest, not the digest — and a
 * person who verified a picture has verified nothing. That is why every place
 * this appears also shows the hex or the words, and why it is decoration in
 * the strict sense: remove it and no promise is weakened.
 *
 * What it *does* prevent is the failure it was drawn for: a row that changed
 * underneath somebody who was not reading carefully.
 *
 * # Why these shapes
 *
 * A five-by-five grid, mirrored about its vertical axis, which is the
 * arrangement that makes two nearby keys look unlike each other rather than
 * like noise — symmetry gives the eye something to hold. Fifteen cells decide
 * the drawing; the other ten are their reflection.
 *
 * Colour comes from the accent and the text tones only, in three steps. It
 * never introduces a hue of its own, because a device is not a colour and a
 * palette that grew one per key would be a palette nobody chose.
 */

export interface IdenticonProps {
  /** The fingerprint, as hex. Anything else is drawn as its own characters. */
  of: string;
  /** Pixels on a side. 16 in a row, 28 in a panel, 44 on its own. */
  size?: number;
  className?: string | undefined;
}

export function Identicon({ of, size = 16, className }: IdenticonProps) {
  // The arithmetic lives in `lib/identicon.ts` so that the property this
  // picture exists for — two keys differing by one character look different —
  // is something a check can measure rather than something a gallery of five
  // specimens implies.
  const cells = useMemo(() => cellsOf(of), [of]);

  const step = 100 / ACROSS;
  // Three weights, from the palette the product already has. Never a new hue.
  const paint = ["", "var(--color-text-3)", "var(--color-text-2)", "var(--color-accent)"];

  return (
    <svg
      viewBox="0 0 100 100"
      width={size}
      height={size}
      // **Decoration, and marked as such.** The fingerprint is beside it in
      // characters, so a screen reader that announced this would be reading
      // out a picture of a thing it is about to read out properly.
      aria-hidden
      focusable="false"
      className={cn("shrink-0 rounded-[2px]", className)}
    >
      {cells.map((weight, i) =>
        weight === 0 ? null : (
          <rect
            key={i}
            x={(i % ACROSS) * step}
            y={Math.floor(i / ACROSS) * step}
            width={step}
            height={step}
            fill={paint[weight]}
          />
        ),
      )}
    </svg>
  );
}
