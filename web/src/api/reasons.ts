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
  2: { title: "The session reached its time limit and ended.", tone: "info" },
  3: { title: "The other device is shutting down. Connect again when it is back.", tone: "info" },
  4: { title: "A newer session with the other device replaced this one.", tone: "info" },
  5: {
    title: "The other device stopped responding, perhaps because it went to sleep or lost its network. Connect again when it is back.",
    tone: "info",
  },
  6: {
    title: "Someone at the other device ended the session. You are still paired, so you can connect again.",
    tone: "info",
  },

  // ── 1xx, the two builds cannot talk ───────────────────────────────────
  100: { title: "The two devices run versions of Esper that can't talk to each other. Update Esper on both.", tone: "danger" },
  101: { title: "One device sent data the other could not read. Update Esper on both, and report it if it happens again.", tone: "danger" },
  102: { title: "One device sent more data at once than the other accepts. Update Esper on both.", tone: "danger" },
  103: { title: "The other device sent something this version doesn't understand. Update the older one.", tone: "danger" },

  // ── 2xx, who the other machine is ─────────────────────────────────────
  200: { title: "This device is not paired with the other one. Pair them first.", tone: "warn" },
  201: {
    // **Never "pair again".** A key that changed is what a replaced machine
    // looks like, and it is also what somebody standing in the middle looks
    // like. The only safe next step is to check the fingerprint with a person
    // who is there.
    title:
      "The other device's key has changed since you paired. Check its fingerprint words with someone who is there before you pair again. A changed key can mean someone is impersonating it.",
    tone: "danger",
  },
  202: { title: "The pairing code did not match. Read it again from the other device.", tone: "warn" },
  203: { title: "The pairing code has expired. Ask for a new one.", tone: "warn" },
  204: { title: "Too many pairing attempts from this device. Wait a little, then try again.", tone: "warn" },

  // ── 3xx, what this device may do ──────────────────────────────────────
  300: { title: "This device doesn't have permission to do that. Ask someone at the other device to grant it.", tone: "warn" },
  301: { title: "This device's access was removed. Someone at the other device has to grant it again.", tone: "warn" },
  302: {
    title: "This device can only connect without asking during set hours, and it is outside them now. Someone at the other device can change the hours or let you in.",
    tone: "warn",
  },
  303: { title: "The other device is not accepting sessions. Someone there has to turn on screen sharing.", tone: "warn" },
  304: { title: "Nobody at the other device allowed the session. Ask them, or ask them to allow access without asking.", tone: "warn" },
  305: { title: "Someone at the other device declined the session. Asking again reaches the same person.", tone: "warn" },
  306: {
    title: "Nobody is at the other device to allow this, and this device can't connect without asking. Ask for the unattended permission there.",
    tone: "warn",
  },
  307: {
    title: "The other device's screen is locked, so it won't accept a connection without asking. Someone has to unlock it.",
    tone: "warn",
  },
  308: {
    title: "This session has asked for more permissions too many times. Start a new session, or have the permission granted on the other device.",
    tone: "warn",
  },
  309: { title: "The other device doesn't let a session ask for more permissions. The permission has to be granted there first.", tone: "warn" },

  // ── 4xx, what that machine can do right now ───────────────────────────
  400: { title: "The two devices have no video format in common. Update Esper on the older one.", tone: "warn" },
  401: { title: "The other device can't capture its screen. Run esper doctor there to see what is missing.", tone: "warn" },
  402: { title: "The other device has no screen to share. It may have no display, or nobody is logged in.", tone: "warn" },
  403: {
    title: "The other device can't receive keyboard or mouse input, so you can only watch. Run esper doctor there to see why.",
    tone: "warn",
  },
  404: { title: "The other device couldn't show that it is being watched, so the session stopped. Check its display.", tone: "warn" },
  405: { title: "The other device can't lock its own screen. Install a screen locker there.", tone: "warn" },
  406: { title: "The other device is already in a session. Try again when it is free.", tone: "info" },

  // ── 5xx, the link between them ────────────────────────────────────────
  500: {
    title: "Could not reach the other device on this network, a private network like Tailscale, or a broker. Check that it is on and online.",
    tone: "warn",
  },
  501: { title: "The broker is full. Try again later, or run your own.", tone: "warn" },
  502: { title: "The connection switched networks and dropped. Connect again to start a new one.", tone: "warn" },
  503: { title: "The session closed after a period of inactivity. You can change this in the other device's settings.", tone: "info" },
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
      return { title: "The two devices could not connect because their versions differ. Update Esper on both.", tone: "danger" };
    case 2:
      return { title: "This device is not paired with the other one. Pair them first.", tone: "warn" };
    case 3:
      return { title: "This device doesn't have permission to do that. Ask someone at the other device to grant it.", tone: "warn" };
    case 4:
      return { title: "The other device can't do this right now. Its message is below.", tone: "info" };
    case 5:
      return { title: "The connection was lost. Esper reconnects if the other device comes back.", tone: "warn" };
    default:
      return { title: "The other device reported a problem this version doesn't recognise. The code is below.", tone: "info" };
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
