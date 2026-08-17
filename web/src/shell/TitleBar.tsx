import * as MB from "@radix-ui/react-menubar";
import { Minus, Square, X } from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "@/lib/cn";

/*
  The window's own title bar.

  The window is frameless, so this is the whole of its chrome: a drag region,
  the menus, and the three buttons the OS used to draw. 32px, one hairline,
  and nothing else — it sits above every screen for the life of the window,
  so anything decorative in it is decoration you look at all day.

  # Why there is no File and no Edit

  They were the obvious names and they are the wrong ones. There are no files
  here and nothing to edit — a remote desktop has *machines*, *sessions* and
  a window. A menu bar earns its place by holding the things that are not
  destinations, because the sidebar already holds the destinations: actions,
  with their shortcuts written next to them, and the window's own commands.

  So: **Session** (what you do with a connection), **View** (what this window
  looks like), **Help**. Three, all verbs, none of them duplicating the rail.
*/

export interface MenuItem {
  label: string;
  /** Shown right-aligned. Not wired here — the shortcut belongs to whoever owns the action. */
  keys?: string | undefined;
  onSelect?: (() => void) | undefined;
  disabled?: boolean | undefined;
}

export interface Menu {
  label: string;
  items: (MenuItem | "separator")[];
}

/**
 * One menu of the bar.
 *
 * A **menubar**, not a row of dropdowns, and that was a bug before it was a
 * preference: as separate `DropdownMenu`s the triggers opened from the
 * keyboard and not from a pointer — proved by driving both, Enter opened one
 * and a real click did not. `Menubar` is the primitive this actually is, and
 * it brings what a row of dropdowns cannot: once one menu is open, moving
 * across the bar switches to the next without clicking again, and arrow keys
 * walk the whole bar.
 */
function MenuOf({ menu }: { menu: Menu }) {
  return (
    <MB.Menu>
      <MB.Trigger
        /*
          **Exempt from the tap minimum, and it says why.**

          A 44px hit area needs 44px to put it in, and this bar is 32 — the
          height the OS itself draws a title bar at, which is the whole reason
          the window is frameless. An `::after` that big would hang six pixels
          into the topbar below and swallow clicks meant for it, which is a
          worse failure than a small target: a mis-click that does something
          else beats a mis-click that does nothing.

          Every exemption carries its reason here and `check-a11y.mjs` counts
          them, so this is a decision somebody can argue with rather than a
          silent hole.
        */
        data-tap-exempt="a title bar is 32px because the OS draws one at 32px"
        className={cn(
          "h-full rounded-sm px-2 text-xs text-text-2 no-drag outline-none",
          "transition-colors duration-[var(--dur-fast)]",
          "hover:bg-surface-2 hover:text-text data-[state=open]:bg-surface-2 data-[state=open]:text-text",
        )}
      >
        {menu.label}
      </MB.Trigger>
      <MB.Portal>
        <MB.Content
          align="start"
          sideOffset={2}
          className={cn(
            "z-[var(--z-popover)] min-w-52 overflow-hidden rounded-md border border-border bg-surface-2 p-1 shadow-[var(--shadow-2)]",
            "data-[state=open]:animate-[overlay-in_var(--dur-base)_var(--ease-out)]",
          )}
        >
          {menu.items.map((item, i) =>
            item === "separator" ? (
              <MB.Separator key={`sep-${i}`} className="my-1 h-px bg-border" />
            ) : (
              <MB.Item
                key={item.label}
                disabled={item.disabled ?? false}
                onSelect={() => item.onSelect?.()}
                className={cn(
                  "flex h-8 cursor-pointer select-none items-center gap-4 rounded-sm px-2 text-sm outline-none",
                  "text-text data-[highlighted]:bg-surface-1",
                  "data-[disabled]:cursor-default data-[disabled]:text-text-disabled",
                )}
              >
                <span className="min-w-0 flex-1 truncate">{item.label}</span>
                {item.keys && <span className="shrink-0 font-mono text-xs text-text-3">{item.keys}</span>}
              </MB.Item>
            ),
          )}
        </MB.Content>
      </MB.Portal>
    </MB.Menu>
  );
}

function WindowButton({
  label,
  onClick,
  danger = false,
  children,
}: {
  label: string;
  onClick: () => void;
  danger?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      onClick={onClick}
      className={cn(
        // The bar's height includes its hairline, so a button as tall as the
        // bar paints over it and the line breaks where the cursor is.
        "no-drag inline-flex h-full w-11 items-center justify-center text-text-3",
        "transition-colors duration-[var(--dur-fast)]",
        danger ? "hover:bg-danger hover:text-danger-fg" : "hover:bg-surface-2 hover:text-text",
      )}
    >
      {children}
    </button>
  );
}

export interface TitleBarProps {
  menus: readonly Menu[];
  onMinimise: () => void;
  onMaximise: () => void;
  onClose: () => void;
  /** A popped-out window says which session it is; the main one says nothing. */
  title?: string | undefined;
}

export function TitleBar({ menus, onMinimise, onMaximise, onClose, title }: TitleBarProps) {
  return (
    <header className="titlebar">
      {/*
        No mark here. It lives on the rail, where collapsing it is a moment
        worth animating; a second copy two centimetres away would be one
        copy too many.
      */}
      <div className="w-2" />
      <MB.Root aria-label="Menus" className="flex h-full shrink-0 items-center gap-0.5">
        {menus.map((m) => (
          <MenuOf key={m.label} menu={m} />
        ))}
      </MB.Root>

      {/* The drag region is the empty middle, which is where a person grabs a
          window anyway. It also carries the title of a popped-out one. */}
      <div className="flex min-w-0 flex-1 items-center justify-center px-4">
        {title && <span className="truncate text-xs text-text-3">{title}</span>}
      </div>

      <div className="flex h-full shrink-0 items-center">
        <WindowButton label="Minimise" onClick={onMinimise}>
          <Minus aria-hidden size={14} strokeWidth={1.75} />
        </WindowButton>
        <WindowButton label="Maximise" onClick={onMaximise}>
          <Square aria-hidden size={11} strokeWidth={1.75} />
        </WindowButton>
        <WindowButton label="Close" onClick={onClose} danger>
          <X aria-hidden size={14} strokeWidth={1.75} />
        </WindowButton>
      </div>
    </header>
  );
}
