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
  // A shell as the user who owns this machine's Esper state — so it can read
  // the identity key and this window's own token, rewrite the device list
  // without asking, and edit the record. Granting it grants every one below
  // whether or not anybody grants them, which is why it is the one that most
  // needs a person at the machine.
  "terminal",
  "unattended",
  "files_in",
  "record",
  "logoff",
  "reboot",
]);

/**
 * The words on screen. Each names this machine, because the list is what
 * another device may do *here*, and "Copy from here" or "Restart" alone read
 * as things you do to that device.
 */
export const PERMISSION_LABEL: Record<Permission, string> = {
  view: "See this screen",
  control: "Use the mouse and keyboard",
  clipboard_out: "Copy text from this machine",
  clipboard_in: "Paste text into this machine",
  files_out: "Take files from this machine",
  files_in: "Send files to this machine",
  audio: "Hear this machine's sound",
  terminal: "Open a terminal on this machine",
  forward: "Reach network services on this machine",
  unattended: "Connect without asking",
  record: "Record the session",
  lock: "Lock this screen",
  logoff: "Log this machine off",
  reboot: "Restart this machine",
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
  // Somebody at the keyboard changed a setting: no device did it and no
  // permission allowed it.
  setting_changed: null,
  // The record trimming itself, which no device asked for either.
  trail_trimmed: null,
  // Like pairing and unpairing: a thing that happened *to* the arrangement of
  // permissions, not something a permission allowed.
  pairing_kept: null,
  schedule_changed: null,
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

/**
 * What each trail event is called, for somebody reading the record.
 *
 * # Why the tag is not enough
 *
 * `session_narrowed` is the word the log is written with and the word somebody
 * greps for, and it is not English. The Activity screen showed it as the whole
 * of the "What" column, with the engine's own sentence in a Detail column that
 * is hidden below 45rem — so on a narrow window the record read as a list of
 * identifiers.
 *
 * Both, then: the name here and the tag beside it, so the screen is readable
 * and the word to grep for is still on it.
 *
 * The keys are `AUTHORISED_BY`'s, and `check-vocabularies` holds both to the
 * Rust — so an event added to the engine and not named here is a failure
 * rather than a column that quietly says `some_new_thing`.
 *
 * In the engine's own order, like the map above it. Not a reading order: the
 * gate compares the two lists *in order*, because a listing and a fixture are
 * compared that way, and one rule for both maps is one thing to remember.
 */
export const EVENT_LABEL: Readonly<Record<string, string>> = {
  recording: "Session recording",
  paired: "Paired",
  unpaired: "Unpaired",
  revoked: "Access removed",
  session_narrowed: "Quality lowered",
  session_asked: "Asked for a permission",
  lock_after_changed: "Auto-lock rule changed",
  permissions_changed: "Permissions changed",
  setting_changed: "Setting changed",
  trail_trimmed: "Log trimmed",
  pairing_kept: "Pairing made permanent",
  schedule_changed: "Access hours changed",
  session_opened: "Session opened",
  locked: "Screen locked",
  // Log off or restart: the detail says which, and the label says what kind
  // of thing it was.
  powered: "Logged off or restarted",
  session_closed: "Session ended",
  terminal_opened: "Terminal opened",
  terminal_closed: "Terminal closed",
  terminal_parked: "Terminal kept open",
  terminal_resumed: "Terminal reconnected",
  forward_opened: "Port forward opened",
  forward_closed: "Port forward closed",
  forward_refused: "Port forward refused",
  file_transferred: "File transferred",
  refused: "Connection refused",
};

/**
 * The name for an event, falling back to the tag.
 *
 * A build talking to a newer engine can be shown an event it has no name for,
 * and the tag is a worse answer than a name but a much better one than a blank
 * cell.
 */
export function eventLabel(event: string): string {
  return EVENT_LABEL[event] ?? event;
}
