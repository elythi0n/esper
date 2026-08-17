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

export interface Capability {
  available: boolean;
  /** Shown to a person; never parsed. */
  detail: string;
}

export interface Status {
  engine: string;
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

/** Whether a device's pairing has been spent or has lapsed. */
export function pairingState(d: Device): "kept" | "once" | "spent" {
  if (d.pairing === "kept") return "kept";
  return d.admits_a_session ? "once" : "spent";
}
