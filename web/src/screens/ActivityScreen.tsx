import {
  Activity,
  ArrowDownLeft,
  ArrowUpRight,
  Ban,
  Cable,
  CalendarClock,
  CircleDot,
  Gauge,
  Handshake,
  Lock,
  MessageCircleQuestion,
  MonitorOff,
  MonitorPlay,
  Power,
  Scissors,
  Settings2,
  ShieldCheck,
  ShieldX,
  SquareTerminal,
  Unlink,
  X,
  type LucideIcon,
} from "lucide-react";
import { useMemo, useState, type ReactNode } from "react";
import { useSessionLog } from "@/api/hooks";
import { AUTHORISED_BY, PERMISSION_LABEL, eventLabel, type Permission } from "@/api/permissions";
import type { Happening } from "@/api/types";
import { Button } from "@/components/button";
import { Identicon } from "@/components/identicon";
import { LogView } from "@/components/log-view";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/select";
import { EmptyState, Skeleton } from "@/components/states";
import { cn } from "@/lib/cn";
import { PageBody, Topbar } from "@/shell/AppShell";
import { Figure, Trouble } from "./parts";

/**
 * Activity — who, when, what was granted, what moved.
 *
 * A record nobody reads is a record that is not doing its job, so it reads
 * as a timeline: a day, then what happened, to which device, and when.
 * The live half arrives on the event stream and joins the top of the list;
 * the recorded half is `/sessions`, which is the audit log rendered.
 */

const clock = (at: number) =>
  new Date(at * 1000).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });

/*
  ── The timeline ────────────────────────────────────────────────────────
  A day heading, then one event per line: what happened, to which device,
  and the sentence the record holds about it. It was a table with the event
  written twice, as a tag and as a label, beside a sentence that said it a
  third time; a person reading a history wants it once, in words.

  The tag stays on the line as `data-event`, for whatever reads the page
  rather than looks at it, and as the icon's tooltip for whoever greps the
  log on disk for it.
*/

/** The mark for each kind of event. Unknown ones get the screen's own icon. */
const ICONS: Readonly<Record<string, LucideIcon>> = {
  session_opened: MonitorPlay,
  session_closed: MonitorOff,
  session_asked: MessageCircleQuestion,
  session_narrowed: Gauge,
  paired: Handshake,
  pairing_kept: Handshake,
  unpaired: Unlink,
  revoked: Ban,
  refused: ShieldX,
  permissions_changed: ShieldCheck,
  schedule_changed: CalendarClock,
  lock_after_changed: Lock,
  locked: Lock,
  powered: Power,
  setting_changed: Settings2,
  trail_trimmed: Scissors,
  recording: CircleDot,
  terminal_opened: SquareTerminal,
  terminal_closed: SquareTerminal,
  terminal_parked: SquareTerminal,
  terminal_resumed: SquareTerminal,
  forward_opened: Cable,
  forward_closed: Cable,
  forward_refused: Cable,
};

/** Events that are somebody being told no. They carry the warning tone, never colour alone. */
const REFUSALS = new Set(["refused", "forward_refused", "revoked"]);

/** The sentence the record holds, starting as a sentence does. */
const sentence = (detail: string) =>
  // Not when it opens on a name: "files.enabled" or "host_fps" is a key, and a
  // key with a capital letter is a different key.
  /^[a-z]+(\s|$)/.test(detail) ? detail.charAt(0).toUpperCase() + detail.slice(1) : detail;

/** "Today", "Yesterday", or the date, for a day heading. */
function dayHeading(at: number): string {
  const d = new Date(at * 1000);
  const today = new Date();
  const start = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((start(today) - start(d)) / 86_400_000);
  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  return d.toLocaleDateString(undefined, {
    weekday: "long",
    month: "long",
    day: "numeric",
    ...(d.getFullYear() !== today.getFullYear() ? { year: "numeric" } : {}),
  });
}

const dayKey = (at: number) => new Date(at * 1000).toDateString();

type Item = { kind: "day"; label: string; key: string } | { kind: "line"; h: Happening; index: number };

