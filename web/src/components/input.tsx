import type { InputHTMLAttributes, LabelHTMLAttributes, ReactNode, TextareaHTMLAttributes } from "react";
import { useId } from "react";
import { cn } from "@/lib/cn";

/**
 * A control's boundary is `--border-strong`, never the hairline: it is how the
 * control is found, so it is held to 3:1 on every surface by the gate.
 */
const control =
  "w-full rounded-sm border border-border-strong bg-surface-0 px-2 text-sm text-text " +
  "placeholder:text-text-3 " +
  "transition-[border-color,background-color] duration-[var(--dur-fast)] ease-[var(--ease)] " +
  "disabled:text-text-disabled disabled:border-border disabled:bg-surface-1 " +
  "aria-[invalid=true]:border-danger";

export function Input({ className, ...props }: InputHTMLAttributes<HTMLInputElement>) {
  return <input className={cn(control, "h-[var(--control-h)]", className)} {...props} />;
}

export function Textarea({ className, ...props }: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return <textarea className={cn(control, "min-h-20 py-1.5", className)} {...props} />;
}

export function Label({ className, ...props }: LabelHTMLAttributes<HTMLLabelElement>) {
  return <label className={cn("text-sm font-medium text-text", className)} {...props} />;
}

export interface FieldProps {
  label: ReactNode;
  /** A sentence under the control: what to type, or what went wrong and what to do. */
  hint?: ReactNode;
  error?: ReactNode;
  children: (id: string, describedBy: string | undefined) => ReactNode;
  className?: string;
}

/** A label, a control and one line of help, wired together with ids. */
export function Field({ label, hint, error, children, className }: FieldProps) {
  const id = useId();
  const helpId = hint || error ? `${id}-help` : undefined;
  return (
    <div className={cn("flex flex-col gap-1.5", className)}>
      <Label htmlFor={id}>{label}</Label>
      {children(id, helpId)}
      {(error || hint) && (
        <p id={helpId} className={cn("text-xs", error ? "text-danger" : "text-text-3")}>
          {error ?? hint}
        </p>
      )}
    </div>
  );
}
