import type { Permission } from "./permissions";

/*
  The shapes the local API answers with, as Go writes them.

  Written out rather than generated: there are six routes, and a generator
  would be a build step between a person and a field name. Where a field is
  nullable in Go it is nullable here — `last_seen` is a pointer because a
  device paired ten seconds ago has never been seen, and a 0 there would read
  as 1970.
*/

export interface Fingerprint {
  /** What `grant`, `revoke` and `forget` take. */
  hex: string;
  /** What somebody reads out to somebody else. */
  words: string[];
}

/** A pairing good for one session, with a deadline of its own. */
export interface Once {
  used: boolean;
  expires_at: number;
}

/**
 * How long a pairing lasts.
 *
 * Rust writes a unit variant as the bare string `"kept"` and a struct variant
 * as `{"once": {...}}`, and Go reads both by hand. Modelled the same way here
 * rather than as an object with an optional field, because `{}` is a pairing
 * the store refuses — "neither kept nor once".
 */
export type Pairing = "kept" | { once: Once };

/**
 * What a session leaves this machine's screen as.
 *
 * The host has one setting for everything; this is the per-device answer for
 * the machine it is wrong about — a laptop connecting to its owner's own
 * desktop should not lock it under them, and a helper's machine should always
 * leave it locked.
 */
export type LockAfter = "unattended" | "always" | "never";

/** Every choice, in the order the engine lists them. */
export const LOCK_AFTER: readonly LockAfter[] = ["unattended", "always", "never"];

/** What the API takes to remove a per-device answer and follow the host again. */
export const LOCK_DEFAULT = "default";

/** What a person calls each one, in the sentence "after a session, …". */
export const LOCK_AFTER_LABEL: Readonly<Record<LockAfter, string>> = {
  unattended: "Lock if nobody was here",
  always: "Always lock",
  never: "Leave it as it is",
};

export interface Device {
  public_key: string;
  /** Peer-supplied at pairing, editable locally, and **untrusted**. */
  name: string;
  permissions: Permission[];
  paired_at: number;
  /** Unix seconds of the last completed session, or null for never. */
  last_seen: number | null;
  last_address?: string;
  overlay_address?: string;
  pairing: Pairing;
  unattended_when?: string;
  /**
   * What a session with this device leaves the screen as, overriding the
   * host's own setting. Absent means the host decides, which is why the store
   * holds nothing rather than a word meaning "the same".
   */
  lock_after?: LockAfter;
  groups?: string[];
  last_mac?: string;
  /** Derived by the API: what somebody reads out to confirm the device. */
  fingerprint: Fingerprint;
  /** Whether the pairing would still let this device in. */
  admits_a_session: boolean;
}

export interface DevicesAnswer {
  devices: Device[];
  /** Which file this came from. With ESPER_HOME set there is more than one answer. */
  read_from: string;
}

/**
 * What kind of thing a setting holds.
 *
 * The engine's own vocabulary, passed through: it is what decides the bounds,
 * the words a choice may be and whether a value is readable at all, and a
 * second list here would be a second thing to keep in step. A screen that
 * offered a value the file will not take is the way that failure shows.
 */
export type SettingKind =
  | "text"
  | "flag"
  | "whole"
  | "whole_or_none"
  | "cores"
  | "address"
  | "key"
  | "choice"
  | "time";

/** One setting, as the engine reports it. */
export interface Setting {
  key: string;
  /** One sentence, from the engine's own table. */
  about: string;
  /** How to write one, as a person reads it. */
  shape: string;
  kind: SettingKind;
  /** Where the value in force came from: a default, the file, the environment, a flag. */
  source: string;
  /** The value, in its own type. `null` is a setting deliberately set to nothing. */
  value: string | number | boolean | null;
  /** The same, as the file writes it and as a change sends it back. */
  raw: string;
  default: string | number | boolean | null;
  /** The bounds, for a number. */
  least?: number;
  most?: number;
  /** The words, for a choice. */
  choices?: string[];
  /**
   * Whether this one may only be changed at the machine, and why not.
   *
   * Six settings are: each either takes away the ability of whoever is in
   * front of the screen to know what is happening to it, or widens what a
   * paired device may reach without a grant. The screen says so rather than
   * offering a control that always fails.
   */
  at_the_machine: boolean;
  why_at_the_machine?: string;
}

export interface SettingsAnswer {
  /** Which file the values came from, or null where there is no home. */
  file: string | null;
  /** Whether that file exists. A machine with none runs on defaults, which is ordinary. */
  present: boolean;
  settings: Setting[];
}

export interface Capability {
  available: boolean;
  /** One short line. Shown to a person; never parsed. */
  detail: string;
  /**
   * Every backend that was tried, in the order they were tried, one line
   * each. Absent where there was nothing to choose between, and absent from
   * an engine older than the field.
   */
  attempts?: string[];
}

/**
 * What a capability tried, for the screens that show it.
 *
 * **Only when something is missing.** A working backend's rejected
 * alternatives are noise beside the one in use; a missing one's are the whole
 * answer, and are what turns "no input backend" from a fault into an errand.
 *
 * In one place because two screens show this and the rule is the same on both
 * — and a rule written twice is a rule that will be true in one of them.
 */
