import { Activity, ArrowRight, Inbox, Infinity, Monitor, Plus, Search, Settings, SlidersHorizontal, Trash2 } from "lucide-react";
import { useState } from "react";
import { Badge, TierBadge, type Tier, type Tone } from "@/components/badge";
import { Button, type ButtonSize, type ButtonVariant } from "@/components/button";
import { Figure, Key, Row } from "@/screens/parts";
import { Alarm } from "@/components/alarm";
import { CopyField } from "@/components/copy-field";
import { Identicon } from "@/components/identicon";
import { Kbd } from "@/components/kbd";
import { LogView } from "@/components/log-view";
import { RowMenu, WithContextMenu, type MenuAction } from "@/components/menu";
import { Lamp } from "@/components/lamp";
import { Popover } from "@/components/popover";
import { SortableTable } from "@/components/sortable-table";
import { Stepper, type Step } from "@/components/stepper";
import { TransferQueue } from "@/components/transfer-queue";
import type { Transfer } from "@/components/transfer-progress";
import { ShortcutSheet, modifier, type Shortcut } from "@/components/shortcuts";
import { Callout } from "@/components/callout";
import { PermissionChecklist } from "@/screens/PermissionChecklist";
import { Card, CardBody, CardHeader, CardTitle } from "@/components/card";
import { CommandPalette, useCommandKey } from "@/components/command-palette";
import { Dialog, DialogContent, DialogFooter, DialogTrigger } from "@/components/dialog";
import { DurationInput, NumberInput, ScheduleInput, type Schedule } from "@/components/form-controls";
import { Field, Input, Textarea } from "@/components/input";
import { Select, SelectContent, SelectGroup, SelectItem, SelectLabel, SelectTrigger, SelectValue } from "@/components/select";
import { EmptyState, Skeleton } from "@/components/states";
import { Checkbox, Switch } from "@/components/switch";
import { Table, TBody, TD, TH, THead, TR } from "@/components/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/tabs";
import { Segmented } from "@/components/theme-toggle";
import { TimeAgo } from "@/components/time-ago";
import { adviceFor, useToast } from "@/components/toast";
import { Tooltip } from "@/components/tooltip";
import { Demo, Section, Sub } from "./ui";

const variants: ButtonVariant[] = ["primary", "secondary", "ghost", "danger"];
const sizes: ButtonSize[] = ["sm", "md", "lg"];
const tones: Tone[] = ["neutral", "ok", "warn", "danger", "info", "signal"];
const tiers: Tier[] = ["lan", "private", "direct", "relayed"];
const reasonCodes = [100, 201, 304, 404, 406, 503] as const;

const sessions = [
  { who: "workstation", when: Date.now() - 4 * 60_000, what: "Screen, mouse and keyboard", moved: "2 files, 14 MB" },
  { who: "build-box", when: Date.now() - 3 * 3600_000, what: "Screen", moved: "—" },
  { who: "laptop", when: Date.now() - 26 * 3600_000, what: "Screen, clipboard", moved: "1 file, 320 KB" },
];

/** One row's actions, for both triggers to show. */
const SAMPLE_ACTIONS: readonly MenuAction[] = [
  { label: "Connect", icon: ArrowRight, onSelect: () => {} },
  { label: "Permissions…", icon: SlidersHorizontal, onSelect: () => {} },
  { label: "Make the pairing last", icon: Infinity, onSelect: () => {} },
  { label: "Forget this device", icon: Trash2, onSelect: () => {}, danger: true, apart: true },
];

/** A few rows, for the sort to move. */
const SAMPLE_ROWS = [
  { name: "workstation", seen: 2, perms: "view, control" },
  { name: "elythion", seen: 47, perms: "view" },
  { name: "rack-01", seen: 9, perms: "terminal, forward" },
  { name: "laptop", seen: 310, perms: "view, control, files_out" },
];

