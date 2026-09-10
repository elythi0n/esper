/**
 * What to do about every reason the protocol can report.
 *
 * # Why this is a table and not five ranges
 *
 * The codes are grouped — 1xx protocol, 2xx identity, 3xx permission, 4xx
 * capability, 5xx transport — and a screen that answered by range gave four
 * sentences to thirty-six codes. The ranges say what *kind* of thing happened;
 * they cannot say what to do about it, and the engine's own notes are explicit
 * that several of these codes exist precisely because the remedy differs from
 * the one beside them. `NOBODY_TO_ASK` is its own code because the fix is to
 * grant `unattended`, and "permission denied" would send somebody looking at
 * the wrong permissions.
 *
 * Answering by range got three things actively wrong:
 *
 *   - **The ordinary endings had no sentence at all.** A session that simply
 *     finished is code 2, which is range *zero*, and fell through to "Something
 *     the host reported has no sentence yet" — on the most common ending there
 *     is.
 *   - **A changed key was answered with "pair it first".** Code 201 means the
 *     machine presented a different key from the one that was paired with,
 *     which is what a replaced machine looks like and what an impersonation
 *     looks like. Telling somebody to pair again is telling them to do the one
 *     thing an attacker wants.
 *   - **A locked screen, a declined prompt and a spent asking budget** all read
 *     as "ask for the permission at the host", and none of them is about a
 *     permission.
 *
 * # The ranges are still here
 *
 * As a fallback for a code a *newer* build sends that this one has never heard
 * of. Two machines at different releases still have to talk, so an unknown code
 * has to produce something — but every code this build's protocol declares has
 * its own line below, and a gate fails if one does not.
 *
 * # What a sentence says
 *
 * What to do, not what went wrong internally. The engine's own terse text
 * travels beside it as the detail, so nothing is lost for somebody reading a
 * log; this is the part somebody acts on.
 */

/** How a reason is drawn: the tone a callout or toast uses. */
export type ReasonTone = "info" | "warn" | "danger";

export interface Advice {
  title: string;
  tone: ReasonTone;
}

/**
 * The codes that are not failures.
 *
 * A session ending because it was asked to end is not something to warn
 * about, and a toast in the warning tone for it is a product that cries wolf.
 * They are listed rather than derived from the range, because "below 100" is
 * not a rule anybody would guess.
 */
export const ORDINARY_ENDINGS: readonly number[] = [1, 2, 3, 4, 5, 6];

