import { Plus } from "lucide-react";
import { useState } from "react";
import { Button } from "@/components/button";
import { DeviceRow } from "@/components/device-row";
import type { Device } from "@/api/types";
import { AppShell, PageBody, StatusBar, StatusSpacer, Topbar, Wordmark } from "@/shell/AppShell";
import { TitleBar, type Menu } from "@/shell/TitleBar";
import { SCREENS } from "@/shell/screens";
import { Demo, Section, Sub, TokenName, TokenTable, useTokens } from "./ui";

const shellTokens: ReadonlyArray<{ token: string; note?: string }> = [
  { token: "--sidebar-w", note: "The longest destination at 13px, beside its icon" },
  { token: "--sidebar-rail-w", note: "Collapsed. Each icon gets a 40px square with room either side" },
  { token: "--topbar-h", note: "The topbar, and the brand block, so their hairlines meet" },
  { token: "--footbar-h", note: "The status strip and the sidebar's foot" },
  { token: "--page-gutter", note: "The one number the topbar, the body and the strip share" },
];

const shellTokenNames = shellTokens.map((r) => r.token);

/** The real menus, minus the actions — the gate reviews the shape, not the wiring. */
const DEMO_MENUS: readonly Menu[] = [
  {
    label: "Session",
    items: [
      { label: "New session…", keys: "⌘K" },
      { label: "All sessions" },
      "separator",
      { label: "Pair a device…" },
    ],
  },
  {
    label: "View",
    items: [
      { label: "Full screen", keys: "F11" },
      "separator",
      { label: "Dark" },
      { label: "Light" },
      { label: "High contrast" },
      "separator",
      { label: "Settings" },
    ],
  },
  {
    label: "Help",
    items: [{ label: "Design system" }, { label: "Activity" }, "separator", { label: "Esper v0.0.0", disabled: true }],
  },
];

const now = Math.floor(Date.now() / 1000);

const devices: Device[] = [
  {
    public_key: "3a9fc1e277b0".repeat(6).slice(0, 64),
    name: "workstation",
    permissions: ["view", "control", "clipboard_out"],
    paired_at: now - 90 * 86400,
    last_seen: now - 31,
    pairing: "kept",
    last_address: "192.168.1.24:8686",
    fingerprint: { hex: "3a9fc1e277b0aa01", words: ["plum", "wander", "amuse", "mimic", "fleet", "inner", "atom", "viking"] },
    admits_a_session: true,
  },
  {
    public_key: "8d210f4abe93".repeat(6).slice(0, 64),
    name: "build-box",
    permissions: ["view", "control"],
    paired_at: now - 40 * 86400,
    last_seen: now - 5 * 3600,
    pairing: "kept",
    fingerprint: { hex: "8d210f4abe93bb02", words: ["ozone", "hammer", "rustic", "anvil", "nebula", "rifle", "drummer", "armor"] },
    admits_a_session: true,
  },
];

/** The Devices screen, roughed in, so the shell is shown holding something. */
function ShellContents({ onSearch }: { onSearch?: (() => void) | undefined }) {
  return (
    <>
      <Topbar
        title="Devices"
        subtitle="Devices paired with this machine. Click one to connect."
        onSearch={onSearch}
        actions={
          <Button variant="primary" size="sm">
            <Plus aria-hidden size={14} /> Pair a device
          </Button>
        }
      />
      <PageBody>
        <div className="flex flex-col">
          {devices.map((d) => (
            <DeviceRow key={d.fingerprint.hex} device={d} />
          ))}
        </div>
      </PageBody>
    </>
  );
}

function ShellStatus() {
  return (
    <StatusBar>
      <span>2 devices</span>
      <span>1 reachable</span>
      <StatusSpacer />
      <span>Host on</span>
    </StatusBar>
  );
}

export function ShellSection() {
  const v = useTokens(shellTokenNames);
  const [wide, setWide] = useState("devices");
  const [narrow, setNarrow] = useState("devices");
  const [phone, setPhone] = useState("devices");

  return (
    <Section
      id="shell"
      title="The shell"
      lede="The frame around every screen: sidebar, top bar, page and status strip."
    >
      <Sub
        title="Wide"
        about="The sidebar collapses to icons and remembers. The current screen is lit."
      >
        <div className="h-[30rem] overflow-hidden rounded-md border border-border">
          <AppShell groups={SCREENS} active={wide} onSelect={setWide} version="0.1.0" status={<ShellStatus />}>
            <ShellContents onSearch={() => undefined} />
          </AppShell>
        </div>
      </Sub>

      <Sub
        title="Narrow, and narrowest"
        about="One shell at three widths. It responds to the window, not the monitor."
      >
        <div className="flex flex-wrap items-start gap-4">
          {/* Widths in pixels on purpose: these boxes are the measurement. */}
          <div style={{ width: 704 }} className="h-[30rem] max-w-full overflow-hidden rounded-md border border-border">
            <AppShell groups={SCREENS} active={narrow} onSelect={setNarrow} version="0.1.0">
              <ShellContents />
            </AppShell>
          </div>
          <div style={{ width: 320 }} className="h-[30rem] max-w-full overflow-hidden rounded-md border border-border">
            <AppShell groups={SCREENS} active={phone} onSelect={setPhone}>
              <ShellContents />
            </AppShell>
          </div>
        </div>
        <p className="max-w-[var(--measure)] text-xs text-text-3">
          At <TokenName>--breakpoint-lg</TokenName> the search trigger loses its words. Below{" "}
          <TokenName>--breakpoint-sm</TokenName> the sidebar splits in two: the brand becomes a strip across the top,
          and the main destinations become a tab bar across the bottom. The rest stay in the command palette. Each
          tab clears{" "}
          <TokenName>--tap-min</TokenName>, and both bars respect the safe-area insets.
        </p>
      </Sub>

      <Sub
        title="The window's own bar"
        about="The window draws its own title bar, with menus that only hold what earns a place."
      >
        <div className="overflow-hidden rounded-md border border-border">
          <TitleBar
            menus={DEMO_MENUS}
            onMinimise={() => undefined}
            onMaximise={() => undefined}
            onClose={() => undefined}
          />
          <div className="h-24 bg-surface-0" />
        </div>
      </Sub>

      <Sub title="Sizing" about="The five sizes the shell is built from.">
        <TokenTable rows={shellTokens} values={v} />
        <Demo className="gap-6">
          <span className="text-sm text-text-2">
            The wordmark is uppercase, one weight, with wide tracking and no effects. It uses tokens like
            everything else:
          </span>
          <Wordmark className="h-3" />
        </Demo>
      </Sub>
    </Section>
  );
}
