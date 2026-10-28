import { AppWindow, Fingerprint as Fingerprint_, FolderOpen, History, Network, ShieldCheck, type LucideIcon } from "lucide-react";
import { useEffect, useState } from "react";
import { api } from "@/api/client";
import { useAsk, useStatus } from "@/api/hooks";
import { triedFor, type Capability, type HostState } from "@/api/types";
import { EngineSettings } from "@/screens/EngineSettings";
import { PermissionChecklist } from "@/screens/PermissionChecklist";
import { Skeleton } from "@/components/states";
import { CopyField } from "@/components/copy-field";
import { Identicon } from "@/components/identicon";
import { Lamp, lampFor } from "@/components/lamp";
import { Said, SettingCard, SettingRow } from "@/components/setting";
import { Switch } from "@/components/switch";
import { MotionToggle, ThemeToggle } from "@/components/theme-toggle";
import { readAutostart, setAutostart, type Autostart } from "@/sessions/shell";
import { PageBody, Topbar } from "@/shell/AppShell";
import { Figure, Fingerprint, Row, Trouble } from "./parts";

/**
 * Settings — every value with its effective source.
 *
 * Two halves, and they are different kinds of thing. What this machine *can
 * do* is the engine's answer and is read-only here: it is a measurement, not
 * a preference, and a screen that let somebody toggle "capture" would be
 * offering to change the world by editing the report of it. What this window
 * *looks like* is a preference, and lives in the browser.
 */

/** A row's name: the interface's face, quiet, in a column of its own. */
function Label({ children, className }: { children: React.ReactNode; className?: string | undefined }) {
  return <span className={`w-32 shrink-0 text-sm text-text-2 ${className ?? ""}`}>{children}</span>;
}

function Line({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <Row className="items-baseline py-3.5">
      <Label>{label}</Label>
      <span className="min-w-0 flex-1 text-sm text-text">{children}</span>
    </Row>
  );
}

/**
 * A fact with a state, and the dot hangs.
 *
 * A lamp in front of the value pushes that row's text across by the dot and
 * its gap, and a column where three rows in eleven start twelve pixels
 * further right is a column with no left edge — the eye reads the ragged edge
 * long before it reads any dot. The dot cannot simply move to a gutter of its
 * own, because a `Lamp` will not render without its word and that rule is
 * right: colour alone says nothing. So it hangs instead — the negative margin
 * is exactly the dot plus the gap, which puts the *word* on the column's edge
 * with every other value and leaves the dot in the margin beside it.
 */
function CapabilityRow({ label, cap }: { label: string; cap: Capability }) {
  const tried = triedFor(cap);
  return (
    <Line label={label}>
      <div className="min-w-0">
        <Lamp state={lampFor(cap.available)} className="-ml-3 min-w-0 items-baseline">
          <span className="min-w-0 font-mono text-xs text-text">{cap.detail || (cap.available ? "yes" : "no")}</span>
        </Lamp>
        {tried.length > 0 && (
          <ul data-tried={label} className="mt-1 space-y-0.5">
            {tried.map((attempt) => (
              <li key={attempt} className="break-words font-mono text-xs leading-relaxed text-text-3">
                {attempt}
              </li>
            ))}
          </ul>
        )}
      </div>
    </Line>
  );
}

/**
 * What this machine is, and the words for saying so out loud.
 *
 * On the Settings screen rather than in a first-run wizard, and that is the
 * decision: a wizard is a screen somebody sees once, and the fingerprint is
 * the thing they need on the day something looks wrong. The words are not
 * decoration — a code proves the same code was typed at both ends, and only
 * these prove the machine at the other end is the one that was meant.
 */
