import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { cn } from "@/lib/cn";
import { pixelsOf } from "@/lib/theme";

/**
 * A long record that scrolls at sixty frames, by not drawing what nobody can see.
 *
 * # Why this exists
 *
 * The trail is the whole history of a machine. Ten thousand lines of it is an
 * ordinary amount and thirty thousand DOM nodes is not — the scroll goes to
 * pieces long before the data does, and the failure arrives as "the app is
 * slow" rather than as "this list is too long", which is the wrong thing to
 * go looking at.
 *
 * # How, and why this way
 *
 * **Fixed row height.** Every row is `--row-h`, which the table already
 * commits to, so the position of row *n* is arithmetic rather than a
 * measurement. Variable heights need measuring every row, which needs drawing
 * every row, which is the thing being avoided — so a virtualised list with
 * variable rows is a different and much larger component, and this one
 * refuses to be it.
 *
 * **Two spacers rather than absolute positioning.** A div above and a div
 * below, sized to the rows that are not drawn. The rows themselves are
 * ordinary flow content, so they inherit everything — hover, focus, selection,
 * the hairlines between them — instead of each needing a `top` nobody can
 * debug.
 *
 * **An overscan.** A few rows either side of the window, so a fast flick does
 * not show the empty space it is scrolling into before React catches up.
 *
 * No dependency. A windowing library is a good library and this is twenty
 * lines of arithmetic over one measured number; the parts of it that are hard
 * — variable heights, sticky groups, horizontal windows — are parts this
 * product does not have.
 */

export interface LogViewProps<Row> {
  rows: readonly Row[];
  /** One row, at exactly the row height. */
  render: (row: Row, index: number) => ReactNode;
  keyOf: (row: Row, index: number) => string;
  /**
   * Pixels, when a caller genuinely draws something other than a table row.
   *
   * **Left out by default, and read from `--row-h` instead.** The arithmetic
   * here has to agree with what the rows actually are, and a component
   * holding its own `28` drifts from the token the first time somebody makes
   * the tables denser — silently, and as a scroll that lands in the wrong
   * place rather than as a wrong-looking row.
   */
  rowHeight?: number;
  /** Rows drawn either side of the window. */
  overscan?: number;
  /** Drawn instead of the list when there is nothing. */
  empty?: ReactNode;
  className?: string | undefined;
}

export function LogView<Row>({
  rows,
  render,
  keyOf,
  rowHeight,
  overscan = 8,
  empty,
  className,
}: LogViewProps<Row>) {
  const box = useRef<HTMLDivElement | null>(null);
  const [top, setTop] = useState(0);
  const [height, setHeight] = useState(0);
  // The token, resolved where this actually sits — a shell that redefined
  // `--row-h` for a denser table would otherwise be read from the document.
  const [tokenHeight, setTokenHeight] = useState(0);

  // The window's own height, watched rather than read once: a shell that
  // collapses its sidebar changes this, and a list sized to the height it had
  // at mount draws the wrong number of rows for the rest of its life.
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const watch = new ResizeObserver(() => setHeight(el.clientHeight));
    watch.observe(el);
    setHeight(el.clientHeight);
    setTokenHeight(pixelsOf("--row-h", el));
    return () => watch.disconnect();
  }, []);

  // A caller's own height wins, then the token, then the token's own value as
  // a last resort — which only happens before the first measurement.
  const row = rowHeight ?? (tokenHeight > 0 ? tokenHeight : 28);

  const onScroll = useCallback(() => {
    const el = box.current;
    if (el) setTop(el.scrollTop);
  }, []);

  const window = useMemo(() => {
    if (height === 0) {
      // Before the first measurement, draw a screenful rather than nothing:
      // a list that is briefly empty is a flash somebody sees.
      const guess = Math.min(rows.length, overscan * 4);
      return { from: 0, to: guess };
    }
    const first = Math.floor(top / row);
    const fits = Math.ceil(height / row);
    return {
      from: Math.max(0, first - overscan),
      to: Math.min(rows.length, first + fits + overscan),
    };
  }, [top, height, row, overscan, rows.length]);

  if (rows.length === 0 && empty) {
    return <div className={className}>{empty}</div>;
  }

  const above = window.from * row;
  const below = Math.max(0, (rows.length - window.to) * row);

  return (
    <div
      ref={box}
      onScroll={onScroll}
      className={cn("min-h-0 flex-1 overflow-y-auto", className)}
      // The count is what a screen reader needs and what the DOM cannot say
      // when most of the rows are not in it.
      role="log"
      aria-label={`${rows.length} entries`}
    >
      {/* The rows nobody can see, as height. */}
      <div style={{ height: above }} aria-hidden />
      {rows.slice(window.from, window.to).map((entry, i) => (
        <div key={keyOf(entry, window.from + i)} style={{ height: row }}>
          {render(entry, window.from + i)}
        </div>
      ))}
      <div style={{ height: below }} aria-hidden />
    </div>
  );
}
