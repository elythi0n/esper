import type {
  Device,
  DevicesAnswer,
  Happening,
  Health,
  HostState,
  Identity,
  Invitation,
  LockAfter,
  Reachable,
  SettingsAnswer,
  Status,
} from "./types";
import { FIRST_WAIT_MS, nextWait } from "./retry";
import { LOCK_DEFAULT } from "./types";
import type { Permission } from "./permissions";

/**
 * The control plane, from the webview.
 *
 * # The token is not in this page
 *
 * The local API wants a bearer token from a 0600 file under the Esper home,
 * and it refuses any request addressed by a name rather than by the literal
 * address it bound — a DNS-rebinding defence. Handing that token to
 * JavaScript would solve the first problem and create two others: it would be
 * a cross-origin request the API has no reason to allow, and the token would
 * be one XSS away from leaving the machine.
 *
 * So **the host proxies**. The desktop shell serves this bundle and forwards
 * `/api/*` to the local API, adding the Authorization header on the way
 * through; `vite dev` does the same from the token file for a browser tab.
 * The page therefore makes same-origin requests with no credential in them,
 * the API's posture is untouched, and there is nothing here to steal.
 *
 * What the page can still tell is whether the proxy has an upstream, which is
 * the honest failure to show: *the control plane did not answer*.
 */

/** Where the proxy lives. Same origin unless something unusual says otherwise. */
const BASE = ((import.meta.env as Record<string, string | undefined>).VITE_ESPER_API ?? "/api").replace(/\/$/, "");

/** The API answered, and what it said was no. Carries its own sentence. */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
    /** True when nothing was reached at all, rather than reached and refused. */
    readonly unreachable = false,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  let response: Response;
  try {
    response = await fetch(`${BASE}${path}`, {
      ...init,
      headers: {
        ...(init?.body ? { "Content-Type": "application/json" } : {}),
        ...init?.headers,
      },
    });
  } catch {
    throw new ApiError(0, "Nothing answered. This window has no control plane behind it.", true);
  }
  if (!response.ok) {
    const said = (await response.text()).trim();
    // 502 and 504 are the proxy saying it could not reach the API — a
    // different fault from the API saying no, and a different thing to do
    // about it.
    const unreachable = response.status === 502 || response.status === 504;
    throw new ApiError(
      response.status,
      said || `The control plane answered ${response.status}.`,
      unreachable,
    );
  }
  if (response.status === 204 || response.headers.get("content-length") === "0") {
    return undefined as T;
  }
  return (await response.json()) as T;
}

