import { CircleAlert, CircleCheck, Info, TriangleAlert } from "lucide-react";
import type { HTMLAttributes, ReactNode } from "react";
import { cn } from "@/lib/cn";

export type CalloutTone = "info" | "ok" | "warn" | "danger";

const tone: Record<CalloutTone, { box: string; Icon: typeof Info }> = {
  info: { box: "bg-info-soft text-info", Icon: Info },
  ok: { box: "bg-ok-soft text-ok", Icon: CircleCheck },
  warn: { box: "bg-warn-soft text-warn", Icon: TriangleAlert },
  danger: { box: "bg-danger-soft text-danger", Icon: CircleAlert },
};

export interface CalloutProps extends Omit<HTMLAttributes<HTMLDivElement>, "title"> {
  tone?: CalloutTone;
  title?: ReactNode;
}

/**
 * A short message with a tone. The tone's text on the tone's own soft fill —
 * a pairing the contrast gate holds to AA on every surface. In high contrast
 * the fill is transparent and `--soft-border-width` draws the edge instead,
 * because a 10% wash carries nothing at that bar.
 *
 * Content rules: say what to do, not what went wrong internally. No
 * exclamation marks. No "Oops".
 */
export function Callout({ tone: t = "info", title, className, children, ...props }: CalloutProps) {
  const { box, Icon } = tone[t];
  return (
    <div
      role={t === "danger" || t === "warn" ? "alert" : "status"}
      className={cn(
        "flex gap-3 rounded-md p-3 text-sm [border-style:solid] [border-width:var(--soft-border-width)] [border-color:currentColor]",
        box,
        className,
      )}
      {...props}
    >
      <Icon aria-hidden size={16} strokeWidth={2} className="mt-0.5 shrink-0" />
      <div className="min-w-0">
        {title && <div className="font-medium">{title}</div>}
        <div className={cn(title ? "mt-0.5" : undefined)}>{children}</div>
      </div>
    </div>
  );
}
