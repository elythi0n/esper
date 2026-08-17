/**
 * What a device may be allowed to do.
 *
 * The third copy of a vocabulary that already exists in Rust (which
 * deserialises it into an enum, so an unknown word fails a whole store) and
 * in Go (which reads the same file). `scripts/check-vocabularies.py` holds all
 * three to each other: a word added to one and not the others is a screen
 * showing a permission nothing enforces, or a store this app calls broken on
 * a machine where nothing is wrong.
 *
 * The order is Rust's.
 */
export const PERMISSIONS = [
  "view",
  "control",
  "clipboard_out",
  "clipboard_in",
  "files_out",
  "files_in",
  "audio",
  "terminal",
  "forward",
  "unattended",
  "record",
  "lock",
  "logoff",
  "reboot",
] as const;

export type Permission = (typeof PERMISSIONS)[number];

/**
 * Granting one of these needs somebody at the machine, and the local API
 * refuses them — `internal/devices.Permission.Sensitive`. A control that
 * offered them anyway would be a switch that always fails, so the screens
 * show them as held at the host rather than as something to toggle.
 */
export const SENSITIVE: ReadonlySet<Permission> = new Set<Permission>([
  "unattended",
  "files_in",
  "record",
  "logoff",
  "reboot",
]);

/**
 * The words on screen. Short on purpose: a permission list is scanned, not
 * read, and "out" and "in" are the store's direction, not a person's.
 */
export const PERMISSION_LABEL: Record<Permission, string> = {
  view: "See the screen",
  control: "Mouse and keyboard",
  clipboard_out: "Copy from here",
  clipboard_in: "Paste to here",
  files_out: "Take files",
  files_in: "Put files",
  audio: "Hear audio",
  terminal: "Terminal",
  forward: "Forward ports",
  unattended: "Connect unasked",
  record: "Record",
  lock: "Lock screen",
  logoff: "Sign out",
  reboot: "Restart",
};

/** The four the product is mostly about, for a summary that is not a list of fourteen. */
export const HEADLINE_PERMISSIONS: readonly Permission[] = ["view", "control", "files_in", "unattended"];

export const isPermission = (word: string): word is Permission => (PERMISSIONS as readonly string[]).includes(word);

/**
 * Every event the trail records, and the permission that authorises it.
 *
 * # Why this mapping exists
 *
 * "Show me everything that used `terminal`" is the question somebody has when
 * they open an audit log, and the log does not answer it: it records *what
 * happened*, in twenty-one words. The permission is what a person thinks in,
 * so the screen has to hold the map between them.
 *
 * `null` is not a gap. Pairing, unpairing and a refusal are not things a
 * permission allowed — they are things that happened *to* the arrangement of
 * permissions, and filing them under one would be a category error.
 *
 * **The fourth copy of a vocabulary**, and held to the others:
 * `scripts/check-vocabularies.py` compares these keys against the tags in
 * `esper-audit`'s `Event::tag`, so an event added in Rust and not here is a
 * filter that silently hides lines, and one here and not in Rust is a filter
 * that can never match. Both were possible before this was checked.
 */
export const AUTHORISED_BY: Readonly<Record<string, Permission | null>> = {
  recording: "record",
  paired: null,
  unpaired: null,
  revoked: null,
  session_narrowed: null,
  session_asked: null,
  lock_after_changed: null,
  permissions_changed: null,
  session_opened: "view",
  locked: "lock",
  powered: "reboot",
  session_closed: "view",
  terminal_opened: "terminal",
  terminal_closed: "terminal",
  terminal_parked: "terminal",
  terminal_resumed: "terminal",
  forward_opened: "forward",
  forward_closed: "forward",
  forward_refused: "forward",
  file_transferred: "files_out",
  refused: null,
};

/** Every event word, in the trail's own order. */
export const EVENTS = Object.keys(AUTHORISED_BY) as readonly string[];
