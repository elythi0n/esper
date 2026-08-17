/**
 * The component inventory: what the product cannot exist without, what makes
 * it pleasant, and what is deliberately not being built.
 *
 * The third list is the one that does the work. A design system without a
 * not-building list grows a component every time somebody wants one, and
 * then the system is a catalogue rather than a set of decisions. Every entry
 * there carries the reason, so a future argument starts from the argument
 * that was already had.
 *
 * Tier 1 names a real export and `scripts/check-inventory.mjs` fails the
 * build if it does not resolve, so this list cannot quietly describe a
 * component that was renamed or never written.
 */

export interface Tier1Entry {
  /** The exported symbol. Checked against the module. */
  name: string;
  /** Module path under `src/`. Checked to exist. */
  from: string;
  what: string;
}

export const TIER_1: readonly Tier1Entry[] = [
  // ── The frame ──────────────────────────────────────────────────────────
  { name: "AppShell", from: "shell/AppShell.tsx", what: "Sidebar, topbar, page body, status strip; a rail when narrow, tabs on a phone" },
  { name: "SessionRoot", from: "session/chrome.tsx", what: "A live session: always dark, every duration capped, chrome above everything" },
  { name: "SessionChrome", from: "session/chrome.tsx", what: "The session's top bar, which retracts and returns" },
  { name: "VideoSurface", from: "session/chrome.tsx", what: "The remote screen. Never transitions" },
  { name: "Presence", from: "session/chrome.tsx", what: "This screen is being watched. Held to a contract, not styled" },
  { name: "ReconnectNotice", from: "session/chrome.tsx", what: "States its state. No pulse, no spinner" },

  // ── The product's own ──────────────────────────────────────────────────
  { name: "DeviceRow", from: "components/device-row.tsx", what: "A device as one line: which machine, when it was last used, and one click" },
  { name: "TierBadge", from: "components/badge.tsx", what: "How a session got there. Icon and word, never colour alone" },
  { name: "PairingCode", from: "components/pairing.tsx", what: "The code, at 44px, read across a room" },
  { name: "PairingPrompt", from: "components/pairing.tsx", what: "Somebody wants in, and exactly what they would be able to do" },
  { name: "PermissionGrid", from: "components/permission-grid.tsx", what: "The store’s fourteen permissions, editable except the five the host keeps" },
  { name: "SessionHUD", from: "components/hud.tsx", what: "RTT, bitrate, fps, encoder, tier. Never animates" },
  { name: "StatTile", from: "components/hud.tsx", what: "One figure with its unit, and a word when it is toned" },
  { name: "Sparkline", from: "components/hud.tsx", what: "Recent values. The only chart this product has" },
  { name: "MonitorSwitcher", from: "components/monitor-switcher.tsx", what: "Which screen of a many-screen host, at its own aspect ratio" },
  { name: "TransferProgress", from: "components/transfer-progress.tsx", what: "One file moving, cancellable, and a failure that says what to do" },

  // ── The instrument panel: what a product screen is actually made of ────
  //
  // These were the most-used components in the product and were in none of
  // the three lists, because they live beside the screens rather than under
  // `components/`. A tier that skips a module skips whatever is in it.
  { name: "Key", from: "screens/parts.tsx", what: "A field name: mono, uppercase, tracked, small. Read as a label, never as content" },
  { name: "Figure", from: "screens/parts.tsx", what: "A number and its unit. Tabular, so a column lines up, and it never animates" },
  { name: "Fact", from: "screens/parts.tsx", what: "A key above its value, for a panel rather than a list" },
  { name: "Row", from: "screens/parts.tsx", what: "The list primitive: a hairline, not a box. No radius, no fill at rest" },
  { name: "Fingerprint", from: "screens/parts.tsx", what: "A key in groups of four, set the way it is spoken and never broken mid-group" },
  { name: "Heading", from: "screens/parts.tsx", what: "A section heading inside a screen: a key and a count, and nothing else" },
  { name: "Trouble", from: "screens/parts.tsx", what: "One request having failed, where its answer would have been. The machine's own sentence underneath" },
  { name: "Lamp", from: "components/lamp.tsx", what: "A dot and the word it is about. The word is required, so colour is never the only carrier" },
  { name: "SettingRow", from: "components/setting.tsx", what: "A setting: its name, what it is now in words, and the one control that changes it" },
  { name: "Said", from: "components/setting.tsx", what: "What the machine was told to do, verbatim, so a switch is not a black box" },

  // ── The general set ────────────────────────────────────────────────────
  { name: "Button", from: "components/button.tsx", what: "Four variants; primary is the inverse fill, danger the only tinted one" },
  { name: "Card", from: "components/card.tsx", what: "A raised plane" },
  { name: "Badge", from: "components/badge.tsx", what: "A small label in a tone" },
  { name: "Callout", from: "components/callout.tsx", what: "A short message with a tone and an icon" },
  { name: "Alarm", from: "components/alarm.tsx", what: "A band across the window for a condition of the machine, and the one thing that fixes it" },
  { name: "ShortcutSheet", from: "components/shortcuts.tsx", what: "Every key this window binds, rendered from the array it binds them from" },
  { name: "CopyField", from: "components/copy-field.tsx", what: "A value beside the one button anybody wants for it, and it stays selectable" },
  { name: "Identicon", from: "components/identicon.tsx", what: "A key drawn, so a device that has become a different device looks different. Never a check" },
  { name: "RowMenu", from: "components/menu.tsx", what: "A row's actions from a button a keyboard can find" },
  { name: "WithContextMenu", from: "components/menu.tsx", what: "The same actions from a right-click — one list, two triggers" },
  { name: "Popover", from: "components/popover.tsx", what: "A small panel from a control, for a choice that should not take the screen" },
  { name: "SortableTable", from: "components/sortable-table.tsx", what: "Columns that sort, on top of Table, stable where they compare equal" },
  { name: "LogView", from: "components/log-view.tsx", what: "A long record that scrolls at sixty frames, by not drawing what nobody can see" },
  { name: "Stepper", from: "components/stepper.tsx", what: "A short sequence with a visible end. Every step reachable from every step" },
  { name: "Kbd", from: "components/kbd.tsx", what: "A key as it is printed, and the one place a chord is split — a part longer than six characters is a word" },
  { name: "TransferQueue", from: "components/transfer-queue.tsx", what: "Several files at once: the whole thing in one line, and what failed sorted to where it will be seen" },
  { name: "Field", from: "components/input.tsx", what: "Label, control and one line of help, wired together" },
  { name: "Input", from: "components/input.tsx", what: "Text, found by its strong border" },
  { name: "NumberInput", from: "components/form-controls.tsx", what: "A number and its unit, in one control" },
  { name: "DurationInput", from: "components/form-controls.tsx", what: "Stored in seconds, written in whatever unit reads" },
  { name: "ScheduleInput", from: "components/form-controls.tsx", what: "Which days, between which times" },
  { name: "Select", from: "components/select.tsx", what: "32px rows, the check in a fixed slot" },
  { name: "Switch", from: "components/switch.tsx", what: "On or off, at a 44px target however small it is drawn" },
  { name: "Checkbox", from: "components/switch.tsx", what: "On, off, or some" },
  { name: "Tabs", from: "components/tabs.tsx", what: "Sections of one screen" },
  { name: "Table", from: "components/table.tsx", what: "28px rows, tabular figures, cells that never animate" },
  { name: "DialogContent", from: "components/dialog.tsx", what: "A dialog or a drawer, opening from where it was invoked" },
  { name: "Toaster", from: "components/toast.tsx", what: "Toasts in a reserved band, wired to the protocol's reason codes" },
  { name: "Tooltip", from: "components/tooltip.tsx", what: "The text label an icon owes" },
  { name: "CommandPalette", from: "components/command-palette.tsx", what: "Everything the app can do, from the keyboard" },
  { name: "EmptyState", from: "components/states.tsx", what: "A list with nothing in it, saying what to do next" },
  { name: "Skeleton", from: "components/states.tsx", what: "The shape of what is coming. Does not shimmer" },
  { name: "TimeAgo", from: "components/time-ago.tsx", what: "A relative time that updates in place" },
  { name: "Segmented", from: "components/theme-toggle.tsx", what: "The one family every exclusive choice uses" },
  { name: "ThemeToggle", from: "components/theme-toggle.tsx", what: "Dark, light, high contrast" },
  { name: "MotionToggle", from: "components/theme-toggle.tsx", what: "The reduced-motion override, so the fallback is confirmed and not assumed" },
  { name: "Textarea", from: "components/input.tsx", what: "The one free-text field the product has" },
  { name: "Arrive", from: "components/motion.tsx", what: "A new item rises and fades in rather than appearing" },
  { name: "Overlay", from: "components/motion.tsx", what: "Opens from 0.97 scale, from where it was invoked" },
];

