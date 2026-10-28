import { ArrowLeft } from "lucide-react";
import { Button } from "@/components/button";
import { MotionToggle, ThemeToggle } from "@/components/theme-toggle";
import { Toaster } from "@/components/toast";
import { TooltipProvider } from "@/components/tooltip";
import { useTheme } from "@/lib/theme";
import { THEMES } from "@/lib/theme";
import { A11ySection } from "./a11y";
import { ComponentsSection } from "./components";
import { Foundations } from "./foundations";
import { InventorySection } from "./inventory";
import { MotionSection } from "./motion";
import { ProductSection } from "./product";
import { ScreensSection } from "./screens";
import { ShellSection } from "./shell";
import { SessionSection } from "./session";
import { VocabularySection } from "./vocabulary";
import { Eyebrow } from "./ui";

const nav = [
  { id: "foundations", label: "Foundations" },
  { id: "motion", label: "Motion" },
  { id: "components", label: "Components" },
  { id: "vocabulary", label: "The instrument panel" },
  { id: "product", label: "Product components" },
  { id: "shell", label: "The shell" },
  { id: "screens", label: "The screens" },
  { id: "session", label: "The session layer" },
  { id: "inventory", label: "The inventory" },
  { id: "a11y", label: "Baseline" },
];

/**
 * The gate. A route in the app, not Storybook: the same bundle, the same
 * tokens, the same themes, so what it shows is what ships. It renders every
 * token and every component in all three themes, with the reduced-motion
 * override, and nothing in the product is built until it is signed off.
 */
export function DesignPage() {
  const [theme] = useTheme();
  const about = THEMES.find((t) => t.id === theme)?.about;
  return (
    <TooltipProvider>
      <Toaster>
        <div className="mx-auto flex w-full max-w-[var(--breakpoint-xl)] gap-10 px-6 pb-24 pt-8 lg:px-10">
          <nav aria-label="Sections" className="design-nav sticky top-8 hidden h-fit w-52 shrink-0 flex-col gap-0.5 lg:flex">
            <Button asChild variant="ghost" size="sm" className="mb-4 -ml-2 justify-start">
              <a href="#/"><ArrowLeft aria-hidden size={14} /> Esper</a>
            </Button>
            {nav.map((n) => (
              <a key={n.id} href={`#/design/${n.id}`} onClick={(e) => { e.preventDefault(); document.getElementById(n.id)?.scrollIntoView(); }} className="design-nav-link rounded-md px-2.5 py-1.5 text-sm text-text-2 transition-[background-color,color] duration-[var(--dur-fast)] hover:bg-text/5 hover:text-text">
                {n.label}
              </a>
            ))}
          </nav>

          <main className="design-sections flex min-w-0 flex-1 flex-col gap-16">
            <header className="flex flex-col gap-6">
              <div className="flex flex-col gap-3">
                <span className="inline-flex w-fit items-center rounded-lg border border-border bg-text/[0.04] px-3 py-1.5 font-mono text-xs uppercase tracking-[0.18em] text-text-2">
                  Esper · Design system
                </span>
                <h1 className="design-title mt-3 font-display text-3xl font-medium sm:text-[64px] sm:leading-[1.02] sm:tracking-[-0.035em]">
                  Design
                </h1>
                <p className="max-w-[var(--measure)] text-lg text-text-2">
                  Every token and component Esper is built from, in each theme, with motion on and off. It uses the same code as the app, so what you see here is what ships.
                </p>
              </div>
              <div className="flex flex-wrap items-center gap-x-8 gap-y-3 rounded-xl border border-border bg-surface-1 p-5">
                <div className="flex flex-col gap-1.5">
                  <Eyebrow>Theme</Eyebrow>
                  <ThemeToggle />
                </div>
                <div className="flex flex-col gap-1.5">
                  <Eyebrow>Motion</Eyebrow>
                  <MotionToggle />
                </div>
                <p className="max-w-[var(--measure-short)] text-xs text-text-3 sm:ml-auto">{about}</p>
              </div>
            </header>

            <Foundations />
            <MotionSection />
            <ComponentsSection />
            <VocabularySection />
            <ProductSection />
            <ShellSection />
            <ScreensSection />
            <SessionSection />
            <InventorySection />
            <A11ySection />
          </main>
        </div>
      </Toaster>
    </TooltipProvider>
  );
}
