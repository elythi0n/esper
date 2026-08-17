import { Check, Copy } from "lucide-react";
import { useEffect, useState, type HTMLAttributes } from "react";
import { Button } from "./button";
import { Card } from "./card";
import { cn } from "@/lib/cn";
import { PERMISSION_LABEL, PERMISSIONS, type Permission } from "@/api/permissions";

/**
 * "482 917" — digits in groups of three, so a code is read aloud the way it
 * is shown.
 *
 * **A code that already carries its own separator is left alone.** The
 * engine formats a pairing code as `DX3N-3E9W`, and regrouping that into
 * threes puts a different string on screen from the one somebody has to
 * type at the other end — which is the one thing a pairing code may never
 * do. Found by pairing two machines and reading the screen.
 */
export function groupCode(code: string): string {
  if (/[^A-Za-z0-9]/.test(code.trim())) return code.trim();
  return code.replace(/\s+/g, "").replace(/(.{3})(?=.)/g, "$1 ");
}

/** "c07e 55d8 12aa" — a fingerprint in groups of four, the way keys are compared. */
export function groupFingerprint(hex: string): string {
  return hex.replace(/\s+/g, "").replace(/(.{4})(?=.)/g, "$1 ");
}

function mmss(ms: number): string {
  const s = Math.max(0, Math.ceil(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** A clock that re-renders once a second, in place. */
function useNow(active: boolean): number {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    if (!active) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [active]);
  return now;
}

export interface PairingCodeProps extends HTMLAttributes<HTMLDivElement> {
  code: string;
  /** When the code stops working. Shown as a countdown that updates in place. */
  expiresAt?: number | undefined;
}

/**
 * The pairing code, at `--text-code`: read across a room, or off a phone held
 * up to a screen, by somebody who has never seen this product. Nothing else
 * on any view is this big. The countdown is text that changes, not a bar that
 * shrinks — a number updating in place is on the never-animate list.
 */
export function PairingCode({ code, expiresAt, className, ...props }: PairingCodeProps) {
  const now = useNow(expiresAt !== undefined);
  const [copied, setCopied] = useState(false);
  const expired = expiresAt !== undefined && now >= expiresAt;

  useEffect(() => {
    if (!copied) return;
    const id = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(id);
  }, [copied]);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(code.replace(/\s+/g, ""));
      setCopied(true);
    } catch {
      // No clipboard here. The code is on screen; that is what it is for.
    }
  };

  return (
    <div className={cn("flex flex-col items-start gap-3", className)} {...props}>
      <output
        aria-label={`Pairing code ${code.replace(/[^A-Za-z0-9]/g, "").split("").join(" ")}`}
        className={cn("font-mono text-code tabular-nums", expired ? "text-text-disabled line-through" : "text-text")}
      >
        {groupCode(code)}
      </output>
      <div className="flex items-center gap-3 text-sm text-text-2">
        {expiresAt !== undefined && (
          <span className="tabular-nums">{expired ? "Expired — ask for a new code" : `Expires in ${mmss(expiresAt - now)}`}</span>
        )}
        <Button variant="ghost" size="sm" onClick={copy} disabled={expired}>
          {copied ? <Check aria-hidden size={14} /> : <Copy aria-hidden size={14} />}
          {copied ? "Copied" : "Copy"}
        </Button>
      </div>
    </div>
  );
}

export interface PairingPromptProps extends HTMLAttributes<HTMLDivElement> {
  /** The name the other device gives itself. Untrusted, so the fingerprint is shown beside it. */
  requester: { name: string; fingerprint: string };
  requested: readonly Permission[];
  expiresAt?: number | undefined;
  onAllow: () => void;
  onDeny: () => void;
}

/**
 * The host-side prompt: somebody wants in, and here is exactly what they
 * would be able to do. Each permission is spelled out in a sentence, because
 * "control" is not a word a person weighs and "use mouse and keyboard" is.
 * Deny is the plain button and Allow is the primary one: the person is being
 * asked to grant something, not warned off it. Doing nothing is a deny.
 */
export function PairingPrompt({ requester, requested, expiresAt, onAllow, onDeny, className, ...props }: PairingPromptProps) {
  const now = useNow(expiresAt !== undefined);
  const asked = PERMISSIONS.filter((p) => requested.includes(p));
  return (
    <Card role="alertdialog" aria-labelledby="pairing-prompt-title" className={cn("flex w-[min(28rem,100%)] flex-col gap-4 p-5", className)} {...props}>
      <div>
        <h3 id="pairing-prompt-title" className="text-lg font-medium text-text">
          {requester.name} wants to connect
        </h3>
        <p className="mt-1 font-mono text-xs text-text-3">{groupFingerprint(requester.fingerprint)}</p>
      </div>
      <ul className="flex flex-col gap-2">
        {asked.map((p) => (
          <li key={p} className="flex items-center gap-2 text-sm">
            <Check aria-hidden size={14} className="shrink-0 text-text-2" />
            <span className="text-text">{PERMISSION_LABEL[p]}</span>
          </li>
        ))}
      </ul>
      <div className="flex items-center justify-between gap-3 border-t border-border pt-4">
        <span className="text-xs text-text-3 tabular-nums">
          {expiresAt !== undefined ? `Closes in ${mmss(expiresAt - now)} if nothing is chosen` : "Nothing happens until you choose"}
        </span>
        <div className="flex gap-2">
          <Button onClick={onDeny}>Deny</Button>
          <Button variant="primary" onClick={onAllow}>
            Allow
          </Button>
        </div>
      </div>
    </Card>
  );
}
