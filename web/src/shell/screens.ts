import { Handshake, History, MonitorSmartphone, Palette, ScreenShare, Settings2, TicketPlus } from "lucide-react";
import type { NavGroup } from "./AppShell";

/**
 * The screens, in the order the sidebar lists them. One list, so the shell,
 * the command palette and the router cannot disagree about what exists.
 *
 * Devices is first because Devices is the product: if connecting to a machine
 * you already trust is not one click from a cold start, the rest has failed.
 * The four that earn a tab on a phone are marked; the rest are reachable from
 * the palette and from the screens that link to them.
 */
export const SCREENS: readonly NavGroup[] = [
  {
    items: [
      { id: "devices", label: "Devices", icon: MonitorSmartphone, compact: true },
      { id: "sessions", label: "Sessions", icon: ScreenShare, compact: true },
    ],
  },
  {
    label: "Connect",
    items: [
      { id: "pair", label: "Pair", icon: Handshake, compact: true },
      { id: "invite", label: "Invite", icon: TicketPlus },
    ],
  },
  {
    label: "This machine",
    items: [
      { id: "activity", label: "Activity", icon: History },
      { id: "settings", label: "Settings", icon: Settings2, compact: true },
      { id: "design", label: "Design", icon: Palette },
    ],
  },
];

export const SCREEN_IDS: readonly string[] = SCREENS.flatMap((g) => g.items.map((i) => i.id));