export interface Tier2Entry {
  name: string;
  what: string;
  /** The screen that will ask for it first. */
  when: string;
}

/**
 * Tier 2: everything that makes it pleasant. Not built, and not missing —
 * each is written down with the screen that will ask for it, so it arrives
 * with a use rather than on spec.
 */
export const TIER_2: readonly Tier2Entry[] = [
  {
    name: "TwoPaneFiles",
    what: "Both filesystems side by side, which is the file manager Phase 13 asks for. Waits for a live session, because the far side of it is a session",
    when: "Session",
  },
  {
    name: "FileDrop",
    what: "The shared drop area a session gets, cleared on teardown. Waits for the same reason, and for the drag-and-drop the window has no surface for yet",
    when: "Session",
  },
];

export interface NotBuildingEntry {
  name: string;
  why: string;
}

/**
 * Not building. As important as the other two lists, and each entry is a
 * decision rather than an omission.
 */
export const NOT_BUILDING: readonly NotBuildingEntry[] = [
  { name: "Spinner", why: "Nothing in this product spins. Reconnect states its state, and a spinner on a latency instrument is motion the user has to rule out as the remote machine stuttering." },
  { name: "Skeleton shimmer", why: "An animation on the reading critical path. The still block already says loading." },
  { name: "Accordion", why: "A list that hides its content behind a click is a list nobody reads. A drawer or a screen instead." },
  { name: "Breadcrumbs", why: "The app is two levels deep. A trail through two steps is furniture." },
  { name: "Charting library", why: "The sparkline is the only chart, it is thirty lines, and a library would arrive with a palette of its own." },
  { name: "Carousel", why: "Nothing here is a sequence somebody should be walked through." },
  { name: "Stacked modals", why: "A dialog over a dialog is a question nobody can answer. One at a time, or a screen." },
  { name: "Rich text editor", why: "Nothing in a remote desktop is prose. The one free-text field is a note, and a textarea is a textarea." },
  { name: "Inline-editable data grid", why: "The editable things are permissions and settings, and both have a real control. A grid that edits in place is a grid that saves by accident." },
  { name: "Notification centre", why: "Activity is the record and a toast is the notification. A third place to look is a place nobody looks." },
  { name: "Onboarding tour", why: "If the Devices screen needs coach marks, the Devices screen is wrong." },
  { name: "Avatars of people", why: "Esper has machines, not users. A face on a fingerprint is a claim the product cannot check." },
  { name: "Renaming a device", why: "The first thing anybody reaches for in a row menu, and the device store has no such operation — a name is what the far machine calls itself, sent at the handshake. A menu item for it would be a control that fails, and a name kept locally would be a second answer to \"which machine is this\" that only this machine can see." },
  { name: "A separate ContextMenu primitive", why: "@radix-ui/react-context-menu is a whole floating primitive for a convenience, when the dropdown already here does the work: a menu positions against its trigger, so the trigger goes where the pointer was. Thirty lines against a dependency — and the menu is provably the same menu because it is the same component." },
  { name: "A separate Popover primitive", why: "The same argument. A second floating primitive means a second set of collision, focus and portal behaviours to keep in step with the first; what a popover needs beyond a menu is arbitrary content and no item semantics, which is one line — `onSelect` prevented, so a panel does not close when a control inside it is used." },
  { name: "A separate MonitorPicker", why: "Tier 2 asked for one and `MonitorSwitcher` already is it — each screen drawn at its own aspect ratio, named by its size, because \"Monitor 2\" tells nobody which one that is. Two components for one job is how a system grows a second answer to a question it had already answered." },
  { name: "A windowing library", why: "LogView is twenty lines of arithmetic over one measured height. What makes a windowing library worth its weight — variable row heights, sticky groups, horizontal windows — is exactly what this product does not have, and a fixed row height is a commitment the table already made." },
  { name: "Themed scrollbars", why: "Thin and quiet is the whole design. A drawn scrollbar is a widget that has to be maintained on three platforms." },
  { name: "Emoji or icon picker", why: "Nothing is user-decorated. A device has a name." },
  { name: "Auto theme by time of day", why: "The OS already answers this, and a window that changes theme mid-session is a flash somebody has to interpret." },
];
