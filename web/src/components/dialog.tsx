import * as RD from "@radix-ui/react-dialog";
import { X } from "lucide-react";
import { useRef, type ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Button } from "./button";

/**
 * Dialog and Drawer, on Radix, re-pointed at the tokens.
 *
 * The overlay is the text colour at low alpha rather than a black wash, so it
 * reads correctly in light too. The content sits on `--z-modal`; anything that
 * floats out of a control inside it — a select — sits on `--z-popover`, which
 * is *above* this, because a select in a dialog is the common case and must
 * not be clipped.
 *
 * Motion is the `overlay` variant expressed in CSS on the tokens, so the
 * reduced-motion sweep applies to it without this file knowing.
 */
export const Dialog = RD.Root;
export const DialogTrigger = RD.Trigger;
export const DialogClose = RD.Close;

/**
 * Give focus back to whatever opened this.
 *
 * # Why it is a hook and not just part of DialogContent
 *
 * Because the command palette builds its own `Radix.Content` — it has a
 * search field and a list rather than a title and a body — and it was the one
 * overlay still failing after `DialogContent` was fixed. Two implementations
 * of "put focus back" is one implementation and one bug.
 *
 * # What it does, and the two things that were wrong first
 *
 * Radix returns focus to the **trigger**, which is right and is nothing when
 * there is no trigger — and half the overlays here are opened by state: the
 * palette from `⌘K`, the shortcut sheet from `?`, a confirmation from a row
 * menu whose row is about to vanish. Closing left focus on `<body>`, the next
 * Tab started from the top of the document, and somebody navigating by
 * keyboard was thrown to the beginning of the window with no idea why.
 *
 * Invisible to every other gate: no wrong pixel, no contrast failure, no
 * dropped frame, and it reads perfectly in the source. `check-focus.mjs`
 * found it on two of four overlays the first time it ran.
 *
 * **Captured in `onOpenAutoFocus`, not in an effect.** An effect runs after
 * the content has mounted and Radix has already moved focus inside, so what
 * it remembers is a control *in the overlay* — the first attempt did that and
 * made all four fail where two had. And restored only if it is **still in the
 * document**, because the row a menu hung from may be the thing the overlay
 * removed.
 */
export function useReturnFocus(): {
  onOpenAutoFocus: (event: Event) => void;
  onCloseAutoFocus: (event: Event) => void;
} {
  const cameFrom = useRef<HTMLElement | null>(null);
  return {
    onOpenAutoFocus: () => {
      const active = document.activeElement;
      cameFrom.current = active instanceof HTMLElement && active !== document.body ? active : null;
    },
    onCloseAutoFocus: (event: Event) => {
      const back = cameFrom.current;
      if (back && back.isConnected) {
        event.preventDefault();
        back.focus();
      }
    },
  };
}

export interface DialogContentProps extends Omit<RD.DialogContentProps, "title"> {
  title: ReactNode;
  description?: ReactNode;
  /** A side sheet rather than a centred dialog. */
  drawer?: boolean;
}

export function DialogContent({
  title,
  description,
  drawer = false,
  className,
  children,
  onOpenAutoFocus,
  onCloseAutoFocus,
  ...props
}: DialogContentProps) {
  // One implementation of "put focus back", shared with anything that builds
  // its own Content — see `useReturnFocus`.
  const returning = useReturnFocus();

  return (
    <RD.Portal>
      <RD.Overlay
        className={cn(
          "fixed inset-0 z-[var(--z-overlay)] bg-[color:var(--color-selection)]",
          "data-[state=open]:animate-[fade-in_var(--dur-slow)_var(--ease-out)]",
          "data-[state=closed]:animate-[fade-out_var(--dur-fast)_var(--ease)]",
        )}
      />
      <RD.Content
        aria-describedby={description ? undefined : undefined}
        onOpenAutoFocus={(event) => {
          onOpenAutoFocus?.(event);
          returning.onOpenAutoFocus(event);
        }}
        onCloseAutoFocus={(event) => {
          onCloseAutoFocus?.(event);
          if (!event.defaultPrevented) returning.onCloseAutoFocus(event);
        }}
        className={cn(
          "fixed z-[var(--z-modal)] flex flex-col gap-3 border border-border bg-surface-2 p-5 text-text shadow-[var(--shadow-3)] outline-none",
          drawer
            ? "inset-y-0 right-0 w-[min(28rem,100%)] rounded-l-lg data-[state=open]:animate-[drawer-in_var(--dur-slow)_var(--ease-out)] data-[state=closed]:animate-[drawer-out_var(--dur-fast)_var(--ease)]"
            : "left-1/2 top-1/2 w-[min(32rem,calc(100%-2rem))] -translate-x-1/2 -translate-y-1/2 rounded-lg data-[state=open]:animate-[overlay-in_var(--dur-slow)_var(--ease-out)] data-[state=closed]:animate-[overlay-out_var(--dur-fast)_var(--ease)]",
          className,
        )}
        {...props}
      >
        <div className="flex items-start justify-between gap-4">
          <div>
            <RD.Title className="text-lg font-medium">{title}</RD.Title>
            {description && <RD.Description className="mt-1 text-sm text-text-2">{description}</RD.Description>}
          </div>
          <RD.Close asChild>
            <Button variant="ghost" size="sm" aria-label="Close" className="-mr-1 -mt-1 px-1.5">
              <X aria-hidden size={16} />
            </Button>
          </RD.Close>
        </div>
        {children}
      </RD.Content>
    </RD.Portal>
  );
}

export function DialogFooter({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("mt-2 flex justify-end gap-2", className)} {...props} />;
}
