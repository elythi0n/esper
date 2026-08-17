import { Activity, X } from "lucide-react";
import { useMemo, useState, type ReactNode } from "react";
import { useSessionLog } from "@/api/hooks";
import { AUTHORISED_BY, PERMISSION_LABEL, type Permission } from "@/api/permissions";
import type { Happening } from "@/api/types";
import { Button } from "@/components/button";
import { Identicon } from "@/components/identicon";
import { LogView } from "@/components/log-view";
import { SortHeader, useSortedRows, type Column } from "@/components/sortable-table";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/select";
import { EmptyState, Skeleton } from "@/components/states";
import { cn } from "@/lib/cn";
import { PageBody, Topbar } from "@/shell/AppShell";
import { Figure, Heading, Key, Trouble } from "./parts";

/**
 * Activity — who, when, what was granted, what moved.
 *
 * A record nobody reads is a record that is not doing its job, so it is a
 * table rather than a paragraph: a time, a direction, a device, an event.
 * The live half arrives on the event stream and joins the top of the list;
 * the recorded half is `/sessions`, which is the audit log rendered.
 */

const clock = (at: number) =>
  new Date(at * 1000).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit" });

const day = (at: number) => new Date(at * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" });

/**
 * The record's columns, which now drive three things instead of one.
 *
 * The header (sortable), the windowed body, and the widths — from one
 * declaration, because a header whose columns do not line up with its rows is
 * the classic table bug and the only reliable cure is to stop writing them
 * twice.
 */
function columnsFor(): readonly Column<Happening>[] {
  return [
    {
      id: "at",
      header: "When",
      className: "w-32",
      // Descending first: the last thing that happened is what somebody came
      // to see, and an audit log that opened on its oldest line would be
      // asking them to scroll for it.
      first: "desc",
      by: (h) => h.at,
      cell: (h) => (
        <span className="flex items-baseline gap-2">
          <Figure className="text-xs text-text-3" value={clock(h.at)} />
          <Figure className="whitespace-nowrap text-xs text-text-3" value={day(h.at)} />
        </span>
      ),
    },
    {
      id: "direction",
      header: "",
      className: "w-6 text-center",
      cell: (h) => (
        <span aria-label={h.direction === "out" ? "Outbound" : "Inbound"} className="text-xs text-text-3">
          {h.direction === "out" ? "→" : "←"}
        </span>
      ),
    },
    {
      id: "device",
      header: "Device",
      // Fixed rather than greedy: the detail is the column worth the spare
      // room, and a device name that stretched to fill it left `What` and
      // `Detail` pinned to the far edge with a hand's width of nothing
      // between them and the name.
      className: "w-56",
      by: (h) => h.device || h.key,
      cell: (h) => (
        <span className="flex min-w-0 items-center gap-2.5">
          <Identicon of={h.key} size={14} />
          <span className="min-w-0 truncate text-sm text-text-2">
            {h.device || <span className="font-mono text-xs text-text-3">{h.key.slice(0, 16)}</span>}
          </span>
        </span>
      ),
    },
    {
      id: "event",
      header: "What",
      className: "w-40",
      by: (h) => h.event,
      cell: (h) => <span className="font-mono text-xs text-text">{h.event}</span>,
    },
    {
      id: "detail",
      header: "Detail",
      // `table-cell` was here, which is a display mode from the table this
      // body no longer is — inside a flex row it does nothing useful.
      className: "hidden min-w-0 flex-1 @[45rem]:block",
      cell: (h) => <span className="truncate text-xs text-text-3">{h.detail}</span>,
    },
  ];
}

/**
 * One row, from the columns.
 *
 * A grid rather than a `<tr>`, because the body is **windowed**: a
 * virtualised `<tbody>` is the variable-height problem `LogView` refuses to
 * be, so the header is a table and the rows are a grid that lines up with
 * it. The widths come from the same declaration either way, which is what
 * stops the two drifting.
 */
function Line({
  h,
  columns,
  fresh,
}: {
  h: Happening;
  columns: readonly Column<Happening>[];
  fresh?: boolean;
}) {
  return (
    <div
      className={cn(
        "flex h-[var(--row-h)] items-center border-t border-border",
        "transition-[background-color] duration-[var(--dur-fast)] ease-[var(--ease)] hover:bg-surface-1",
        fresh ? "text-text" : "text-text-2",
      )}
    >
      {columns.map((c) => (
        <div key={c.id} className={cn("px-2", c.className ?? "min-w-0 flex-1")}>
          {c.cell(h)}
        </div>
      ))}
    </div>
  );
}

/** How far back to look. Absolute cut-offs, in seconds, so the label and the filter cannot drift. */
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
  return (
    <label className="flex min-w-0 flex-col gap-1.5">
      <Key>{label}</Key>
      <Select value={value} onValueChange={onChange}>
        <SelectTrigger className={width} aria-label={label}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>{children}</SelectContent>
      </Select>
    </label>
  );
}

