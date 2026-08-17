import { useEffect, useState } from "react";
import { api } from "@/api/client";
import { useAsk, useStatus } from "@/api/hooks";
import type { Capability, HostState } from "@/api/types";
import { Skeleton } from "@/components/states";
import { CopyField } from "@/components/copy-field";
import { Identicon } from "@/components/identicon";
import { Lamp, lampFor } from "@/components/lamp";
import { Said, SettingRow } from "@/components/setting";
import { Switch } from "@/components/switch";
import { MotionToggle, ThemeToggle } from "@/components/theme-toggle";
import { readAutostart, setAutostart, type Autostart } from "@/sessions/shell";
import { PageBody, Topbar } from "@/shell/AppShell";
import { Figure, Fingerprint, Heading, Key, Row, Trouble } from "./parts";

/**
 * Settings — every value with its effective source.
 *
 * Two halves, and they are different kinds of thing. What this machine *can
 * do* is the engine's answer and is read-only here: it is a measurement, not
 * a preference, and a screen that let somebody toggle "capture" would be
 * offering to change the world by editing the report of it. What this window
 * *looks like* is a preference, and lives in the browser.
 */

function CapabilityRow({ label, cap }: { label: string; cap: Capability }) {
  return (
    <Row className="items-baseline py-2.5">
      <span className="w-24 shrink-0">
        <Key>{label}</Key>
      </span>
      <Lamp state={lampFor(cap.available)} className="min-w-0 flex-1 items-baseline">
        <span className="min-w-0 flex-1 font-mono text-xs text-text">{cap.detail || (cap.available ? "yes" : "no")}</span>
      </Lamp>
    </Row>
  );
}

function Line({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <Row className="items-baseline py-2.5">
      <span className="w-24 shrink-0">
        <Key>{label}</Key>
      </span>
      <span className="min-w-0 flex-1 text-sm text-text">{children}</span>
    </Row>
  );
}

/**
 * What this machine is, and the words for saying so out loud.
 *
 * On the Settings screen rather than in a first-run wizard, and that is the
 * decision: a wizard is a screen somebody sees once, and the fingerprint is
 * the thing they need on the day something looks wrong. The words are not
 * decoration — a code proves the same code was typed at both ends, and only
 * these prove the machine at the other end is the one that was meant.
 */
function ThisIdentity() {
  const { data, error, loading } = useAsk(() => api.identity(), []);

  if (loading) return <Skeleton className="mt-2 h-3 w-64" />;
  if (error) return <Trouble detail={error.message}>This machine could not say who it is.</Trouble>;
  if (!data) return null;

  return (
    <>
      <Row className="items-center py-2.5">
        <span className="w-24 shrink-0">
          <Key>Key</Key>
        </span>
        <span className="flex min-w-0 flex-1 items-center gap-3">
          <Identicon of={data.key} size={28} />
          <Fingerprint hex={data.key} className="min-w-0" />
        </span>
      </Row>
      <Row className="items-start py-2.5">
        <span className="w-24 shrink-0 pt-0.5">
          <Key>Words</Key>
        </span>
        <span className="flex min-w-0 flex-1 flex-col gap-1">
          <CopyField value={data.words.join(" ")} wrap />
          <span className="text-xs leading-relaxed text-text-3">
            Read these aloud when you pair. A code proves the same code was typed; only these prove the
            machine at the other end is the one you meant.
          </span>
        </span>
      </Row>
      {data.file && (
        <Row className="items-baseline py-2.5">
          <span className="w-24 shrink-0">
            <Key>Key file</Key>
          </span>
          {/* A path somebody is about to paste into a terminal. */}
          <CopyField value={data.file} className="min-w-0 flex-1" />
        </Row>
      )}
    </>
  );
}

/**
 * Whether this machine serves its screen.
 *
 * # Why a switch here rather than a command
 *
 * Turning the host on is most of what this product is *for*, and it has to
 * work by being pressed: the alternative is asking somebody to copy four
 * commands out of a comment block, one of which — `enable-linger` — is the
 * one everybody misses and whose absence looks exactly like Esper crashing
 * whenever they disconnect.
 *
 * **It is not a black box.** Every command the change ran is shown, so a
 * machine where it half-worked is a machine somebody can finish by hand, and
 * the next one they can do without this.
 *
 * Two lines of state, because "running now" and "starts at login" are
 * different facts and a host that has only the first will be gone after a
 * reboot.
 */
