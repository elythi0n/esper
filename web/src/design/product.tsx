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
  { id: "t3", name: "backup.tar", bytes: 410_000_000, total: 1_900_000_000, direction: "send", state: "failed", error: "The other side stopped answering. Try again when it is back." },
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
      lede="The pieces the screens are made of, in the states they will actually be in. Devices is the product: if connecting to a machine you already trust is not one click from a cold start, nothing else here matters."
    >
      <Sub title="DeviceRow" about="A row and not a card, and that was decided twice: the card carried a readout under every name, and five of them is five paragraphs. The row keeps what answers the question somebody opens this screen with — which machine, and can I go there now — and the rest moved to the panel beside the list.">
        <div className="flex flex-col">
          {devices.map((d) => (
            <DeviceRow
              key={d.fingerprint.hex}
              device={d}
              onSelect={() => toast.show({ title: `${d.name} — the panel opens beside the list` })}
              onConnect={() => toast.show({ title: `Connecting to ${d.name}` })}
            />
          ))}
        </div>
      </Sub>

      <div className="grid gap-8 lg:grid-cols-2">
        <Sub title="PairingCode" about="44px, grouped in threes, a countdown that updates in place. Nothing else on any view is this big.">
          <Demo className="flex-col items-start gap-6">
            <PairingCode code="482917" expiresAt={nowMs + 4 * 60_000 + 37_000} />
            <PairingCode code="103448" expiresAt={nowMs - 1000} />
          </Demo>
        </Sub>
        <Sub title="PairingPrompt" about="The host-side accept or deny, with what is being asked for spelled out in sentences. Doing nothing is a deny.">
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

      <Sub title="SessionHUD, StatTile, Sparkline" about="RTT, bitrate, fps, encoder, tier. The numbers update in place and declare no transition; a toned value carries a word beside it so the tone is never the only signal.">
        <div className="flex flex-col gap-3">
          <SessionHUD stats={{ rttMs: 18, bitrateKbps: 4200, fps: 60, encoder: "h264 sw", tier: "lan", rttHistory, fpsHistory }} />
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
        <Sub title="MonitorSwitcher" about="Each screen at its own aspect ratio, named with its size. Arrow keys move between them. Switching never animates the video.">
          <Demo className="items-start">
            <MonitorSwitcher monitors={monitors} value={monitor} onChange={setMonitor} allowAll />
            <p className="w-full text-xs text-text-3">Showing {monitor === ALL_SCREENS ? "all screens, tiled" : monitors.find((m) => m.id === monitor)?.label}.</p>
          </Demo>
        </Sub>
        <Sub title="PermissionGrid" about="What a device may do, with the sentence that says what each means. Seeing the screen cannot be turned off, and the switch says so.">
          <PermissionGrid value={perms} onChange={setPerms} />
        </Sub>
      </div>

      <Sub title="TransferProgress" about="Per file, cancellable, and the bar is a measurement with no transition. A failure says what to do.">
        <div className="grid gap-3 sm:grid-cols-2">
          {transfers.map((t) => <TransferProgress key={t.id} transfer={t} onCancel={(x) => toast.show({ title: `Stopped ${x.name}` })} />)}
        </div>
      </Sub>
    </Section>
  );
}
