import { Check, TicketPlus, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "@/api/client";
import { useStatus } from "@/api/hooks";
import type { Invitation } from "@/api/types";
import { Button } from "@/components/button";
import { CopyField } from "@/components/copy-field";
import { Field, Input } from "@/components/input";
import { EmptyState, Skeleton } from "@/components/states";
import { PageBody, Topbar } from "@/shell/AppShell";
import { Figure, Heading, Key, Trouble } from "./parts";

/**
 * Invite — a support link, or an explanation of why there is none.
 *
 * An invitation is a one-session pairing handed to somebody who is not on
 * this network, and reaching them needs a rendezvous. Esper operates no
 * infrastructure of its own, so unless the person running this configured a
 * broker there is nowhere for the two to meet — and the honest screen says
 * that rather than offering a button that cannot work.
 *
 * # Why it waits
 *
 * The engine does not mint a link and finish; it prints the link and then
 * holds a rendezvous open for whoever it was sent to. So this screen is the
 * same shape as showing a pairing code: started, polled, and either taken,
 * lapsed or stopped.
 */

/** The link, and the one thing anybody does with it. */
function TheLink({ link }: { link: string }) {
  return (
    <CopyField
      label="Send them this link"
      value={link}
      wrap
      className="max-w-lg rounded-md border border-border bg-surface-1 p-3"
    />
  );
}

function Offering({ broker }: { broker: string }) {
  const [who, setWho] = useState("");
  const [live, setLive] = useState<Invitation | null>(null);
  const [busy, setBusy] = useState(false);
  const [trouble, setTrouble] = useState<string | null>(null);
  const polling = useRef<ReturnType<typeof setInterval> | undefined>(undefined);

  const stopPolling = () => clearInterval(polling.current);
  useEffect(() => () => stopPolling(), []);

  // An invitation outlives the screen that made it, so what is already
  // running is picked up rather than a second one started over it.
  useEffect(() => {
    let alive = true;
    api
      .inviteState()
      .then((now) => {
        if (alive && now.state === "offering") setLive(now);
      })
      .catch(() => {
        // Nothing offered, which is the ordinary case.
      });
    return () => {
      alive = false;
    };
  }, []);

  const watch = useCallback(() => {
    stopPolling();
    polling.current = setInterval(async () => {
      try {
        const now = await api.inviteState();
        setLive(now.state === "none" ? null : now);
        if (now.state !== "offering") stopPolling();
      } catch {
        stopPolling();
      }
    }, 3000);
  }, []);

  const offer = async () => {
    setBusy(true);
    setTrouble(null);
    try {
      setLive(await api.invite(who.trim()));
      watch();
    } catch (cause) {
      setTrouble(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  const stop = async () => {
    stopPolling();
    setLive(null);
    try {
      await api.inviteCancel();
    } catch {
      // It expires on its own; nothing here needs to succeed.
    }
  };

  if (live?.state === "paired") {
    return (
      <div className="flex flex-col items-start gap-4 py-6">
        <p className="flex items-center gap-2 text-sm text-text">
          <Check aria-hidden size={16} className="text-ok" /> They accepted. You are now paired.
        </p>
        <Button size="sm" onClick={() => setLive(null)}>
          Make another
        </Button>
      </div>
    );
  }

  if (live?.state === "expired" || live?.state === "failed") {
    return (
      <div className="flex flex-col items-start gap-4 py-6">
        <p className="text-sm text-text-2">
          {live.state === "expired" ? "That invitation expired." : live.detail || "The invitation stopped working."}
        </p>
        <Button size="sm" onClick={() => setLive(null)}>
          Make another
        </Button>
      </div>
    );
  }

  if (live?.state === "offering") {
    return (
      <div className="flex flex-col items-start gap-6 py-6">
        <TheLink link={live.link} />

        <div className="flex max-w-lg flex-col gap-1.5">
          <p className="text-xs leading-relaxed text-text-3">
            Anyone with this link can pair with this machine until it expires. It allows one session
            and connects through {live.broker || broker}.
          </p>
        </div>

        <Button variant="ghost" size="sm" onClick={stop}>
          <X aria-hidden size={14} /> Cancel invitation
        </Button>
      </div>
    );
  }

  return (
    <form
      className="flex flex-col items-start gap-4 py-6"
      onSubmit={(e) => {
        e.preventDefault();
        void offer();
      }}
    >
      <Field label="Who it is for" hint="A label to tell invitations apart. It is not checked.">
        {(id, by) => (
          <Input
            id={id}
            aria-describedby={by}
            value={who}
            onChange={(e) => setWho(e.target.value)}
            placeholder="Dad"
            className="w-64"
          />
        )}
      </Field>

      {trouble && <Trouble detail={trouble}>Could not make an invitation.</Trouble>}

      <Button variant="primary" type="submit" disabled={busy}>
        {busy ? "Making…" : "Make an invitation"}
      </Button>
    </form>
  );
}

export function InviteScreen() {
  const { data, error, loading } = useStatus();

  return (
    <>
      <Topbar title="Invite" />
      <PageBody>
        {/*
          With no broker there is nothing to do here yet, so the screen is the
          empty state alone: centred in the window, with a title that says
          what this screen is for, rather than a heading over a note in the
          corner.
        */}
        {data && !data.broker ? (
          <div data-grow="true" data-empty="true" className="flex">
            <EmptyState
              icon={TicketPlus}
              title="Invite someone on another network"
              action={
                <Button size="sm" onClick={() => (window.location.hash = "/settings/connections")}>
                  Open connection settings
                </Button>
              }
            >
              Invitations need a broker: a small server you run on a computer both devices can reach, such as
              a cloud server. Run <span className="font-mono">esper broker serve</span> in a terminal on it, then
              add the address and key it prints in Settings.
            </EmptyState>
          </div>
        ) : (
          <div className="max-w-xl">
            <Heading>Invite someone on another network</Heading>

            {loading && <Skeleton className="mt-4 h-3 w-72" />}

            {error && <Trouble detail={error.message}>Could not reach Esper on this machine.</Trouble>}

            {data?.broker && (
              <>
                <div className="flex flex-col gap-2 border-b border-border py-4">
                  <Key>Broker</Key>
                  <Figure value={data.broker} />
                </div>
                <Offering broker={data.broker} />
              </>
            )}
          </div>
        )}
      </PageBody>
    </>
  );
}
