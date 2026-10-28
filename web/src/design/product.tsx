import { useState } from "react";
import { DeviceRow } from "@/components/device-row";
import type { Device } from "@/api/types";
import { Sparkline, StatTile, SessionHUD } from "@/components/hud";
import { ALL_SCREENS, MonitorSwitcher } from "@/components/monitor-switcher";
import { PairingCode, PairingPrompt } from "@/components/pairing";
import { heldFrom, PermissionGrid } from "@/components/permission-grid";
import { useToast } from "@/components/toast";
import { TransferProgress, type Transfer } from "@/components/transfer-progress";
import { Demo, Section, Sub } from "./ui";

const now = Math.floor(Date.now() / 1000);
/** The pairing widgets count down against a millisecond clock, not the store's seconds. */
const nowMs = Date.now();

/** The wire shape, so the row on the gate is the row the screen renders. */
function device(
  name: string,
  seen: number | null,
  perms: Device["permissions"],
  pairing: Device["pairing"] = "kept",
): Device {
  const hex = name.replace(/\W/g, "").padEnd(16, "0").slice(0, 16);
  return {
    public_key: hex.repeat(4),
    name,
    permissions: perms,
    paired_at: now - 90 * 86400,
    last_seen: seen,
    pairing,
    fingerprint: { hex, words: ["plum", "wander", "amuse", "mimic", "fleet", "inner", "atom", "viking"] },
    admits_a_session: pairing === "kept" || (!pairing.once.used && now < pairing.once.expires_at),
  };
}

const devices: Device[] = [
  device("workstation", now - 42, ["view", "control", "clipboard_out", "files_out", "audio"]),
  device("build-box", now - 5 * 3600, ["view", "control", "terminal", "forward"]),
  device("mum's laptop", now - 90, ["view", "control", "audio"]),
  device("office-mini", null, ["view"], { once: { used: false, expires_at: now + 600 } }),
  device("old-nuc", now - 40 * 86400, ["view", "control"], { once: { used: true, expires_at: now - 86400 } }),
];

const monitors = [
  { id: "m1", label: "Left", width: 2560, height: 1440, primary: true },
  { id: "m2", label: "Right", width: 1920, height: 1080 },
  { id: "m3", label: "Portrait", width: 1080, height: 1920 },
];

const transfers: Transfer[] = [
  { id: "t1", name: "release-notes-2026-09.pdf", bytes: 3_400_000, total: 8_200_000, rate: 1_200_000, direction: "send", state: "running" },
  { id: "t2", name: "IMG_4821.HEIC", bytes: 2_100_000, total: 2_100_000, direction: "receive", state: "done" },
  { id: "t3", name: "backup.tar", bytes: 410_000_000, total: 1_900_000_000, direction: "send", state: "failed", error: "The other device stopped responding. Try again when it is back." },
  { id: "t4", name: "notes.md", bytes: 12_000, total: 40_000, direction: "receive", state: "cancelled" },
];

const rttHistory = [18, 19, 18, 22, 31, 24, 19, 18, 18, 20, 17, 18];
const fpsHistory = [60, 60, 59, 60, 58, 60, 60, 60, 57, 60, 60, 60];

export function ProductSection() {
  const toast = useToast();
  const [monitor, setMonitor] = useState("m1");
  const [perms, setPerms] = useState(heldFrom(["view", "control", "clipboard_out"]));
  const [prompt, setPrompt] = useState(true);

  return (
    <Section
      id="product"
      title="Product components"
      lede="The pieces the screens are built from, in the states they will really be in."
    >
      <Sub title="DeviceRow" about="One line per device: its name, when it was last seen, and Connect.">
        <div className="flex flex-col">
          {devices.map((d) => (
            <DeviceRow
              key={d.fingerprint.hex}
              device={d}
              onSelect={() => toast.show({ title: `Opening ${d.name}'s details beside the list` })}
              onConnect={() => toast.show({ title: `Connecting to ${d.name}` })}
            />
          ))}
        </div>
      </Sub>

      <div className="grid gap-8 lg:grid-cols-2">
        <Sub title="PairingCode" about="The pairing code, big enough to read across a room, with a live countdown.">
          <Demo className="flex-col items-start gap-6">
            <PairingCode code="482917" expiresAt={nowMs + 4 * 60_000 + 37_000} />
            <PairingCode code="103448" expiresAt={nowMs - 1000} />
          </Demo>
        </Sub>
        <Sub title="PairingPrompt" about="Asking the host to allow a session, in plain sentences. Doing nothing means no.">
          <Demo className="items-start">
            {prompt ? (
              <PairingPrompt
                requester={{ name: "the office laptop", fingerprint: "c07e55d812aa9f30" }}
                requested={["view", "control", "clipboard_out"]}
                expiresAt={nowMs + 58_000}
                onAllow={() => { setPrompt(false); toast.show({ title: "Allowed. The session starts now.", tone: "ok" }); }}
                onDeny={() => { setPrompt(false); toast.show({ title: "Denied. Nothing connected." }); }}
              />
            ) : (
              <button type="button" className="text-sm text-text-2 underline underline-offset-2" onClick={() => setPrompt(true)}>Show the prompt again</button>
            )}
          </Demo>
        </Sub>
      </div>

      <Sub title="SessionHUD, StatTile, Sparkline" about="Live session numbers that update in place, each with a word so colour is never the only signal.">
        <div className="flex flex-col gap-3">
          <SessionHUD stats={{ rttMs: 18, bitrateKbps: 4200, fps: 60, hopMs: 2, encoder: "h264 sw", tier: "lan", rttHistory, fpsHistory }} />
          <SessionHUD stats={{ rttMs: 212, bitrateKbps: 900, fps: 24, encoder: "h264 sw", tier: "relayed", degraded: true, rttHistory: rttHistory.map((x) => x * 11), fpsHistory: fpsHistory.map((x) => x * 0.4) }} />
          <Demo className="gap-8">
            <StatTile label="rtt" value={18} unit="ms" />
            <StatTile label="rtt" value={212} unit="ms" tone="warn" note="high" />
            <StatTile label="rtt" value={640} unit="ms" tone="danger" note="very high" />
            <Sparkline values={rttHistory} label="Round-trip time" />
          </Demo>
        </div>
      </Sub>

      <div className="grid gap-8 lg:grid-cols-2">
        <Sub title="MonitorSwitcher" about="Pick a monitor. Arrow keys move between them.">
          <Demo className="items-start">
            <MonitorSwitcher monitors={monitors} value={monitor} onChange={setMonitor} allowAll />
            <p className="w-full text-xs text-text-3">Showing {monitor === ALL_SCREENS ? "all screens, tiled" : monitors.find((m) => m.id === monitor)?.label}.</p>
          </Demo>
        </Sub>
        <Sub title="PermissionGrid" about="What a device may do, each with a sentence that explains it.">
          <PermissionGrid value={perms} onChange={setPerms} />
        </Sub>
      </div>

      <Sub title="TransferProgress" about="One file's progress, with a cancel button and a clear error.">
        <div className="grid gap-3 sm:grid-cols-2">
          {transfers.map((t) => <TransferProgress key={t.id} transfer={t} onCancel={(x) => toast.show({ title: `Stopped ${x.name}` })} />)}
        </div>
      </Sub>
    </Section>
  );
}
