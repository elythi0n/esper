import * as RD from "@radix-ui/react-dropdown-menu";
import {
  DockviewReact,
  type DockviewApi,
  type DockviewReadyEvent,
  type IDockviewHeaderActionsProps,
  type IDockviewPanelProps,
} from "dockview-react";
import { MonitorPlay, PictureInPicture2, Plus } from "lucide-react";
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import { useDevices } from "@/api/hooks";
import { pairingState, type Device } from "@/api/types";
import { kindOf } from "@/components/device-row";
import { Tooltip } from "@/components/tooltip";
import { cn } from "@/lib/cn";
import { Topbar } from "@/shell/AppShell";
import { shortAgo } from "@/screens/parts";
import { TimeAgo } from "@/components/time-ago";
import { shortAgoAt } from "@/lib/ago";
import { SessionView } from "./SessionView";
import { SessionTab } from "./SessionTab";
import { popOutWindow, shellCapabilities } from "./shell";
import "./dock.css";

/*
  Sessions — several machines at once, arranged however somebody wants them.

  A remote desktop with one session at a time is a remote desktop for one
  machine. The dock is the whole feature: a tab per session, drag to reorder,
  drag to the edge to split two machines side by side, drag out to float, and
  a "+" that opens another. dockview owns the mechanics; everything visible
  is this system's.

  The layout is remembered, and **only the layout**: which devices were open
  and where their panels sat. A session itself is not a thing that survives a
  restart, so what comes back is the arrangement, ready to reconnect.
*/

const LAYOUT_KEY = "esper.sessions.layout";

/** The panel for one session. `name` travels in params so the tab has it without a lookup. */
function SessionPanel(props: IDockviewPanelProps<{ fingerprint: string; name: string }>) {
  return <SessionView fingerprint={props.params.fingerprint} name={props.params.name} />;
}

const components = { session: SessionPanel };

/** Devices that can be opened, minus the ones already on screen. */
function openable(devices: Device[], open: Set<string>): Device[] {
  return devices
    .filter((d) => pairingState(d) !== "spent" && !open.has(d.fingerprint.hex))
    .sort((a, b) => (b.last_seen ?? 0) - (a.last_seen ?? 0));
}

/**
 * What the dock's own slots need to know.
 *
 * dockview constructs the watermark and the header actions itself and
 * decides when they re-render, so neither can be handed a prop and expect to
 * see it change — the watermark rendered once, before the device list had
 * arrived, and showed an empty menu for ever. A context is read at render
 * time by whoever is rendering, which is the thing that actually holds.
 */
interface DockState {
  devices: Device[];
  open: Set<string>;
  add: (d: Device, group?: unknown) => void;
}

const Dock = createContext<DockState | null>(null);

function useDock(): DockState {
  const ctx = useContext(Dock);
  if (!ctx) throw new Error("the dock's slots need <Dock.Provider> above them");
  return ctx;
}

function AddSession({
  align = "end",
  group,
  label,
}: {
  align?: "end" | "start";
  group?: unknown;
  /** Drawn as a labelled button, for the empty dock, rather than a bare + in a tab strip. */
  label?: string | undefined;
}) {
  const { devices, open, add } = useDock();
  const choices = openable(devices, open);
  return (
    <RD.Root>
      <RD.Trigger asChild>
        <button
          type="button"
          // 28px in a tab strip: small in both directions, so it takes the
          // hit area the system provides for exactly this.
          data-tap-min=""
          aria-label={label ? undefined : "Open another session"}
          className={
            label
              ? "inline-flex h-8 items-center gap-2 rounded-md bg-accent px-3.5 text-sm font-semibold text-accent-fg transition-opacity duration-[var(--dur-fast)] hover:opacity-90"
              : "inline-flex size-7 items-center justify-center rounded-sm text-text-3 transition-colors duration-[var(--dur-fast)] hover:bg-text/5 hover:text-text"
          }
        >
          <Plus aria-hidden size={15} strokeWidth={1.75} />
          {label}
        </button>
      </RD.Trigger>
      <RD.Portal>
        <RD.Content
          align={align}
          sideOffset={6}
          className={cn(
            "z-[var(--z-popover)] min-w-56 overflow-hidden rounded-lg border border-border bg-surface-2 p-1 shadow-[var(--shadow-2)]",
            "data-[state=open]:animate-[overlay-in_var(--dur-base)_var(--ease-out)]",
          )}
        >
          {choices.length === 0 && (
            <p className="px-3 py-4 text-center text-sm text-text-2">
              {devices.length ? "All your devices are already open." : "No devices paired yet."}
            </p>
          )}
          {choices.map((d) => {
            const Icon = kindOf(d.name);
            return (
              <RD.Item
                key={d.fingerprint.hex}
                onSelect={() => add(d, group)}
                data-focus-exempt="a menu moves a highlight rather than focus; data-[highlighted] is the indicator"
                className="flex h-8 cursor-pointer select-none items-center gap-2 rounded-sm px-2 text-sm text-text outline-none data-[highlighted]:bg-surface-1"
              >
                <Icon aria-hidden size={14} strokeWidth={1.5} className="shrink-0 text-text-3" />
                <span className="min-w-0 flex-1 truncate">{d.name}</span>
                {/* Ticking, for the same reason the Devices list does. */}
                <span className="shrink-0 font-mono text-xs tabular-nums text-text-3">
                  {d.last_seen === null ? shortAgo(null) : <TimeAgo date={d.last_seen * 1000} format={shortAgoAt} />}
                </span>
              </RD.Item>
            );
          })}
        </RD.Content>
      </RD.Portal>
    </RD.Root>
  );
}

