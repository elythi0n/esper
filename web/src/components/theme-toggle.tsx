import { Contrast, Monitor, Moon, Sun } from "lucide-react";
import { cn } from "@/lib/cn";
import { THEMES, useMotion, useTheme, type Motion, type Theme } from "@/lib/theme";

const icon: Record<Theme, typeof Sun> = { auto: Monitor, dark: Moon, light: Sun, hc: Contrast };

/**
 * A segmented control, the one family every exclusive choice in the product
 * uses: the selected segment is the inverse fill, the rest are secondary text.
 */
export function Segmented<T extends string>({
  value,
  onChange,
  options,
  label,
  className,
}: {
  value: T;
  onChange: (v: T) => void;
  options: ReadonlyArray<{ id: T; label: string; icon?: typeof Sun }>;
  label: string;
  className?: string | undefined;
}) {
  return (
    <div role="radiogroup" aria-label={label} className={cn("inline-flex rounded-sm border border-border-strong bg-surface-0 p-0.5", className)}>
      {options.map((o) => {
        const Icon = o.icon;
        const on = o.id === value;
        return (
          <button
            key={o.id}
            type="button"
            role="radio"
            aria-checked={on}
            data-tap-min=""
            onClick={() => onChange(o.id)}
            className={cn(
              "inline-flex h-6 cursor-pointer items-center gap-1.5 rounded-[4px] px-2 text-xs",
              "transition-[background-color,color] duration-[var(--dur-fast)] ease-[var(--ease)]",
              on ? "bg-accent text-accent-fg" : "text-text-2 hover:text-text",
            )}
          >
            {Icon && <Icon aria-hidden size={12} />}
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

export function ThemeToggle({ className }: { className?: string }) {
  const [theme, setTheme] = useTheme();
  return (
    <Segmented
      label="Theme"
      value={theme}
      onChange={setTheme}
      options={THEMES.map((t) => ({ id: t.id, label: t.label, icon: icon[t.id] }))}
      className={className}
    />
  );
}

const motions: ReadonlyArray<{ id: Motion; label: string }> = [
  { id: "system", label: "System" },
  { id: "reduced", label: "Reduced" },
  { id: "full", label: "Full" },
];

/**
 * The reduced-motion override. Not a preference: it exists so every animation
 * on /design can be re-run with motion off and the fallback confirmed.
 */
export function MotionToggle({ className }: { className?: string }) {
  const [motion, setMotion] = useMotion();
  return <Segmented label="Motion" value={motion} onChange={setMotion} options={motions} className={className} />;
}
