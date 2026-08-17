import { ArrowRight, Check } from "lucide-react";
import { useEffect, useState } from "react";
import { api } from "@/api/client";
import { useAsk } from "@/api/hooks";
import type { HostState } from "@/api/types";
import { Button } from "@/components/button";
import { CopyField } from "@/components/copy-field";
import { Identicon } from "@/components/identicon";
import { Skeleton } from "@/components/states";
import { Stepper, type Step } from "@/components/stepper";
import { Switch } from "@/components/switch";
import { PageBody, Topbar } from "@/shell/AppShell";
import { Fingerprint, Key, Trouble } from "./parts";

/**
 * Setting up this machine: who it is, whether it can be reached, and pairing.
 *
 * # Why this is a sequence and almost nothing else here is
 *
 * These three are asked **once**, in an order, and the second only makes
 * sense after the first: what a paired device connects *to* is this machine's
 * key, and turning the host on before knowing which machine you have turned
 * on is a switch flipped in the dark. Everything else in this product is a
 * screen somebody returns to, and a wizard around one of those is a screen
 * you have to escape.
 *
 * # Why it is reachable rather than forced
 *
 * A first run that appears once and never again is a first run nobody can
 * find the second time they need it — and "once" is a piece of state that has
 * to live somewhere, be wrong on a second machine, and be reset by hand when
 * somebody wants it back. So there is no "once": the Devices screen offers
 * this while nothing is paired, which is the honest signal for a machine that
 * has not been set up, and the route stays open afterwards.
 *
 * # What is missing, and why it is not drawn
 *
 * **A name.** The spec's first run asks for one and this does not, because
 * nothing writes `config.toml` — `device.name` is read from it and the engine
 * falls back to the hostname. A field that accepted a name and dropped it
 * would be worse than the absence, so what is here says where the name comes
 * from instead.
 */

function Identity() {
  const { data, error, loading } = useAsk(() => api.identity(), []);

  if (loading) return <Skeleton className="h-16 w-full max-w-md" />;
  if (error) return <Trouble detail={error.message}>This machine could not say who it is.</Trouble>;
  if (!data) return null;

  return (
    <div className="flex max-w-lg flex-col gap-5">
      <div className="flex items-center gap-4">
        <Identicon of={data.key} size={44} />
        <div className="flex min-w-0 flex-col gap-1">
          <Key>Key</Key>
          <Fingerprint hex={data.key} />
        </div>
      </div>

      <CopyField label="Read these aloud when you pair" value={data.words.join(" ")} wrap />

      <p className="text-xs leading-relaxed text-text-3">
        A pairing code proves the same code was typed at both ends. Only these words prove the machine at
        the other end is the one you meant — so they are the half worth saying out loud.
      </p>
    </div>
  );
}

function Reachable() {
  const [state, setState] = useState<HostState | null>(null);
  const [busy, setBusy] = useState(false);
  const [trouble, setTrouble] = useState<string | null>(null);
  const [said, setSaid] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    api
      .host()
      .then((now) => live && setState(now))
      .catch((cause: unknown) => live && setTrouble(cause instanceof Error ? cause.message : String(cause)));
    return () => {
      live = false;
    };
  }, []);

  if (trouble && !state) return <Trouble detail={trouble}>Whether this machine serves its screen is unknown.</Trouble>;
  if (!state) return <Skeleton className="h-10 w-full max-w-md" />;

  if (!state.managed) {
    return (
      <div className="flex max-w-lg flex-col gap-3">
        <p className="text-sm text-text-2">
          {state.why || "There is no service manager here that Esper turns the host on with."}
        </p>
        <p className="text-xs leading-relaxed text-text-3">
          Run <code className="font-mono text-text-2">esper host</code> and let whatever starts programs at
          login on this machine run it. Nothing here is blocked by this — a machine that only{" "}
          <em>connects</em> to others never needs it.
        </p>
      </div>
    );
  }

  const change = (on: boolean) => {
    setBusy(true);
    setTrouble(null);
    api
      .setHost(on)
      .then((now) => {
        setState(now);
        setSaid(now.said ?? null);
      })
      .catch((cause: unknown) => setTrouble(cause instanceof Error ? cause.message : String(cause)))
      .finally(() => setBusy(false));
  };

  return (
    <div className="flex max-w-lg flex-col gap-4">
      <div className="flex items-start justify-between gap-6 rounded-md border border-border p-4">
        <div className="flex min-w-0 flex-col gap-1">
          <p className="text-sm text-text">Let paired devices reach this machine</p>
          <p className="text-xs text-text-3">
            {state.active ? "Running now" : "Not running"} ·{" "}
            {state.enabled ? "starts at login" : "does not start at login"} · {state.manager}
          </p>
        </div>
        <Switch
          checked={state.enabled || state.active}
          disabled={busy}
          onCheckedChange={change}
          aria-label="Serve this machine's screen to paired devices"
        />
      </div>

      <p className="text-xs leading-relaxed text-text-3">
        Off is a perfectly good answer. A machine you only ever <em>connect from</em> — a laptop you use to
        reach a server — never serves anything, and leaving this off is one fewer thing listening.
      </p>

      {trouble && <Trouble detail={trouble}>That could not be changed.</Trouble>}
      {said && (
        <pre className="overflow-x-auto whitespace-pre-wrap break-words border-l-2 border-border py-2 pl-3 font-mono text-xs leading-relaxed text-text-3">
          {said}
        </pre>
      )}
    </div>
  );
}

function Pairing({ onPair }: { onPair: () => void }) {
  return (
    <div className="flex max-w-lg flex-col gap-4">
      <p className="text-sm text-text-2">
        Two machines pair once, with a short code read from one screen and typed on the other, and know each
        other by key from then on. There is no account and nothing to register.
      </p>
      <div className="flex items-center gap-2">
        <Button variant="primary" onClick={onPair}>
          Pair a device <ArrowRight aria-hidden size={14} />
        </Button>
      </div>
      <p className="text-xs leading-relaxed text-text-3">
        If the other machine is not on this network, an invitation is the other way in — one link, through a
        broker you run.
      </p>
    </div>
  );
}

export function WelcomeScreen({ onPair, onDone }: { onPair: () => void; onDone: () => void }) {
  const [at, setAt] = useState("who");

  const steps: readonly Step[] = [
    {
      id: "who",
      label: "This machine",
      about: "Its key is what other devices trust. Nothing here is set — this is what it already is.",
      content: <Identity />,
    },
    {
      id: "reachable",
      label: "Reachable",
      about: "Whether paired devices can connect to this machine's screen.",
      content: <Reachable />,
    },
    {
      id: "pair",
      label: "Pair",
      about: "The one exchange that has to happen at both ends.",
      content: <Pairing onPair={onPair} />,
    },
  ];

  return (
    <>
      <Topbar title="Set up this machine" />
      <PageBody>
        <Stepper
          steps={steps}
          at={at}
          onGo={setAt}
          done={
            <Button onClick={onDone}>
              <Check aria-hidden size={14} /> Done
            </Button>
          }
        />
      </PageBody>
    </>
  );
}
