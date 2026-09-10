import { ArrowRight, Infinity, MonitorSmartphone, Plus, SlidersHorizontal, Trash2, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "@/api/client";
import { useDevices, useReachable, useStatus } from "@/api/hooks";
import { PERMISSIONS, type Permission } from "@/api/permissions";
import { heldFrom, PermissionGrid } from "@/components/permission-grid";
import { asWhen, asWindow, readSchedule, type Window } from "@/api/schedule";
import { ScheduleInput } from "@/components/form-controls";
import { Input } from "@/components/input";
import { LOCK_AFTER, LOCK_AFTER_LABEL, LOCK_DEFAULT, pairingState, type Device, type LockAfter } from "@/api/types";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/select";
import { Button } from "@/components/button";
import { Dialog, DialogClose, DialogContent, DialogFooter, DialogTrigger } from "@/components/dialog";
import { CopyField } from "@/components/copy-field";
import { DeviceRow } from "@/components/device-row";
import { Identicon } from "@/components/identicon";
import type { MenuAction } from "@/components/menu";
import { EmptyState, Skeleton } from "@/components/states";
import { PermissionChecklist } from "@/screens/PermissionChecklist";
import { useToast } from "@/components/toast";
import { cn } from "@/lib/cn";
import { PageBody, Topbar } from "@/shell/AppShell";
import { Fact, Figure, Fingerprint, Heading, Key, Trouble } from "./parts";

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

  /*
    When that unattended access applies.

    Only offered for a device that *holds* it: a schedule on a device that
    cannot walk in unasked is a control that changes nothing, and a screen full
    of those is a screen where the ones that matter are harder to find.

    What is sent is the engine's own vocabulary, built by `asWhen` — one
    grammar, and the thing that decides what a schedule means is the parser
    that has always decided it.
  */
  const setWhen = async (window: Window) => {
    setBusy(true);
    try {
      const when = asWhen(window);
      await api.schedule(device.fingerprint.hex, when);
      toast.show({
        title:
          when === "always"
            ? `${device.name} may connect unasked at any time`
            : `${device.name} may connect unasked ${readSchedule(when) ?? when}`,
        tone: "ok",
      });
      onChanged();
    } catch (cause) {
      toast.show({ title: cause instanceof Error ? cause.message : String(cause), tone: "warn" });
    } finally {
      setBusy(false);
    }
  };

  /*
    What a session with this device leaves the screen as.

    Said back as a sentence rather than as the word that was sent: "never" on
    its own reads as a refusal, and what somebody has just chosen is that the
    screen is left alone.
  */
  const setLockAfter = async (chosen: string) => {
    const after = chosen === LOCK_DEFAULT ? LOCK_DEFAULT : (chosen as LockAfter);
    setBusy(true);
    try {
      await api.lockAfter(device.fingerprint.hex, after);
      toast.show({
        title:
          after === LOCK_DEFAULT
            ? `A session with ${device.name} leaves the screen as this machine is set to`
            : after === "never"
              ? `A session with ${device.name} leaves the screen as it is`
              : after === "always"
                ? `A session with ${device.name} always locks the screen`
                : `A session with ${device.name} locks the screen if nobody was here`,
        tone: "ok",
      });
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

  /*
    Wake it, if this machine has learned its hardware address.

    **What is said back is that the packet went out.** Nothing answers a magic
    packet — there is no reply to wait for and no way to tell a machine that
    woke from one that never heard — so a message saying "it is waking up"
    would be a message claiming something nobody knows. It says what happened
    and what to do next.
  */
  const wake = async () => {
    setBusy(true);
    try {
      await api.wake(device.fingerprint.hex);
      toast.show({
        title: `A wake-up packet went out to ${device.name}`,
        description: "Nothing answers one, so give it a moment and see whether it turns up on the network.",
        tone: "ok",
      });
      onChanged();
    } catch (cause) {
      toast.show({ title: cause instanceof Error ? cause.message : String(cause), tone: "warn" });
    } finally {
      setBusy(false);
    }
  };

  /*
    One helper for the three, because they differ only in the verb and the
    sentence. Each says what happened rather than what was asked for: a lock
    that the far host refused is not a lock, and the host's own words are what
    name the permission that is missing.
  */
  const reachOut = async (
    act: (fingerprint: string) => Promise<unknown>,
    done: string,
  ) => {
    setBusy(true);
    try {
      await act(device.fingerprint.hex);
      toast.show({ title: done, tone: "ok" });
      onChanged();
    } catch (cause) {
      toast.show({ title: cause instanceof Error ? cause.message : String(cause), tone: "warn" });
    } finally {
      setBusy(false);
    }
  };

  const lockThere = () => void reachOut(api.lockHost, `${device.name}'s screen is locked`);
  const logOffThere = () => void reachOut(api.logOffHost, `${device.name} was logged off`);
  const restartThere = () =>
    void reachOut(
      api.restartHost,
      // Not "it restarted": nothing answers a restart, and a window that
      // claimed it did would be claiming something nobody knows.
      `${device.name} was asked to restart. Nothing answers one, so watch for it coming back.`,
    );

  const joinGroup = async (name: string, remove: boolean) => {
    setBusy(true);
    try {
      await api.group(device.fingerprint.hex, name, remove);
      toast.show({
        title: remove ? `${device.name} is out of ${name}` : `${device.name} is in ${name}`,
        tone: "ok",
      });
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
          {/*
            **The design system's own grid**, not a second version of it.

            This screen had fourteen lamps of its own, with the "granted at the
            machine" rule in a `title` — invisible on a touchscreen and to
            anybody who does not hover. `PermissionGrid` was built in Phase 10
            for exactly this list, says "at the host" in words, and switches
            what a lamp could only be clicked. It reached no screen until now.
          */}
          <PermissionGrid
            value={heldFrom(device.permissions)}
            when={device.unattended_when}
            disabled={busy}
            onChange={(next) => {
              // The grid answers with the whole set; this screen grants and
              // revokes one at a time, because that is what the API does and
              // what the trail records.
              for (const p of PERMISSIONS) {
                const had = device.permissions.includes(p);
                if (had !== next.has(p)) {
                  void change(p, !had);
                  return;
                }
              }
            }}
          />

          {/*
            And the hours, for a device that may walk in at all. The control
            was built in Phase 10 and reached no screen: the only way to bound
            unattended access was `esper schedule` in a terminal, which is the
            thing this window exists to stop being necessary.
          */}
          {device.permissions.includes("unattended") && (
            <div data-when="" className="flex flex-col gap-2 pt-1">
              <Key>When it may connect unasked</Key>
              <ScheduleInput value={asWindow(device.unattended_when)} onChange={setWhen} disabled={busy} />
            </div>
          )}

          {/*
            And what a session with it leaves this machine's screen as.

            The host has one setting for everything, in `esper config`. This is
            the per-device answer for the machine that setting is wrong about —
            a laptop connecting to its owner's own desktop should not lock it
            under them. The engine has had the verb since the setting existed
            and nothing reached it, so the only way to say so was a terminal.

            "Follow this machine" is a real choice and not the absence of one:
            taking a per-device answer off has to be as easy as giving it.
          */}
          <div data-lock-after="" className="flex flex-col gap-2 pt-1">
            <Key>After a session with it</Key>
            <Select value={device.lock_after ?? LOCK_DEFAULT} onValueChange={setLockAfter} disabled={busy}>
              <SelectTrigger aria-label="After a session with it">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={LOCK_DEFAULT}>Follow this machine</SelectItem>
                {LOCK_AFTER.map((a) => (
                  <SelectItem key={a} value={a}>
                    {LOCK_AFTER_LABEL[a]}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
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

      {/*
        Wake it, for a device this machine has seen on the network before.

        Offered only when there is a hardware address to send to, because that
        is the whole of what makes it possible — a magic packet is six 0xFF
        bytes and the target's address sixteen times, and there is nothing to
        send without one. A button that was always there and failed for most
        devices would be a button that teaches people it does not work.

        The engine has had `wake` since Phase 12 and only a terminal could
        send one, so a machine asleep on the desk was one the window could
        list and not reach.
      */}
      {device.last_mac && (
        <div data-wake="" className="flex items-center justify-between gap-4 pt-2">
          <span className="min-w-0 text-xs text-text-3">
            Asleep on this network? Nothing answers a wake-up packet, so there is nothing to wait for.
          </span>
          <Button variant="ghost" size="sm" disabled={busy} onClick={wake} className="shrink-0">
            Wake it
          </Button>
        </div>
      )}

      {/*
        Which groups this device is in.

        A group is an address rather than a grant: `esper grant --group ops
        control` resolves the members and writes the permission onto each, so
        being in one gives a device nothing by itself. What it buys is a way
        to say "these machines" once instead of eight times.

        Shown even when empty, because a panel that hid the row would make
        "this device is in no groups" and "this build has no groups" look the
        same.
      */}
      <div data-groups="" className="flex flex-col gap-2 pt-4">
        <Key>Groups</Key>
        <div className="flex flex-wrap items-center gap-2">
          {(device.groups ?? []).map((name) => (
            <span
              key={name}
              className="flex items-center gap-1 rounded-sm border border-border bg-surface-1 py-0.5 pl-2 pr-1 text-xs text-text"
            >
              {name}
              <button
                type="button"
                aria-label={`Take ${device.name} out of ${name}`}
                disabled={busy}
                data-tap-min=""
                onClick={() => void joinGroup(name, true)}
                className="rounded-sm p-0.5 text-text-3 transition-[color] duration-[var(--dur-fast)] hover:text-danger disabled:text-text-disabled"
              >
                <X aria-hidden size={12} />
              </button>
            </span>
          ))}
          {(device.groups ?? []).length === 0 && <span className="text-xs text-text-3">None</span>}
          <NewGroup disabled={busy} onAdd={(name) => void joinGroup(name, false)} />
        </div>
      </div>

      {/*
        Three things this machine can do *to* that one.

        **Whether any of them is allowed is decided at the other end.** They
        are permissions that host granted this one, and this side keeps no
        copy of somebody else's trust store — so they are offered and the
        host's own answer is what is shown. Its sentence names the permission
        that is missing, which is the thing to do something about.

        Offered at all because the window could list a machine and not lock
        it: `esper lock` has existed since the permission did, and a terminal
        was the only thing that could send one.

        The two that outlive the session ask first. Locking a screen does not:
        the worst it costs is typing a password, and a confirmation for that
        is a dialog people learn to click through.
      */}
      {device.admits_a_session && (
        <div data-reaching="" className="flex flex-col gap-2 pt-4">
          <Key>On that machine</Key>
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="ghost" size="sm" disabled={busy} onClick={lockThere}>
              Lock its screen
            </Button>
            <AskFirst
              label="Log it off"
              title={`Log ${device.name} off?`}
              description="Anything open there is closed, and anybody using it is signed out. It is not undone from here."
              confirm="Log it off"
              disabled={busy}
              onConfirm={logOffThere}
            />
            <AskFirst
              label="Restart it"
              title={`Restart ${device.name}?`}
              description="The machine goes away and comes back. Nothing answers a restart, so there is no reply to wait for — and whether it returns depends on that machine, not this one."
              confirm="Restart it"
              danger
              disabled={busy}
              onConfirm={restartThere}
            />
          </div>
        </div>
      )}

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
              <DialogClose asChild>
                <Button>Keep</Button>
              </DialogClose>
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
/**
 * A small field for a group's name, folded away until it is wanted.
 *
 * Not a permanent text box: a device is in no groups on most machines, and a
 * row with an empty field in it reads as something half-finished. The button
 * is what says a group can be added; the field appears when somebody means it.
 */
function NewGroup({ disabled, onAdd }: { disabled: boolean; onAdd: (name: string) => void }) {
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState("");

  if (!open) {
    return (
      <Button variant="ghost" size="sm" disabled={disabled} onClick={() => setOpen(true)}>
        Add to a group
      </Button>
    );
  }

  const send = () => {
    const name = draft.trim();
    setOpen(false);
    setDraft("");
    if (name) onAdd(name);
  };

  return (
    <Input
      autoFocus
      value={draft}
      disabled={disabled}
      placeholder="Group name"
      aria-label="Group name"
      onChange={(e) => setDraft(e.target.value)}
      onBlur={send}
      onKeyDown={(e) => {
        if (e.key === "Enter") send();
        if (e.key === "Escape") {
          setOpen(false);
          setDraft("");
        }
      }}
      className="w-40"
    />
  );
}

/**
 * A button that asks before it does something that outlives the session.
 *
 * The same shape as "Forget this device" above, which is the only other thing
 * on this screen that cannot be undone from here — one dialog component, one
 * place the wording lives, and a reader who has seen one has seen both.
 */
function AskFirst({
  label,
  title,
  description,
  confirm,
  danger,
  disabled,
  onConfirm,
}: {
  label: string;
  title: string;
  description: string;
  confirm: string;
  danger?: boolean;
  disabled?: boolean;
  onConfirm: () => void;
}) {
  return (
    <Dialog>
      <DialogTrigger asChild>
        <Button variant="ghost" size="sm" disabled={disabled} className={danger ? "hover:text-danger" : undefined}>
          {label}
        </Button>
      </DialogTrigger>
      <DialogContent title={title} description={description}>
        <DialogFooter>
          {/*
            **Both wrapped in `DialogClose`.** A button inside a dialog is an
            ordinary button: it runs its handler and the dialog stays exactly
            where it was. So the one that means "no" did nothing at all — it
            looked like a cancel, and only the X in the corner and Escape
            actually worked — and the one that means "yes" left the dialog
            sitting over the thing it had just done.
          */}
          <DialogClose asChild>
            <Button>Leave it</Button>
          </DialogClose>
          <DialogClose asChild>
            <Button variant={danger ? "danger" : "primary"} disabled={disabled} onClick={onConfirm}>
              {confirm}
            </Button>
          </DialogClose>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

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
  initial,
}: {
  onConnect: (d: Device) => void;
  onPair: () => void;
  onSetUp: () => void;
  /** A device whose panel is open from the start, by its fingerprint's hex. */
  initial?: string | undefined;
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
  /*
    Which device's panel is open.

    `initial` opens one without a click. It is what the design gallery uses to
    render the panel at all — every control in it (the permission grid, the
    schedule, what a session leaves the screen as, wake, lock, log off,
    restart, groups) is behind a selection, so a gallery that rendered only
    the list was checking the densest control surface in the product not at
    all.
  */
  const [chosen, setChosen] = useState<string | null>(initial ?? null);

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


