import { Check, Clock, X } from "lucide-react";
import { NOT_BUILDING, TIER_1, TIER_2 } from "@/components/inventory";
import { Section, Sub } from "./ui";

/**
 * The three lists. Read from the same module the check script reads, so what
 * this page shows is what the build was held to — a Tier 1 entry naming a
 * component that no longer exists fails `pnpm check:inventory` before it can
 * be rendered here as if it did.
 */
export function InventorySection() {
  return (
    <Section
      id="inventory"
      title="The inventory"
      lede="What is built, what is planned, and what is deliberately left out."
    >
      <Sub
        title={`Tier 1: ${TIER_1.length} components`}
        about="Each one is a real component, and the build fails if it disappears."
      >
        {/* Its own scroller. Three columns of prose have a min-content width
            the page cannot go under, and without this the *page* scrolled
            sideways at phone width rather than the table. */}
        <div className="w-full overflow-x-auto">
        <table className="w-full border-collapse text-sm">
          <thead className="text-left text-xs text-text-3">
            <tr className="h-[var(--row-h)]">
              <th className="pr-4 font-medium">Component</th>
              <th className="pr-4 font-medium">What it is for</th>
              <th className="font-medium">Where</th>
            </tr>
          </thead>
          <tbody className="[&_tr]:border-t [&_tr]:border-border">
            {TIER_1.map((e) => (
              <tr key={`${e.from}:${e.name}`} className="h-[var(--row-h)]">
                <td className="whitespace-nowrap py-1.5 pr-4">
                  <Check aria-hidden size={12} className="mr-1.5 inline text-ok" />
                  <span className="font-medium text-text">{e.name}</span>
                </td>
                <td className="py-1.5 pr-4 text-text-2">{e.what}</td>
                <td className="py-1.5 font-mono text-xs text-text-3">{e.from}</td>
              </tr>
            ))}
          </tbody>
        </table>
        </div>
      </Sub>

      <div className="grid gap-8 lg:grid-cols-2">
        <Sub
          title={`Tier 2: ${TIER_2.length} coming`}
          about="Planned, each with the screen that will need it first."
        >
          <ul className="flex flex-col divide-y divide-border">
            {TIER_2.map((e) => (
              <li key={e.name} className="flex gap-3 py-2.5">
                <Clock aria-hidden size={14} className="mt-0.5 shrink-0 text-text-3" />
                <div className="min-w-0">
                  <div className="flex flex-wrap items-baseline gap-2">
                    <span className="font-medium text-text">{e.name}</span>
                    <span className="font-mono text-xs uppercase tracking-[0.12em] text-text-3">{e.when}</span>
                  </div>
                  <p className="text-sm text-text-2">{e.what}</p>
                </div>
              </li>
            ))}
          </ul>
        </Sub>

        <Sub
          title={`Not building: ${NOT_BUILDING.length}`}
          about="Left out on purpose, with the reason."
        >
          <ul className="flex flex-col divide-y divide-border">
            {NOT_BUILDING.map((e) => (
              <li key={e.name} className="flex gap-3 py-2.5">
                <X aria-hidden size={14} className="mt-0.5 shrink-0 text-text-3" />
                <div className="min-w-0">
                  <span className="font-medium text-text">{e.name}</span>
                  <p className="text-sm text-text-2">{e.why}</p>
                </div>
              </li>
            ))}
          </ul>
        </Sub>
      </div>
    </Section>
  );
}