function ThisIdentity() {
  const { data, error, loading } = useAsk(() => api.identity(), []);

  if (loading) return <Skeleton className="mt-2 h-3 w-64" />;
  if (error) return <Trouble detail={error.message}>Could not read this machine's identity.</Trouble>;
  if (!data) return null;

  return (
    <>
      <Row className="items-center py-3.5">
        <Label>Key</Label>
        <span className="flex min-w-0 flex-1 flex-col gap-1">
          <span className="flex min-w-0 items-center gap-3">
            <Identicon of={data.key} size={28} />
            <Fingerprint hex={data.key} className="min-w-0" />
          </span>
          <span className="text-xs leading-relaxed text-text-3">
            Safe to share. Other devices use it to recognise this machine.
          </span>
        </span>
      </Row>
      <Row className="items-start py-3.5">
        <Label className="pt-0.5">Words</Label>
        <span className="flex min-w-0 flex-1 flex-col gap-1">
          <CopyField value={data.words.join(" ")} wrap />
          <span className="text-xs leading-relaxed text-text-3">
            Read these aloud when you pair. They prove you are pairing with the right machine.
          </span>
        </span>
      </Row>
      {data.file && (
        <Row className="items-baseline py-3.5">
          <Label>Key file</Label>
          {/* A path somebody is about to paste into a terminal. */}
          <span className="flex min-w-0 flex-1 flex-col gap-1">
            <CopyField value={data.file} />
            <span className="text-xs leading-relaxed text-text-3">
              Keep this file private. Anyone who has it can pretend to be this machine.
            </span>
          </span>
        </Row>
      )}
    </>
  );
}

/**
 * Whether this machine serves its screen.
 *
 * # Why a switch here rather than a command
 *
 * Turning the host on is most of what this product is *for*, and it has to
 * work by being pressed: the alternative is asking somebody to copy four
 * commands out of a comment block, one of which — `enable-linger` — is the
 * one everybody misses and whose absence looks exactly like Esper crashing
 * whenever they disconnect.
 *
 * **It is not a black box.** Every command the change ran is shown, so a
 * machine where it half-worked is a machine somebody can finish by hand, and
 * the next one they can do without this.
 *
 * Two lines of state, because "running now" and "starts at login" are
 * different facts and a host that has only the first will be gone after a
 * reboot.
 */
function Serving() {
  const [state, setState] = useState<HostState | null>(null);
  const [busy, setBusy] = useState(false);
  const [trouble, setTrouble] = useState<string | null>(null);
  const [said, setSaid] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    api
      .host()
      .then((now) => live && setState(now))
      .catch((cause: unknown) => live && setTrouble(cause instanceof Error ? cause.message : String(cause)));
    return () => {
      live = false;
    };
  }, []);

  if (trouble && !state) return <Trouble detail={trouble}>Could not check whether this machine shares its screen.</Trouble>;
  if (!state) return <Skeleton className="mt-2 h-3 w-48" />;

  if (!state.managed) {
    return (
      <SettingRow
        label="Allow connections"
        state={
          <>
            {state.why || "Esper can't turn screen sharing on by itself on this machine."} Run{" "}
            <code className="font-mono text-text-2">esper host</code> and set it to start when you log in.
          </>
        }
      />
    );
  }

  const change = (on: boolean) => {
    setBusy(true);
    setTrouble(null);
    api
      .setHost(on)
      .then((now) => {
        setState(now);
        setSaid(now.said ?? null);
      })
      .catch((cause: unknown) => setTrouble(cause instanceof Error ? cause.message : String(cause)))
      .finally(() => setBusy(false));
  };

  return (
    <SettingRow
      label="Allow connections"
      state={
        // Which service manager runs it is for whoever debugs it, so it is
        // on hover rather than in the sentence.
        <span title={`Run by ${state.manager}`}>
          {state.active ? "Other devices can connect right now." : "Other devices can't connect right now."}{" "}
          {state.enabled ? "It turns on when you log in." : "It stays off after you log in."}
        </span>
      }
      control={
        <Switch
          checked={state.enabled || state.active}
          disabled={busy}
          onCheckedChange={change}
          aria-label="Share this machine's screen with paired devices"
        />
      }
      trouble={trouble ? <Trouble detail={trouble}>Could not change this setting.</Trouble> : undefined}
      said={said ? <Said>{said}</Said> : undefined}
    />
  );
}

/**
 * Opening at login, which is a fact about the machine rather than a value in
 * this window's storage — so the switch reads the file every time and shows
 * where it is. A person who wants it gone can delete that file, and the
 * switch will agree with them.
 *
 * Not drawn where the platform has nowhere to put one: a switch that refuses
 * is worse than a sentence saying it is not built yet.
 */
