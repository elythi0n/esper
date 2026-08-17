import * as RTo from "@radix-ui/react-toast";
import { X } from "lucide-react";
import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from "react";
import { cn } from "@/lib/cn";
import type { CalloutTone } from "./callout";

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

/** What a person should do about a reason, by range and by the codes that need their own sentence. */
export function adviceFor(reason: Reason): { title: string; tone: CalloutTone } {
  const known: Record<number, string> = {
    100: "Update Esper on one of the two machines — they speak different protocol versions.",
    304: "Nobody at the host allowed the session. Ask them, or have them grant unattended access.",
    404: "The host could not show that it is being watched, so it stopped. Check its display.",
    406: "That machine is already in a session. Try again when it is free.",
  };
  const sentence = known[reason.code];
  if (sentence) return { title: sentence, tone: reason.code >= 400 ? "warn" : "info" };
  const range = Math.floor(reason.code / 100);
  switch (range) {
    case 1: return { title: "The two machines could not agree on how to talk. Update both.", tone: "danger" };
    case 2: return { title: "This device is not paired with that machine. Pair it first.", tone: "warn" };
    case 3: return { title: "This device is not allowed to do that. Ask for the permission at the host.", tone: "warn" };
    case 4: return { title: "That machine cannot do this right now. Its own output says why.", tone: "info" };
    case 5: return { title: "The connection was lost. Esper will reconnect if the machine comes back.", tone: "warn" };
    default: return { title: "Something the host reported has no sentence yet. The code is below.", tone: "info" };
  }
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
      show({ title, tone, description: `${r.detail ?? ""}${r.detail ? " " : ""}(${r.code})`.trim() });
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
            "relative grid w-[22rem] gap-1 overflow-hidden rounded-md border border-border bg-surface-2 p-3 pl-4 text-sm shadow-[var(--shadow-2)]",
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
      <RTo.Viewport className="fixed bottom-4 right-4 z-[var(--z-toast)] flex flex-col gap-2 outline-none" />
    </RTo.Provider>
  );
}