function Serving() {
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
  if (!state) return <Skeleton className="mt-2 h-3 w-48" />;

  if (!state.managed) {
    return (
      <SettingRow
        label="Serving"
        state={
          <>
            {state.why || "There is no service manager here that Esper turns the host on with."} Run{" "}
            <code className="font-mono text-text-2">esper host</code> and let whatever starts programs at
            login on this machine run it.
          </>
        }
      />
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
    <SettingRow
      label="Serving"
      state={
        <>
          {state.active ? "Running now" : "Not running"}
          {" · "}
          {state.enabled ? "starts at login" : "does not start at login"}
          {" · "}
          {state.manager}
        </>
      }
      control={
        <Switch
          checked={state.enabled || state.active}
          disabled={busy}
          onCheckedChange={change}
          aria-label="Serve this machine's screen to paired devices"
        />
      }
      trouble={trouble ? <Trouble detail={trouble}>That could not be changed.</Trouble> : undefined}
      said={said ? <Said>{said}</Said> : undefined}
    />
  );
}

/**
 * Opening at login, which is a fact about the machine rather than a value in
 * this window's storage — so the switch reads the file every time and shows
 * where it is. A person who wants it gone can delete that file, and the
 * switch will agree with them.
 *
 * Not drawn where the platform has nowhere to put one: a switch that refuses
 * is worse than a sentence saying it is not built yet.
 */
function AtLogin() {
  const [state, setState] = useState<Autostart | null>(null);
  const [busy, setBusy] = useState(false);
  const [trouble, setTrouble] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    void readAutostart().then((now) => live && setState(now));
    return () => {
      live = false;
    };
  }, []);

  if (!state || !state.available) return null;

  const change = (on: boolean) => {
    setBusy(true);
    setTrouble(null);
    setAutostart(on)
      .then(setState)
      .catch((cause: unknown) => setTrouble(cause instanceof Error ? cause.message : String(cause)))
      .finally(() => setBusy(false));
  };

  return (
    <SettingRow
      label="At login"
      state={state.path ? <span className="block truncate font-mono">{state.path}</span> : undefined}
      control={
        <Switch checked={state.on} disabled={busy} onCheckedChange={change} aria-label="Open Esper when I log in" />
      }
      trouble={trouble ? <Trouble detail={trouble}>That could not be changed.</Trouble> : undefined}
    />
  );
}

export function SettingsScreen() {
  const { data, error, loading } = useStatus();

  return (
    <>
      <Topbar title="Settings" />
      <PageBody>
        <div className="grid gap-10 @[45rem]:grid-cols-2">
          <div className="min-w-0">
            <Heading>This machine</Heading>
            <ThisIdentity />
            <Serving />

            {loading && (
              <div className="flex flex-col gap-3 py-2">
                {[0, 1, 2, 3].map((i) => (
                  <Skeleton key={i} className="h-3 w-full max-w-sm" />
                ))}
              </div>
            )}

            {error && (
              <Trouble detail={error.message}>
                The engine did not answer, so what this machine can do is unknown.
              </Trouble>
            )}

            {data && (
              <>
                <CapabilityRow label="Capture" cap={data.capture} />
                <CapabilityRow label="Encode" cap={data.encoder} />
                <CapabilityRow label="Input" cap={data.input} />
                <Line label="Engine">
                  <Figure value={data.engine} className="text-xs" />
                </Line>
                <Line label="Overlays">
                  {data.overlays.length ? (
                    <Figure value={data.overlays.join(" · ")} className="text-xs" />
                  ) : (
                    <span className="text-text-3">None found</span>
                  )}
                </Line>
                <Line label="Local">
                  {data.mdns ? "Devices on this network can be found" : <span className="text-text-3">Not announcing</span>}
                </Line>
                <Line label="Broker">
                  {data.broker ? (
                    <Figure value={data.broker} className="text-xs" />
                  ) : (
                    <span className="text-text-3">None. Esper operates no infrastructure.</span>
                  )}
                </Line>
              </>
            )}
          </div>

          <div className="min-w-0">
            <Heading>This window</Heading>
            <SettingRow label="Theme" control={<ThemeToggle />} />
            <SettingRow label="Motion" control={<MotionToggle />} />
            <AtLogin />
          </div>
        </div>
      </PageBody>
    </>
  );
}
