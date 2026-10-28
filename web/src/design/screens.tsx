import { useState } from "react";
import { useDevices } from "@/api/hooks";
import type { Device } from "@/api/types";
import { ActivityScreen } from "@/screens/ActivityScreen";
import { DevicesScreen } from "@/screens/DevicesScreen";
import { FirstRun } from "@/screens/FirstRun";
import { WelcomeScreen } from "@/screens/WelcomeScreen";
import { InviteScreen } from "@/screens/InviteScreen";
import { PairScreen, UncheckableOffer } from "@/screens/PairScreen";
import { SessionsScreen } from "@/sessions/SessionsScreen";
import { SettingsScreen } from "@/screens/SettingsScreen";
import { EngineTrouble, StatusStrip, StreamTrouble } from "@/screens/StatusStrip";
import { AppShell, StatusBar, StatusSpacer } from "@/shell/AppShell";
import { SCREENS } from "@/shell/screens";
import { Section, Sub } from "./ui";

/**
 * The screens themselves, on the gate.
 *
 * **Live, not fixtures.** Each of these is the component the product routes
 * to, reading the same control plane, so what the gate shows is what ships
 * down to the sentence in an error. With nothing running they show their own
 * unreachable states, which are the states worth reviewing most and the ones
 * a fixture would have quietly replaced with a happy path.
 *
 * Each sits in a shell of its own at a window's size, because a screen
 * reviewed outside its frame is a screen whose topbar and status strip
 * nobody looked at.
 */

const FRAME = { width: 1000, height: 620 };

function Framed({ children, active }: { children: React.ReactNode; active: string }) {
  const [, setWhere] = useState(active);
  return (
    <div
      style={FRAME}
      className="max-w-full shrink-0 overflow-hidden rounded-md border border-border"
    >
      <AppShell
        groups={SCREENS}
        active={active}
        onSelect={setWhere}
        version="—"
        collapsed={false}
        onCollapsedChange={() => undefined}
        status={
          <StatusBar>
            <StatusStrip />
            {/*
              And the state the strip shows when the thing that does the work
              is missing, which the real one never reaches here: the gate's own
              engine is running. Beside it rather than instead of it, because
              both are the strip and a gallery that showed only the failure
              would be a gallery of one state.
            */}
            <EngineTrouble why="the engine is not running: engine binary not found: looked in [/usr/lib/esper/esper-engine $PATH] — esper-engine and esper ship together, so this is usually a partial install" />
            {/*
              And the other state the strip has that the real one does not
              reach here: the live stream down while everything else answers.
              It is the one that looks like a working machine, so it is the
              one worth having on a gate.
            */}
            <StreamTrouble why="The event stream answered 503. This machine is already sending 64 event streams, which is as many as it keeps open. One is enough for a window: close the ones that are no longer read, or ask again" />
            <StatusSpacer />
          </StatusBar>
        }
      >
        {children}
      </AppShell>
    </div>
  );
}

/**
 * A device to open the panel on, from whatever this machine actually has.
 *
 * Not a fingerprint written down here: a fingerprint is derived from a key,
 * so one in this file would be a constant that is right for one store and
 * silently wrong — an unopened panel — for every other. The one with the most
 * to say is chosen instead, because a device with no schedule, no group and
 * no hardware address renders a panel with half its controls absent, and that
 * is the panel nobody needs checking.
 */
function useRichestDevice(): string | undefined {
  const { data } = useDevices();
  const best = [...(data ?? [])].sort((a, b) => weightOf(b) - weightOf(a))[0];
  return best?.fingerprint.hex;
}

function weightOf(d: Device): number {
  return (
    d.permissions.length +
    (d.unattended_when ? 4 : 0) +
    (d.lock_after ? 2 : 0) +
    (d.last_mac ? 2 : 0) +
    (d.groups?.length ?? 0)
  );
}

export function ScreensSection() {
  const nothing = () => undefined;
  const opened = useRichestDevice();
  return (
    <Section
      id="screens"
      title="The screens"
      lede="Every screen, live against this machine, including the states where something is missing."
    >
      <Sub
        title="Devices"
        about="The home screen: each paired device, and one click to connect."
      >
        <Framed active="devices">
          <DevicesScreen onConnect={nothing} onPair={nothing} onSetUp={nothing} />
        </Framed>
      </Sub>

      <Sub
        title="A device, opened"
        about="Everything about one device: its permissions, its hours, and what you can do to it."
      >
        <Framed active="devices">
          <DevicesScreen onConnect={nothing} onPair={nothing} onSetUp={nothing} initial={opened} />
        </Framed>
      </Sub>

      <Sub
        title="Set up this machine"
        about="Setting up this machine, in three steps."
      >
        <Framed active="devices">
          <WelcomeScreen onPair={nothing} onDone={nothing} />
        </Framed>
      </Sub>

      <Sub title="Activity" about="Everything that has happened on this machine, sortable and searchable.">
        <Framed active="activity">
          <ActivityScreen />
        </Framed>
      </Sub>

      <Sub title="Settings" about="What this machine can do, measured, and how this window looks, chosen.">
        <Framed active="settings">
          <SettingsScreen />
        </Framed>
      </Sub>

      {/*
        A grid, not a wrapping flex row.

        Each specimen is a shell at a window's width, and a flex item's
        automatic minimum size is its *content* — so two 1000px frames in a
        row refused to shrink below 1000px however narrow the window got, and
        `max-w-full` on the frame never came into it. The page grew a
        horizontal scrollbar 644px wide at phone width, on the gate itself,
        for months. `grid-cols-2` is `minmax(0, 1fr)`, which is the bound the
        flex row did not have; found by `check-overflow`.
      */}
      <div className="grid gap-6 lg:grid-cols-2">
        <Sub className="min-w-0" title="Pair" about="Show a code or type one in. The eight words prove you reached the right machine.">
          <Framed active="pair">
            <PairScreen onDone={nothing} />
          </Framed>
        </Sub>
        <Sub className="min-w-0" title="Invite" about="Invitations need a broker, and the screen says so when there is none.">
          <Framed active="invite">
            <InviteScreen />
          </Framed>
        </Sub>
      </div>

      <Sub
        title="A code with nothing to check it against"
        about="An incoming pairing is only shown with its eight words, so it can always be checked."
      >
        <div className="max-w-lg">
          <UncheckableOffer onAgain={nothing} />
        </div>
      </Sub>

      <Sub
        title="Sessions"
        about="Several sessions at once, as tabs you can reorder, split or pop out."
      >
        <Framed active="sessions">
          <SessionsScreen />
        </Framed>
      </Sub>

      <Sub title="First run" about="What the window shows when the control plane is not running. It retries on its own.">
        <div style={FRAME} className="max-w-full overflow-hidden rounded-md border border-border [&_main]:h-full">
          {/* A specimen: it must not poll, and above all must not reload the
              gate every two seconds when a control plane answers. */}
          <FirstRun watch={false} />
        </div>
      </Sub>
    </Section>
  );
}
