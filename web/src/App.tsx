import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { Contrast, Keyboard, Maximize, MonitorSmartphone, Moon, Sun, UserPlus } from "lucide-react";
import { api, ApiError } from "@/api/client";
import { useEngine } from "@/api/engine";
import { useDevices } from "@/api/hooks";
import type { Device } from "@/api/types";
import { CommandPalette, type Command } from "@/components/command-palette";
import { ShortcutSheet, held, modifier, useShortcuts, type Shortcut } from "@/components/shortcuts";
import { Toaster } from "@/components/toast";
import { TooltipProvider } from "@/components/tooltip";
import { DesignPage } from "@/design/DesignPage";
import { Alarm } from "@/components/alarm";
import { Button } from "@/components/button";
import { AppShell, StatusBar, StatusSpacer } from "@/shell/AppShell";
import { TitleBar, type Menu } from "@/shell/TitleBar";
import { host, windowAction } from "@/sessions/shell";
import { painted } from "@/lib/painted";
import { applyTheme, watchTheDesktop, type Theme } from "@/lib/theme";
import { SCREENS } from "@/shell/screens";
import { ActivityScreen } from "@/screens/ActivityScreen";
import { DevicesScreen } from "@/screens/DevicesScreen";
import { InviteScreen } from "@/screens/InviteScreen";
import { PairScreen } from "@/screens/PairScreen";
import { SessionsScreen } from "@/sessions/SessionsScreen";
import { MachineCard } from "@/shell/MachineCard";
import { SettingsScreen } from "@/screens/SettingsScreen";
import { WelcomeScreen } from "@/screens/WelcomeScreen";
import { FirstRun } from "@/screens/FirstRun";
import { StatusStrip } from "@/screens/StatusStrip";

/**
 * The router.
 *
 * Hash-based, because the bundle is loaded from a file in a webview with no
 * server-side routing to lean on, and because a hash survives a reload
 * without anybody configuring anything.
 */
