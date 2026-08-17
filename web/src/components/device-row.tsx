import { ArrowRight, Laptop, Monitor, Server } from "lucide-react";
import { pairingState, type Device } from "@/api/types";
import { Identicon } from "@/components/identicon";
import { Lamp } from "@/components/lamp";
import { RowMenu, WithContextMenu, type MenuAction } from "@/components/menu";
import { cn } from "@/lib/cn";
import { Figure, Key, Row, shortAgo } from "@/screens/parts";

/**
 * A device, as one line.
 *
 * **A row and not a card**, and that was a decision made twice. The card came
 * first and carried a readout — path, permissions, key — under every name; a
 * list of five of them is five paragraphs, and the one question somebody
 * opens this screen with is *which machine, and can I go there now*.
 * Everything else moved to the panel that opens beside the list, and the row
 * kept the three things that answer that question.
 *
 * The arrow is always drawn rather than appearing on hover: one click to
 * connect is the product, and an affordance that needs a pointer over it is
 * one a keyboard never finds.
 */

/** A guess from the name, and only ever decoration — the store has no kind. */
export function kindOf(name: string): typeof Monitor {
  const n = name.toLowerCase();
  if (/(book|laptop|air|portable)/.test(n)) return Laptop;
  if (/(box|server|nuc|rack|node|vm|host)/.test(n)) return Server;
  return Monitor;
}

export interface DeviceRowProps {
  device: Device;
  /**
   * Announcing itself on this network right now.
   *
   * **"On this network", never "online".** Only the LAN rung announces
   * itself; a device absent from it may be perfectly reachable over an
   * overlay or at an address. A lamp meaning reachable would be read as a
   * promise and would break the first time somebody clicked a green device
   * that was only ever reachable through a broker.
   */
  here?: boolean | undefined;
  /** A change to this device is in flight. The row says so rather than freezing. */
  busy?: boolean | undefined;
  /**
   * What this device's menu offers.
   *
   * One list, reached two ways: the button, which a keyboard and a touch
   * screen can both find, and a right-click, which is the convenience. Two
   * lists would drift, and then which actions a device has would depend on
   * how you asked for them.
   */
  actions?: readonly MenuAction[] | undefined;
  selected?: boolean | undefined;
  /** Opens the panel. Omitted where a row is not selectable. */
  onSelect?: (() => void) | undefined;
  onConnect?: (() => void) | undefined;
}

export function DeviceRow({
  device,
  here = false,
  busy = false,
  actions,
  selected = false,
  onSelect,
  onConnect,
}: DeviceRowProps) {
  const Icon = kindOf(device.name);
  const state = pairingState(device);
  const spent = state === "spent";
  const row = (
    <Row
      className={cn(
        "pl-3",
        onSelect && "cursor-pointer",
        selected ? "bg-surface-1" : onSelect && "hover:bg-surface-1",
        busy && "opacity-60",
      )}
      aria-busy={busy || undefined}
    >
      <button
        type="button"
        disabled={!onSelect}
        onClick={onSelect}
        className="flex min-w-0 flex-1 items-center gap-4 text-left disabled:cursor-default"
      >
        <Icon aria-hidden size={16} strokeWidth={1.5} className={cn("shrink-0", spent ? "text-text-3" : "text-text-2")} />
        {/*
          The key, drawn. **Beside the name and never instead of it**: what
          this buys is that a device which has quietly become a different
          device looks different from across a desk, and it buys nothing at
          all as a check — see `Identicon`.
        */}
        <Identicon of={device.fingerprint.hex} size={16} className={spent ? "opacity-40" : undefined} />
        <span
          className={cn(
            "min-w-0 flex-1 truncate font-display text-lg font-medium",
            spent ? "text-text-3" : "text-text",
          )}
        >
          {device.name}
        </span>
        {here && (
          <Lamp state="ok" className="shrink-0" title="Announcing itself on this network">
            <Key className="shrink-0">on this network</Key>
          </Lamp>
        )}
        {state === "once" && <Key className="shrink-0">once</Key>}
        {spent && <Key className="shrink-0">spent</Key>}
        <Figure className="w-10 shrink-0 text-right text-sm text-text-2" value={shortAgo(device.last_seen)} />
      </button>
      <button
        type="button"
        aria-label={`Connect to ${device.name}`}
        disabled={spent || !onConnect}
        onClick={onConnect}
        data-tap-min=""
        className={cn(
          "shrink-0 rounded-sm p-1.5 transition-[color] duration-[var(--dur-fast)]",
          spent || !onConnect ? "cursor-default text-text-disabled" : "text-text-3 group-hover:text-text hover:!text-text",
        )}
      >
        <ArrowRight aria-hidden size={16} strokeWidth={1.75} />
      </button>
      {actions && actions.length > 0 && <RowMenu actions={actions} label={`Actions for ${device.name}`} />}
    </Row>
  );

  // The right-click is layered on rather than replacing anything: a menu only
  // a right-click reaches is one a keyboard cannot, and a touch screen has no
  // right button.
  return actions && actions.length > 0 ? (
    <WithContextMenu actions={actions}>{row}</WithContextMenu>
  ) : (
    row
  );
}