function AtLogin() {
  const [state, setState] = useState<Autostart | null>(null);
  const [busy, setBusy] = useState(false);
  const [trouble, setTrouble] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    void readAutostart().then((now) => live && setState(now));
    return () => {
      live = false;
    };
  }, []);

  if (!state || !state.available) return null;

  const change = (on: boolean) => {
    setBusy(true);
    setTrouble(null);
    setAutostart(on)
      .then(setState)
      .catch((cause: unknown) => setTrouble(cause instanceof Error ? cause.message : String(cause)))
      .finally(() => setBusy(false));
  };

  return (
    <SettingRow
      label="At login"
      state={state.path ? <span className="block truncate font-mono">{state.path}</span> : undefined}
      control={
        <Switch checked={state.on} disabled={busy} onCheckedChange={change} aria-label="Open Esper when I log in" />
      }
      trouble={trouble ? <Trouble detail={trouble}>Could not change this setting.</Trouble> : undefined}
    />
  );
}

/*
  ── Sections ────────────────────────────────────────────────────────────
  Forty-odd things on one page was a page somebody scrolled through looking
  for the one they came for. So the screen is sections, each a short list of
  related things, listed down the left and reachable by link
  (`#/settings/privacy`) so a toast or a remedy can send somebody straight
  to the right place.

  The engine's settings are placed by their group, so a setting the engine
  adds later lands in its group's section without this list changing — and a
  group nobody placed falls through to Advanced rather than disappearing.
*/
interface Section {
  id: string;
  label: string;
  icon: LucideIcon;
  /** One line under the section's title: what is here. */
  about: string;
  /** The engine's settings groups this section shows. */
  groups: readonly string[];
}

const SECTIONS: readonly Section[] = [
  { id: "general", label: "General", icon: AppWindow, about: "How this window looks and when it opens.", groups: [] },
  {
    id: "machine",
    label: "This machine",
    icon: Fingerprint_,
    about: "Its identity, whether other devices can connect to it, and what it can share.",
    groups: ["device"],
  },
  {
    id: "connections",
    label: "Connections",
    icon: Network,
    about: "How other devices find and reach this one.",
    groups: ["host", "network", "discovery", "connectivity", "broker"],
  },
  {
    id: "privacy",
    label: "Privacy",
    icon: ShieldCheck,
    about: "What a connected device may do, and when you are asked first.",
    groups: ["policy", "consent", "session"],
  },
  {
    id: "files",
    label: "Files and forwarding",
    icon: FolderOpen,
    about: "Which files a paired device can send and receive, and which services here it can reach.",
    groups: ["files", "forward"],
  },
  {
    id: "activity",
    label: "Activity log",
    icon: History,
    about: "How long the record of what happened is kept.",
    groups: ["audit"],
  },
];

const PLACED = new Set(SECTIONS.flatMap((x) => x.groups));

