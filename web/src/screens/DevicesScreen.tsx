import { ArrowRight, Infinity, Lock, LogOut, MonitorSmartphone, Plus, Power, RotateCcw, SlidersHorizontal, Trash2, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
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
import { Badge } from "@/components/badge";
import { DeviceRow, kindOf } from "@/components/device-row";
import { Identicon } from "@/components/identicon";
import type { MenuAction } from "@/components/menu";
import { EmptyState, Skeleton } from "@/components/states";
import { PermissionChecklist } from "@/screens/PermissionChecklist";
import { useToast } from "@/components/toast";
import { cn } from "@/lib/cn";
import { PageBody, Topbar } from "@/shell/AppShell";
import { Fingerprint, Heading, Trouble } from "./parts";
import { TimeAgo } from "@/components/time-ago";
import { Tooltip } from "@/components/tooltip";
import { shortAgoAt } from "@/lib/ago";

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

  /*
    **What was just chosen, shown before the store says it back.**

    A switch or a select that waited for the reload stood still, then
    jumped, and the whole card dimmed and disabled around it while it waited:
    a flicker on every change. So the choice shows at once, the controls stay
    as they are, and the next read of the store (a new `device`) replaces it
    with the truth, which is also what undoes it if the change was refused.
  */
  const [shown, setShown] = useState<{ perms?: Permission[]; lock?: string }>({});
  useEffect(() => setShown({}), [device]);
  const perms = shown.perms ?? device.permissions;

  const change = async (p: Permission, on: boolean) => {
    setShown((was) => ({ ...was, perms: on ? [...perms, p] : perms.filter((x) => x !== p) }));
    try {
      await (on ? api.grant(device.fingerprint.hex, [p]) : api.revoke(device.fingerprint.hex, [p]));
      onChanged();
    } catch (cause) {
      setShown(({ perms: _, ...rest }) => rest);
      toast.show({ title: cause instanceof Error ? cause.message : String(cause), tone: "warn" });
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
    try {
      const when = asWhen(window);
      await api.schedule(device.fingerprint.hex, when);
      toast.show({
        title:
          when === "always"
            ? `${device.name} can connect without asking, at any time`
            : `${device.name} can connect without asking ${readSchedule(when) ?? when}`,
        tone: "ok",
      });
      onChanged();
    } catch (cause) {
      toast.show({ title: cause instanceof Error ? cause.message : String(cause), tone: "warn" });
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
    setShown((was) => ({ ...was, lock: chosen }));
    try {
      await api.lockAfter(device.fingerprint.hex, after);
      toast.show({
        title:
          after === LOCK_DEFAULT
            ? `After a session with ${device.name}, this machine's usual setting applies`
            : after === "never"
              ? `After a session with ${device.name}, the screen stays unlocked`
              : after === "always"
                ? `After a session with ${device.name}, the screen locks`
                : `After a session with ${device.name}, the screen locks if nobody was here`,
        tone: "ok",
      });
      onChanged();
    } catch (cause) {
      setShown(({ lock: _, ...rest }) => rest);
      toast.show({ title: cause instanceof Error ? cause.message : String(cause), tone: "warn" });
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
        title: `Sent a wake-up signal to ${device.name}`,
        description: "Give it a minute, then check whether it comes online.",
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
      `${device.name} was asked to restart. Check that it comes back.`,
    );

  const joinGroup = async (name: string, remove: boolean) => {
    setBusy(true);
    try {
      await api.group(device.fingerprint.hex, name, remove);
      toast.show({
        title: remove ? `Removed ${device.name} from ${name}` : `Added ${device.name} to ${name}`,
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

  const Icon = kindOf(device.name);
  const seen = device.last_seen === null ? null : <TimeAgo date={device.last_seen * 1000} format={shortAgoAt} />;

  return (
    <aside
      aria-label={`${device.name}, details`}
      // Beside the list it scrolls on its own; under it, on a phone, it is
      // just more of the page.
      className="flex min-w-0 flex-col gap-5 overflow-x-clip border-t border-border pb-6 pt-6 [&>*]:shrink-0 @[45rem]:h-full @[45rem]:overflow-y-auto @[45rem]:border-l @[45rem]:border-t-0 @[45rem]:pl-6 @[45rem]:pt-0"
    >
      {/*
        Who it is, said the way the row says it, one size up: the same tile,
        the same key drawn small, and where the pairing stands in words.
      */}
      <header className="flex items-start gap-3.5">
        <span className="flex size-12 shrink-0 items-center justify-center rounded-xl border border-border bg-text/[0.05] text-text-2">
          <Icon aria-hidden size={20} strokeWidth={1.6} />
        </span>
        <div className="flex min-w-0 flex-1 flex-col gap-1 pt-0.5">
          <h2 className="truncate font-display text-xl font-medium leading-tight text-text">{device.name}</h2>
          <p className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-xs text-text-3">
            <Identicon of={device.fingerprint.hex} size={12} />
            <span>{seen === null ? "Never seen" : <>Seen {seen} ago</>}</span>
            {state !== "kept" && <Badge tone={state === "spent" ? "neutral" : "info"}>{PAIRING_BADGE[state]}</Badge>}
          </p>
        </div>
        <button
          type="button"
          aria-label="Close"
          onClick={onClose}
          className="-mr-1 inline-flex size-8 shrink-0 items-center justify-center rounded-md text-text-3 transition-colors duration-[var(--dur-fast)] hover:bg-text/5 hover:text-text"
          data-tap-min=""
        >
          <X aria-hidden size={16} />
        </button>
      </header>

      <div className="flex items-center gap-2">
        <Button variant="primary" className="flex-1" disabled={state === "spent"} onClick={onConnect}>
          <span className="truncate">Connect to {device.name}</span>
          <ArrowRight aria-hidden size={14} />
        </Button>
        {/*
          Wake it, for a device this machine has seen on the network before.

          Offered only when there is a hardware address to send to, because
          that is the whole of what makes it possible — a magic packet is the
          target's address sixteen times, and there is nothing to send without
          one. A button that was always there and failed for most devices
          would be a button that teaches people it does not work.
        */}
        {device.last_mac && (
          <Tooltip label="Send a wake-up signal, for a machine asleep on this network">
            <Button data-wake="" disabled={busy} onClick={wake}>
              <Power aria-hidden size={14} /> Wake
            </Button>
          </Tooltip>
        )}
      </div>

      {/*
        A one-session pairing is spent when the session ends, and until the
        API grew `keep` the only way to change that was a terminal — which is
        the thing this window exists to stop being necessary.
      */}
      {state !== "kept" && (
        <div className="flex flex-col items-start gap-3 rounded-xl border border-border bg-surface-1 p-4">
          <p className="text-sm text-text-2">
            {state === "once"
              ? "This pairing is for one session. It ends when the session ends."
              : "This pairing has ended. Pair the device again to connect."}
          </p>
          <Button size="sm" disabled={busy} onClick={keep}>
            <Infinity aria-hidden size={14} /> Make pairing permanent
          </Button>
        </div>
      )}

      <Block
        title={`What ${device.name} may do on this machine`}
        about={`When ${device.name} connects to this machine, these decide what it can do here.`}
      >
        <div>
          {/*
            **The design system's own grid**, not a second version of it:
            it says which switches only a terminal here can move, in words.
          */}
          <PermissionGrid
            value={heldFrom(perms)}
            when={device.unattended_when}
            onChange={(next) => {
              // The grid answers with the whole set; this screen grants and
              // revokes one at a time, because that is what the API does and
              // what the trail records.
              for (const p of PERMISSIONS) {
                const had = perms.includes(p);
                if (had !== next.has(p)) {
                  void change(p, !had);
                  return;
                }
              }
            }}
          />
        </div>
      </Block>

      <Block title={`When ${device.name} connects`}>
        <div className="flex flex-col gap-4 py-4">
          {/*
            The hours, for a device that may walk in at all. Before this the
            only way to bound unattended access was `esper schedule` in a
            terminal.
          */}
          {perms.includes("unattended") && (
            <div data-when="" className="flex flex-col gap-2">
              <span className="text-sm text-text-2">Hours it can connect without asking first</span>
              <ScheduleInput value={asWindow(device.unattended_when)} onChange={setWhen} />
            </div>
          )}

          {/*
            What a session with it leaves this machine's screen as: the
            per-device answer for the machine the host-wide setting is wrong
            about. "Use this machine's setting" is a real choice and not the
            absence of one: taking a per-device answer off has to be as easy
            as giving it.
          */}
          <div data-lock-after="" className="flex flex-col gap-2">
            <span className="text-sm text-text-2">This screen after it disconnects</span>
            <Select value={shown.lock ?? device.lock_after ?? LOCK_DEFAULT} onValueChange={setLockAfter}>
              <SelectTrigger aria-label="This screen after it disconnects">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={LOCK_DEFAULT}>Same as in Settings</SelectItem>
                {LOCK_AFTER.map((a) => (
                  <SelectItem key={a} value={a}>
                    {LOCK_AFTER_LABEL[a]}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </div>
      </Block>

      {/*
        Three things this machine can do *to* that one.

        **Whether any of them is allowed is decided at the other end.** They
        are permissions that host granted this one, and this side keeps no
        copy of somebody else's trust store — so they are offered and the
        host's own answer is what is shown.

        The two that outlive the session ask first. Locking a screen does not:
        the worst it costs is typing a password, and a confirmation for that
        is a dialog people learn to click through.
      */}
      {device.admits_a_session && (
        <Block title={`Control ${device.name} from here`}>
          <div data-reaching="" className="flex flex-wrap items-center gap-2 py-3.5">
            <Button size="sm" disabled={busy} onClick={lockThere}>
              <Lock aria-hidden size={14} /> Lock its screen
            </Button>
            <AskFirst
              label="Log it off"
              icon={LogOut}
              title={`Log ${device.name} off?`}
              description="Open apps on it close, and anyone using it is signed out. You can't undo this from here."
              confirm="Log it off"
              disabled={busy}
              onConfirm={logOffThere}
            />
            <AskFirst
              label="Restart it"
              icon={RotateCcw}
              title={`Restart ${device.name}?`}
              description="It goes offline and should come back on its own. You won't get a reply, so check that it returns."
              confirm="Restart it"
              danger
              disabled={busy}
              onConfirm={restartThere}
            />
          </div>
        </Block>
      )}

      {/*
        Which groups this device is in. A group is an address rather than a
        grant: being in one gives a device nothing by itself. What it buys is
        a way to say "these machines" once instead of eight times.

        Shown even when empty, because a panel that hid the row would make
        "this device is in no groups" and "this build has no groups" look the
        same.
      */}
      <Block
        title="Groups"
        about="A group names several devices at once, so a terminal command can change all of them. Being in one changes nothing by itself."
      >
        <div data-groups="" className="flex flex-wrap items-center gap-2 py-3.5">
          {(device.groups ?? []).map((name) => (
            <span
              key={name}
              className="flex h-7 items-center gap-1 rounded-full border border-border bg-text/[0.04] pl-3 pr-1 text-xs text-text"
            >
              {name}
              <button
                type="button"
                aria-label={`Take ${device.name} out of ${name}`}
                disabled={busy}
                data-tap-min=""
                onClick={() => void joinGroup(name, true)}
                className="inline-flex size-5 items-center justify-center rounded-full text-text-3 transition-[color] duration-[var(--dur-fast)] hover:text-danger disabled:text-text-disabled"
              >
                <X aria-hidden size={12} />
              </button>
            </span>
          ))}
          {(device.groups ?? []).length === 0 && <span className="text-sm text-text-3">In no groups yet.</span>}
          <NewGroup disabled={busy} onAdd={(name) => void joinGroup(name, false)} />
        </div>
      </Block>

      <Block title="About this device">
        <InfoRow label="Last seen">{device.last_seen ? whenShort(device.last_seen) : "Never"}</InfoRow>
        <InfoRow label="Paired on">{whenShort(device.paired_at)}</InfoRow>
        {device.last_address && <InfoRow label="Last address">{device.last_address}</InfoRow>}
        {device.overlay_address && <InfoRow label="Private network address">{device.overlay_address}</InfoRow>}
        <InfoRow label="Pairing">{PAIRING_WORD[state]}</InfoRow>
        <div className="flex flex-col gap-2 py-3.5">
          <span className="text-sm text-text-2">Its key</span>
          <span className="text-xs text-text-3">
            When pairing, check that these words match the ones shown on {device.name}.
          </span>
          <Fingerprint hex={device.fingerprint.hex} />
          {/* The words are read aloud *and* pasted into a message. */}
          <CopyField value={device.fingerprint.words.join(" ")} wrap />
        </div>
      </Block>

      <div className="pt-1">
        <Dialog>
          <DialogTrigger asChild>
            <Button variant="ghost" size="sm" className="-ml-2 text-text-3 hover:text-danger">
              <Trash2 aria-hidden size={14} /> Forget this device
            </Button>
          </DialogTrigger>
          <DialogContent
            title={`Forget ${device.name}?`}
            description="You'll need to pair it again to connect. Nothing changes on that machine."
          >
            <DialogFooter>
              <DialogClose asChild>
                <Button>Cancel</Button>
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

/** How a pairing stands, in words rather than the store's. */
const PAIRING_WORD = { kept: "Permanent", once: "For one session", spent: "Ended" } as const;
/** The same, said on its own as a badge, where no "Pairing" label sits beside it. */
const PAIRING_BADGE = { once: "One-time pairing", spent: "Pairing ended" } as const;

/** A titled card in the panel, the same shape as a card in Settings. */
function Block({ title, about, children }: { title: string; about?: string | undefined; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-2">
      <div className="flex flex-col gap-0.5 px-1">
        <h3 className="text-sm font-medium text-text">{title}</h3>
        {about && <p className="text-xs leading-relaxed text-text-3">{about}</p>}
      </div>
      <div className="rounded-xl border border-border bg-surface-1 px-4 [&>*:last-child]:border-b-0">{children}</div>
    </section>
  );
}

/** A label and its value on one line, inside a `Block`. */
function InfoRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-4 border-b border-border py-3 text-sm">
      <span className="shrink-0 text-text-2">{label}</span>
      <span className="min-w-0 truncate text-right tabular-nums text-text">{children}</span>
    </div>
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
  icon: Icon,
  title,
  description,
  confirm,
  danger,
  disabled,
  onConfirm,
}: {
  label: string;
  icon?: typeof Lock | undefined;
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
        <Button size="sm" disabled={disabled} className={danger ? "hover:text-danger" : undefined}>
          {Icon && <Icon aria-hidden size={14} />} {label}
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
            <Button>Cancel</Button>
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
      label: state === "kept" ? "Pairing is permanent" : "Make pairing permanent",
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
  const { data, error, loading: asking, reload } = useDevices();
  // Placeholders are for the first answer only. A reload after every change
  // drew them back above the list it already had, and the page jumped.
  const loading = asking && data === null;
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
            title: `Could not keep ${d.name} paired`,
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
            description: "Pair it again to connect.",
            tone: "warn",
            // **Honest about what it can do.** Pairing is an exchange between
            // two machines and no button here can redo it; what this offers
            // is the permissions back, and only while the device is still
            // there to grant them to.
            action: {
              label: "Restore its permissions",
              run: () => {
                api
                  .grant(d.fingerprint.hex, had)
                  .then(() => {
                    reload();
                    toast.show({ title: `${d.name}'s permissions are restored`, tone: "ok" });
                  })
                  .catch(() =>
                    toast.show({
                      title: "Too late to restore its permissions",
                      description:
                        "The device is already removed. Pair it again, then grant its permissions.",
                      tone: "warn",
                    }),
                  );
              },
            },
          });
        })
        .catch((cause: unknown) =>
          toast.show({
            title: `Could not forget ${d.name}`,
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
          className={cn("grid min-h-0 flex-1 gap-6", selected && "@[45rem]:grid-cols-[1fr_26rem]")}
        >
          <div className="flex min-w-0 flex-col">
            {(loading || devices.length > 0 || error) && (
              <Heading className="px-3" count={loading ? "" : devices.length}>Paired</Heading>
            )}

            {loading && (
              <div className="flex flex-col gap-px">
                {[0, 1, 2].map((i) => (
                  <div key={i} className="flex items-center gap-3.5 px-3 py-2.5">
                    <Skeleton className="size-10 rounded-lg" />
                    <span className="flex flex-col gap-1.5">
                      <Skeleton className="h-4 w-40" />
                      <Skeleton className="h-3 w-24" />
                    </span>
                    <Skeleton className="ml-auto h-7 w-20 rounded-sm" />
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
                Could not load your devices.
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
                No devices yet. Pair one, then connect to it in one click.
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
              <p className="px-3 pt-6 text-xs leading-relaxed text-text-3">
                Esper only looks for devices on {looked}. Devices you reach another way, such as over
                Tailscale, can still connect.
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


