import { Check, MailOpen, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "@/api/client";
import { offerIsCheckable } from "@/api/types";
import { Button } from "@/components/button";
import { Field, Input } from "@/components/input";
import { PairingCode } from "@/components/pairing";
import { PageBody, Topbar } from "@/shell/AppShell";
import { Heading, Key, Trouble } from "./parts";

/**
 * Pair — both directions, one screen, no terminal.
 *
 * The engine owns pairing in both directions and the control plane asks it,
 * exactly as it does for grant and revoke. What is different is that an
 * offer *waits*, so showing a code is a thing with a lifetime: it is started,
 * polled, and either paired, lapsed or cancelled.
 *
 * The fingerprint words are shown beside the code and are not decoration.
 * A code proves somebody typed what you read out; the words prove the machine
 * at the other end is the one you meant. Both halves, or neither.
 */

interface Offer {
  code: string;
  port: number;
  words: string[];
  expires_at: number;
  state: "offering" | "paired" | "expired" | "failed" | "none";
  detail?: string;
}

function Showing({ onDone }: { onDone: () => void }) {
  const [offer, setOffer] = useState<Offer | null>(null);
  const [busy, setBusy] = useState(false);
  const [trouble, setTrouble] = useState<string | null>(null);
  const polling = useRef<ReturnType<typeof setInterval> | undefined>(undefined);

  const stopPolling = () => clearInterval(polling.current);

  const start = useCallback(async () => {
    setBusy(true);
    setTrouble(null);
    try {
      const made = (await api.pairOffer()) as Offer;
      setOffer(made);
      stopPolling();
      polling.current = setInterval(async () => {
        try {
          const now = (await api.pairState()) as Offer;
          setOffer(now);
          if (now.state !== "offering") stopPolling();
        } catch {
          stopPolling();
        }
      }, 1500);
    } catch (cause) {
      setTrouble(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => () => stopPolling(), []);

  const cancel = async () => {
    stopPolling();
    setOffer(null);
    try {
      await api.pairCancel();
    } catch {
      // The offer expires on its own; nothing here needs to succeed.
    }
  };

  if (trouble) {
    return (
      <Trouble detail={trouble} action={<Button size="sm" onClick={start}>Try again</Button>}>
        A code could not be offered.
      </Trouble>
    );
  }

  if (!offer) {
    return (
      <div className="flex flex-col items-start gap-4 py-6">
        <p className="text-sm text-text-2">Show a code for somebody to type on the other machine.</p>
        <Button variant="primary" disabled={busy} onClick={start}>
          Show a code
        </Button>
      </div>
    );
  }

  if (offer.state === "paired") {
    return (
      <div className="flex flex-col items-start gap-4 py-6">
        <p className="flex items-center gap-2 text-sm text-text">
          <Check aria-hidden size={16} className="text-ok" /> Paired.
        </p>
        <Button size="sm" onClick={onDone}>
          See devices
        </Button>
      </div>
    );
  }

  if (offer.state === "expired" || offer.state === "failed") {
    return (
      <div className="flex flex-col items-start gap-4 py-6">
        <p className="text-sm text-text-2">
          {offer.state === "expired" ? "That code lapsed." : offer.detail || "The offer stopped."}
        </p>
        <Button size="sm" onClick={start}>
          Show another
        </Button>
      </div>
    );
  }

  /*
    **A code with no fingerprint is not shown at all.**

    The words are the half a code cannot do: a code proves somebody typed what
    you read out, and only the fingerprint proves the machine at the other end
    is the one you meant. Showing the code under an empty space where the
    words belong asks somebody to compare a blank — and a blank reads as
    "nothing to check", which is the one conclusion that must never be
    available here.

    The control plane refuses an offer that arrives without them, so this is
    the second of two locks on the same door. The way out is one button.
  */
  if (!offerIsCheckable(offer)) {
    return <UncheckableOffer onAgain={start} />;
  }

  return (
    <div className="flex flex-col items-start gap-6 py-6">
      {/* Exactly as the engine formats it: this is the string somebody types. */}
      <PairingCode code={offer.code} expiresAt={offer.expires_at * 1000} />

      <div className="flex flex-col gap-1.5">
        <Key>Read this aloud — it must match there</Key>
        <p className="max-w-sm break-words font-mono text-sm leading-relaxed text-text-2">{offer.words.join(" ")}</p>
      </div>

      <Button variant="ghost" size="sm" onClick={cancel}>
        <X aria-hidden size={14} /> Stop showing
      </Button>
    </div>
  );
}

/**
 * A code arrived without the fingerprint that goes with it, so it is not
 * shown.
 *
 * # Why this is its own component
 *
 * Because it is a state nothing renders. The gallery draws the real pairing
 * screen against a machine whose engine answers properly, so the words,
 * contrast and accessibility gates sweep the code and never this — and this is
 * the one state on that screen where the wrong design costs somebody a
 * machine.
 */
export function UncheckableOffer({ onAgain }: { onAgain: () => void }) {
  return (
    /*
      A left rule and a paragraph rather than the top-bar `Alarm`: that one
      holds a sentence that must survive a narrow window and therefore never
      wraps, which turns two sentences into one unbreakable line six hundred
      pixels past the edge of a phone. Found by `check-overflow`.
    */
    <div className="flex w-full min-w-0 flex-col items-start gap-3 border-l-2 border-danger py-2 pl-4">
      <p className="max-w-[var(--measure)] text-sm text-text">
        This machine offered a code without the fingerprint that goes with it, so there is nothing
        to read aloud and nothing to check.
      </p>
      <p className="max-w-[var(--measure)] text-sm leading-relaxed text-text-2">
        The code is not shown: a code proves that whoever typed it was read it, and only the
        fingerprint proves the machine at the other end is the one you meant.
      </p>
      <Button size="sm" onClick={onAgain}>
        Show another
      </Button>
    </div>
  );
}

function Entering({ onDone }: { onDone: () => void }) {
  const [address, setAddress] = useState("");
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [trouble, setTrouble] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  const join = async () => {
    setBusy(true);
    setTrouble(null);
    try {
      await api.pairJoin(address.trim(), code.trim().toUpperCase());
      setDone(true);
    } catch (cause) {
      setTrouble(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  if (done) {
    return (
      <div className="flex flex-col items-start gap-4 py-6">
        <p className="flex items-center gap-2 text-sm text-text">
          <Check aria-hidden size={16} className="text-ok" /> Paired.
        </p>
        <Button size="sm" onClick={onDone}>
          See devices
        </Button>
      </div>
    );
  }

  return (
    <form
      className="flex flex-col items-start gap-4 py-6"
      onSubmit={(e) => {
        e.preventDefault();
        void join();
      }}
    >
      <Field label="Where it is" hint="The address and port the other machine is showing.">
        {(id, by) => (
          <Input
            id={id}
            aria-describedby={by}
            value={address}
            onChange={(e) => setAddress(e.target.value)}
            placeholder="192.168.1.24:41748"
            className="w-64 font-mono"
          />
        )}
      </Field>

      <Field label="The code">
        {(id) => (
          <Input
            id={id}
            value={code}
            onChange={(e) => setCode(e.target.value.toUpperCase())}
            placeholder="XXXX-XXXX"
            className="w-40 font-mono uppercase tracking-[0.1em]"
          />
        )}
      </Field>

      {trouble && <Trouble detail={trouble}>That pairing did not complete.</Trouble>}

      <Button variant="primary" type="submit" disabled={busy || !address.trim() || !code.trim()}>
        {busy ? "Pairing…" : "Pair"}
      </Button>
    </form>
  );
}

/** An invitation, as a link. Rebuilt from the token so only the token travels. */
function linkFor(token: string): string {
  return `esper://join/${token}`;
}

/**
 * An invitation, arrived at deliberately.
 *
 * # Why a link does not pair the machine
 *
 * Following it could: the token is everything the exchange needs. It would
 * also make one click enough to trust a stranger, and establishing which key
 * to trust is the entire point of pairing — so the link opens *this*, with a
 * person in front of it, and the exchange happens when they say so.
 *
 * What is on screen is the token, grouped, because an invitation is a secret
 * somebody was sent and the only check available is that it looks like the
 * one they were sent.
 */
function Invited({ token, onDone }: { token: string; onDone: () => void }) {
  const [busy, setBusy] = useState(false);
  const [trouble, setTrouble] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  const accept = async () => {
    setBusy(true);
    setTrouble(null);
    try {
      await api.pairAccept(linkFor(token));
      setDone(true);
    } catch (cause) {
      setTrouble(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  if (done) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-5 py-20 text-center">
        <p className="flex items-center gap-2 text-base text-text">
          <Check aria-hidden size={18} className="text-ok" /> Paired.
        </p>
        <Button size="sm" onClick={onDone}>
          See devices
        </Button>
      </div>
    );
  }

  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-6 py-16 text-center">
      <span className="text-text-3/50" aria-hidden>
        <MailOpen size={28} strokeWidth={1} />
      </span>
      <div className="flex flex-col items-center gap-3">
        <p className="max-w-[var(--measure)] text-base text-text-2">
          Somebody has invited this machine to pair. Accepting meets them through the broker in the
          invitation — Esper operates none of its own.
        </p>
        <p className="max-w-md break-all font-mono text-xs leading-relaxed text-text-3">{token}</p>
      </div>

      {trouble && <Trouble detail={trouble}>That invitation could not be used.</Trouble>}

      <div className="flex items-center gap-2">
        <Button variant="primary" disabled={busy} onClick={accept}>
          {busy ? "Pairing…" : "Accept"}
        </Button>
        <Button variant="ghost" onClick={onDone}>
          Not now
        </Button>
      </div>
    </div>
  );
}

/** The same exchange, pasted rather than clicked — because not every link opens an app. */
function Pasting({ onDone }: { onDone: () => void }) {
  const [link, setLink] = useState("");
  const [busy, setBusy] = useState(false);
  const [trouble, setTrouble] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  const accept = async () => {
    setBusy(true);
    setTrouble(null);
    try {
      await api.pairAccept(link.trim());
      setDone(true);
    } catch (cause) {
      setTrouble(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  if (done) {
    return (
      <div className="flex flex-col items-start gap-4 py-6">
        <p className="flex items-center gap-2 text-sm text-text">
          <Check aria-hidden size={16} className="text-ok" /> Paired.
        </p>
        <Button size="sm" onClick={onDone}>
          See devices
        </Button>
      </div>
    );
  }

  return (
    <form
      className="flex flex-col items-start gap-4 py-6"
      onSubmit={(e) => {
        e.preventDefault();
        void accept();
      }}
    >
      <Field label="An invitation link" hint="Clicking one opens this window on it. Pasting it here does the same thing.">
        {(id, by) => (
          <Input
            id={id}
            aria-describedby={by}
            value={link}
            onChange={(e) => setLink(e.target.value)}
            placeholder="esper://join/…"
            className="w-full max-w-md font-mono"
          />
        )}
      </Field>

      {trouble && <Trouble detail={trouble}>That invitation could not be used.</Trouble>}

      <Button variant="primary" type="submit" disabled={busy || !link.trim()}>
        {busy ? "Pairing…" : "Accept"}
      </Button>
    </form>
  );
}

export function PairScreen({ onDone, invitation }: { onDone: () => void; invitation?: string | undefined }) {
  // Arriving on an invitation, the screen *is* the invitation. Two ways to
  // pair beside a decision somebody has been asked to make would be three
  // things competing for one answer.
  if (invitation) {
    return (
      <>
        <Topbar title="An invitation" />
        <PageBody>
          <div data-grow="true" data-empty="true" className="flex">
            <Invited token={invitation} onDone={onDone} />
          </div>
        </PageBody>
      </>
    );
  }

  return (
    <>
      <Topbar title="Pair" />
      <PageBody>
        <div className="grid gap-12 @[45rem]:grid-cols-2">
          <div>
            <Heading>Show a code</Heading>
            <Showing onDone={onDone} />
          </div>
          <div>
            <Heading>Enter one</Heading>
            <Entering onDone={onDone} />
          </div>
        </div>
        <div className="mt-4">
          <Heading>Or use an invitation</Heading>
          <Pasting onDone={onDone} />
        </div>
      </PageBody>
    </>
  );
}


