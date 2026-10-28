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
            todo: "Allow Esper to record the screen, then restart Esper. Until you do, sessions show an empty desktop.",
            where: "System Settings → Privacy & Security → Screen Recording",
          },
        ]
      : [
          {
            what,
            todo: "Allow Esper to control this computer. Without it, the other device can watch but not use the mouse or keyboard.",
            where: "System Settings → Privacy & Security → Accessibility",
          },
        ];
  }

  if (os === "windows") {
    return what === "capture"
      ? [{ what, todo: "Attach a monitor and sign in to Windows. Esper can't capture the screen without both." }]
      : [{ what, todo: "Run Esper as administrator to control apps that run as administrator. Windows blocks input to them otherwise." }];
  }

  if (os === "linux" && display === "wayland") {
    return what === "capture"
      ? [
          {
            what,
            todo: "Install xdg-desktop-portal and the portal for your desktop, and make sure both are running. Wayland needs them to share the screen.",
          },
          { what, todo: "Your desktop asks which screen to share when the first session starts. The session waits until you answer." },
        ]
      : [
          {
            what,
            todo: "Give this account access to /dev/uinput, usually by joining the input group and logging in again.",
          },
        ];
  }

  if (os === "linux" && display === "headless") {
    return [
      {
        what,
        todo:
          what === "capture"
            ? "This machine has no display, so there is no screen to share. A session would show a test pattern."
            : "This machine has no display, so there is nothing to send input to.",
      },
    ];
  }

  if (os === "linux") {
    return what === "capture"
      ? [{ what, todo: "Esper reads the screen through the X server. Make sure Esper was started with your session's DISPLAY. A service started before you logged in usually isn't." }]
      : [{ what, todo: "Esper sends input through the X server's XTEST extension. Make sure it is turned on and Esper has your session's DISPLAY." }];
  }

  return [
    {
      what,
      todo: "Esper doesn't know what kind of machine this is. Run `esper doctor` to see what it tried.",
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
