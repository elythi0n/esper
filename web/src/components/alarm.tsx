import { AnimatePresence, motion } from "motion/react";
import { TriangleAlert, type LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import { motionIsReduced } from "@/lib/theme";
import { duration, ease } from "@/motion";

export type AlarmTone = "warn" | "danger";

export interface AlarmProps {
  /** Nothing is drawn when this is false, and what was drawn leaves. */
  show: boolean;
  tone?: AlarmTone;
  icon?: LucideIcon;
  /** What is wrong, in a sentence somebody can act on. */
  children: ReactNode;
  /** The machine's own words, when it left any. Monospaced, and truncated. */
  detail?: string | undefined;
  /** The one thing that fixes it. */
  action?: ReactNode;
}

/**
 * A band across the whole window for a condition of the **machine**.
 *
 * Distinct from `Callout`, which is a message inside a screen, and from
 * `Trouble`, which is one screen's request having failed. This is the third
 * case and it needed its own shape: the engine on this machine has stopped,
 * which is true on every screen at once and is nobody's page in particular.
 * So it sits above the shell rather than inside a page — over the sidebar as
 * well, because the sidebar is no more true than the rest of it.
 *
 * It is **not dismissible**. A band somebody can close is a band that stops
 * being a fact about the machine and starts being a notification; this one
 * goes away when the thing it describes does, and not before.
 */
export function Alarm({ show, tone = "warn", icon: Icon = TriangleAlert, children, detail, action }: AlarmProps) {
  const reduced = motionIsReduced();
  return (
    <AnimatePresence initial={false}>
      {show && (
        <motion.div
          className="alarm"
          data-tone={tone}
          role="alert"
          initial={reduced ? false : { height: 0, opacity: 0 }}
          animate={{ height: "auto", opacity: 1 }}
          exit={reduced ? { opacity: 0 } : { height: 0, opacity: 0 }}
          transition={{ duration: reduced ? 0 : duration.base, ease: [...ease.out] }}
          style={{ overflow: "hidden" }}
        >
          <div className="alarm-inner">
            <Icon aria-hidden size={14} strokeWidth={2} className="shrink-0" />
            <p className="alarm-said">{children}</p>
            {detail && <p className="alarm-detail">{detail}</p>}
            {action && <div className="alarm-action">{action}</div>}
          </div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
