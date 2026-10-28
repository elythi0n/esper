import { Check, Copy } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Key } from "@/screens/parts";

/**
 * A value beside the one button anybody wants for it.
 *
 * # Why this is a component and not two elements
 *
 * Every value in this product that somebody has to *move* — a pairing
 * address, an invitation link, a fingerprint to read aloud, the path to a key
 * file — is a value they will otherwise select with a mouse and lose half of.
 * The button is not a nicety; it is the difference between a link that
 * arrives whole and one that arrives with the last character missing.
 *
 * It was written twice by hand before it was a component, and the second copy
 * had already drifted: one reset its "Copied" after two seconds and the other
 * never reset it at all.
 *
 * # What it promises
 *
 * **The value is always selectable.** The clipboard is a permission, and in a
 * webview it can be refused; a component whose only affordance is a button
 * would then have no affordance at all. So the text is real text, and the
 * button is the fast path.
 *
 * **It says what happened, briefly.** Two seconds of "Copied", then back —
 * long enough to be read, short enough that it is not still claiming it when
 * somebody returns to the screen a minute later.
 */

export interface CopyFieldProps {
  /** What it is, above the value. Omitted where the surroundings already say. */
  label?: ReactNode;
  /** The value itself, and exactly what goes on the clipboard. */
  value: string;
  /** Monospaced by default: these are addresses, keys and paths. */
  mono?: boolean;
  /** Wraps rather than truncating — for a long link, where the whole of it matters. */
  wrap?: boolean;
  className?: string | undefined;
}

export function CopyField({ label, value, mono = true, wrap = false, className }: CopyFieldProps) {
  const [copied, setCopied] = useState(false);
  const [refused, setRefused] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  useEffect(() => () => clearTimeout(timer.current), []);

  const copy = () => {
    const done = () => {
      setCopied(true);
      setRefused(false);
      clearTimeout(timer.current);
      timer.current = setTimeout(() => setCopied(false), 2000);
    };
    // `navigator.clipboard` is absent on an insecure origin and refused
    // without permission. Neither is a failure worth a dialog: the text is
    // on screen and selectable, and saying so is more use than an error.
    if (!navigator.clipboard) {
      setRefused(true);
      return;
    }
    navigator.clipboard.writeText(value).then(done, () => setRefused(true));
  };

  return (
    <div className={cn("flex min-w-0 flex-col gap-1.5", className)}>
      {label && <Key>{label}</Key>}
      <div className="flex min-w-0 items-start gap-2">
        <p
          className={cn(
            "min-w-0 flex-1 select-text text-sm text-text",
            mono && "font-mono text-xs leading-relaxed",
            // Breaks between words where there are words, and inside a long link only
            // where it must: "tomato" split as "t / omato" is a word nobody can read aloud.
            wrap ? "[overflow-wrap:anywhere]" : "truncate",
          )}
        >
          {value}
        </p>
        <button
          type="button"
          onClick={copy}
          data-tap-min=""
          aria-label={copied ? "Copied" : `Copy ${typeof label === "string" ? label.toLowerCase() : "this"}`}
          className={cn(
            "shrink-0 cursor-pointer rounded-sm p-1 text-text-3",
            "transition-colors duration-[var(--dur-fast)] hover:text-text",
          )}
        >
          {copied ? (
            <Check aria-hidden size={14} className="text-ok" />
          ) : (
            <Copy aria-hidden size={14} />
          )}
        </button>
      </div>
      {refused && (
        <p className="text-xs text-text-3">
          This window can't use the clipboard. Select the text above and copy it yourself.
        </p>
      )}
    </div>
  );
}
