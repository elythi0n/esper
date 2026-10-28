import { Laptop, Monitor, Server } from "lucide-react";
import { pairingState, type Device } from "@/api/types";
import { Identicon } from "@/components/identicon";
import { Lamp } from "@/components/lamp";
import { RowMenu, WithContextMenu, type MenuAction } from "@/components/menu";
import { cn } from "@/lib/cn";
import { Button } from "@/components/button";
import { Figure } from "@/screens/parts";
import { Badge } from "@/components/badge";
import { TimeAgo } from "./time-ago";
import { shortAgoAt } from "@/lib/ago";

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
 * Connect is always drawn, and drawn as the button it is, rather than an
 * arrow that appears on hover: one click to connect is the product, and an
 * affordance that needs a pointer over it is one a keyboard never finds.
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
  const seen =
    device.last_seen === null ? null : (
      <TimeAgo date={device.last_seen * 1000} format={shortAgoAt} />
    );
  const row = (
    <div
      className={cn(
        "group relative flex items-center gap-3 rounded-xl px-3 py-2.5",
        "transition-[background-color] duration-[var(--dur-fast)] ease-[var(--ease)]",
        onSelect && "cursor-pointer",
        selected ? "bg-text/[0.06]" : onSelect && "hover:bg-text/[0.035]",
        busy && "opacity-60",
      )}
      aria-busy={busy || undefined}
    >
      <button
        type="button"
        disabled={!onSelect}
        onClick={onSelect}
        className="flex min-w-0 flex-1 items-center gap-3.5 text-left disabled:cursor-default"
      >
        <span
          className={cn(
            "flex size-10 shrink-0 items-center justify-center rounded-lg bg-text/[0.05]",
            spent ? "text-text-3" : "text-text-2",
          )}
        >
          <Icon aria-hidden size={17} strokeWidth={1.75} />
        </span>
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className={cn("truncate text-base font-medium", spent ? "text-text-3" : "text-text")}>
            {device.name}
          </span>
          <span className="flex min-w-0 items-center gap-2 text-xs text-text-3">
            {/*
              The key, drawn. **Beside the name and never instead of it**: what
              this buys is that a device which has quietly become a different
              device looks different from across a desk, and it buys nothing at
              all as a check — see `Identicon`.
            */}
            <Identicon of={device.fingerprint.hex} size={12} className={spent ? "opacity-40" : undefined} />
            {here ? (
              <Lamp state="ok" className="shrink-0" title="Found on this network">
                On this network
              </Lamp>
            ) : (
              /*
                Live, not a snapshot. A list somebody leaves open — which is
                what the Devices screen is for — used to say "14s" for as long
                as it was open. The element ticks itself, and carries the
                instant as well as the words for anything that reads the page.
              */
              <span data-last-seen="" className="truncate">
                {seen === null ? "Never seen" : <>Seen <Figure className="text-text-3" value={seen} /> ago</>}
              </span>
            )}
          </span>
        </span>
        {/* Off on a phone, where the name needs the room; the greyed Connect still says it. */}
        {(state === "once" || spent) && (
          <span className="hidden shrink-0 @[40rem]:block">
            <Badge tone={spent ? "neutral" : "info"}>{spent ? "Pairing ended" : "One-time pairing"}</Badge>
          </span>
        )}
      </button>
      <Button
        variant={spent || !onConnect ? "ghost" : "secondary"}
        size="sm"
        aria-label={`Connect to ${device.name}`}
        disabled={spent || !onConnect}
        onClick={onConnect}
        /*
          The same outlined button on every row, lit only under the pointer
          itself. Lighting it for the whole row, or the selected one, read as
          a click about to happen when somebody was only looking.
        */
        className="shrink-0"
      >
        Connect
      </Button>
      {actions && actions.length > 0 && <RowMenu actions={actions} label={`Actions for ${device.name}`} />}
    </div>
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
