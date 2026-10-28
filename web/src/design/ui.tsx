import { useEffect, useState, type HTMLAttributes, type ReactNode, type RefObject } from "react";
import { cn } from "@/lib/cn";
import "./design.css";

/*
  The gate's own furniture. Nothing here is a product component; it is how the
  product components are shown.
*/

export function Section({ id, title, lede, children }: { id: string; title: string; lede: ReactNode; children: ReactNode }) {
  return (
    <section id={id} aria-labelledby={`${id}-title`} className="design-section flex scroll-mt-8 flex-col gap-8 border-t border-border pt-14">
      <header className="flex flex-col gap-3">
        {/* The number is a CSS counter, so the order on the page is the only order there is. */}
        <span aria-hidden className="design-index font-mono text-xs tracking-[0.18em] text-text-3" />
        <h2 id={`${id}-title`} className="design-title font-display text-3xl font-medium text-text sm:text-[44px] sm:leading-[1.05] sm:tracking-[-0.03em]">
          {title}
        </h2>
        <p className="max-w-[var(--measure)] text-base text-text-2">{lede}</p>
      </header>
      {children}
    </section>
  );
}

export function Sub({ title, about, children, className }: { title: string; about?: ReactNode; children: ReactNode; className?: string | undefined }) {
  return (
    <div className={cn("flex flex-col gap-3", className)}>
      <div className="flex flex-col gap-1">
        <h3 className="font-display text-lg font-medium text-text">{title}</h3>
        {about && <p className="max-w-[var(--measure)] text-sm text-text-2">{about}</p>}
      </div>
      {children}
    </div>
  );
}

/** A framed area on the canvas surface, so what is shown sits where it will sit in the product. */
export function Demo({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("flex flex-wrap items-center gap-3 rounded-xl border border-border bg-surface-0 p-6", className)} {...props} />;
}

export function Eyebrow({ children, className }: { children: ReactNode; className?: string | undefined }) {
  return <span className={cn("font-mono text-xs uppercase tracking-[0.18em] text-text-3", className)}>{children}</span>;
}

export function TokenName({ children }: { children: ReactNode }) {
  return <code className="font-mono text-xs text-text-3">{children}</code>;
}

/**
 * The values of custom properties as the document resolves them right now,
 * re-read whenever the theme or motion attribute changes. This is what makes
 * the gate render the tokens rather than describe them: a value shown here is
 * the value a component would get.
 *
 * The effect depends on the *names*, not on the array holding them. A caller
 * that builds its list inline hands over a new array every render, and with
 * the array in the dependency list that is an infinite loop rather than a
 * mistake anybody sees. Found by rendering the page.
 */
export function useTokens(names: readonly string[], target?: RefObject<HTMLElement | null>): Record<string, string> {
  const [values, setValues] = useState<Record<string, string>>({});
  const key = names.join(",");
  useEffect(() => {
    const readAll = () => {
      const el = target?.current ?? document.documentElement;
      const cs = getComputedStyle(el);
      const next: Record<string, string> = {};
      for (const n of names) next[n] = cs.getPropertyValue(n).trim();
      setValues(next);
    };
    readAll();
    const mo = new MutationObserver(readAll);
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme", "data-motion"] });
    return () => mo.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `key` is `names`
  }, [key, target]);
  return values;
}

export interface SwatchProps {
  token: string;
  /** The utility that paints it, e.g. `bg-surface-1` — so the swatch is the utility, not a copy of it. */
  className: string;
  note?: string | undefined;
  value?: string | undefined;
}

export function Swatch({ token, className, note, value }: SwatchProps) {
  return (
    <div className="flex min-w-36 flex-1 flex-col gap-2">
      <div className={cn("h-16 rounded-lg border border-border", className)} />
      <div className="flex flex-col gap-0.5">
        <TokenName>{token}</TokenName>
        {value && <span className="truncate font-mono text-xs text-text-2" title={value}>{value}</span>}
        {note && <span className="text-xs text-text-3">{note}</span>}
      </div>
    </div>
  );
}

export function TokenTable({ rows, values }: { rows: ReadonlyArray<{ token: string; note?: string }>; values: Record<string, string> }) {
  return (
    <table className="w-full border-collapse text-sm">
      <tbody className="[&_tr]:border-t [&_tr]:border-border">
        {rows.map((r) => (
          <tr key={r.token} className="h-[var(--row-h)]">
            <td className="pr-4 align-middle">
              <TokenName>{r.token}</TokenName>
            </td>
            <td className="pr-4 align-middle font-mono text-xs text-text tabular-nums">{values[r.token] ?? "—"}</td>
            <td className="align-middle text-xs text-text-3">{r.note}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
