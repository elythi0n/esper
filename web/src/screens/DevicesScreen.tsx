import { ArrowRight, Infinity, MonitorSmartphone, Plus, SlidersHorizontal, Trash2, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "@/api/client";
import { useDevices, useReachable, useStatus } from "@/api/hooks";
import { PERMISSION_LABEL, PERMISSIONS, SENSITIVE, type Permission } from "@/api/permissions";
import { pairingState, type Device } from "@/api/types";
import { Button } from "@/components/button";
import { Dialog, DialogContent, DialogFooter, DialogTrigger } from "@/components/dialog";
import { CopyField } from "@/components/copy-field";
import { DeviceRow } from "@/components/device-row";
import { Identicon } from "@/components/identicon";
import { Lamp } from "@/components/lamp";
import type { MenuAction } from "@/components/menu";
import { EmptyState, Skeleton } from "@/components/states";
import { PermissionChecklist } from "@/screens/PermissionChecklist";
import { useToast } from "@/components/toast";
import { cn } from "@/lib/cn";
import { PageBody, Topbar } from "@/shell/AppShell";
import { Fact, Figure, Fingerprint, Heading, Trouble } from "./parts";

/*
  Devices — the home, and the product.

  What is on screen is the shortest true answer: a name, when it was last
  used, and one click. Everything else about a device — its key, what it may
  do, where it was reached — is in the panel beside it, because a list where
  every row explains itself is a list nobody scans.

  # What this screen does not claim

  There is no reachability dot. The store knows when a device was last seen
  and whether its pairing still admits a session; it does not know whether the
  machine is up, and nothing in the local API probes. A green light that meant
  "paired" would be read as "reachable", and the first time somebody clicked a
  green device and waited thirty seconds for a timeout, the light would have
  lied. What is shown is what is known.
*/

/** "10 Sep 11:02" — short enough not to wrap in a panel column. */
function whenShort(unix: number): string {
  return new Date(unix * 1000).toLocaleString(undefined, {
    day: "numeric", month: "short", hour: "2-digit", minute: "2-digit",
  });
}

/** What a device may do, as fourteen lamps rather than fourteen sentences. */
function Permissions({ held, onChange }: { held: readonly Permission[]; onChange?: ((p: Permission, on: boolean) => void) | undefined }) {
  return (
    <div className="flex flex-col gap-1">
      {PERMISSIONS.map((p) => {
        const on = held.includes(p);
        const locked = SENSITIVE.has(p);
        const label = PERMISSION_LABEL[p];
        return (
          <button
            key={p}
            type="button"
            disabled={!onChange || locked}
            title={locked ? "Only somebody at that machine can grant this" : undefined}
            onClick={() => onChange?.(p, !on)}
            className={cn(
              "flex items-center gap-2 text-left text-xs",
              "transition-[color] duration-[var(--dur-fast)]",
              on ? "text-text" : "text-text-3",
              onChange && !locked && "cursor-pointer hover:text-text",
              !onChange || locked ? "cursor-default" : "",
            )}
          >
            <Lamp state={on ? (locked ? "warn" : "ok") : "off"} className="gap-2">
              <span className="truncate">{label}</span>
            </Lamp>
          </button>
        );
      })}
    </div>
  );
}

function Detail({ device, onClose, onChanged, onConnect }: { device: Device; onClose: () => void; onChanged: () => void; onConnect: () => void }) {
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const state = pairingState(device);

  const change = async (p: Permission, on: boolean) => {
    setBusy(true);
    try {
      await (on ? api.grant(device.fingerprint.hex, [p]) : api.revoke(device.fingerprint.hex, [p]));
      onChanged();
    } catch (cause) {
      toast.show({ title: cause instanceof Error ? cause.message : String(cause), tone: "warn" });
    } finally {
      setBusy(false);
    }
  };

  const keep = async () => {
    setBusy(true);
    try {
      await api.keep(device.fingerprint.hex);
      toast.show({ title: `${device.name} stays paired`, tone: "ok" });
      onChanged();
    } catch (cause) {
      toast.show({ title: cause instanceof Error ? cause.message : String(cause), tone: "warn" });
    } finally {
      setBusy(false);
    }
  };

  const forget = async () => {
    setBusy(true);
    try {
      await api.forget(device.fingerprint.hex);
      toast.show({ title: `${device.name} is no longer paired.` });
      onClose();
      onChanged();
    } catch (cause) {
      toast.show({ title: cause instanceof Error ? cause.message : String(cause), tone: "warn" });
    } finally {
      setBusy(false);
    }
  };

  return (
    <aside className="flex h-full min-w-0 flex-col gap-6 overflow-y-auto overflow-x-hidden border-l border-border pl-6">
      <div className="flex items-start justify-between gap-4">
        <h2 className="flex min-w-0 items-center gap-3 font-display text-2xl font-medium text-text">
          {/* The key drawn beside the name, the same picture the row shows. */}
          <Identicon of={device.fingerprint.hex} size={28} />
          <span className="min-w-0 truncate">{device.name}</span>
        </h2>
        <button type="button" aria-label="Close" onClick={onClose} className="shrink-0 text-text-3 hover:text-text" data-tap-min="">
          <X aria-hidden size={16} />
        </button>
      </div>

      <Button variant="primary" disabled={state === "spent"} onClick={onConnect}>
        Connect
        <ArrowRight aria-hidden size={14} />
      </Button>

      <Fact label="Key">
        <Fingerprint hex={device.fingerprint.hex} />
        {/* The words are read aloud *and* pasted into a message. */}
        <CopyField value={device.fingerprint.words.join(" ")} wrap className="mt-1.5" />
      </Fact>

      {/*
        A one-session pairing is spent when the session ends, and until the
        API grew `keep` the only way to change that was a terminal — which is
        the thing this window exists to stop being necessary.
      */}
      {state !== "kept" && (
        <Fact label="Pairing">
          <p className="text-sm text-text-2">
            {state === "once"
              ? "Made for one session. It is spent when that session ends."
              : "Spent. This device has to pair again before it can connect."}
          </p>
          <Button size="sm" className="mt-2" disabled={busy} onClick={keep}>
            <Infinity aria-hidden size={14} /> Make it last
          </Button>
        </Fact>
      )}

      <Fact label="May">
        <div className={cn("mt-1", busy && "opacity-60")}>
          <Permissions held={device.permissions} onChange={change} />
        </div>
      </Fact>

      <div className="grid grid-cols-2 gap-4">
        <Fact label="Last seen">
          <Figure value={device.last_seen ? whenShort(device.last_seen) : "Never"} className="text-xs" />
        </Fact>
        <Fact label="Paired">
          <Figure value={whenShort(device.paired_at)} className="text-xs" />
        </Fact>
        {device.last_address && (
          <Fact label="Address">
            <Figure value={device.last_address} className="truncate text-xs" />
          </Fact>
        )}
        {device.overlay_address && (
          <Fact label="Overlay">
            <Figure value={device.overlay_address} className="text-xs" />
          </Fact>
        )}
        <Fact label="Pairing">
          <Figure value={state} className="text-xs" />
        </Fact>
      </div>

      <div className="mt-auto pt-4">
        <Dialog>
          <DialogTrigger asChild>
            <Button variant="ghost" size="sm" className="text-text-3 hover:text-danger">
              Forget this device
            </Button>
          </DialogTrigger>
          <DialogContent
            title={`Forget ${device.name}?`}
            description="It has to be paired again before it can connect. Nothing on that machine changes."
          >
            <DialogFooter>
              <Button>Keep</Button>
              <Button variant="danger" disabled={busy} onClick={forget}>
                Forget
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      </div>
    </aside>
  );
}

/**
 * What a device's menu offers, built once for both ways of reaching it.
 *
 * # Why forgetting is undoable and nothing else is
 *
 * Three of these are reversible by doing them again. Forgetting is not — the
 * store loses the key, and the far machine has to pair again with somebody
 * reading a code aloud — so it is the one that carries an undo, and the undo
 * is honest about what it can do: **it re-pairs nothing.** It cannot; pairing
 * is an exchange between two machines. What it does is put the *permissions*
 * back if the removal has not yet reached the store, which is the window
 * between the click and the engine's write, and say so if it is too late.
 *
 * That is a narrow promise and it is the true one. A button labelled "Undo"
 * that silently did less than the word implies would be worse than no button.
 */
function actionsFor({
  device,
  onConnect,
  onDetails,
  onKeep,
  onForget,
}: {
  device: Device;
  onConnect: () => void;
  onDetails: () => void;
  onKeep: () => void;
  onForget: () => void;
}): MenuAction[] {
  const state = pairingState(device);
  return [
    { label: "Connect", icon: ArrowRight, onSelect: onConnect, disabled: state === "spent" },
    { label: "Permissions…", icon: SlidersHorizontal, onSelect: onDetails },
    {
      label: state === "kept" ? "Pairing already lasts" : "Make the pairing last",
      icon: Infinity,
      onSelect: onKeep,
      // Already lasting is not a failure to report; it is a menu item with
      // nothing to do.
      disabled: state === "kept",
    },
    { label: "Forget this device", icon: Trash2, onSelect: onForget, danger: true, apart: true },
  ];
}

export function DevicesScreen({
  onConnect,
  onPair,
  onSetUp,
}: {
  onConnect: (d: Device) => void;
  onPair: () => void;
  onSetUp: () => void;
}) {
  const { data, error, loading, reload } = useDevices();
  // What this machine can do, for the checklist below — asked here rather
  // than inside it so a screen with nothing to say makes no request of its
  // own on every render.
  const { data: machine } = useStatus();
  const { seen, looked } = useReachable();
  const toast = useToast();
  const [busy, setBusy] = useState<string | null>(null);

  const keep = useCallback(
    (d: Device) => {
      setBusy(d.fingerprint.hex);
      api
        .keep(d.fingerprint.hex)
        .then(() => {
          reload();
          toast.show({ title: `${d.name} stays paired`, tone: "ok" });
        })
        .catch((cause: unknown) =>
          toast.show({
            title: `${d.name}'s pairing could not be made to last`,
            description: cause instanceof Error ? cause.message : String(cause),
            tone: "danger",
          }),
        )
        .finally(() => setBusy(null));
    },
    [reload, toast],
  );

  const forget = useCallback(
    (d: Device) => {
      const had = d.permissions;
      setBusy(d.fingerprint.hex);
      api
        .forget(d.fingerprint.hex)
        .then(() => {
          reload();
          toast.show({
            title: `${d.name} was forgotten`,
            description: "It has to pair again before it can connect at all.",
            tone: "warn",
            // **Honest about what it can do.** Pairing is an exchange between
            // two machines and no button here can redo it; what this offers
            // is the permissions back, and only while the device is still
            // there to grant them to.
            action: {
              label: "Put its permissions back",
              run: () => {
                api
                  .grant(d.fingerprint.hex, had)
                  .then(() => {
                    reload();
                    toast.show({ title: `${d.name}'s permissions are back`, tone: "ok" });
                  })
                  .catch(() =>
                    toast.show({
                      title: "Too late to put them back",
                      description:
                        "The device is gone from the store. It has to pair again, and the permissions are granted then.",
                      tone: "warn",
                    }),
                  );
              },
            },
          });
        })
        .catch((cause: unknown) =>
          toast.show({
            title: `${d.name} could not be forgotten`,
            description: cause instanceof Error ? cause.message : String(cause),
            tone: "danger",
          }),
        )
        .finally(() => setBusy(null));
    },
    [reload, toast],
  );
  const [chosen, setChosen] = useState<string | null>(null);

  const devices = useMemo(
    () => [...(data ?? [])].sort((a, b) => (b.last_seen ?? 0) - (a.last_seen ?? 0)),
    [data],
  );
  const selected = devices.find((d) => d.fingerprint.hex === chosen) ?? null;

  // A device that was forgotten should not leave a panel behind.
  useEffect(() => {
    if (chosen && data && !data.some((d) => d.fingerprint.hex === chosen)) setChosen(null);
  }, [data, chosen]);

  return (
    <>
      <Topbar
        title="Devices"
        actions={
          <Button variant="primary" size="sm" onClick={onPair}>
            <Plus aria-hidden size={14} /> Pair
          </Button>
        }
      />
      <PageBody>
        <div
          data-grow="true"
          data-empty={!loading && !error && devices.length === 0}
          className={cn("grid min-h-0 flex-1 gap-6", selected && "@[45rem]:grid-cols-[1fr_24rem]")}
        >
          <div className="flex min-w-0 flex-col">
            {(loading || devices.length > 0 || error) && (
              <Heading count={loading ? "" : devices.length}>Paired</Heading>
            )}

            {loading && (
              <div className="flex flex-col gap-px">
                {[0, 1, 2].map((i) => (
                  <div key={i} className="flex items-center gap-4 border-b border-border py-4">
                    <Skeleton className="size-4" />
                    <Skeleton className="h-4 w-40" />
                    <Skeleton className="ml-auto h-3 w-8" />
                  </div>
                ))}
              </div>
            )}

            {error && (
              <Trouble
                detail={error.message}
                action={
                  <Button size="sm" onClick={reload}>
                    Try again
                  </Button>
                }
              >
                The device list could not be read.
              </Trouble>
            )}

            {/*
              **The first run, which is a machine with nothing paired.**

              A checklist of what this machine still needs somebody to do,
              shown here because this is where a person lands and because a
              machine with no devices is the machine that has not been set up
              — the honest signal, and one that needs no "have they seen this
              yet" stored anywhere. It is not drawn at all when there is
              nothing to do.
            */}
            {!loading && !error && devices.length === 0 && machine && (
              <PermissionChecklist
                className="mb-4"
                platform={machine.platform ?? { os: "", display: "" }}
                have={{ capture: machine.capture.available, input: machine.input.available }}
              />
            )}

            {!loading && !error && devices.length === 0 && (
              <EmptyState
                icon={MonitorSmartphone}
                action={
                  <span className="flex flex-wrap items-center justify-center gap-2">
                    <Button variant="primary" size="sm" onClick={onPair}>
                      <Plus aria-hidden size={14} /> Pair a device
                    </Button>
                    {/*
                      A machine with nothing paired is the machine that has
                      not been set up — the honest signal, and one that needs
                      no "have they seen this yet" stored anywhere.
                    */}
                    <Button size="sm" onClick={onSetUp}>
                      Set up this machine
                    </Button>
                  </span>
                }
              >
                No devices paired yet. Pair one and it appears here, ready to connect in one click.
              </EmptyState>
            )}

            {devices.map((d) => (
              <DeviceRow
                key={d.fingerprint.hex}
                device={d}
                here={d.fingerprint.hex in seen}
                busy={busy === d.fingerprint.hex}
                actions={actionsFor({
                  device: d,
                  onConnect: () => onConnect(d),
                  onDetails: () => setChosen(d.fingerprint.hex),
                  onKeep: () => keep(d),
                  onForget: () => forget(d),
                })}
                selected={d.fingerprint.hex === chosen}
                onSelect={() => setChosen(d.fingerprint.hex === chosen ? null : d.fingerprint.hex)}
                onConnect={() => onConnect(d)}
              />
            ))}

            {/*
              What presence here actually means, said once and quietly.

              Only the LAN rung announces itself. Without this line a device
              with no dot reads as "not reachable", which is a much stronger
              claim than "we did not look there" — and the wrong one for a
              machine that lives behind a broker.
            */}
            {looked && devices.length > 0 && (
              <p className="pt-6 text-xs leading-relaxed text-text-3">
                Presence is {looked} only. A device reachable over an overlay network, at an address, or
                through a broker does not announce itself, and connecting still works.
              </p>
            )}
          </div>

          {selected && (
            <Detail
              device={selected}
              onClose={() => setChosen(null)}
              onChanged={reload}
              onConnect={() => onConnect(selected)}
            />
          )}
        </div>
      </PageBody>
    </>
  );
}


