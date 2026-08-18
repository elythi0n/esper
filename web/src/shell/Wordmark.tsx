import { AnimatePresence, motion } from "motion/react";
import { useId } from "react";
import { motionIsReduced } from "@/lib/theme";
import { arrive, duration, ease } from "@/motion";
import { cn } from "@/lib/cn";

/**
 * ESPER, set in live type as letterforms.
 *
 * The same five paths the homepage draws, so the product and the site are
 * the same mark rather than two drawings of one — and drawn rather than set
 * in a typeface because the letterforms *are* the artwork, which is also why
 * it never arrives with a colour of its own: `currentColor`, every time.
 *
 * # Why it animates
 *
 * The rail collapsing is the one moment the wordmark comes and goes, and a
 * mark that simply vanishes reads as a rendering fault. The letters arrive
 * one at a time, left to right, and leave the same way — the `arrive` motion
 * the design system already names, applied per letter. With reduced motion
 * it is there or it is not, which is the correct fallback and not a
 * degraded one.
 *
 * # Why each letter sits in a group of its own
 *
 * Because a CSS transform on an SVG element **replaces** its `transform`
 * presentation attribute — including `transform: none`, which is what an
 * animation library leaves behind when it finishes. The letters used to carry
 * their own x offset as that attribute *and* be the elements whose transform
 * was animated, and the two cannot coexist: measured, three paths at x = 0,
 * 250 and 500 all collapsed to x = 0 the moment a style transform appeared.
 *
 * The result was a mark that imploded into a single blob for the whole of
 * every animation in WebKit, and — because Chromium honours `transform: none`
 * where WebKit falls back to the attribute — one that was stacked *at rest*
 * in every browser, on this page and in the dev preview. It had been that way
 * since the mark was drawn.
 *
 * So the offset lives on a `<g>` that nothing animates, and the animated path
 * sits inside it. The group's attribute and the path's CSS transform compose,
 * which is what was wanted all along.
 */

/** The five letterforms, on a 100-unit body, spaced 125 apart. */
const LETTERS = [
  { x: 0, d: "M0 0H70V20H20V40H55V60H20V80H70V100H0Z" },
  { x: 125, d: "M0 0H70V20H20V40H70V100H0V80H50V60H0Z" },
  { x: 250, d: "M0 0H70V60H20V100H0ZM20 20V40H50V20Z", evenodd: true },
  { x: 375, d: "M0 0H70V20H20V40H55V60H20V80H70V100H0Z" },
  { x: 500, d: "M0 0H70V60H20V100H0ZM20 20V40H50V20ZM36 60H56L70 100H50Z", evenodd: true },
];

export function Wordmark({ className, show = true }: { className?: string | undefined; show?: boolean }) {
  const id = useId().replace(/[^a-zA-Z0-9]/g, "");
  const reduced = motionIsReduced();

  return (
    <svg
      viewBox="0 0 570 100"
      role="img"
      aria-label="Esper"
      aria-hidden={!show}
      overflow="visible"
      className={cn("w-auto shrink-0 select-none text-text", className)}
    >
      <AnimatePresence initial={false}>
        {show &&
          LETTERS.map((letter, i) => (
            <g key={`${id}-${i}`} transform={`translate(${letter.x} 0)`}>
              <motion.path
                d={letter.d}
                fill="currentColor"
                fillRule={letter.evenodd ? "evenodd" : undefined}
                initial={reduced ? false : { opacity: 0, y: arrive.rise * 2 }}
                animate={{ opacity: 1, y: 0 }}
                exit={reduced ? { opacity: 0 } : { opacity: 0, y: -arrive.leave * 2 }}
                transition={{
                  duration: reduced ? 0 : duration.base,
                  ease: [...ease.out],
                  delay: reduced ? 0 : i * 0.04,
                }}
              />
            </g>
          ))}
      </AnimatePresence>
    </svg>
  );
}
