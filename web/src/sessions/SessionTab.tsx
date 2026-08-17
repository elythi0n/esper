import type { IDockviewPanelHeaderProps } from "dockview-react";
import { X } from "lucide-react";
import { useEffect, useState } from "react";
import { kindOf } from "@/components/device-row";
import { cn } from "@/lib/cn";

/**
 * One tab in the session strip.
 *
 * Our own chrome rather than dockview's default, so the glyph, the close
 * affordance and the middle-click all behave like the rest of the product.
 * dockview still owns the container: active styling, the drag to reorder,
 * and the drag out to split or float.
 */
export function SessionTab(props: IDockviewPanelHeaderProps<{ name: string }>) {
  const { api, params } = props;
  const [active, setActive] = useState(api.isActive);
  useEffect(() => {
    const sub = api.onDidActiveChange((e) => setActive(e.isActive));
    return () => sub.dispose();
  }, [api]);

  const name = params.name || api.title || "session";
  const Icon = kindOf(name);

  return (
    <span
      className="flex h-full min-w-0 items-center gap-2 px-3"
      onMouseDown={(e) => {
        // Middle-click closes, as every tab strip has since browsers had tabs.
        if (e.button === 1) {
          e.preventDefault();
          api.close();
        }
      }}
    >
      <Icon aria-hidden size={13} strokeWidth={1.75} className={cn("shrink-0", active ? "text-text" : "text-text-3")} />
      <span className="min-w-0 truncate">{name}</span>
      <button
        type="button"
        aria-label={`Close ${name}`}
        onClick={(e) => {
          e.stopPropagation();
          api.close();
        }}
        className="-mr-1 shrink-0 rounded-sm p-0.5 text-text-3 transition-colors duration-[var(--dur-fast)] hover:bg-surface-2 hover:text-text"
      >
        <X aria-hidden size={12} strokeWidth={2} />
      </button>
    </span>
  );
}
