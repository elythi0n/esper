import type { ReactNode } from "react";
import { cn } from "@/lib/cn";

export interface SettingRowProps {
  label: string;
  /** What it is now, in words. Never the control's own state repeated. */
  state?: ReactNode;
  /** The control. Absent where the setting is a fact rather than a choice. */
  control?: ReactNode;
  /** What went wrong changing it, and what to do. */
  trouble?: ReactNode;
  /** Whatever the machine said while it changed — see `Said`. */
  said?: ReactNode;
  className?: string | undefined;
}

/**
 * A setting: its name, what it is now, and the one control that changes it.
 *
 * # Why this is a component
 *
 * Because it was written three times on one screen and had already drifted.
 * Serving put the state on a second line and the switch on the right; At
 * login did the same with a path instead of a sentence and a different
 * truncation; the unmanaged case was a third shape with no control at all and
 * its explanation in the state's slot. All three had their own padding.
 *
 * The label stays at a fixed width and the control stays at the right edge,
 * so a column of settings has one left edge and one right edge whatever
 * length the sentences in the middle are. That is the whole reason a settings
 * screen reads as a panel rather than as a stack of paragraphs.
 *
 * # The state line is words, not a repeat of the control
 *
 * A switch already says on or off. What it cannot say is *"running now, does
 * not start at login, systemd"* — three facts a person needs and no control
 * shows, and the reason a machine that will be gone after a reboot looks
 * exactly like one that will not.
 */
export function SettingRow({ label, state, control, trouble, said, className }: SettingRowProps) {
  return (
    <div className={cn("flex flex-col gap-3 border-b border-border py-4", className)}>
      {/* The control drops under the words when there is no room beside them, rather than squeezing them into a column. */}
      <div className="flex flex-wrap items-center gap-x-6 gap-y-3">
        {/*
          The label in the interface's own face, sentence case, at body size.
          Tracked mono capitals were how a readout's field names look, and a
          column of forty of them read as a spreadsheet header rather than a
          list of things somebody might want to change.
        */}
        <span className="flex min-w-[min(100%,16rem)] flex-1 flex-col gap-1">
          <span className="text-sm font-medium text-text">{label}</span>
          {state && <span className="min-w-0 max-w-[60ch] text-sm leading-relaxed text-text-2">{state}</span>}
        </span>
        {control && <span className="shrink-0">{control}</span>}
      </div>
      {trouble}
      {said}
    </div>
  );
}

/**
 * Related settings, together in one rounded card, the way a phone's or a
 * desktop's own settings group them. The rows keep their hairlines between
 * each other; the card's edge replaces the last one.
 */
export function SettingCard({ title, children, className }: { title?: ReactNode; children: ReactNode; className?: string | undefined }) {
  return (
    <section className={cn("flex flex-col gap-2", className)}>
      {title && <h3 className="px-1 font-display text-base font-medium text-text">{title}</h3>}
      <div className="rounded-xl border border-border bg-surface-1 px-5 [&>*:last-child]:border-b-0">{children}</div>
    </section>
  );
}

/**
 * What the machine was told to do, verbatim.
 *
 * Turning the host on runs four commands, and one of them —
 * `enable-linger` — is the one everybody misses. Showing them is what keeps
 * the switch from being a black box: a machine where it half-worked is a
 * machine somebody can finish by hand, and the next one they can do without
 * the switch entirely.
 *
 * Mono, muted, and it **wraps rather than scrolls sideways**. A command with
 * a long path in it is the case, and a horizontal scrollbar hides the end of
 * exactly the line somebody needs to copy.
 */
export function Said({ children, className }: { children: ReactNode; className?: string | undefined }) {
  return (
    <pre
      className={cn(
        "whitespace-pre-wrap break-words border-l-2 border-border py-2 pl-3 font-mono text-xs leading-relaxed text-text-3",
        className,
      )}
    >
      {children}
    </pre>
  );
}