/** A queue mid-flight, with one of each state — including the one that failed. */
const SAMPLE_TRANSFERS: readonly Transfer[] = [
  { id: "a", name: "capture-2026-09-10.mkv", bytes: 41_000_000, total: 92_000_000, rate: 5_400_000, direction: "receive", state: "running" },
  { id: "b", name: "notes.md", bytes: 2_100, total: 2_100, direction: "send", state: "done" },
  { id: "c", name: "build/esper-engine", bytes: 3_900_000, total: 14_700_000, direction: "send", state: "failed", error: "The far side refused: files_in is not granted." },
  { id: "d", name: "screenshot.png", bytes: 810_000, total: 810_000, direction: "receive", state: "done" },
];

/** Ten thousand of them, so the windowing is doing something. */
const SAMPLE_LOG = Array.from({ length: 10000 }, (_, i) => i);

/** The real first-run sequence's shape, with its content stood in for. */
const SAMPLE_STEPS: readonly Step[] = [
  { id: "who", label: "This machine", about: "Its key is what other devices trust.", content: <Identicon of="019a11258f68cdd0" size={44} /> },
  { id: "reachable", label: "Reachable", about: "Whether paired devices can connect to this screen.", content: <p className="text-sm text-text-2">A switch, and off is a good answer.</p> },
  { id: "pair", label: "Pair", about: "The one exchange that has to happen at both ends.", content: <p className="text-sm text-text-2">A code, read from one screen and typed on the other.</p> },
];

/** A few of the window's real bindings, for the gate to render. */
const SAMPLE_KEYS: readonly Shortcut[] = [
  { keys: `${modifier} K`, what: "Commands, devices and everything else", group: "Getting around", when: null },
  { keys: "?", what: "This sheet", group: "Getting around", when: null },
  { keys: `${modifier} 1`, what: "Devices", group: "Getting around", when: null },
  { keys: "F11", what: "Full screen", group: "This window", when: null },
  { keys: `${modifier} ,`, what: "Settings", group: "This window", when: null },
  { keys: "Drag a tab", what: "Split, reorder, or pull a session out", group: "Sessions", when: null },
];