/** Every line, with a heading wherever the day changes. Newest first. */
function timeline(lines: readonly Happening[]): Item[] {
  const out: Item[] = [];
  let last = "";
  lines.forEach((h, index) => {
    const k = dayKey(h.at);
    if (k !== last) {
      out.push({ kind: "day", label: dayHeading(h.at), key: k });
      last = k;
    }
    out.push({ kind: "line", h, index });
  });
  return out;
}

/** One line's height, and a day heading's: the list is windowed, so every item is the same. */
const ITEM_HEIGHT = 60;

function Line({ h, fresh }: { h: Happening; fresh?: boolean }) {
  const Icon = ICONS[h.event] ?? Activity;
  const refusal = REFUSALS.has(h.event);
  const inbound = h.direction !== "out";
  return (
    <div
      data-line=""
      data-event={h.event}
      className={cn(
        "flex h-full items-center gap-3.5 rounded-lg px-3",
        "transition-[background-color] duration-[var(--dur-fast)] ease-[var(--ease)] hover:bg-text/[0.035]",
      )}
    >
      <span
        title={h.event}
        className={cn(
          "flex size-9 shrink-0 items-center justify-center rounded-lg bg-text/[0.05]",
          refusal ? "text-warn" : "text-text-2",
        )}
      >
        <Icon aria-hidden size={16} strokeWidth={1.6} />
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="flex min-w-0 items-center gap-2 text-sm">
          <span className="shrink-0 font-medium text-text" data-fresh={fresh || undefined}>{eventLabel(h.event)}</span>
          <span className="text-text-3" aria-hidden>
            ·
          </span>
          {h.key === "" ? (
            <span className="min-w-0 truncate text-text-2">This machine</span>
          ) : (
            <span className="flex min-w-0 items-center gap-1.5 text-text-2">
              <Identicon of={h.key} size={12} />
              <span className="min-w-0 truncate">
                {h.device || <span className="font-mono text-xs text-text-3">{h.key.slice(0, 16)}</span>}
              </span>
            </span>
          )}
        </span>
        <span className="min-w-0 truncate text-xs text-text-3">{sentence(h.detail)}</span>
      </span>
      <span className="flex shrink-0 items-center gap-2 text-xs text-text-3">
        <Figure className="whitespace-nowrap text-xs text-text-3" value={clock(h.at)} />
        <span title={inbound ? "Another device acted on this machine" : "This machine acted on another device"}>
          {inbound ? (
            <ArrowDownLeft aria-label="Incoming" size={13} strokeWidth={1.75} />
          ) : (
            <ArrowUpRight aria-label="Outgoing" size={13} strokeWidth={1.75} />
          )}
        </span>
      </span>
    </div>
  );
}

const WINDOWS = [
  { id: "all", label: "Any time", seconds: 0 },
  { id: "day", label: "Last 24 hours", seconds: 24 * 60 * 60 },
  { id: "week", label: "Last 7 days", seconds: 7 * 24 * 60 * 60 },
  { id: "month", label: "Last 30 days", seconds: 30 * 24 * 60 * 60 },
] as const;

/** One filter: a mono label and a select, so three of them read as one row. */
function Filter({
  label,
  value,
  onChange,
  children,
  width = "w-40",
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  children: ReactNode;
  width?: string;
}) {
  // The select says its own value ("Any device"), so the name is for a screen
  // reader rather than a caps label over every control.
  return (
    <span className={cn("shrink-0", width)}>
      <Select value={value} onValueChange={onChange}>
        <SelectTrigger aria-label={label}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>{children}</SelectContent>
      </Select>
    </span>
  );
}

