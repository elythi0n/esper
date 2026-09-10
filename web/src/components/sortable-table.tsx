import { ArrowDown, ArrowUp } from "lucide-react";
import { useCallback, useMemo, useState, type ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Table, TBody, TD, TH, THead, TR } from "./table";

/**
 * A table whose columns can be sorted, on top of `Table`.
 *
 * # Why the sort lives in the component
 *
 * Because otherwise it lives in every screen, three times, slightly
 * differently — and the part that is easy to get wrong is not the comparison
 * but the *stability*: two rows that compare equal must keep the order they
 * arrived in, or a re-render shuffles rows nobody asked to move. `Array.sort`
 * is stable in every engine this ships to, and this leans on that
 * deliberately rather than adding a tiebreak nobody can predict.
 *
 * # What it does not do
 *
 * It does not fetch, page, or filter. Sorting a list is arithmetic on a list
 * somebody already has; a table that also decided *which* rows to hold would
 * be two components, and the screen already owns its filters.
 *
 * The header is a real `<th>` with `aria-sort`, and the control is the whole
 * cell rather than an icon: a target the width of a column is one nobody
 * misses, and the arrow is a hint about the state rather than the way in.
 */

export interface Column<Row> {
  /** Stable key, used for the sort state and for React. */
  id: string;
  header: ReactNode;
  /** What to draw in the cell. */
  cell: (row: Row) => ReactNode;
  /**
   * What to sort by. Absent means this column cannot be sorted — a column of
   * buttons, say — and then its header is not a control.
   */
  by?: ((row: Row) => string | number) | undefined;
  /** Tailwind width and alignment for the column. */
  className?: string | undefined;
  /** Which way the first click sorts. Descending for times and sizes. */
  first?: "asc" | "desc" | undefined;
}

export interface SortableTableProps<Row> {
  rows: readonly Row[];
  columns: readonly Column<Row>[];
  /** Row identity, so React and the sort agree about what moved. */
  keyOf: (row: Row) => string;
  /** Which column is sorted before anybody clicks. */
  sortedBy?: string | undefined;
  onRowClick?: ((row: Row) => void) | undefined;
  /** Drawn in place of the body when there are no rows. */
  empty?: ReactNode;
  className?: string | undefined;
}

/** Which column is sorted, and which way. */
export interface Sorted {
  id: string;
  way: "asc" | "desc";
}

/**
 * The sort, as a hook, so a virtualised list and a plain table can share it.
 *
 * Activity's record is long enough to need windowing (`LogView`) and its
 * columns are worth sorting, and those two wants pull in opposite directions:
 * a virtualised `<tbody>` is the variable-height problem `LogView` refuses to
 * be. So the *sorting* is extracted and the two components compose over it —
 * one comparison, one notion of which column is active, two ways of drawing
 * the rows.
 */
export function useSortedRows<Row>(
  rows: readonly Row[],
  columns: readonly Column<Row>[],
  sortedBy?: string | undefined,
): { rows: readonly Row[]; sort: Sorted | null; toggle: (column: Column<Row>) => void } {
  const initial = columns.find((c) => c.id === sortedBy);
  const [sort, setSort] = useState<Sorted | null>(
    initial?.by ? { id: initial.id, way: initial.first ?? "asc" } : null,
  );

  const ordered = useMemo(() => {
    const column = columns.find((c) => c.id === sort?.id);
    if (!column?.by || !sort) return rows;
    const by = column.by;
    const sign = sort.way === "asc" ? 1 : -1;
    // A copy, because sorting the caller's array in place would reorder
    // whatever else is holding it — including the list a parent is about to
    // re-render from.
    //
    // **Stability is the part that matters.** Two rows that compare equal
    // must keep the order they arrived in, or a re-render shuffles rows
    // nobody asked to move. `Array.sort` is stable in every engine this
    // ships to, and this leans on that rather than inventing a tiebreak
    // nobody can predict.
    return [...rows].sort((a, b) => {
      const left = by(a);
      const right = by(b);
      if (typeof left === "number" && typeof right === "number") return (left - right) * sign;
      return String(left).localeCompare(String(right)) * sign;
    });
  }, [rows, columns, sort]);

  const toggle = useCallback((column: Column<Row>) => {
    if (!column.by) return;
    setSort((now) =>
      now?.id === column.id
        ? { id: column.id, way: now.way === "asc" ? "desc" : "asc" }
        : { id: column.id, way: column.first ?? "asc" },
    );
  }, []);

  return { rows: ordered, sort, toggle };
}

/**
 * One sortable header cell, so a table and a windowed list draw the same one.
 *
 * The control is the whole cell rather than an icon: a target the width of a
 * column is one nobody misses, and the arrow is a hint about the state rather
 * than the way in. Only the sorted column draws one — an arrow on every
 * column is an arrow that says nothing.
 */
export function SortHeader<Row>({
  column,
  sort,
  onToggle,
}: {
  column: Column<Row>;
  sort: Sorted | null;
  onToggle: (column: Column<Row>) => void;
}) {
  const active = sort?.id === column.id;
  if (!column.by) {
    return <span className="flex h-[var(--row-h)] items-center px-2">{column.header}</span>;
  }
  return (
    <button
      type="button"
      onClick={() => onToggle(column)}
      className={cn(
        "flex h-[var(--row-h)] w-full cursor-pointer items-center gap-1 px-2 text-left",
        "transition-colors duration-[var(--dur-fast)] hover:text-text",
        active ? "text-text" : "text-text-3",
      )}
    >
      <span className="min-w-0 truncate">{column.header}</span>
      {active &&
        (sort.way === "asc" ? (
          <ArrowUp aria-hidden size={12} strokeWidth={2} />
        ) : (
          <ArrowDown aria-hidden size={12} strokeWidth={2} />
        ))}
    </button>
  );
}

export function SortableTable<Row>({
  rows,
  columns,
  keyOf,
  sortedBy,
  onRowClick,
  empty,
  className,
}: SortableTableProps<Row>) {
  const { rows: ordered, sort, toggle } = useSortedRows(rows, columns, sortedBy);

  return (
    <Table className={className}>
      <THead>
        <TR className="hover:bg-transparent">
          {columns.map((c) => (
            <TH
              key={c.id}
              className={cn("p-0", c.className)}
              aria-sort={
                sort?.id === c.id ? (sort.way === "asc" ? "ascending" : "descending") : undefined
              }
            >
              <SortHeader column={c} sort={sort} onToggle={toggle} />
            </TH>
          ))}
        </TR>
      </THead>
      <TBody>
        {ordered.length === 0 && empty ? (
          <tr>
            <td colSpan={columns.length} className="px-2 py-6">
              {empty}
            </td>
          </tr>
        ) : (
          ordered.map((row) => (
            <TR
              key={keyOf(row)}
              onClick={onRowClick ? () => onRowClick(row) : undefined}
              className={onRowClick ? "cursor-pointer" : undefined}
            >
              {columns.map((c) => (
                <TD key={c.id} className={c.className}>
                  {c.cell(row)}
                </TD>
              ))}
            </TR>
          ))
        )}
      </TBody>
    </Table>
  );
}
