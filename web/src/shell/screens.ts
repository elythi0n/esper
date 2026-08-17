import { Activity, Monitor, MonitorPlay, Send, Settings, SwatchBook, UserPlus } from "lucide-react";
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
      { id: "devices", label: "Devices", icon: Monitor, compact: true },
      { id: "sessions", label: "Sessions", icon: MonitorPlay, compact: true },
    ],
  },
  {
    label: "Connect",
    items: [
      { id: "pair", label: "Pair", icon: UserPlus, compact: true },
      { id: "invite", label: "Invite", icon: Send },
    ],
  },
  {
    label: "This machine",
    items: [
      { id: "activity", label: "Activity", icon: Activity },
      { id: "settings", label: "Settings", icon: Settings, compact: true },
      { id: "design", label: "Design", icon: SwatchBook },
    ],
  },
];

export const SCREEN_IDS: readonly string[] = SCREENS.flatMap((g) => g.items.map((i) => i.id));
