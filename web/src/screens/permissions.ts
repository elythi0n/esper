/**
 * What a person has to go and do, when this machine cannot do something yet.
 *
 * # Why the platform decides and not the fault
 *
 * "No capture backend" is one fault and four remedies. On a Mac somebody has
 * to allow screen recording in System Settings; on Wayland a portal has to be
 * installed and running; on X11 the display is usually just not the one this
 * process was given; and on a server there is no screen to capture at all.
 * A screen that had the fault but not the platform could say what is wrong
 * and never what to do about it — which is the one thing this product's
 * content rules ask of every failure.
 *
 * # Why it is a table and not sentences in a component
 *
 * Because it is the part that has to be right on machines nobody here can
 * test. A table can be read by somebody who owns that platform and corrected
 * without touching a screen, and the words gate reads every one of these the
 * same way it reads the rest of the product's writing.
 */

/** What the engine says this machine is. See `Status.platform`. */
export interface Platform {
  os: string;
  display: string;
}

/** One thing to do, on one platform, when one capability is missing. */
export interface Step {
  /** What is missing, in the product's own words. */
  what: "capture" | "input";
  /** The step, as an instruction somebody can follow. */
  todo: string;
  /** Where it is done, when that is a place rather than a command. */
  where?: string;
}

/**
 * What to do about a capability this machine does not have.
 *
 * Returns nothing at all when the capability is there — a checklist of things
 * that are already true is a checklist nobody reads, and it would be on
 * screen for ever on a machine where everything works.
 */
export function stepsFor(platform: Platform, what: "capture" | "input", available: boolean): Step[] {
  if (available) return [];
  const os = platform.os || "";
  const display = platform.display || "";

  if (os === "macos") {
    return what === "capture"
      ? [
          {
            what,
            todo: "Allow Esper to record the screen, then start it again. macOS only asks once, and until it is allowed a session shows an empty desktop.",
            where: "System Settings → Privacy & Security → Screen Recording",
          },
        ]
      : [
          {
            what,
            todo: "Allow Esper to control the computer. Without it a session can be watched and not driven.",
            where: "System Settings → Privacy & Security → Accessibility",
          },
        ];
  }

  if (os === "windows") {
    return what === "capture"
      ? [{ what, todo: "Esper captures through Desktop Duplication, which needs a display adapter and a signed-in session. A machine with no monitor attached has neither." }]
      : [{ what, todo: "Run Esper as administrator to drive windows that are themselves elevated. Windows refuses input from a program with less privilege than the window it is sent to." }];
  }

  if (os === "linux" && display === "wayland") {
    return what === "capture"
      ? [
          {
            what,
            todo: "Install xdg-desktop-portal and the backend for your desktop, and make sure both are running. Wayland has no way for a program to read the screen without one.",
          },
          { what, todo: "The first session asks you which screen to share. That question comes from the desktop, not from Esper, and a session cannot start until it is answered." },
        ]
      : [
          {
            what,
            todo: "Give this account access to /dev/uinput, usually by joining the input group and logging in again. Wayland compositors accept injected input through the kernel rather than from another program.",
          },
        ];
  }

  if (os === "linux" && display === "headless") {
    return [
      {
        what,
        todo:
          what === "capture"
            ? "This machine has no display. Esper can host a screen only where there is one — a server reached over SSH has none, and a session would show a test pattern."
            : "This machine has no display, so there is nothing to inject input into.",
      },
    ];
  }

  if (os === "linux") {
    return what === "capture"
      ? [{ what, todo: "Esper reads the screen through the X server. Check that this process was started with the session's DISPLAY — a service started before you logged in usually was not." }]
      : [{ what, todo: "Esper injects input through the X server's XTEST extension. Check that it is enabled and that this process has the session's DISPLAY." }];
  }

  return [
    {
      what,
      todo: "Esper cannot say what to do on this machine, because it does not know what kind it is. Run `esper doctor` and read what it says it tried.",
    },
  ];
}

/** Every step for a machine, in the order somebody would work through them. */
export function checklist(
  platform: Platform,
  have: { capture: boolean; input: boolean },
): Step[] {
  return [...stepsFor(platform, "capture", have.capture), ...stepsFor(platform, "input", have.input)];
}