export const api = {
  health: () => call<Health>("/health"),
  status: () => call<Status>("/status"),
  devices: () => call<DevicesAnswer>("/devices"),
  /** Which paired devices are announcing themselves on this network. Takes a couple of seconds. */
  reachable: () => call<Reachable>("/devices/reachable"),

  /** What this machine is: its fingerprint, and the words for reading it aloud. */
  identity: () => call<Identity>("/identity"),
  /** Whether this machine serves its screen. */
  host: () => call<HostState>("/host"),
  /** Turn it on or off through the machine's own service manager. */
  setHost: (on: boolean) => call<HostState>("/host", { method: "POST", body: JSON.stringify({ on }) }),
  sessions: (most = 50) => call<Happening[]>(`/sessions?most=${most}`),

  /** Start the engine again after it stopped. Refused where this plane did not start it. */
  restartEngine: () => call<void>("/engine/restart", { method: "POST" }),

  grant: (fingerprint: string, permissions: Permission[]) =>
    call<DevicesAnswer>(`/devices/${fingerprint}/grant`, { method: "POST", body: JSON.stringify({ permissions }) }),
  revoke: (fingerprint: string, permissions: Permission[]) =>
    call<DevicesAnswer>(`/devices/${fingerprint}/revoke`, { method: "POST", body: JSON.stringify({ permissions }) }),
  forget: (fingerprint: string) =>
    call<{ forgotten: string; read_from: string }>(`/devices/${fingerprint}`, { method: "DELETE" }),
  /** Turn a one-session pairing into a lasting one. The screen showed `once` and could do nothing. */
  keep: (fingerprint: string) => call<DevicesAnswer>(`/devices/${fingerprint}/keep`, { method: "POST" }),
  /**
   * When a device's unattended access applies.
   *
   * `when` is the engine's own vocabulary — `weekdays 08:00-18:00`, `always`,
   * `clear` — passed through rather than translated, so there is one grammar
   * and the thing that decides what a schedule means is the engine's parser.
   */
  schedule: (fingerprint: string, when: string) =>
    call<DevicesAnswer>(`/devices/${fingerprint}/schedule`, {
      method: "POST",
      body: JSON.stringify({ when }),
    }),
  /**
   * What a session with this device leaves the screen as.
   *
   * One of the three choices, or `"default"` to take the per-device answer
   * off so the host's own setting decides again. Taking it off has to be as
   * easy as giving it, or somebody who changes their mind edits a file.
   */
  lockAfter: (fingerprint: string, after: LockAfter | typeof LOCK_DEFAULT) =>
    call<DevicesAnswer>(`/devices/${fingerprint}/lock-after`, {
      method: "POST",
      body: JSON.stringify({ after }),
    }),
  /**
   * Send a magic packet to a device asleep on this network.
   *
   * **Answers when the packet has gone out, not when anything woke.** Nothing
   * replies to a magic packet — there is no way to tell a machine that woke
   * from one that never heard — so whatever says so to a person has to say
   * that much. It takes no destination: the broadcast address is the only
   * place the control plane will send one.
   */
  wake: (fingerprint: string) => call<DevicesAnswer>(`/devices/${fingerprint}/wake`, { method: "POST" }),

  /*
    Three actions on a paired host, each behind a permission that host granted
    this one.

    **Whether they are allowed is decided at the other end**, and this side
    keeps no copy of somebody else's trust store — so these are offered and
    the host's own answer is what comes back. Its sentence names the permission
    that is missing, which is the thing to do something about.

    Nothing answers a restart: the machine is gone before it could, so a
    request that ends without a reply is the ordinary case rather than a
    failure.
  */
  lockHost: (fingerprint: string) => call<Device>(`/devices/${fingerprint}/lock`, { method: "POST" }),
  logOffHost: (fingerprint: string) => call<Device>(`/devices/${fingerprint}/logoff`, { method: "POST" }),
  restartHost: (fingerprint: string) => call<Device>(`/devices/${fingerprint}/reboot`, { method: "POST" }),

  /**
   * Put a device in a named group, or take it out of one.
   *
   * **A group is an address, not a grant.** `esper grant --group ops control`
   * resolves the members and writes the permission onto each, so membership
   * by itself gives a device nothing — what it changes is who a later command
   * is about.
   */
  group: (fingerprint: string, name: string, remove = false) =>
    call<Device>(`/devices/${fingerprint}/groups`, {
      method: "POST",
      body: JSON.stringify({ name, remove }),
    }),

  /**
   * Every setting, what it is now, and where that came from.
   *
   * The whole registry rather than the values: what control a setting wants
   * depends on its kind, and the bounds and the choices travel with it so the
   * page never declares them a second time.
   */
  settings: () => call<SettingsAnswer>("/config"),
  /**
   * Change one setting, or take it back out of the file.
   *
   * `value` is what the file would write, which is the `raw` the answer gives
   * — so what came back can be sent back. Unsetting is its own field because
   * an empty string is a real value for the settings whose kind is text.
   */
  setSetting: (key: string, value: string) =>
    call<SettingsAnswer>("/config", { method: "POST", body: JSON.stringify({ key, value }) }),
  unsetSetting: (key: string) =>
    call<SettingsAnswer>("/config", { method: "POST", body: JSON.stringify({ key, unset: true }) }),

  // Pairing, both directions. An offer waits, so it has a lifetime: started,
  // polled, and either paired, lapsed or cancelled.
  pairOffer: () => call<unknown>("/pair/offer", { method: "POST" }),
  pairState: () => call<unknown>("/pair/offer"),
  pairCancel: () => call<void>("/pair/offer", { method: "DELETE" }),
  pairJoin: (address: string, code: string) =>
    call<void>("/pair/join", { method: "POST", body: JSON.stringify({ address, code }) }),
  /** Take somebody up on an invitation — a different exchange from a code, not a shortcut to one. */
  pairAccept: (link: string) => call<void>("/pair/accept", { method: "POST", body: JSON.stringify({ link }) }),

  // Offering one, which waits at the broker exactly as a code waits on a port.
  invite: (forWhom: string) => call<Invitation>("/invite", { method: "POST", body: JSON.stringify({ for: forWhom }) }),
  inviteState: () => call<Invitation>("/invite"),
  inviteCancel: () => call<void>("/invite", { method: "DELETE" }),
};

/** Where this window is talking to, for the status strip. */
export const apiBase = BASE;

/**
 * The one event stream, shared by everything that wants it.
 *
 * **One, deliberately.** A stream never closes, so a component that opens its
 * own holds a connection for the life of the page: with a handful of them the
 * browser's per-origin limit is reached and every *other* request queues
 * behind them for ever — seven screens on one page and not one device list
 * loads. The stream is a broadcast anyway, so one reader and a list of
 * listeners is also the smaller thing.
 */
