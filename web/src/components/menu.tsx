import * as DM from "@radix-ui/react-dropdown-menu";
import { MoreHorizontal, type LucideIcon } from "lucide-react";
import { useCallback, useRef, useState, type ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * Row actions: a menu from a button, and the same menu from a right-click.
 *
 * # One list, two ways in
 *
 * The inventory asked for a `DropdownMenu` ("row actions on a device") and a
 * `ContextMenu` ("right-click a card and get the same actions") as separate
 * components, and building them separately is how the two lists drift — one
 * grows an action and the other does not, and then which actions a device has
 * depends on how you asked. So there is one `MenuAction[]` and two triggers
 * over it.
 *
 * # Why the right-click needs no second dependency
 *
 * `@radix-ui/react-context-menu` exists and would be the obvious answer. It
 * is also a whole primitive for a convenience, when the primitive already
 * here does the work: a dropdown positions against its trigger, so the
 * trigger is a zero-size element moved to the pointer. Thirty lines against a
 * dependency, and the menu is *provably* the same menu because it is the same
 * component.
 *
 * # What is not offered
 *
 * Renaming. It is the first thing anybody reaches for and the device store
 * has no such operation — a name is what the far machine calls itself, sent
 * at the handshake — so a menu item for it would be a control that fails.
 * Recorded in the inventory's not-building list rather than drawn and broken.
 */

export interface MenuAction {
  label: string;
  icon?: LucideIcon;
  /** Shown right-aligned, when the action also has a key. */
  keys?: string | undefined;
  onSelect: () => void;
  disabled?: boolean | undefined;
  /** Draws in the danger tone. For the one that cannot be undone. */
  danger?: boolean | undefined;
  /** A line above it, for separating the destructive from the ordinary. */
  apart?: boolean | undefined;
}

function Items({ actions }: { actions: readonly MenuAction[] }) {
  return (
    <>
      {actions.map((a) => {
        const Icon = a.icon;
        return (
          <div key={a.label} className="contents">
            {a.apart && <DM.Separator className="my-1 h-px bg-border" />}
            <DM.Item
              disabled={a.disabled ?? false}
              onSelect={a.onSelect}
              className={cn(
                "flex h-8 cursor-pointer select-none items-center gap-2.5 rounded-sm px-2 text-sm outline-none",
                "data-[highlighted]:bg-surface-1",
                a.danger ? "text-danger" : "text-text",
                "data-[disabled]:cursor-default data-[disabled]:text-text-disabled",
              )}
            >
              <span className="flex w-4 shrink-0 justify-center" aria-hidden>
                {Icon && <Icon size={14} strokeWidth={1.75} />}
              </span>
              <span className="min-w-0 flex-1 truncate">{a.label}</span>
              {a.keys && <span className="shrink-0 font-mono text-xs text-text-3">{a.keys}</span>}
            </DM.Item>
          </div>
        );
      })}
    </>
  );
}

const content =
  "z-[var(--z-popover)] min-w-48 overflow-hidden rounded-md border border-border bg-surface-2 p-1 " +
  "shadow-[var(--shadow-2)] data-[state=open]:animate-[overlay-in_var(--dur-base)_var(--ease-out)]";

export interface RowMenuProps {
  actions: readonly MenuAction[];
  /** What the button is for, since it has no text. */
  label: string;
  className?: string | undefined;
}

/** The visible way in: a button that is always drawn, so a keyboard finds it. */
export function RowMenu({ actions, label, className }: RowMenuProps) {
  return (
    <DM.Root>
      <DM.Trigger
        aria-label={label}
        data-tap-min=""
        onClick={(e) => e.stopPropagation()}
        className={cn(
          "shrink-0 cursor-pointer rounded-sm p-1.5 text-text-3 outline-none",
          "transition-colors duration-[var(--dur-fast)] hover:text-text data-[state=open]:text-text",
          className,
        )}
      >
        <MoreHorizontal aria-hidden size={16} strokeWidth={1.75} />
      </DM.Trigger>
      <DM.Portal>
        <DM.Content align="end" sideOffset={4} className={content}>
          <Items actions={actions} />
        </DM.Content>
      </DM.Portal>
    </DM.Root>
  );
}

/**
 * The same menu, from an icon of the caller's own.
 *
 * The session's HUD needs a menu under a monitor and a slider icon, and both
 * have to look like every other control on that bar. Same list, same content,
 * a trigger the caller draws — rather than a second menu component whose
 * items would drift from these.
 *
 * `side="top"` because the HUD is along the bottom of a session: a menu
 * opening downward from it would be off the window.
 */
export function MenuUnder({
  actions,
  children,
  side = "top",
}: {
  actions: readonly MenuAction[];
  children: ReactNode;
  side?: "top" | "bottom";
}) {
  return (
    <DM.Root>
      <DM.Trigger asChild>{children}</DM.Trigger>
      <DM.Portal>
        <DM.Content side={side} align="end" sideOffset={6} className={content}>
          <Items actions={actions} />
        </DM.Content>
      </DM.Portal>
    </DM.Root>
  );
}

/**
 * The same menu, from a right-click anywhere in `children`.
 *
 * **A convenience over a visible control, never instead of one.** A menu that
 * only a right-click reaches is a menu a keyboard cannot, and a touch screen
 * has no right button — so this wraps a row that already carries a `RowMenu`.
 */
export function WithContextMenu({
  actions,
  children,
  className,
}: {
  actions: readonly MenuAction[];
  children: ReactNode;
  className?: string | undefined;
}) {
  const [open, setOpen] = useState(false);
  const at = useRef({ x: 0, y: 0 });

  const onContextMenu = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    at.current = { x: e.clientX, y: e.clientY };
    setOpen(true);
  }, []);

  return (
    <div className={className} onContextMenu={onContextMenu}>
      {children}
      <DM.Root open={open} onOpenChange={setOpen}>
        {/*
          A zero-size anchor at the pointer. This is the whole of what a
          separate context-menu primitive would have bought: a dropdown
          positions against its trigger, so the trigger goes where the
          pointer was.
        */}
        <DM.Trigger
          aria-hidden
          tabIndex={-1}
          style={{ position: "fixed", left: at.current.x, top: at.current.y, width: 0, height: 0 }}
        />
        <DM.Portal>
          <DM.Content align="start" sideOffset={0} className={content}>
            <Items actions={actions} />
          </DM.Content>
        </DM.Portal>
      </DM.Root>
    </div>
  );
}