function sectionFromHash(): string {
  const part = window.location.hash.replace(/^#\/?/, "").split("/")[1];
  return SECTIONS.some((x) => x.id === part) ? (part as string) : "general";
}

export function SettingsScreen() {
  const { data, error, loading } = useStatus();
  const [current, setCurrent] = useState(sectionFromHash);
  useEffect(() => {
    const on = () => setCurrent(sectionFromHash());
    window.addEventListener("hashchange", on);
    return () => window.removeEventListener("hashchange", on);
  }, []);
  const section = SECTIONS.find((x) => x.id === current) ?? SECTIONS[0]!;
  const pick = (id: string) => {
    window.location.hash = `/settings/${id}`;
  };

  return (
    <>
      <Topbar title="Settings" />
      <PageBody>
        <div className="grid grid-cols-[minmax(0,1fr)] gap-6 @[45rem]:grid-cols-[13rem_minmax(0,1fr)] @[45rem]:gap-10">
          <nav aria-label="Settings sections" className="settings-nav">
            {SECTIONS.map((x) => (
              <button
                key={x.id}
                type="button"
                className="settings-nav-item"
                // On a phone the list scrolls sideways; keep the current one in view.
                ref={x.id === section.id ? (el) => el?.scrollIntoView({ block: "nearest", inline: "nearest" }) : undefined}
                aria-current={x.id === section.id ? "page" : undefined}
                onClick={() => pick(x.id)}
              >
                <x.icon aria-hidden size={16} strokeWidth={1.6} />
                {x.label}
              </button>
            ))}
          </nav>

          <div className="min-w-0 max-w-3xl">
            <header className="flex flex-col gap-1 pb-6">
              <h2 className="font-display text-2xl font-medium text-text">{section.label}</h2>
              <p className="text-sm text-text-2">{section.about}</p>
            </header>

            {section.id === "general" && (
              <SettingCard title="Appearance and startup">
                <SettingRow label="Theme" control={<ThemeToggle />} />
                <SettingRow label="Motion" control={<MotionToggle />} />
                <AtLogin />
              </SettingCard>
            )}

            {section.id === "machine" && (
              <div className="flex min-w-0 flex-col gap-8">
                <SettingCard title="Identity">
                  <ThisIdentity />
                </SettingCard>
                <SettingCard title="Connections">
                  <Serving />
                </SettingCard>

                {loading && (
                  <div className="flex flex-col gap-3 py-2">
                    {[0, 1, 2, 3].map((i) => (
                      <Skeleton key={i} className="h-3 w-full max-w-sm" />
                    ))}
                  </div>
                )}

                {error && (
                  <Trouble detail={error.message}>
                    Could not reach Esper on this machine, so its features can't be checked.
                  </Trouble>
                )}

                {data && (
                  <>
                    <SettingCard title="What this machine can do">
                      <CapabilityRow label="Screen capture" cap={data.capture} />
                      <CapabilityRow label="Video encoding" cap={data.encoder} />
                      <CapabilityRow label="Remote input" cap={data.input} />
                      <Line label="Esper version">
                        <Figure value={data.engine} className="text-xs" />
                      </Line>
                    </SettingCard>
                    {/*
                      What to do about anything above that is missing, on this
                      platform, beside the capabilities: the fault and the
                      remedy belong together.
                    */}
                    <PermissionChecklist
                      platform={data.platform ?? { os: "", display: "" }}
                      have={{ capture: data.capture.available, input: data.input.available }}
                    />
                  </>
                )}

                <EngineSettings show={showFor(section)} />
              </div>
            )}

            {section.id === "connections" && (
              <div className="min-w-0">
                {data && (
                  <SettingCard title="How this machine can be reached">
                    <Line label="Private networks">
                      {data.overlays.length ? (
                        <Figure value={data.overlays.join(" · ")} className="text-xs" />
                      ) : (
                        <span className="text-text-3">None found</span>
                      )}
                    </Line>
                    <Line label="Local">
                      {data.mdns ? (
                        "Visible to devices on this network"
                      ) : (
                        <span className="text-text-3">Hidden from this network</span>
                      )}
                    </Line>
                    <Line label="Broker">
                      {data.broker ? (
                        <Figure value={data.broker} className="text-xs" />
                      ) : (
                        <span className="text-text-3">None set up. Only needed for invitations.</span>
                      )}
                    </Line>
                  </SettingCard>
                )}
                <div className="mt-8">
                  <EngineSettings show={showFor(section)} />
                </div>
              </div>
            )}

            {(section.id === "privacy" || section.id === "files" || section.id === "activity") && (
              <EngineSettings show={showFor(section)} />
            )}
          </div>
        </div>
      </PageBody>
    </>
  );
}

/**
 * Which engine settings a section shows. The last section also takes any
 * group no section names, so a setting the engine adds under a new group is
 * still somewhere rather than nowhere.
 */
const filters = new Map<string, (group: string) => boolean>(
  SECTIONS.map((x, i) => [
    x.id,
    i === SECTIONS.length - 1 ? (g: string) => x.groups.includes(g) || !PLACED.has(g) : (g: string) => x.groups.includes(g),
  ]),
);

function showFor(section: Section): (group: string) => boolean {
  return filters.get(section.id) ?? (() => false);
}