export function SessionsScreen({ initial }: { initial?: string | undefined }) {
  const { data } = useDevices();
  const devices = data ?? [];
  const [api, setApi] = useState<DockviewApi | null>(null);
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [canPopOut, setCanPopOut] = useState(false);
  const devicesRef = useRef(devices);
  devicesRef.current = devices;

  useEffect(() => {
    void shellCapabilities().then((c) => setCanPopOut(c.popout));
  }, []);

  const openSession = useCallback(
    (dock: DockviewApi, d: Device) => {
      if (dock.getPanel(d.fingerprint.hex)) {
        dock.getPanel(d.fingerprint.hex)?.api.setActive();
        return;
      }
      dock.addPanel({
        id: d.fingerprint.hex,
        component: "session",
        params: { fingerprint: d.fingerprint.hex, name: d.name },
        title: d.name,
      });
    },
    [],
  );

  /** Which panels exist, kept in step so the "+" list can exclude them. */
  const syncOpen = useCallback((dock: DockviewApi) => {
    setOpen(new Set(dock.panels.map((p) => p.id)));
    try {
      localStorage.setItem(LAYOUT_KEY, JSON.stringify(dock.toJSON()));
    } catch {
      // Storage may be unavailable; the arrangement then lasts this run.
    }
  }, []);

  const onReady = useCallback(
    (event: DockviewReadyEvent) => {
      setApi(event.api);
      let restored = false;
      try {
        const saved = localStorage.getItem(LAYOUT_KEY);
        if (saved) {
          event.api.fromJSON(JSON.parse(saved));
          restored = event.api.panels.length > 0;
        }
      } catch {
        // A layout this build cannot read is not a layout. Start empty.
        event.api.clear();
      }
      if (!restored && initial) {
        const d = devicesRef.current.find((x) => x.fingerprint.hex === initial);
        if (d) openSession(event.api, d);
      }
      setOpen(new Set(event.api.panels.map((p) => p.id)));
      event.api.onDidLayoutChange(() => syncOpen(event.api));
      // A hook the frame-and-layout checks drive, so "two machines side by
      // side" is something a script can assert rather than something a
      // person has to drag. Same call dockview's own drop performs.
      (window as unknown as { __dockSplit?: () => void }).__dockSplit = () => {
        const [first, second] = event.api.panels;
        // `position` alone has nothing to be right *of*; the group it is
        // moving relative to is the other half of the instruction.
        if (first && second) second.api.moveTo({ group: first.api.group, position: "right" });
      };
    },
    [initial, openSession, syncOpen],
  );

  // A route asking for a device opens it in the arrangement already on screen.
  useEffect(() => {
    if (!api || !initial) return;
    const d = devices.find((x) => x.fingerprint.hex === initial);
    if (d) openSession(api, d);
  }, [api, initial, devices, openSession]);

  const RightActions = useCallback(
    (props: IDockviewHeaderActionsProps) => (
      <div className="flex items-center gap-0.5 pr-1">
        <AddSession group={props.group} />
        {canPopOut && props.group.panels.length > 0 && (
          <Tooltip label="Open in its own window">
            <button
              type="button"
              aria-label="Open in its own window"
              onClick={() => {
                const panel = props.group.activePanel;
                if (panel) void popOutWindow(`/sessions/${panel.id}`);
              }}
              className="inline-flex size-7 items-center justify-center rounded-sm text-text-3 transition-colors duration-[var(--dur-fast)] hover:bg-surface-2 hover:text-text"
            >
              <PictureInPicture2 aria-hidden size={15} strokeWidth={1.75} />
            </button>
          </Tooltip>
        )}
      </div>
    ),
    [open, canPopOut],
  );

  // dockview's own slot for "nothing is open". Overlaying the dock instead
  // put this behind the vendor's watermark, which is the thing a slot exists
  // to avoid.
  const dock = useMemo<DockState>(
    () => ({
      devices,
      open,
      add: (d, group) => {
        if (!api) return;
        const existing = api.getPanel(d.fingerprint.hex);
        if (existing) {
          existing.api.setActive();
          return;
        }
        api.addPanel({
          id: d.fingerprint.hex,
          component: "session",
          params: { fingerprint: d.fingerprint.hex, name: d.name },
          title: d.name,
          ...(group ? { position: { referenceGroup: group as never, direction: "within" as const } } : {}),
        });
      },
    }),
    [api, devices, open],
  );

  return (
    <Dock.Provider value={dock}>
      {/*
        **Dark only while something is on screen.** The session layer is dark
        because a remote desktop is somebody else's picture; an empty dock is
        just a screen of this app, and painting it black under a light theme
        read as a broken half of the window. So the dark root, and the title
        bar's absence, both wait for the first session.
      */}
      {open.size === 0 && <Topbar title="Sessions" />}
      <div className="relative min-h-0 flex-1" data-session={open.size > 0 ? "" : undefined}>
        <DockviewReact
          className="dockview-theme-esper h-full"
          components={components}
          defaultTabComponent={SessionTab}
          rightHeaderActionsComponent={RightActions}
          watermarkComponent={Watermark}
          onReady={onReady}
        />
      </div>
    </Dock.Provider>
  );
}

/** "Nothing is open", in dockview's own slot rather than laid over it. */
function Watermark() {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-5 bg-surface-0 text-center">
      <span
        aria-hidden
        className="flex size-14 items-center justify-center rounded-xl border border-border bg-text/[0.04] text-text-3"
      >
        <MonitorPlay size={22} strokeWidth={1.5} />
      </span>
      <div className="flex max-w-[var(--measure-short)] flex-col gap-1.5">
        <p className="font-display text-lg font-medium text-text">No sessions open</p>
        <p className="text-sm text-text-2">
          Open a session to see another machine's screen. Drag a tab to an edge to show two side by side.
        </p>
      </div>
      <AddSession align="start" label="Open a session" />
    </div>
  );
}

