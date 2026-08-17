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
            <motion.path
              key={`${id}-${i}`}
              d={letter.d}
              transform={`translate(${letter.x} 0)`}
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
          ))}
      </AnimatePresence>
    </svg>
  );
}