export function ComponentsSection() {
  const toast = useToast();
  const [palette, setPalette] = useState(false);
  const [keys, setKeys] = useState(false);
  const [step, setStep] = useState("who");
  useCommandKey(() => setPalette((o) => !o));
  const [density, setDensity] = useState<"dense" | "comfortable">("dense");
  const [n, setN] = useState<number | "">(60);
  const [dur, setDur] = useState(3600);
  const [schedule, setSchedule] = useState<Schedule>({ days: [0, 1, 2, 3, 4], from: "09:00", to: "17:00" });

  return (
    <Section
      id="components"
      title="Components"
      lede="Every borrowed component — Radix underneath, shadcn's shapes — has its colours and durations re-pointed at the tokens before it is used here. A hardcoded colour fails the lint, not review. Buttons name the action; errors say what to do."
    >
      <Sub title="Button" about="primary is the inverse fill. danger is the only tinted button, because destroying something deserves a warning and not a highlight. Feedback is --dur-fast on colour only.">
        <Demo className="flex-col items-stretch gap-4">
          {sizes.map((s) => (
            <div key={s} className="flex flex-wrap items-center gap-2">
              <span className="w-8 font-mono text-xs text-text-3">{s}</span>
              {variants.map((v) => (
                <Button key={v} variant={v} size={s}>
                  {v === "danger" ? "Forget device" : v === "primary" ? "Connect" : v === "ghost" ? "Cancel" : "Details"}
                </Button>
              ))}
              <Button size={s} disabled>Disabled</Button>
              <Button size={s} variant="primary"><Plus aria-hidden size={14} /> With icon</Button>
            </div>
          ))}
        </Demo>
      </Sub>

      <Sub title="Badge and TierBadge" about="Tier is never colour alone: icon and word, always. LAN, private and direct are equally fine and carry the same weight; relayed takes the signal; degraded is text-level warn beside it, so one signal per view still holds.">
        <Demo>
          {tones.map((t) => <Badge key={t} tone={t}>{t}</Badge>)}
        </Demo>
        <Demo className="gap-6">
          {tiers.map((t) => <TierBadge key={t} tier={t} />)}
          <TierBadge tier="direct" degraded />
          <TierBadge tier="relayed" degraded />
        </Demo>
      </Sub>

      <Sub title="Callout" about="The tone's text on the tone's own soft fill. In high contrast the fill goes and a border in the tone takes its place.">
        <div className="grid gap-3 sm:grid-cols-2">
          <Callout tone="info" title="Unattended access is off">This machine asks every time. Turn it on in Settings to skip the prompt for devices you trust.</Callout>
          <Callout tone="ok" title="Paired">workstation can connect to this machine. Change what it may do in its permissions.</Callout>
          <Callout tone="warn" title="Relayed through a broker">Your bytes cross a machine that is not an endpoint. They are encrypted end to end, and this is slower.</Callout>
          <Callout tone="danger" title="This device's key has changed">Somebody may be impersonating it. Do not connect until you have checked with the person who owns it.</Callout>
        </div>
      </Sub>

      <Sub
        title="Permission checklist"
        about="What a person has to go and do, on the platform they are on. Every one of these is rendered here because the part that has to be right is the part nobody here can test: a person who owns a Mac can read these words and correct them without touching a screen."
      >
        <div className="grid gap-3 sm:grid-cols-2">
          {(
            [
              ["macOS", { os: "macos", display: "" }],
              ["Wayland", { os: "linux", display: "wayland" }],
              ["X11", { os: "linux", display: "x11" }],
              ["A machine with no screen", { os: "linux", display: "headless" }],
              ["Windows", { os: "windows", display: "" }],
              ["An engine that did not say", { os: "", display: "" }],
            ] as const
          ).map(([what, platform]) => (
            <div key={what} className="flex flex-col gap-1.5">
              <Key>{what}</Key>
              <PermissionChecklist platform={platform} have={{ capture: false, input: false }} />
            </div>
          ))}
          <div className="flex flex-col gap-1.5">
            <Key>A machine that can do both</Key>
            <p className="text-sm text-text-2">
              Nothing is drawn. A checklist of things that are already true is one nobody reads, and it would sit here for ever
              on a machine where everything works.
            </p>
          </div>
        </div>
      </Sub>

      <Sub
        title="Alarm"
        about="A condition of the machine, not of a screen — so it bands across the whole window, above the sidebar as well, and it cannot be dismissed. It leaves when the thing it describes does."
      >
        <div className="flex flex-col gap-3">
          <div className="shell-frame overflow-hidden rounded-md border border-border">
            <Alarm
              show
              detail="the engine stopped on its own: signal: killed"
              action={
                <Button size="sm" onClick={() => {}}>
                  Start it again
                </Button>
              }
            >
              The engine on this machine has stopped.
            </Alarm>
          </div>
          <div className="shell-frame overflow-hidden rounded-md border border-border">
            <Alarm show tone="danger" detail="a session was refused: the device key has changed">
              This machine is refusing every connection.
            </Alarm>
          </div>
        </div>
      </Sub>

      <Sub
        title="Kbd"
        about="Eleven characters of markup, written four times, already drifted: a 3px radius and a 4px radius, px-1 and px-1.5, one grey and one inheriting. A key cap is recognised by its shape, and four shapes is the shape meaning nothing. The chord is split here rather than by the caller, because every caller that split on a space printed `Drag a tab` as three keys — so a part longer than six characters is a word and is set as one."
      >
        <Demo className="gap-4">
          <Kbd keys="⌘ K" />
          <Kbd keys="Ctrl ," />
          <Kbd keys="F11" />
          <Kbd keys="?" />
          <Kbd keys="esc" />
          <Kbd keys="Drag a tab" />
        </Demo>
      </Sub>

      <Sub
        title="Shortcut sheet"
        about="The list is the bindings. `useShortcuts` walks the same array this renders, so a key that is on the sheet is a key that works — a hand-written sheet teaches one that does nothing the first time somebody adds a binding."
      >
        <Demo>
          <Button size="sm" onClick={() => setKeys(true)}>
            Show the sheet
          </Button>
        </Demo>
        <ShortcutSheet open={keys} onOpenChange={setKeys} shortcuts={SAMPLE_KEYS} />
      </Sub>

      <Sub
        title="CopyField"
        about="Every value in this product that somebody has to move — an address, an invitation link, a path, eight words — is one they will otherwise select with a mouse and lose half of. The text stays real text, because the clipboard is a permission a webview can refuse and a component whose only affordance is a button would then have none."
      >
        <div className="grid max-w-2xl gap-4 sm:grid-cols-2">
          <CopyField label="Key file" value="~/.esper/identity.key" />
          <CopyField label="Address" value="192.168.1.24:41748" />
          <CopyField
            label="Read these aloud"
            value="actor otter basil cocoa notice jubilee sodium splash"
            wrap
            className="sm:col-span-2"
          />
        </div>
      </Sub>

      <Sub
        title="Identicon"
        about="A key drawn, so a device that has quietly become a *different* device looks different from across a desk. **Not a check, ever** — the drawing is a few dozen bits of the digest, so two keys can be made to draw the same picture, and every place it appears shows the hex or the words beside it. Exactly seven of fifteen half-cells carry ink, because sampling each cell independently drew a solid block about one key in eighty, and a solid block is the same picture for every key that draws it."
      >
        <Demo className="gap-6">
          {[
            "7503856abe9926c3",
            "7503856abe9926c4",
            "019a11258f68cdd0",
            "5715145c38309cd2",
            "aa11bb22cc33dd44",
            "f37ec81b2443fbbb",
          ].map((key) => (
            <span key={key} className="flex flex-col items-center gap-2">
              <Identicon of={key} size={44} />
              <span className="font-mono text-xs text-text-3">{key.slice(0, 8)}</span>
            </span>
          ))}
        </Demo>
        <p className="max-w-[var(--measure)] pt-3 text-xs leading-relaxed text-text-3">
          The second differs from the first by one character. `check-identicon.mjs` holds that property over
          four thousand keys and sixteen thousand one-character neighbours.
        </p>
      </Sub>

      <Sub
        title="Row menu, and the same menu on a right-click"
        about="The inventory asked for a DropdownMenu and a ContextMenu separately, and building them separately is how the two lists drift — one grows an action and the other does not, and then which actions a row has depends on how you asked. One list, two triggers, and the right-click needs no second dependency: a menu positions against its trigger, so the trigger goes where the pointer was."
      >
        <div className="max-w-xl">
          <WithContextMenu actions={SAMPLE_ACTIONS}>
            <Row className="pl-3">
              <span className="flex min-w-0 flex-1 items-center gap-4">
                <Identicon of="7503856abe9926c3" size={16} />
                <span className="min-w-0 flex-1 truncate font-display text-lg font-medium">workstation</span>
                <Figure className="w-10 shrink-0 text-right text-sm text-text-2" value="2m" />
              </span>
              <RowMenu actions={SAMPLE_ACTIONS} label="Actions for workstation" />
            </Row>
          </WithContextMenu>
          <p className="pt-3 text-xs leading-relaxed text-text-3">
            Right-click anywhere on the row. Renaming is deliberately absent — the device store has no such
            operation, and a name is what the far machine calls itself.
          </p>
        </div>
      </Sub>

      <Sub
        title="Popover"
        about="A small panel from a control, for a choice that should not take the screen — the quality profile mid-session is the case, since a session is somebody else's screen and a dialog over it covers the thing they came to look at. Built on the menu primitive with its item semantics removed, so there is one focus trap and one animation rather than two."
      >
        <Demo className="gap-4">
          <Popover
            label="What capture means"
            trigger={
              <Button size="sm">
                <Lamp state="ok">capture</Lamp>
              </Button>
            }
          >
            <div className="flex flex-col gap-2">
              <p className="text-sm text-text">
                <Lamp state="ok" className="gap-2">
                  capture <span className="ml-2 text-text-3">available</span>
                </Lamp>
              </p>
              <p className="text-xs leading-relaxed text-text-2">
                How this machine reads its own screen. Without it there is nothing to send.
              </p>
              <p className="font-mono text-xs text-text-3">x11</p>
            </div>
          </Popover>
        </Demo>
      </Sub>

      <Sub
        title="SortableTable"
        about="Columns that sort, on top of Table. The part that is easy to get wrong is not the comparison but the stability: two rows that compare equal must keep the order they arrived in, or a re-render shuffles rows nobody asked to move. The whole header cell is the control, and only the sorted column draws an arrow — an arrow on every column is an arrow that says nothing."
      >
        <div className="max-w-2xl">
          <SortableTable
            rows={SAMPLE_ROWS}
            keyOf={(r) => r.name}
            sortedBy="seen"
            columns={[
              { id: "name", header: "Device", by: (r) => r.name, cell: (r) => r.name },
              { id: "seen", header: "Last seen", className: "w-28", first: "desc", by: (r) => r.seen, cell: (r) => `${r.seen}m ago` },
              { id: "perms", header: "May", className: "w-40", by: (r) => r.perms, cell: (r) => <span className="font-mono text-xs text-text-3">{r.perms}</span> },
            ]}
          />
        </div>
      </Sub>

      <Sub
        title="LogView"
        about="A long record that scrolls at sixty frames, by not drawing what nobody can see. Ten thousand lines of trail is an ordinary amount and thirty thousand DOM nodes is not — and the failure arrives as 'the app is slow' rather than 'this list is too long', which is the wrong thing to go looking at. Two spacers and an overscan, over a fixed row height the table already commits to."
      >
        <div className="flex h-64 max-w-2xl flex-col rounded-md border border-border">
          <LogView
            rows={SAMPLE_LOG}
            keyOf={(r) => String(r)}
            render={(r) => (
              <div className="flex h-[var(--row-h)] items-center gap-4 border-t border-border px-3 text-xs">
                <span className="w-16 font-mono text-text-3">{String(r).padStart(5, "0")}</span>
                <span className="text-text-2">session_opened</span>
                <span className="font-mono text-text-3">workstation</span>
              </div>
            )}
          />
        </div>
        <p className="pt-3 text-xs leading-relaxed text-text-3">
          Ten thousand rows. Scroll it — the DOM holds about thirty.
        </p>
      </Sub>

      <Sub
        title="Stepper"
        about="A short sequence with a visible end, and the one place this product has one: first run asks three things, once, in an order. Every step is drawn from every step, because a sequence that shows only where you are has an unknown end — and the first thing anybody wants to know about a setup flow is how much of it there is. Backwards is reachable; forwards is the step's own action."
      >
        <div className="max-w-xl rounded-md border border-border p-4">
          <Stepper
            steps={SAMPLE_STEPS}
            at={step}
            onGo={setStep}
            done={<Button size="sm">Done</Button>}
          />
        </div>
      </Sub>

      <Sub
        title="TransferQueue"
        about="Several files at once. One file asks how far along it is; six ask whether it is going to finish and whether anything failed — and a column of six bars answers neither without the reader adding up, so there is a line at the top that says the whole thing. **The order is not the order they were started**: running, then failed, then finished, because a failure that scrolled away under nine successes is a failure nobody sees."
      >
        <div className="max-w-xl">
          <TransferQueue transfers={SAMPLE_TRANSFERS} onCancel={() => {}} onClearFinished={() => {}} />
        </div>
      </Sub>

      <Sub title="Card, Tabs, Segmented">
        <div className="grid gap-4 lg:grid-cols-2">
          <Card>
            <CardHeader>
              <CardTitle>Session on workstation</CardTitle>
              <Badge tone="ok">Live</Badge>
            </CardHeader>
            <CardBody>
              <Tabs defaultValue="stats">
                <TabsList>
                  <TabsTrigger value="stats">Statistics</TabsTrigger>
                  <TabsTrigger value="perm">Permissions</TabsTrigger>
                  <TabsTrigger value="log">Log</TabsTrigger>
                </TabsList>
                <TabsContent value="stats" className="pt-3">Round trip 18 ms, 4.2 Mbit/s, 60 fps, software H.264.</TabsContent>
                <TabsContent value="perm" className="pt-3">Screen, mouse and keyboard, clipboard.</TabsContent>
                <TabsContent value="log" className="pt-3">Started 4 minutes ago. Monitor switched once.</TabsContent>
              </Tabs>
            </CardBody>
          </Card>
          <Card>
            <CardHeader><CardTitle>Segmented</CardTitle></CardHeader>
            <CardBody className="flex flex-col gap-3">
              <p>The one family every exclusive choice uses; the theme and motion toggles above are this component.</p>
              <Segmented label="Density" value={density} onChange={setDensity} options={[{ id: "dense", label: "Dense" }, { id: "comfortable", label: "Comfortable" }]} />
            </CardBody>
          </Card>
        </div>
      </Sub>

      <Sub title="Form controls" about="A control's boundary is --border-strong, never the hairline: it is how the control is found. Help is one line under the control — what to type, or what went wrong and what to do.">
        <Demo className="flex-col items-stretch gap-5">
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            <Field label="Device name" hint="What this machine is called on other devices.">
              {(id, by) => <Input id={id} aria-describedby={by} defaultValue="workstation" />}
            </Field>
            <Field label="Broker" error="That is not an address. Use host:port, like broker.example.org:443.">
              {(id, by) => <Input id={id} aria-describedby={by} aria-invalid defaultValue="broker.example" />}
            </Field>
            <Field label="Disabled" hint="Set by the administrator.">
              {(id, by) => <Input id={id} aria-describedby={by} disabled defaultValue="Managed" />}
            </Field>
            <Field label="Quality profile" hint="Selects and menus are one family: 32px rows, the check in a fixed slot.">
              {(id, by) => (
                <Select defaultValue="balanced">
                  <SelectTrigger id={id} aria-describedby={by}><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      <SelectLabel>Profiles</SelectLabel>
                      <SelectItem value="sharp">Sharp — text and code</SelectItem>
                      <SelectItem value="balanced">Balanced</SelectItem>
                      <SelectItem value="smooth">Smooth — video and motion</SelectItem>
                      <SelectItem value="saver" disabled>Data saver — needs a 1.x host</SelectItem>
                    </SelectGroup>
                  </SelectContent>
                </Select>
              )}
            </Field>
            <Field label="Frame rate cap" hint="A number and its unit, in one control.">
              {(id, by) => <NumberInput id={id} aria-describedby={by} value={n} onChange={setN} min={1} max={144} unit="fps" />}
            </Field>
            <Field label="Idle timeout" hint="Always stored in seconds; the unit is a way of writing it.">
              {(id, by) => <DurationInput id={id} aria-describedby={by} value={dur} onChange={setDur} />}
            </Field>
          </div>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Unattended access allowed" hint="Which days, between which times, on this machine's clock.">
              {() => <ScheduleInput value={schedule} onChange={setSchedule} />}
            </Field>
            <Field label="Note to the other side" hint="Shown once, when they connect.">
              {(id, by) => <Textarea id={id} aria-describedby={by} placeholder="Back at 14:00 — leave the build running." />}
            </Field>
          </div>
          <div className="flex flex-wrap items-center gap-6 text-sm">
            <label className="flex items-center gap-2"><Switch defaultChecked /> Start with the system</label>
            <label className="flex items-center gap-2"><Switch /> Off</label>
            <label className="flex items-center gap-2"><Switch disabled /> Disabled</label>
            <label className="flex items-center gap-2"><Checkbox defaultChecked /> Remember this device</label>
            <label className="flex items-center gap-2"><Checkbox checked="indeterminate" /> Some</label>
            <label className="flex items-center gap-2"><Checkbox disabled /> Disabled</label>
          </div>
        </Demo>
      </Sub>

      <Sub title="Table" about="28px rows, tabular numbers, hairlines between rows only. Cell content never animates.">
        <Table>
          <THead>
            <TR><TH>Device</TH><TH>When</TH><TH>Granted</TH><TH className="text-right">Moved</TH></TR>
          </THead>
          <TBody>
            {sessions.map((s) => (
              <TR key={s.who}>
                <TD className="font-medium">{s.who}</TD>
                <TD className="text-text-2"><TimeAgo date={s.when} /></TD>
                <TD className="text-text-2">{s.what}</TD>
                <TD className="text-right text-text-2">{s.moved}</TD>
              </TR>
            ))}
          </TBody>
        </Table>
      </Sub>

      <Sub title="Dialog, Tooltip, Command palette" about="Overlays open from where they were invoked. A tooltip is the text label an icon owes. The palette is ⌘K or Ctrl+K anywhere on this page.">
        <Demo>
          <Dialog>
            <DialogTrigger asChild><Button variant="danger"><Trash2 aria-hidden size={14} /> Forget device</Button></DialogTrigger>
            <DialogContent title="Forget workstation?" description="It will have to be paired again before it can connect. Nothing on the other machine changes.">
              <DialogFooter>
                <Button>Keep</Button>
                <Button variant="danger">Forget</Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>
          <Tooltip label="Settings">
            <Button variant="ghost" aria-label="Settings" className="px-2"><Settings aria-hidden size={16} /></Button>
          </Tooltip>
          <Button onClick={() => setPalette(true)}><Search aria-hidden size={14} /> Open the palette <Kbd keys="⌘K" className="ml-1" /></Button>
          <CommandPalette
            open={palette}
            onOpenChange={setPalette}
            commands={[
              { id: "connect-ws", label: "Connect to workstation", hint: "Reachable · direct", icon: Monitor, keywords: ["session"], run: () => toast.show({ title: "Connecting to workstation" }) },
              { id: "connect-bb", label: "Connect to build-box", hint: "Unreachable", icon: Monitor, run: () => toast.show({ title: "build-box is unreachable", tone: "warn" }) },
              { id: "pair", label: "Pair a new device", hint: "Show or enter a code", icon: Plus, run: () => toast.show({ title: "Opening Pair" }) },
              { id: "activity", label: "Activity", hint: "Who, when, what was granted", icon: Activity, run: () => toast.show({ title: "Opening Activity" }) },
              { id: "settings", label: "Settings", icon: Settings, run: () => toast.show({ title: "Opening Settings" }) },
            ]}
          />
        </Demo>
      </Sub>

      <Sub title="Toast and reason codes" about="Every failure the protocol can report has a human sentence. 1xx protocol, 2xx authentication, 3xx authorization, 4xx capability, 5xx transport — and the codes that need their own words have them.">
        <Demo className="flex-col items-stretch gap-3">
          <div className="flex flex-wrap gap-2">
            {reasonCodes.map((c) => (
              <Button key={c} size="sm" onClick={() => toast.reason(c === 404 ? { code: c, detail: "indicator not confirmed" } : { code: c })}>
                Reason {c}
              </Button>
            ))}
          </div>
          <ul className="grid gap-1 text-sm sm:grid-cols-2">
            {reasonCodes.map((c) => {
              const a = adviceFor({ code: c });
              return (
                <li key={c} className="flex gap-3">
                  <span className="w-8 shrink-0 font-mono text-xs text-text-3">{c}</span>
                  <span className="text-text-2">{a.title}</span>
                </li>
              );
            })}
          </ul>
        </Demo>
      </Sub>

      <Sub
        title="Empty and loading states"
        about="Every list has an empty state that says what to do next, and a skeleton the shape of what is coming so nothing pops. Skeletons do not shimmer. There were two empty states for a while — this one and a Nothing in the screens' own vocabulary — and every real screen used the other; they are one component now, with the treatment the screens proved they wanted: centred, no box, a hairline mark."
      >
        <div className="grid gap-4 divide-y divide-border lg:grid-cols-3 lg:divide-x lg:divide-y-0">
          <EmptyState icon={Monitor} title="No devices yet" action={<Button variant="primary">Pair a device</Button>}>
            Pair this machine with another and it will appear here, ready to connect in one click.
          </EmptyState>
          <EmptyState icon={Inbox} title="No sessions">
            When a device connects to this machine, or this machine connects out, it is listed here.
          </EmptyState>
          <EmptyState icon={Activity} title="No activity">
            Everything granted, connected or moved is recorded here, on this machine only.
          </EmptyState>
        </div>
        <Card className="flex flex-col gap-3 p-4">
          <div className="flex items-center gap-3">
            <Skeleton className="size-9" />
            <div className="flex flex-1 flex-col gap-2"><Skeleton className="h-3.5 w-40" /><Skeleton className="h-3 w-24" /></div>
            <Skeleton className="h-5 w-24" />
          </div>
          <Skeleton className="h-3 w-3/4" />
          <div className="flex justify-between border-t border-border pt-3"><Skeleton className="h-3 w-20" /><Skeleton className="h-[var(--control-h)] w-20" /></div>
        </Card>
      </Sub>
    </Section>
  );
}