export function ActivityScreen() {
  const { data, error, loading, live } = useSessionLog(200);
  const [who, setWho] = useState("all");
  const [when, setWhen] = useState("all");
  const [what, setWhat] = useState("all");

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
      return true;
    });
  }, [lines.all, who, when, what]);

  // The columns, and the sort over them. One declaration drives the header,
  // the rows and the widths.
  const columns = useMemo(() => columnsFor(), []);
  const { rows: ordered, sort, toggle } = useSortedRows(shown, columns, "at");

  const filtered = who !== "all" || when !== "all" || what !== "all";
  const clear = () => {
    setWho("all");
    setWhen("all");
    setWhat("all");
  };

  return (
    <>
      <Topbar title="Activity" />
      <PageBody>
        {(loading || lines.all.length > 0 || error) && (
          <Heading count={loading ? "" : shown.length}>On this machine</Heading>
        )}

        {/*
          The filters, drawn only when there is something to filter. Three
          empty selects above an empty page is furniture.
        */}
        {!loading && !error && lines.all.length > 0 && (
          <div className="flex flex-wrap items-end gap-4 pb-4">
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
          </div>
        )}

        {loading && (
          <div className="flex flex-col">
            {[0, 1, 2, 3].map((i) => (
              <div key={i} className="flex items-center gap-4 border-b border-border py-2.5">
                <Skeleton className="h-3 w-16" />
                <Skeleton className="h-3 w-10" />
                <Skeleton className="h-3 w-32" />
              </div>
            ))}
          </div>
        )}

        {error && <Trouble detail={error.message}>What has happened here could not be read.</Trouble>}

        {!loading && !error && lines.all.length === 0 && (
          <div data-grow="true" data-empty="true" className="flex">
            <EmptyState icon={Activity}>
              Nothing has happened on this machine yet. Sessions, grants and transfers are recorded here.
            </EmptyState>
          </div>
        )}

        {/*
          The header is a table row and the body is windowed. The record is
          the whole history of a machine — ten thousand lines is ordinary, and
          thirty thousand DOM nodes is not — so only what fits on screen is
          drawn, and the scroll stays a scroll.
        */}
        {ordered.length > 0 && (
          <div data-grow="true" className="flex min-h-0 flex-col">
            <div className="flex text-xs text-text-3" role="row">
              {columns.map((c) => (
                <div
                  key={c.id}
                  className={cn(c.className ?? "min-w-0 flex-1")}
                  role="columnheader"
                  aria-sort={
                    sort?.id === c.id ? (sort.way === "asc" ? "ascending" : "descending") : undefined
                  }
                >
                  <SortHeader column={c} sort={sort} onToggle={toggle} />
                </div>
              ))}
            </div>
            {/* No height here: `LogView` reads `--row-h`, which is what the
                rows are actually drawn at. */}
            <LogView
              rows={ordered}
              keyOf={(h, i) => `${h.at}-${h.key}-${h.event}-${i}`}
              render={(h, i) => (
                <Line h={h} columns={columns} fresh={i < lines.fresh.length && !filtered} />
              )}
            />
          </div>
        )}

        {/*
          Filtered to nothing is not the same as nothing having happened, and
          the difference decides what somebody does next.
        */}
        {!loading && !error && lines.all.length > 0 && shown.length === 0 && (
          <EmptyState
            action={
              <Button size="sm" onClick={clear}>
                Clear them
              </Button>
            }
          >
            Nothing here matches those filters.
          </EmptyState>
        )}

        {lines.all.length > 0 && (
          <p className="pt-6">
            <Key>Kept on this machine only</Key>
          </p>
        )}
      </PageBody>
    </>
  );
}
