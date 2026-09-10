import * as RD from "@radix-ui/react-dialog";
import { CornerDownLeft, Search, type LucideIcon } from "lucide-react";
import { useEffect, useMemo, useState, type KeyboardEvent } from "react";
import { cn } from "@/lib/cn";
import { useReturnFocus } from "./dialog";
import { Kbd } from "./kbd";

export interface Command {
  id: string;
  label: string;
  /** Where it goes or what it does, in a few words. */
  hint?: string | undefined;
  keywords?: readonly string[] | undefined;
  icon?: LucideIcon | undefined;
  run: () => void;
}

export interface CommandPaletteProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  commands: readonly Command[];
  placeholder?: string | undefined;
}

function matches(c: Command, q: string): boolean {
  const hay = [c.label, c.hint ?? "", ...(c.keywords ?? [])].join(" ").toLowerCase();
  return q.split(/\s+/).every((word) => hay.includes(word));
}

/**
 * Everything the app can do, from the keyboard. Same family as the select:
 * 32px rows, a fixed icon slot so labels line up, the highlighted row is the
 * raised surface. It opens with the `overlay` motion and, with motion off,
 * simply appears.
 */
export function CommandPalette({ open, onOpenChange, commands, placeholder = "Type a command or a device name" }: CommandPaletteProps) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);

  const returning = useReturnFocus();
  const q = query.trim().toLowerCase();
  const shown = useMemo(() => (q ? commands.filter((c) => matches(c, q)) : [...commands]), [commands, q]);

  useEffect(() => {
    if (open) {
      setQuery("");
      setActive(0);
    }
  }, [open]);
  useEffect(() => setActive(0), [q]);

  const run = (c: Command | undefined) => {
    if (!c) return;
    onOpenChange(false);
    c.run();
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => Math.min(shown.length - 1, i + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => Math.max(0, i - 1));
    } else if (e.key === "Enter") {
      e.preventDefault();
      run(shown[active]);
    }
  };

  return (
    <RD.Root open={open} onOpenChange={onOpenChange}>
      <RD.Portal>
        <RD.Overlay
          className={cn(
            "fixed inset-0 z-[var(--z-overlay)] bg-[color:var(--color-selection)]",
            "data-[state=open]:animate-[fade-in_var(--dur-base)_var(--ease-out)] data-[state=closed]:animate-[fade-out_var(--dur-fast)_var(--ease)]",
          )}
        />
        {/* It builds its own Content, so it takes the focus return explicitly
            — the same implementation, not a second one. */}
        <RD.Content
          data-focus-exempt="a dialog panel is not a control; it takes focus on open so the next Tab starts inside it"
          onOpenAutoFocus={returning.onOpenAutoFocus}
          onCloseAutoFocus={returning.onCloseAutoFocus}
          aria-describedby={undefined}
          className={cn(
            "fixed left-1/2 top-[18vh] z-[var(--z-modal)] w-[min(36rem,calc(100%-2rem))] -translate-x-1/2 overflow-hidden rounded-lg border border-border bg-surface-2 shadow-[var(--shadow-3)] outline-none",
            "data-[state=open]:animate-[overlay-in_var(--dur-base)_var(--ease-out)] data-[state=closed]:animate-[overlay-out_var(--dur-fast)_var(--ease)]",
          )}
        >
          <RD.Title className="sr-only">Commands</RD.Title>
          <div className="flex items-center gap-2 border-b border-border px-3">
            <Search aria-hidden size={16} className="shrink-0 text-text-3" />
            <input
              /*
                No `autoFocus`.

                React applies it when the element mounts, which is *before*
                Radix's `onOpenAutoFocus` runs — so the handler that
                remembers where focus came from read the input itself, and
                closing the palette put focus back on an element that no
                longer existed. `check-focus.mjs` reported it as focus
                landing on `<body>`, which it was, by a longer route.

                It is redundant besides: Radix's focus scope focuses the
                first focusable thing in the content, and that is this.
              */
              data-focus-exempt="the palette opens onto this field and nothing else in it takes focus; the caret says where typing goes"
              role="combobox"
              aria-expanded
              aria-controls="command-list"
              aria-activedescendant={shown[active] ? `command-${shown[active].id}` : undefined}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={onKey}
              placeholder={placeholder}
              className="h-11 w-full bg-transparent text-base text-text outline-none placeholder:text-text-3"
            />
            <Kbd keys="esc" />
          </div>
          <ul id="command-list" role="listbox" className="max-h-[50vh] overflow-y-auto p-1">
            {shown.length === 0 && <li className="px-3 py-6 text-center text-sm text-text-2">Nothing matches. Try another word.</li>}
            {shown.map((c, i) => {
              const Icon = c.icon;
              const on = i === active;
              return (
                <li
                  key={c.id}
                  id={`command-${c.id}`}
                  role="option"
                  aria-selected={on}
                  onPointerMove={() => setActive(i)}
                  onClick={() => run(c)}
                  className={cn(
                    "flex h-8 cursor-pointer items-center gap-2 rounded-sm pl-2 pr-2 text-sm text-text",
                    on && "bg-surface-1",
                  )}
                >
                  <span className="inline-flex w-5 shrink-0 items-center justify-center text-text-3">{Icon && <Icon aria-hidden size={16} />}</span>
                  <span className="min-w-0 flex-1 truncate">{c.label}</span>
                  {c.hint && <span className="truncate text-xs text-text-3">{c.hint}</span>}
                  {on && <CornerDownLeft aria-hidden size={12} className="shrink-0 text-text-3" />}
                </li>
              );
            })}
          </ul>
        </RD.Content>
      </RD.Portal>
    </RD.Root>
  );
}

/** Ctrl+K or ⌘K toggles the palette. */
export function useCommandKey(toggle: () => void) {
  useEffect(() => {
    const on = (e: globalThis.KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        toggle();
      }
    };
    window.addEventListener("keydown", on);
    return () => window.removeEventListener("keydown", on);
  }, [toggle]);
}