/** What to do about each reason this build's protocol declares. */
export const ADVICE: Readonly<Record<number, Advice>> = {
  // ── Ordinary endings ──────────────────────────────────────────────────
  1: { title: "The session was closed.", tone: "info" },
  2: { title: "The session ran for as long as it was asked to.", tone: "info" },
  3: { title: "That machine is shutting down. Connect again when it is back.", tone: "info" },
  4: { title: "A newer session with that machine replaced this one.", tone: "info" },
  5: {
    title: "That machine stopped answering — a lid closed, or a network gone. It will be reachable again when it is back.",
    tone: "info",
  },
  6: {
    title: "Somebody at that machine ended the session. The pairing is intact, so you can connect again.",
    tone: "info",
  },

  // ── 1xx, the two builds cannot talk ───────────────────────────────────
  100: { title: "Update Esper on one of the two machines — they speak different protocol versions.", tone: "danger" },
  101: { title: "One machine sent something the other could not read. Update both; if it keeps happening, report it.", tone: "danger" },
  102: { title: "One machine sent more in one piece than the other will accept. Update both.", tone: "danger" },
  103: { title: "That machine sent something this build does not know. Update the older of the two.", tone: "danger" },

  // ── 2xx, who the other machine is ─────────────────────────────────────
  200: { title: "This device is not paired with that machine. Pair it first.", tone: "warn" },
  201: {
    // **Never "pair again".** A key that changed is what a replaced machine
    // looks like, and it is also what somebody standing in the middle looks
    // like. The only safe next step is to check the fingerprint with a person
    // who is there.
    title:
      "That machine is presenting a different key from the one you paired with. Check its fingerprint with somebody who is there before pairing again — a changed key is also what an impersonation looks like.",
    tone: "danger",
  },
  202: { title: "The pairing code did not match. Read it again from the other machine.", tone: "warn" },
  203: { title: "The pairing code has expired. Ask for a new one.", tone: "warn" },
  204: { title: "Too many pairing attempts from here. Wait a little before trying again.", tone: "warn" },

  // ── 3xx, what this device may do ──────────────────────────────────────
  300: { title: "That machine has not granted this device permission for that. Ask at the host.", tone: "warn" },
  301: { title: "This device's access to that machine was taken away. It has to be granted again there.", tone: "warn" },
  302: {
    title: "This device may connect unattended only during set hours, and it is outside them. Somebody at that machine can widen them, or be there to allow it.",
    tone: "warn",
  },
  303: { title: "That machine is not accepting sessions. Somebody there has to turn the host on.", tone: "warn" },
  304: { title: "Nobody at the host allowed the session. Ask them, or have them grant unattended access.", tone: "warn" },
  305: { title: "Somebody at that machine declined the session. Asking again reaches the same person.", tone: "warn" },
  306: {
    title: "Nobody is at that machine to allow this, and this device may not connect unattended. Ask for the unattended permission there.",
    tone: "warn",
  },
  307: {
    title: "The screen at that machine is locked, and it does not admit unattended devices while it is. Somebody has to unlock it.",
    tone: "warn",
  },
  308: {
    title: "This session has asked for more as often as it may. Open a new session, or have the permission granted at the host.",
    tone: "warn",
  },
  309: { title: "That machine does not let a session ask for more. The permission has to be granted there first.", tone: "warn" },

  // ── 4xx, what that machine can do right now ───────────────────────────
  400: { title: "The two machines have no video codec in common. Update the older of the two.", tone: "warn" },
  401: { title: "That machine has no way to capture its screen. Running esper doctor there says what is missing.", tone: "warn" },
  402: { title: "That machine has no display to capture. It may be headless, or nobody is logged in.", tone: "warn" },
  403: {
    title: "That machine cannot send keys or pointer moves, so a session there can watch and not drive. Running esper doctor there says why.",
    tone: "warn",
  },
  404: { title: "The host could not show that it is being watched, so it stopped. Check its display.", tone: "warn" },
  405: { title: "That machine has no way to lock its own screen. A screen locker has to be installed there.", tone: "warn" },
  406: { title: "That machine is already in a session. Try again when it is free.", tone: "info" },

  // ── 5xx, the link between them ────────────────────────────────────────
  500: {
    title: "Nothing could reach that machine: not this network, not an overlay, and not a broker. Check that it is on and reachable.",
    tone: "warn",
  },
  501: { title: "The broker is at capacity. Try again, or run one of your own.", tone: "warn" },
  502: { title: "The connection changed networks and could not be carried over. Connecting again starts a fresh one.", tone: "warn" },
  503: { title: "The session was closed because nothing happened for a while. That is a setting on the host.", tone: "info" },
};

/**
 * What a *newer* build's code means, as far as its range says.
 *
 * Only ever reached by a code this build has never heard of, because every one
 * its own protocol declares is in the table above and a gate says so. Two
 * machines at different releases still have to talk, and "no sentence" is a
 * worse answer than "something in this family".
 */
function byRange(code: number): Advice {
  switch (Math.floor(code / 100)) {
    case 0:
      return { title: "The session ended.", tone: "info" };
    case 1:
      return { title: "The two machines could not agree on how to talk. Update both.", tone: "danger" };
    case 2:
      return { title: "This device is not paired with that machine. Pair it first.", tone: "warn" };
    case 3:
      return { title: "This device is not allowed to do that. Ask for the permission at the host.", tone: "warn" };
    case 4:
      return { title: "That machine cannot do this right now. Its own words are below.", tone: "info" };
    case 5:
      return { title: "The connection was lost. Esper reconnects if the machine comes back.", tone: "warn" };
    default:
      return { title: "That machine reported something this build has no sentence for. The code is below.", tone: "info" };
  }
}

/** Whether this code is a session ending normally rather than a failure. */
export function isOrdinary(code: number): boolean {
  return ORDINARY_ENDINGS.includes(code);
}

/** What a person should do about a reason. */
export function adviceFor(code: number): Advice {
  return ADVICE[code] ?? byRange(code);
}