export function triedFor(cap: Capability): string[] {
  if (cap.available) return [];
  return cap.attempts ?? [];
}

export interface Status {
  engine: string;
  /**
   * What kind of machine this is, and what it is showing on.
   *
   * What makes a remedy possible: "no capture backend" is a different
   * sentence on a Mac from on Wayland, and a screen with the fault but not
   * the platform can only say what is wrong. Optional because an engine older
   * than the field answers without it, and "not said" has a sentence of its
   * own.
   */
  platform?: { os: string; display: string };
  capture: Capability;
  encoder: Capability;
  input: Capability;
  overlays: string[];
  mdns: boolean;
  /** No default broker exists anywhere in Esper, so null is the ordinary answer. */
  broker: string | null;
  paired: number;
}

/** One line of what has happened on this machine. */
export interface Happening {
  at: number;
  peer: string;
  key: string;
  /** What that device is called now, or empty if it is not paired any more. */
  device: string;
  event: string;
  detail: string;
  /** "in" for somebody who connected here, "out" for something this machine did. */
  direction?: "in" | "out";
}

/**
 * A way in that this machine is currently offering.
 *
 * **The link is the secret.** Anybody holding it can pair with this machine
 * until it expires — which is what it is for, and why it is short-lived, why
 * the pairing it grants is single use, and why the screen says so out loud
 * rather than presenting it as a share link.
 */
export interface Invitation {
  link: string;
  for?: string;
  /** The rendezvous it meets at, so the screen can say where. */
  broker?: string;
  expires_at: number;
  state: "offering" | "paired" | "expired" | "failed" | "none";
  detail?: string;
}

/** What this machine is, to anybody who has paired with it. */
export interface Identity {
  /** The short fingerprint, the same form the device store uses. */
  key: string;
  /**
   * The whole key, which is what a peer pins.
   *
   * Not what a screen shows: sixty-four hex characters is not something a
   * person compares, and the words below are what two people read to each
   * other. It is here because a caller that wants to pin something wants
   * this and not the fingerprint, and because a field called `key` holding
   * sixty-four bits is how somebody comes to pin a birthday bound.
   */
  public_key?: string;
  /**
   * What somebody reads aloud to check it.
   *
   * The half of pairing a code cannot do: a code proves the same code was
   * typed at both ends, and these prove the machine at the other end is the
   * one that was meant.
   */
  words: string[];
  /** Where the key lives, for somebody who would rather look themselves. */
  file?: string;
}

/**
 * Whether this machine serves its screen.
 *
 * **Two flags, not one.** `enabled` is "it starts at login" and `active` is
 * "it is running now", and they can disagree — a host that is running but
 * not enabled will be gone after a reboot, which is not a thing to let
 * somebody find out the hard way.
 */
export interface HostState {
  /** False on a platform with no service manager we wrap. Nothing else means anything then. */
  managed: boolean;
  enabled: boolean;
  active: boolean;
  /** Which manager, for the line under the switch. */
  manager: string;
  why?: string;
  /** Every command the change ran, in order, so it is not a black box. */
  said?: string;
}

/** A paired device announcing itself on this network, right now. */
export interface Seen {
  address: string;
}

/**
 * What a look at the network found.
 *
 * **One rung of the ladder.** Only the LAN announces itself, so a device here
 * is genuinely reachable and a device absent may still be perfectly reachable
 * over an overlay or at an address. That is why the screen says "on this
 * network" and never "online": a lamp meaning reachable would be read as a
 * promise, and would break the first time somebody clicked a green device
 * that was only ever reachable through a broker.
 */
export interface Reachable {
  seen: Record<string, Seen>;
  /** What was actually looked at, so a screen need not imply more. */
  looked: string;
}

export interface Health {
  running: boolean;
  /**
   * Why there is no engine, when the control plane knows. Absent while one
   * is running, and absent when the control plane did not start it and so
   * has nothing to say beyond the fact.
   */
  why?: string;
  /**
   * Whether *this* control plane can start it again. Only the process
   * holding the supervisor can, so a window talking to somebody else's
   * `esper api` is told plainly that the button would not work.
   */
  restartable: boolean;
}

/**
 * Whether a pairing offer can be checked by the person holding it.
 *
 * **Both halves, or neither.** A code proves somebody typed what you read out;
 * only the fingerprint proves the machine at the other end is the one you
 * meant. An offer with a code and no words is a code somebody will use without
 * checking anything — and the screen that showed it would have an empty space
 * where the check belongs, which reads as "nothing to check".
 *
 * In one place because two things depend on it: the screen that refuses to
 * show such an offer, and this rule being something a check can hold.
 */
export function offerIsCheckable(offer: { code?: string; words?: string[] }): boolean {
  return Boolean(offer.code) && Array.isArray(offer.words) && offer.words.length > 0;
}

/** Whether a device's pairing has been spent or has lapsed. */
export function pairingState(d: Device): "kept" | "once" | "spent" {
  if (d.pairing === "kept") return "kept";
  return d.admits_a_session ? "once" : "spent";
}