const listeners = new Set<(h: Happening) => void>();
let openStream: (() => void) | null = null;

/**
 * Whether the live stream is carrying anything, and why not when it is not.
 *
 * **Because a window with no stream looks exactly like a quiet machine.**
 * Every screen here also asks outright — the device list, the record, the
 * status — so a failed stream costs nothing but *liveness*, and nothing on
 * screen would say the liveness had gone. A person watching an Activity
 * screen that had silently stopped updating would read it as nothing having
 * happened.
 */
const watchers = new Set<(trouble: Error | null) => void>();
let streamTrouble: Error | null = null;

/** How long to wait before trying the stream again. See `./retry`. */
let nextRetryMs = FIRST_WAIT_MS;
let retryTimer: ReturnType<typeof setTimeout> | null = null;

function announce(trouble: Error | null): void {
  streamTrouble = trouble;
  for (const watch of watchers) watch(trouble);
}

/**
 * Watch whether the live stream is up. Answers immediately with what it is
 * now, so a screen that mounts while the stream is already down says so.
 */
export function onStreamTrouble(watch: (trouble: Error | null) => void): () => void {
  watchers.add(watch);
  watch(streamTrouble);
  return () => {
    watchers.delete(watch);
  };
}

function stopRetrying(): void {
  if (retryTimer !== null) {
    clearTimeout(retryTimer);
    retryTimer = null;
  }
  nextRetryMs = FIRST_WAIT_MS;
}

function beginStream(): void {
  openStream = streamSessions(
    (h) => listeners.forEach((l) => l(h)),
    // Open again: whatever it was is over, and the wait starts from the
    // shortest one next time.
    () => {
      nextRetryMs = FIRST_WAIT_MS;
      if (streamTrouble) announce(null);
    },
    (cause) => {
      openStream = null;
      announce(cause);
      // **Tried again, and with a wait that grows.** A control plane that is
      // restarting comes back in a second; one refusing because too many
      // streams are open comes back when something closes one — and asking
      // it four times a second in the meantime would be this window making
      // that worse.
      if (listeners.size === 0) return;
      retryTimer = setTimeout(() => {
        retryTimer = null;
        if (listeners.size > 0 && !openStream) beginStream();
      }, nextRetryMs);
      nextRetryMs = nextWait(nextRetryMs);
    },
  );
}

export function onHappening(listener: (h: Happening) => void): () => void {
  listeners.add(listener);
  if (!openStream && retryTimer === null) beginStream();
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) {
      stopRetrying();
      if (openStream) {
        openStream();
        openStream = null;
      }
      // Nobody is listening, so there is no trouble to report.
      if (streamTrouble) announce(null);
    }
  };
}

/**
 * The event stream, as sessions happen.
 *
 * `EventSource` would do, and this does not use it: the stream is read by
 * hand so a failure arrives as an error with a sentence rather than as
 * `EventSource`'s silent reconnect loop, which would hide a control plane
 * that had gone away.
 *
 * **A notification, not a replay** — the API sends nothing for what happened
 * before the stream opened, because `/sessions` is the record.
 */
function streamSessions(
  deliver: (h: Happening) => void,
  onOpen?: () => void,
  onTrouble?: (e: Error) => void,
): () => void {
  const stop = new AbortController();

  void (async () => {
    try {
      const response = await fetch(`${BASE}/events`, { signal: stop.signal });
      if (!response.ok || !response.body) {
        // The reason, not only the number: the control plane refuses a stream
        // past its ceiling with a sentence saying what to do about it, and
        // "503" alone sends nobody anywhere.
        const said = response.ok ? "" : await response.text().catch(() => "");
        throw new ApiError(
          response.status,
          said.trim() || `The event stream answered ${response.status}.`,
        );
      }
      onOpen?.();
      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      for (;;) {
        const { done, value } = await reader.read();
        if (done) return;
        buffer += decoder.decode(value, { stream: true });
        // SSE frames are separated by a blank line.
        let split = buffer.indexOf("\n\n");
        while (split >= 0) {
          const frame = buffer.slice(0, split);
          buffer = buffer.slice(split + 2);
          const data = frame
            .split("\n")
            .filter((l) => l.startsWith("data:"))
            .map((l) => l.slice(5).trim())
            .join("");
          if (data) {
            try {
              deliver(JSON.parse(data) as Happening);
            } catch {
              // A frame this build cannot read is skipped, not fatal.
            }
          }
          split = buffer.indexOf("\n\n");
        }
      }
    } catch (cause) {
      if (stop.signal.aborted) return;
      onTrouble?.(cause instanceof Error ? cause : new Error(String(cause)));
    }
  })();

  return () => stop.abort();
}
