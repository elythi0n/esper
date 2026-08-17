import { useState } from "react";
import { ActivityScreen } from "@/screens/ActivityScreen";
import { DevicesScreen } from "@/screens/DevicesScreen";
import { FirstRun } from "@/screens/FirstRun";
import { WelcomeScreen } from "@/screens/WelcomeScreen";
import { InviteScreen } from "@/screens/InviteScreen";
import { PairScreen } from "@/screens/PairScreen";
import { SessionsScreen } from "@/sessions/SessionsScreen";
import { SettingsScreen } from "@/screens/SettingsScreen";
import { StatusStrip } from "@/screens/StatusStrip";
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
            <StatusSpacer />
          </StatusBar>
        }
      >
        {children}
      </AppShell>
    </div>
  );
}

export function ScreensSection() {
  const nothing = () => undefined;
  return (
    <Section
      id="screens"
      title="The screens"
      lede="Every product screen, live against this machine's control plane. Not fixtures: these are the components the app routes to, so what is reviewed here is what ships — including the states where something is missing, which are the ones a made-up device list would have hidden."
    >
      <Sub
        title="Devices"
        about="The home, and the product. A name, the key drawn beside it, when it was last used, and one click. Presence says 'on this network' and never 'online': only the LAN rung announces itself, so a device with no dot may be perfectly reachable over an overlay or through a broker — a lamp meaning reachable would be read as a promise. Every row carries the same actions two ways: a menu button a keyboard can find, and a right-click."
      >
        <Framed active="devices">
          <DevicesScreen onConnect={nothing} onPair={nothing} onSetUp={nothing} />
        </Framed>
      </Sub>

      <Sub
        title="Set up this machine"
        about="The one sequence in this product, because these three are asked once and in an order: what a paired device connects to is this machine's key, so turning the host on before knowing which machine you have turned on is a switch flipped in the dark. Reachable rather than forced — 'shown once' is a piece of state that is wrong on the second machine and has to be reset by hand."
      >
        <Framed active="devices">
          <WelcomeScreen onPair={nothing} onDone={nothing} />
        </Framed>
      </Sub>

      <Sub title="Activity" about="The record, sortable, filtered, and windowed — ten thousand lines is an ordinary amount of history and thirty thousand DOM nodes is not.">
        <Framed active="activity">
          <ActivityScreen />
        </Framed>
      </Sub>

      <Sub title="Settings" about="What this machine can do is the engine's answer and is read-only — a measurement, not a preference. What this window looks like is the preference.">
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
        <Sub className="min-w-0" title="Pair" about="Both directions, no terminal. Showing a code starts a real listener in the engine and polls it to paired, lapsed or cancelled; the code is rendered exactly as the engine formats it, because it is the string somebody types at the other end. The fingerprint words beside it are not decoration — a code proves somebody typed what you read out, the words prove the machine is the one you meant.">
          <Framed active="pair">
            <PairScreen onDone={nothing} />
          </Framed>
        </Sub>
        <Sub className="min-w-0" title="Invite" about="A link needs a broker, and Esper operates none by default. The screen says so rather than offering a button that cannot work.">
          <Framed active="invite">
            <InviteScreen />
          </Framed>
        </Sub>
      </div>

      <Sub
        title="Sessions"
        about="Several machines at once. A tab each, dragged to reorder, dragged to an edge to put two side by side, dragged out to float — and a + that opens another. The tab strip is chrome you navigate with, so it stays; the HUD sits over the picture, so it retracts. The picture itself is missing and says so, rather than showing a black rectangle that reads as a session that failed."
      >
        <Framed active="sessions">
          <SessionsScreen />
        </Framed>
      </Sub>

      <Sub title="First run" about="Which, for this window, is the run where nothing answers. One command, and it retries on its own — somebody who starts the control plane in another terminal should not have to come back and press something.">
        <div style={FRAME} className="max-w-full overflow-hidden rounded-md border border-border [&_main]:h-full">
          {/* A specimen: it must not poll, and above all must not reload the
              gate every two seconds when a control plane answers. */}
          <FirstRun watch={false} />
        </div>
      </Sub>
    </Section>
  );
}