function useHashRoute(): [string, (to: string) => void] {
  const read = () =>
    window.location.hash.replace(/^#/, "") ||
    (import.meta.env.VITE_START ?? "/devices");
  const [route, setRoute] = useState(read);
  useEffect(() => {
    const on = () => setRoute(read());
    window.addEventListener("hashchange", on);
    return () => window.removeEventListener("hashchange", on);
  }, []);
  const go = useCallback((to: string) => {
    window.location.hash = to;
  }, []);
  return [route, go];
}

/** Which sidebar destination a route belongs to. */
function screenOf(route: string): string {
  const first = route.split("/").filter(Boolean)[0] ?? "devices";
  return first;
}

/** What is behind this window: nothing, something that refused us, or the API. */
type Plane = { ok: true } | { ok: false; refused: boolean };

/**
 * Whether anything is behind this window.
 *
 * Asked once, before a screen is drawn, because every screen would otherwise
 * draw its own version of the same failure — five copies of "the control
 * plane did not answer" is worse than one, and none of them can offer the
 * command that fixes it.
 *
 * A **401 is kept apart** from everything else. The host attaches the token,
 * so the page can only be refused by something that is not the API that
 * token belongs to — another Esper, from another home, holding the address.
 * Folded in with the rest it reads as an absence, and the screen then offers
 * a remedy that cannot work.
 */
function usePlane(): Plane | null {
  const [plane, setPlane] = useState<Plane | null>(null);
  useEffect(() => {
    let live = true;
    api
      .health()
      .then(() => live && setPlane({ ok: true }))
      .catch((trouble: unknown) => {
        if (!live) return;
        const said = trouble instanceof ApiError ? trouble : null;
        setPlane({ ok: false, refused: said?.status === 401 });
      });
    return () => {
      live = false;
    };
  }, []);
  return plane;
}

export function App() {
  const [route, go] = useHashRoute();
  const plane = usePlane();
  const engine = useEngine();
  // Known before the first paint: the host writes it onto the document.
  const ownChrome = host.chrome;
  const screen = screenOf(route);
  const [palette, setPalette] = useState(false);
  const [keys, setKeys] = useState(false);
  const { data: devices } = useDevices();

  const connect = useCallback(
    (d: Device) => go(`/sessions/${d.fingerprint.hex}`),
    [go],
  );

  /*
    The destinations, the machines, and the things somebody does — all in one
    list, because the palette is the answer to "I know what I want and not
    where it is", and a palette that only held the sidebar would answer a
    question the sidebar has already answered.

    Devices come first when there are any: the product's whole promise is one
    click to a machine, and typing its name is the fastest click there is.
  */
  const commands: readonly Command[] = useMemo(() => {
    const machines: Command[] = (devices ?? []).map((d) => ({
      id: `device-${d.fingerprint.hex}`,
      label: d.name,
      hint: "Connect",
      keywords: [d.fingerprint.hex, "connect", "session"],
      icon: MonitorSmartphone,
      run: () => connect(d),
    }));
    const doing: Command[] = [
      { id: "do-pair", label: "Pair a device", hint: "Show a code, or use one", icon: UserPlus, run: () => go("/pair") },
      { id: "do-fullscreen", label: "Full screen", hint: "F11", icon: Maximize, run: () => void windowAction("fullscreen") },
      { id: "do-keys", label: "Keyboard shortcuts", hint: "?", icon: Keyboard, run: () => setKeys(true) },
      { id: "do-dark", label: "Dark", hint: "Theme", icon: Moon, run: () => applyTheme("dark") },
      { id: "do-light", label: "Light", hint: "Theme", icon: Sun, run: () => applyTheme("light") },
      { id: "do-hc", label: "High contrast", hint: "Theme", icon: Contrast, run: () => applyTheme("hc") },
    ];
    const places: Command[] = SCREENS.flatMap((g) =>
      g.items.map((i) => ({
        id: i.id,
        label: i.label,
        hint: g.label ? g.label.toLowerCase() : "go to",
        icon: i.icon,
        run: () => go(`/${i.id}`),
      })),
    );
    return [...machines, ...places, ...doing];
  }, [devices, connect, go]);

  /*
    Every binding this window has, in one array that both the handler and the
    sheet read. A sheet written separately teaches keys that do nothing the
    first time somebody adds one.
  */
  const shortcuts: readonly Shortcut[] = useMemo(() => {
    const destinations = SCREENS.flatMap((g) => g.items);
    return [
      {
        keys: `${modifier} K`,
        what: "Search commands and devices",
        group: "Getting around",
        when: (e) => held(e) && e.key.toLowerCase() === "k",
        run: () => setPalette((o) => !o),
      },
      {
        keys: "?",
        what: "Show keyboard shortcuts",
        group: "Getting around",
        when: (e) => e.key === "?" && !held(e),
        run: () => setKeys((o) => !o),
      },
      ...destinations.slice(0, 9).map((entry, i) => ({
        keys: `${modifier} ${i + 1}`,
        what: entry.label,
        group: "Getting around" as const,
        when: (e: KeyboardEvent) => held(e) && e.key === String(i + 1),
        run: () => go(`/${entry.id}`),
      })),
      {
        keys: "F11",
        what: "Full screen",
        group: "This window",
        when: (e) => e.key === "F11",
        run: () => void windowAction("fullscreen"),
      },
      {
        keys: `${modifier} ,`,
        what: "Settings",
        group: "This window",
        when: (e) => held(e) && e.key === ",",
        run: () => go("/settings"),
      },
      // Owned by the dock, and on the sheet because knowing it exists is the
      // point of a sheet. Bound here it would fire twice.
      { keys: "Drag a tab", what: "Split, reorder, or pull a session out", group: "Sessions", when: null },
    ];
  }, [go]);

  useShortcuts(shortcuts);

  // While `system` is chosen, the desktop keeps deciding — somebody who turns
  // high contrast on should not have to relaunch, which is exactly the moment
  // they most want it to have worked.
  useEffect(() => watchTheDesktop(), []);

  /*
    The first screen to render says so, once — everything after it is a
    navigation, which is a different number from the one that was promised.

    **Above the early returns, and that is not a style choice.** It was below
    them, so the render that showed the blank pre-flight ran twenty-seven
    hooks and the render that showed a screen ran twenty-eight: React threw
    "rendered more hooks than during the previous render" and the window
    opened empty. A conditional return makes every hook after it conditional.
  */
  useEffect(() => {
    if (plane?.ok) painted(screen);
  }, [plane, screen]);

  /*
    The menus.

    Actions, not destinations — the sidebar already holds the destinations,
    and a menu that repeated them would be two answers to "where is
    Activity". What is here is what the rail cannot hold: the window's own
    commands, and the things you do *to* a session.
  */
  const setTheme = (t: Theme) => applyTheme(t);
  const menus: readonly Menu[] = [
    {
      label: "Session",
      items: [
        { label: "New session…", keys: `${modifier} K`, onSelect: () => setPalette(true) },
        { label: "All sessions", onSelect: () => go("/sessions") },
        "separator",
        { label: "Pair a device…", onSelect: () => go("/pair") },
      ],
    },
    {
      label: "View",
      items: [
        {
          label: "Full screen",
          keys: "F11",
          onSelect: () => void windowAction("fullscreen"),
        },
        "separator",
        { label: "Dark", onSelect: () => setTheme("dark") },
        { label: "Light", onSelect: () => setTheme("light") },
        { label: "High contrast", onSelect: () => setTheme("hc") },
        "separator",
        { label: "Settings", keys: `${modifier} ,`, onSelect: () => go("/settings") },
      ],
    },
    {
      label: "Help",
      items: [
        { label: "Keyboard shortcuts", keys: "?", onSelect: () => setKeys(true) },
        "separator",
        { label: "Design system", onSelect: () => go("/design") },
        { label: "Activity", onSelect: () => go("/activity") },
        "separator",
        { label: `Esper ${__ESPER_VERSION__}`, disabled: true },
      ],
    },
  ];

  const chrome = ownChrome ? (
    <TitleBar
      menus={menus}
      onMinimise={() => void windowAction("minimise")}
      onMaximise={() => void windowAction("maximise")}
      onClose={() => void windowAction("close")}
    />
  ) : null;

  /** Everything sits under the bar, which is why this is not in AppShell. */
  const framed = (body: ReactNode) => (
    <div className="flex h-dvh flex-col overflow-hidden bg-surface-0">
      {chrome}
      <div className="min-h-0 flex-1">{body}</div>
    </div>
  );

  // The gate is its own page: it is the review surface, not a product screen,
  // and it brings its own scrolling and its own width. It is also the one
  // route that is worth opening with nothing behind the window.
  if (screen === "design") {
    return ownChrome ? (
      framed(
        <div className="h-full overflow-y-auto">
          <DesignPage />
        </div>,
      )
    ) : (
      <DesignPage />
    );
  }

  // Nothing answered, or something did and refused us. One screen either
  // way, with the remedy that matches, and it retries on its own.
  if (plane && !plane.ok) return framed(<FirstRun refused={plane.refused} at={host.api} />);
  if (plane === null) return framed(<div className="h-full bg-surface-0" />);

  const body = (() => {
    switch (screen) {
      case "sessions":
        return <SessionsScreen initial={route.split("/")[2]} />;
      case "pair": {
        // `/pair/invited/<token>` is where a clicked `esper://` link lands.
        const parts = route.split("/").filter(Boolean);
        const invitation = parts[1] === "invited" ? parts[2] : undefined;
        return <PairScreen onDone={() => go("/devices")} invitation={invitation} />;
      }
      case "invite":
        return <InviteScreen />;
      case "activity":
        return <ActivityScreen />;
      case "settings":
        return <SettingsScreen />;
      case "welcome":
        return <WelcomeScreen onPair={() => go("/pair")} onDone={() => go("/devices")} />;
      default:
        return (
          <DevicesScreen
            onConnect={connect}
            onPair={() => go("/pair")}
            onSetUp={() => go("/welcome")}
          />
        );
    }
  })();

  return (
    <TooltipProvider>
      <Toaster>
        <div
          className={
            ownChrome ? "flex h-dvh flex-col overflow-hidden bg-surface-0" : "contents"
          }
        >
          {chrome}
          <AppShell
            className={ownChrome ? "min-h-0 flex-1" : "h-dvh"}
            /*
              A machine with no engine is true on every screen at once, so it
              is said once, here, rather than by each of them. What it offers
              is the only thing that fixes it — and only when this control
              plane is the one that could.
            */
            alarm={
              <Alarm
                show={engine.health?.running === false}
                detail={engine.refused ?? engine.health?.why}
                action={
                  engine.health?.restartable ? (
                    <Button size="sm" onClick={engine.restart} disabled={engine.restarting}>
                      {engine.restarting ? "Starting…" : "Restart Esper"}
                    </Button>
                  ) : null
                }
              >
                Esper's background service on this machine has stopped.
              </Alarm>
            }
            groups={SCREENS}
            active={screen}
            onSelect={(id) => go(`/${id}`)}
            onSearch={() => setPalette(true)}
            sidebarFoot={<MachineCard onOpen={() => go("/settings")} />}
            version={__ESPER_VERSION__}
            status={
              <StatusBar>
                <StatusStrip />
                <StatusSpacer />
              </StatusBar>
            }
          >
            {body}
          </AppShell>
        </div>
        <CommandPalette open={palette} onOpenChange={setPalette} commands={commands} />
        <ShortcutSheet open={keys} onOpenChange={setKeys} shortcuts={shortcuts} />
      </Toaster>
    </TooltipProvider>
  );
}
