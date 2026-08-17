import { useEffect } from "react";
import { Dialog, DialogContent } from "./dialog";
import { Kbd } from "./kbd";

/**
 * Every key this window binds, on one sheet.
 *
 * # Why the list lives here and not in a document
 *
 * A shortcut sheet written by hand goes stale the first time somebody adds a
 * binding, and the version that is wrong is worse than none — it teaches a
 * key that does nothing. So this array *is* the list the window binds from:
 * `useShortcuts` walks it, and the sheet renders it. Adding a shortcut in one
 * place adds it to both.
 */

export interface Shortcut {
  /** As it is printed. `⌘K` on a Mac keyboard, `Ctrl K` elsewhere — see `printed`. */
  keys: string;
  what: string;
  /** Which group it belongs under on the sheet. */
  group: "Getting around" | "This window" | "Sessions";
  /**
   * How to recognise it. Kept beside the label so the two cannot drift.
   * `null` for a binding something else owns — the dock's own keys, say —
   * which is on the sheet to be *known*, not to be bound here.
   */
  when: ((e: KeyboardEvent) => boolean) | null;
  /** What it does. Absent for a binding owned elsewhere. */
  run?: (() => void) | undefined;
}

/** Whether the modifier key on this keyboard prints as ⌘ or as Ctrl. */
export const usesCommand = (() => {
  if (typeof navigator === "undefined") return false;
  return /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
})();

/** The modifier, spelled for the keyboard in front of somebody. */
export const modifier = usesCommand ? "⌘" : "Ctrl";

/** Whether the platform's own modifier is held. */
export function held(e: KeyboardEvent): boolean {
  return usesCommand ? e.metaKey : e.ctrlKey;
}

/**
 * Bind every shortcut that has something to run.
 *
 * Ignored while somebody is typing: a window where `?` opened a dialog from
 * inside a text field would be a window you cannot type a question mark in.
 */
export function useShortcuts(shortcuts: readonly Shortcut[]) {
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target && (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName))) {
        // Except the modifier ones, which are not characters anybody types.
        if (!held(e)) return;
      }
      for (const s of shortcuts) {
        if (s.when && s.run && s.when(e)) {
          e.preventDefault();
          s.run();
          return;
        }
      }
    };
    window.addEventListener("keydown", on);
    return () => window.removeEventListener("keydown", on);
  }, [shortcuts]);
}

export function ShortcutSheet({
  open,
  onOpenChange,
  shortcuts,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  shortcuts: readonly Shortcut[];
}) {
  const groups = ["Getting around", "This window", "Sessions"] as const;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent title="Keyboard" description="Everything this window binds.">
        <div className="flex flex-col gap-5">
          {groups.map((group) => {
            const rows = shortcuts.filter((s) => s.group === group);
            if (rows.length === 0) return null;
            return (
              <div key={group} className="flex flex-col">
                <p className="pb-1 font-mono text-xs uppercase tracking-[0.12em] text-text-3">{group}</p>
                {rows.map((s) => (
                  <div key={s.keys + s.what} className="flex items-center justify-between gap-6 border-b border-border py-2 last:border-b-0">
                    <span className="min-w-0 text-sm text-text">{s.what}</span>
                    <Kbd keys={s.keys} />
                  </div>
                ))}
              </div>
            );
          })}
        </div>
      </DialogContent>
    </Dialog>
  );
}