export function ActivityScreen() {
  const { data, error, loading, live } = useSessionLog(200);
  const [who, setWho] = useState("all");
  const [when, setWhen] = useState("all");
  const [what, setWhat] = useState("all");
  /*
    Which kind of line, as against which permission allowed it.

    The permission filter answers "everything that used `terminal`", which is
    the question somebody usually opens a trail with. It cannot answer the
    other one: ten of the twenty-three events are authorised by no permission
    at all — pairing, unpairing, revoking, a schedule changing — and
    **`refused` is one of them**. A run of refusals is what an attack looks
    like from this side, and it was the one thing this screen could not be
    asked for.
  */
  const [kind, setKind] = useState("all");

  // The stream is a notification and `/sessions` is the record, so the two
  // are joined here rather than either pretending to be the other. A
  // happening already in the record is not shown twice.
  const lines = useMemo(() => {
    const recorded = data ?? [];
    const known = new Set(recorded.map((h) => `${h.at}:${h.key}:${h.event}`));
    const fresh = live.filter((h) => !known.has(`${h.at}:${h.key}:${h.event}`));
    return { fresh, all: [...fresh, ...recorded] };
  }, [data, live]);

  /*
    Which devices appear, from the record rather than from the device store.

    Deliberately: a device that was unpaired is still in the trail and is
    exactly what somebody looks for afterwards. Offering only currently
    paired devices would hide the history of the one they came here about.
  */
  const machines = useMemo(() => {
    const byKey = new Map<string, string>();
    for (const h of lines.all) {
      if (!byKey.has(h.key)) byKey.set(h.key, h.device || h.key.slice(0, 16));
    }
    return [...byKey].sort((a, b) => a[1].localeCompare(b[1]));
  }, [lines.all]);

  /*
    Which permissions appear, through the map from event to the permission
    that authorised it — because "everything that used `terminal`" is what
    somebody asks, and the trail records *what happened* instead.

    Only permissions that are actually represented are offered: a filter with
    fourteen entries, twelve of which match nothing, is a filter nobody
    trusts the second time.
  */
  const permissions = useMemo(() => {
    const present = new Set<Permission>();
    for (const h of lines.all) {
      const by = AUTHORISED_BY[h.event];
      if (by) present.add(by);
    }
    return [...present].sort();
  }, [lines.all]);

  const shown = useMemo(() => {
    const cut = WINDOWS.find((w) => w.id === when)?.seconds ?? 0;
    const since = cut ? Math.round(Date.now() / 1000) - cut : 0;
    return lines.all.filter((h) => {
      if (who !== "all" && h.key !== who) return false;
      if (cut && h.at < since) return false;
      if (what !== "all" && AUTHORISED_BY[h.event] !== what) return false;
      if (kind !== "all" && h.event !== kind) return false;
      return true;
    });
  }, [lines.all, who, when, what, kind]);

  /*
    Which kinds appear, for the same reason only the permissions present are
    offered: a filter of twenty-three entries where twenty match nothing is a
    filter nobody trusts the second time.
  */
  const kinds = useMemo(() => {
    const present = new Set<string>();
    for (const h of lines.all) present.add(h.event);
    return [...present].sort((a, b) => eventLabel(a).localeCompare(eventLabel(b)));
  }, [lines.all]);

  // Newest first, always: the last thing that happened is what somebody came
  // to see. A history read oldest-first is a thing nobody asked this for.
  const ordered = useMemo(() => [...shown].sort((x, y) => y.at - x.at), [shown]);
  const items = useMemo(() => timeline(ordered), [ordered]);

  const filtered = who !== "all" || when !== "all" || what !== "all" || kind !== "all";
  const clear = () => {
    setWho("all");
    setWhen("all");
    setWhat("all");
    setKind("all");
  };

  return (
    <>
      <Topbar title="Activity" />
      <PageBody>
        {/*
          The filters and the count, as one toolbar, drawn only when there is
          something to filter. Three empty selects above an empty page is
          furniture.
        */}
        {!loading && !error && lines.all.length > 0 && (
          <div className="flex flex-wrap items-center gap-2 pb-1">
            <Filter label="Device" value={who} onChange={setWho}>
              <SelectItem value="all">Any device</SelectItem>
              {machines.map(([key, name]) => (
                <SelectItem key={key} value={key}>
                  {name}
                </SelectItem>
              ))}
            </Filter>

            <Filter label="When" value={when} onChange={setWhen}>
              {WINDOWS.map((w) => (
                <SelectItem key={w.id} value={w.id}>
                  {w.label}
                </SelectItem>
              ))}
            </Filter>

            {kinds.length > 1 && (
              <Filter label="Event" value={kind} onChange={setKind} width="w-48">
                <SelectItem value="all">Any event</SelectItem>
                {kinds.map((e) => (
                  <SelectItem key={e} value={e}>
                    {eventLabel(e)}
                  </SelectItem>
                ))}
              </Filter>
            )}

            {permissions.length > 0 && (
              <Filter label="Permission" value={what} onChange={setWhat} width="w-44">
                <SelectItem value="all">Any permission</SelectItem>
                {permissions.map((p) => (
                  <SelectItem key={p} value={p}>
                    {PERMISSION_LABEL[p]}
                  </SelectItem>
                ))}
              </Filter>
            )}

            {filtered && (
              <Button variant="ghost" size="sm" onClick={clear}>
                <X aria-hidden size={14} /> Clear
              </Button>
            )}

            <span className="ml-auto text-sm text-text-3">
              <Figure className="text-sm text-text-2" value={shown.length} /> {shown.length === 1 ? "event" : "events"}
            </span>
          </div>
        )}

        {loading && (
          <div className="flex flex-col gap-2">
            {[0, 1, 2, 3].map((i) => (
              <div key={i} className="flex items-center gap-3.5 px-3 py-2.5">
                <Skeleton className="size-9 rounded-lg" />
                <span className="flex flex-col gap-1.5">
                  <Skeleton className="h-3.5 w-48" />
                  <Skeleton className="h-3 w-72" />
                </span>
              </div>
            ))}
          </div>
        )}

        {error && <Trouble detail={error.message}>Could not load the activity log.</Trouble>}

        {!loading && !error && lines.all.length === 0 && (
          <div data-grow="true" data-empty="true" className="flex">
            <EmptyState icon={Activity} title="Nothing has happened yet">
              Sessions, pairings, permission changes and file transfers on this machine will appear here.
            </EmptyState>
          </div>
        )}

        {/*
          Filters that match nothing, which is not the same as nothing having
          happened, and the difference decides what somebody does next: the
          count says the record is there, and the button is the way back.
        */}
        {!loading && !error && lines.all.length > 0 && ordered.length === 0 && (
          <div data-grow="true" data-empty="filtered" className="flex">
            <EmptyState
              icon={Activity}
              action={
                <Button variant="secondary" size="sm" onClick={clear}>
                  Clear filters
                </Button>
              }
            >
              Nothing matches these filters. This machine has {lines.all.length}{" "}
              {lines.all.length === 1 ? "entry" : "entries"} in total.
            </EmptyState>
          </div>
        )}

        {/*
          Windowed: the record is the whole history of a machine, and ten
          thousand lines is ordinary. Day headings are items of the same height
          as a line, so the window's arithmetic stays one multiplication.
        */}
        {ordered.length > 0 && (
          <div data-grow="true" className="flex min-h-0 flex-col">
            <LogView
              rows={items}
              rowHeight={ITEM_HEIGHT}
              keyOf={(item, i) => (item.kind === "day" ? `day-${item.key}` : `${item.h.at}-${item.h.key}-${item.h.event}-${i}`)}
              render={(item) =>
                item.kind === "day" ? (
                  <h2 className="flex h-full items-end px-3 pb-2 text-sm font-medium text-text-2">{item.label}</h2>
                ) : (
                  <Line h={item.h} fresh={item.index < lines.fresh.length && !filtered} />
                )
              }
            />
          </div>
        )}

        {lines.all.length > 0 && (
          <p className="flex items-center gap-2 px-3 pt-6 text-xs text-text-3">
            <Lock aria-hidden size={13} strokeWidth={1.75} />
            This record is kept only on this machine. Nothing is sent anywhere.
          </p>
        )}
      </PageBody>
    </>
  );
}
