import { AnimatePresence, motion, type HTMLMotionProps } from "motion/react";
import type { ReactNode } from "react";
import { arrive as arriveDistance, duration, ease, overlayFrom } from "@/motion";
import { motionIsReduced } from "@/lib/theme";

/**
 * The two named motions, and nothing else.
 *
 * Every number here comes from src/motion.ts, which is generated from the
 * tokens. Reduced motion is checked at render, so the override on /design and
 * the OS setting both collapse these to an instant appearance — the CSS sweep
 * would catch a library spring anyway, but a component that *asks* is one
 * whose fallback is confirmed rather than assumed.
 */

/** Zero durations when motion is reduced, the token values otherwise. */
function timing(seconds: number) {
  return motionIsReduced() ? 0 : seconds;
}

/**
 * The transition a *layout* animation uses.
 *
 * Anything that changes size or position — a list resorting, a row leaving
 * and the rows below closing the gap, a panel taking the space another gave
 * up — is animated with Motion's `layout` prop rather than a keyframe. The
 * library measures the element before and after the change and animates the
 * difference as a transform, so nothing is laid out again mid-flight and
 * nothing has to guess at a height the browser already knows. The discipline
 * lint refuses a keyframe that names a layout property, which is the same
 * rule written where it can be enforced.
 *
 * Pass it as `transition={{ layout: layoutTiming() }}` beside `layout`.
 */
export function layoutTiming() {
  return { duration: timing(duration.base), ease: [...ease.out] as const };
}

/**
 * `arrive`: a new item rises and fades in, so it arrives rather than appears.
 * It leaves upward, faster. For rows, cards, anything joining a list.
 */
export function Arrive({ children, ...props }: HTMLMotionProps<"div"> & { children?: ReactNode }) {
  return (
    <motion.div
      initial={{ opacity: 0, y: arriveDistance.rise }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0, y: -arriveDistance.leave }}
      transition={{
        duration: timing(duration.base),
        ease: [...ease.out],
        exit: { duration: timing(duration.fast) },
        // Present whenever the caller passes `layout`; ignored otherwise.
        layout: layoutTiming(),
      }}
      {...props}
    >
      {children}
    </motion.div>
  );
}

/**
 * `overlay`: opens from 0.97 scale, from where it was invoked and not from the
 * screen centre. For dialogs, drawers and popovers. `origin` is a CSS
 * transform-origin, which the caller derives from the trigger's position.
 */
export function Overlay({
  children,
  origin = "center",
  ...props
}: HTMLMotionProps<"div"> & { children?: ReactNode; origin?: string }) {
  return (
    <motion.div
      style={{ transformOrigin: origin }}
      initial={{ opacity: 0, scale: overlayFrom }}
      animate={{ opacity: 1, scale: 1 }}
      exit={{ opacity: 0, scale: overlayFrom }}
      transition={{ duration: timing(duration.slow), ease: [...ease.out] }}
      {...props}
    >
      {children}
    </motion.div>
  );
}

export { AnimatePresence };
