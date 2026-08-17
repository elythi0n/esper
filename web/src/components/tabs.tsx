import * as RT from "@radix-ui/react-tabs";
import { cn } from "@/lib/cn";

/** Tabs. The active one is the text colour and an underline; the rest are secondary text. */
export const Tabs = RT.Root;
export const TabsContent = RT.Content;

export function TabsList({ className, ...props }: RT.TabsListProps) {
  return (
    <RT.List
      className={cn("flex items-end gap-1 border-b border-border", className)}
      {...props}
    />
  );
}

export function TabsTrigger({ className, ...props }: RT.TabsTriggerProps) {
  return (
    <RT.Trigger
      data-tap-min=""
      className={cn(
        "-mb-px inline-flex h-[var(--control-h)] cursor-pointer items-center gap-2 border-b-2 border-transparent px-3 text-sm text-text-2",
        "transition-[color,border-color] duration-[var(--dur-fast)] ease-[var(--ease)]",
        "hover:text-text data-[state=active]:border-accent data-[state=active]:text-text",
        "disabled:text-text-disabled",
        className,
      )}
      {...props}
    />
  );
}
