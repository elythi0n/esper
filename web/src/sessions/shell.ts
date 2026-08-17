/**
 * What the window around this page can do.
 *
 * The desktop shell answers `/shell/*` the same way it proxies `/api/*`, so
 * the page asks over HTTP rather than through generated bindings — one seam,
 * no vendor globals, and it works identically in a browser tab (where the
 * answer is simply "no window to open").
 */

export interface ShellCapabilities {
  /** The shell can open a second OS window. */
  popout: boolean;
  /** What is hosting this page, for the About line. */
  host: "desktop" | "browser";
}

/** A browser tab: no window to control, and nothing to draw for one. */
const NO_SHELL: ShellCapabilities = { popout: false, host: "browser" };

/**
 * What the host wrote onto the document before this bundle loaded.
 *
 * The two facts the *first paint* depends on, which is why they are not
 * asked for over HTTP like everything else: a bar that arrives a frame late
 * shifts the layout by its own height, and a frameless window whose request
 * for its capabilities fails has no close button and cannot be shut.
 *
 * Absent means a browser tab, which owes a window nothing.
 */
export const host = {
  /** The window is frameless, so the page owes it a title bar and buttons. */
  chrome: document.documentElement.dataset.desktop === "true",
  /** Where the host forwards `/api/*`. Empty in a browser tab, which forwards nowhere. */
  api: document.documentElement.dataset.api ?? "",
} as const;

let asked: Promise<ShellCapabilities> | null = null;

export function shellCapabilities(): Promise<ShellCapabilities> {
  asked ??= fetch("/shell/capabilities")
    .then((r) => (r.ok ? (r.json() as Promise<ShellCapabilities>) : NO_SHELL))
    .catch(() => NO_SHELL);
  return asked;
}

/** Whether this window opens when somebody logs in. */
export interface Autostart {
  on: boolean;
  /** False on a platform with nowhere to put an entry. The switch is not drawn. */
  available: boolean;
  /** Where the entry lives, for somebody who would rather look themselves. */
  path?: string;
  why?: string;
}

/** A browser tab has no login to open at. */
const NO_AUTOSTART: Autostart = { on: false, available: false };

export async function readAutostart(): Promise<Autostart> {
  try {
    const r = await fetch("/shell/autostart");
    return r.ok ? ((await r.json()) as Autostart) : NO_AUTOSTART;
  } catch {
    return NO_AUTOSTART;
  }
}

/**
 * Turn it on or off, and answer with what is now true on disk.
 *
 * The reply is read back from the same place the switch reads it, rather than
 * the page assuming the write worked: autostart is a file somebody else can
 * delete, and a switch that remembers what it was told is a switch that lies
 * the first time something goes wrong.
 */
export async function setAutostart(on: boolean): Promise<Autostart> {
  const r = await fetch("/shell/autostart", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ on }),
  });
  if (!r.ok) throw new Error((await r.text()).trim() || "That could not be changed.");
  return (await r.json()) as Autostart;
}

/** The three the OS used to draw. No-ops in a browser tab, which has no window. */
export async function windowAction(what: "minimise" | "maximise" | "close" | "fullscreen"): Promise<void> {
  try {
    await fetch(`/shell/window/${what}`, { method: "POST" });
  } catch {
    // Nothing to control. The button is not drawn in that case anyway.
  }
}

/**
 * Ask the shell for a second window on a route.
 *
 * **Why this is not `window.open`.** In the WebKitGTK webview Wails 2 uses,
 * `window.open` returns `null` — measured, not assumed — so dockview's own
 * pop-out cannot work there and neither can anything else built on it. The
 * shell spawns a second process with its own window instead, which is a
 * better fit for this product anyway: a popped-out session is its own
 * webview with its own media pipeline rather than a second video element
 * competing inside one.
 */
export async function popOutWindow(route: string): Promise<boolean> {
  try {
    const r = await fetch("/shell/popout", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ route }),
    });
    return r.ok;
  } catch {
    return false;
  }
}
