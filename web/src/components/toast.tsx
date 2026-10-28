import * as RTo from "@radix-ui/react-toast";
import { X } from "lucide-react";
import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from "react";
import { cn } from "@/lib/cn";
import type { CalloutTone } from "./callout";
import { adviceFor as adviceForCode, isOrdinary } from "@/api/reasons";

/**
 * Toasts, and the protocol's reasons rendered as sentences a person can act on.
 *
 * Every failure the protocol can report has a code in a range that says what
 * kind of thing went wrong: 1xx protocol, 2xx authentication, 3xx
 * authorization, 4xx capability, 5xx transport. The engine's own text for each
 * code is precise and terse; what a toast shows is the sentence that says what
 * to *do*, and that lives here so every screen says the same thing for the
 * same code. Errors say what to do, not what went wrong internally. No
 * exclamation marks. No "Oops".
 */
export interface Reason {
  code: number;
  /** The engine's own detail, shown smaller under the sentence. */
  detail?: string;
}

/**
 * What a person should do about a reason.
 *
 * The table lives in `api/reasons.ts` and not here, for two reasons. It is
 * data — one line per code, no React in it — so a gate running under node can
 * import it and hold it to the protocol's own list, which is what stops
 * thirty-six codes sharing four sentences again. And a sentence that says what
 * to do is not a property of a toast: a callout, a screen and a log line want
 * the same words for the same code.
 */
export function adviceFor(reason: Reason): { title: string; tone: CalloutTone } {
  return adviceForCode(reason.code);
}

export interface ToastMessage {
  id: number;
  title: ReactNode;
  description?: ReactNode;
  tone?: CalloutTone;
  /**
   * The one thing to do about what this reported.
   *
   * # Why the action belongs on the toast
   *
   * "Undo" anywhere else is a search. The moment somebody wants it is the
   * moment they read the sentence, and the place they are looking is the
   * sentence — so a confirmation that cannot be acted on is a confirmation
   * that has to be *read carefully*, which is the opposite of what it is for.
   *
   * One action, never two: a toast is not a dialog, and a choice belongs in
   * one. It dismisses the toast when taken, because a toast still offering to
   * undo a thing already undone is a lie with a button on it.
   */
  action?: { label: string; run: () => void } | undefined;
}

interface ToastApi {
  show: (t: Omit<ToastMessage, "id">) => void;
  reason: (r: Reason) => void;
}

const Ctx = createContext<ToastApi | null>(null);

export function useToast(): ToastApi {
  const api = useContext(Ctx);
  if (!api) throw new Error("useToast needs a <Toaster> above it");
  return api;
}

const toneBar: Record<CalloutTone, string> = {
  info: "before:bg-info",
  ok: "before:bg-ok",
  warn: "before:bg-warn",
  danger: "before:bg-danger",
};

/** Mount once; toasts sit on `--z-toast`, in a reserved band, and arrive with the `arrive` motion. */
export function Toaster({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<ToastMessage[]>([]);
  const show = useCallback((t: Omit<ToastMessage, "id">) => {
    setItems((all) => [...all, { ...t, id: Date.now() + Math.random() }]);
  }, []);
  const reason = useCallback(
    (r: Reason) => {
      const { title, tone } = adviceFor(r);
      /*
        The code, except where there is nothing to look up.

        A session that simply finished is not a fault and has no ticket
        number: putting "(2)" under "The session ran for as long as it was
        asked to" makes an ordinary ending look like an error report. The
        codes that *are* faults keep theirs, because that is what somebody
        quotes when they ask about one.
      */
      const trail = isOrdinary(r.code) ? (r.detail ?? "") : `${r.detail ?? ""} (${r.code})`;
      show({ title, tone, description: trail.trim() || undefined });
    },
    [show],
  );
  const api = useMemo(() => ({ show, reason }), [show, reason]);

  return (
    <RTo.Provider duration={6000}>
      <Ctx.Provider value={api}>{children}</Ctx.Provider>
      {items.map((t) => (
        <RTo.Root
          key={t.id}
          onOpenChange={(open) => !open && setItems((all) => all.filter((x) => x.id !== t.id))}
          className={cn(
            "relative grid w-[22rem] gap-1 overflow-hidden rounded-lg border border-border bg-surface-2 p-3 pl-4 text-sm shadow-[var(--shadow-2)]",
            "before:absolute before:inset-y-0 before:left-0 before:w-1",
            toneBar[t.tone ?? "info"],
            "data-[state=open]:animate-[arrive-in_var(--dur-base)_var(--ease-out)]",
            "data-[state=closed]:animate-[arrive-out_var(--dur-fast)_var(--ease)]",
          )}
        >
          <RTo.Title className="pr-6 font-medium text-text">{t.title}</RTo.Title>
          {t.description && <RTo.Description className="text-xs text-text-2">{t.description}</RTo.Description>}
          {t.action && (
            <RTo.Action
              asChild
              altText={t.action.label}
              onClick={() => t.action?.run()}
            >
              <button
                type="button"
                className={cn(
                  "mt-1.5 w-fit cursor-pointer rounded-sm border border-border bg-surface-0 px-2 py-1 text-xs text-text",
                  "transition-colors duration-[var(--dur-fast)] hover:border-border-strong",
                )}
              >
                {t.action.label}
              </button>
            </RTo.Action>
          )}
          <RTo.Close aria-label="Dismiss" className="absolute right-2 top-2 cursor-pointer rounded-sm p-1 text-text-3 hover:text-text">
            <X aria-hidden size={14} />
          </RTo.Close>
        </RTo.Root>
      ))}
      <RTo.Viewport data-focus-exempt="a toast viewport is a region, not a control" className="fixed bottom-4 right-4 z-[var(--z-toast)] flex flex-col gap-2 outline-none" />
    </RTo.Provider>
  );
}
