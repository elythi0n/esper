import { PanelLeft, Search, type LucideIcon } from "lucide-react";
import {
  useCallback,
  useState,
  type HTMLAttributes,
  type ReactNode,
  type UIEvent,
} from "react";
import { Kbd } from "@/components/kbd";
import { Tooltip } from "@/components/tooltip";
import { cn } from "@/lib/cn";
import { Wordmark } from "./Wordmark";

export { Wordmark };
import "./shell.css";

/*
  The shell every screen outside a session sits in.

  A session is the exception and has its own root (see src/session/chrome.tsx):
  inside one the UI is not the content, so it gets chrome that hides rather
  than chrome that frames.
*/

export interface NavEntry {
  id: string;
  label: string;
  icon: LucideIcon;
  /** Something is happening behind this destination. The signal, on one element. */
  busy?: boolean | undefined;
  /** Whether it earns a tab on the narrowest arrangement. Most do not. */
  compact?: boolean | undefined;
}

export interface NavGroup {
  /** Absent for the first group, which needs no heading to be understood. */
  label?: string | undefined;
  items: readonly NavEntry[];
}

export function NavItem({
  entry,
  active,
  collapsed = false,
  onSelect,
}: {
  entry: NavEntry;
  active: boolean;
  collapsed?: boolean | undefined;
  onSelect: (id: string) => void;
}) {
  const Icon = entry.icon;
  const button = (
    <button
      type="button"
      className="navitem"
      data-compact={entry.compact ?? false}
      aria-current={active ? "page" : undefined}
      aria-label={collapsed ? entry.label : undefined}
      onClick={() => onSelect(entry.id)}
    >
      <span className="navitem-icon">
        <Icon aria-hidden size={17} strokeWidth={1.75} />
      </span>
      <span className="navitem-label">{entry.label}</span>
      {entry.busy && <span className="navitem-dot" aria-label="Active" role="img" />}
    </button>
  );
  // Collapsed, the label is gone and the icon owes one. Every icon has a
  // text label somewhere; on a rail that somewhere is a tooltip.
  return collapsed ? (
    <Tooltip label={entry.label} side="right">
      {button}
    </Tooltip>
  ) : (
    button
  );
}

export interface SidebarProps {
  /** Off when the window's own title bar already carries it. Two is one too many. */
  wordmark?: boolean | undefined;
  groups: readonly NavGroup[];
  active: string;
  onSelect: (id: string) => void;
  collapsed: boolean;
  onCollapsedChange: (collapsed: boolean) => void;
  /** Shown in the foot, beside the collapse control. */
  version?: string | undefined;
  foot?: ReactNode;
}

export function Sidebar({
  wordmark = true,
  groups,
  active,
  onSelect,
  collapsed,
  onCollapsedChange,
  version,
  foot,
}: SidebarProps) {
  return (
    <div
      className="sidebar"
      data-collapsed={collapsed}
      // Collapsed, the empty rail is itself the target that opens it back up.
      onClick={collapsed ? () => onCollapsedChange(false) : undefined}
    >
      <div className="sidebar-brand">
        {wordmark && <Wordmark className="sidebar-wordmark" show={!collapsed} />}
        <button
          type="button"
          className="sidebar-collapse inline-flex size-6 items-center justify-center rounded-sm text-text-3 transition-colors duration-[var(--dur-fast)] hover:text-text"
          data-tap-min=""
          aria-label={collapsed ? "Expand the sidebar" : "Collapse the sidebar"}
          aria-pressed={collapsed}
          onClick={(e) => {
            e.stopPropagation();
            onCollapsedChange(!collapsed);
          }}
        >
          <PanelLeft aria-hidden size={16} strokeWidth={1.75} />
        </button>
      </div>

      <nav className="sidebar-nav" aria-label="Screens">
        {groups.map((g, i) => (
          <div className="sidebar-group" key={g.label ?? `group-${i}`}>
            {g.label && <p className="sidebar-group-label">{g.label}</p>}
            {g.items.map((entry) => (
              <NavItem
                key={entry.id}
                entry={entry}
                active={entry.id === active}
                collapsed={collapsed}
                onSelect={onSelect}
              />
            ))}
          </div>
        ))}
      </nav>

      <div className="sidebar-foot">
        {foot}
        {/* This window's own version. The engine's is in the status bar, said
            as the engine's, because the two are different programs. */}
        {version && (
          <span className="sidebar-version font-mono" title={`This window is version ${version}.`}>
            {version}
          </span>
        )}
      </div>
    </div>
  );
}

export interface TopbarProps {
  title: ReactNode;
  subtitle?: ReactNode;
  actions?: ReactNode;
  /** Opens the command palette. Omitted where a screen has no search. */
  onSearch?: (() => void) | undefined;
}

export function Topbar({ title, subtitle, actions, onSearch }: TopbarProps) {
  return (
    <header className="topbar">
      <div className="topbar-heading">
        <h1 className="page-title">{title}</h1>
        {subtitle && <p className="page-subtitle">{subtitle}</p>}
      </div>
      {onSearch && (
        <button
          type="button"
          onClick={onSearch}
          className={cn(
            "inline-flex h-[var(--control-h)] flex-shrink-0 items-center gap-2 rounded-sm border border-border bg-surface-0 px-2 text-sm text-text-3",
            "transition-[color,border-color] duration-[var(--dur-fast)] ease-[var(--ease)] hover:border-border-strong hover:text-text",
          )}
          aria-label="Search, or run a command"
        >
          <Search aria-hidden size={14} />
          <span className="topbar-search-label">Search</span>
          <Kbd keys="⌘K" className="topbar-search-kbd" />
        </button>
      )}
      {actions && <div className="page-actions">{actions}</div>}
    </header>
  );
}

export interface PageBodyProps extends HTMLAttributes<HTMLElement> {
  /** For a screen that is one table: the body stops scrolling and the child owns the space. */
  fill?: boolean | undefined;
}

/**
 * The page's scroll container, and the one thing it tells the chrome above it.
 *
 * `data-scrolled` goes on while there is content above the fold, and the
 * topbar draws a boundary when it is set. The bar does not move and never
 * hides — what somebody reported was the bar *appearing* to go missing as
 * content slid under it, and a bar with the same hairline whether or not
 * anything is behind it gives the eye nothing to read the difference from.
 *
 * The attribute is written straight to the node rather than held in state: a
 * scroll handler that re-renders the whole screen is a scroll handler that
 * drops frames, and nothing in React needs to know this.
 */
export function PageBody({ fill = false, className, onScroll, ...props }: PageBodyProps) {
  const mark = useCallback((e: UIEvent<HTMLElement>) => {
    const el = e.currentTarget;
    const past = el.scrollTop > 0;
    if ((el.dataset.scrolled === "true") !== past) el.dataset.scrolled = String(past);
    onScroll?.(e);
  }, [onScroll]);
  return (
    <main className={cn("page-body", className)} data-fill={fill} onScroll={mark} {...props} />
  );
}

export function StatusBar({
  children,
  className,
  ...props
}: HTMLAttributes<HTMLDivElement>) {
  return (
    <footer className={cn("statusbar", className)} {...props}>
      {children}
    </footer>
  );
}

export function StatusSpacer() {
  return <span className="statusbar-spacer" />;
}

const COLLAPSE_KEY = "esper.sidebar";

/** Remembered across restarts, because a rail somebody chose should stay chosen. */
function readCollapsed(): boolean {
  try {
    return localStorage.getItem(COLLAPSE_KEY) === "collapsed";
  } catch {
    return false;
  }
}

export function useSidebarCollapsed(): [boolean, (next: boolean) => void] {
  const [collapsed, set] = useState(readCollapsed);
  const change = useCallback((next: boolean) => {
    set(next);
    try {
      localStorage.setItem(COLLAPSE_KEY, next ? "collapsed" : "open");
    } catch {
      // Storage may be unavailable. The rail still works for this window.
    }
  }, []);
  return [collapsed, change];
}

export interface AppShellProps {
  /** Off when the window's own title bar already carries it. */
  wordmark?: boolean | undefined;
  groups: readonly NavGroup[];
  active: string;
  onSelect: (id: string) => void;
  children: ReactNode;
  version?: string | undefined;
  sidebarFoot?: ReactNode;
  status?: ReactNode;
  /** A condition of the machine, banded across the top of everything. */
  alarm?: ReactNode;
  /** Controlled collapse, for a shell whose state something else owns. */
  collapsed?: boolean | undefined;
  onCollapsedChange?: ((collapsed: boolean) => void) | undefined;
  className?: string | undefined;
}

export function AppShell({
  wordmark,
  groups,
  active,
  onSelect,
  children,
  version,
  sidebarFoot,
  status,
  alarm,
  collapsed,
  onCollapsedChange,
  className,
}: AppShellProps) {
  const [ownCollapsed, setOwnCollapsed] = useSidebarCollapsed();
  const isCollapsed = collapsed ?? ownCollapsed;
  const setCollapsed = onCollapsedChange ?? setOwnCollapsed;
  return (
    <div className={cn("shell-frame", className)}>
      {alarm}
      <div className="shell">
        <Sidebar
          wordmark={wordmark ?? true}
          groups={groups}
          active={active}
          onSelect={onSelect}
          collapsed={isCollapsed}
          onCollapsedChange={setCollapsed}
          version={version}
          foot={sidebarFoot}
        />
        <div className="shell-main">
          {children}
          {status}
        </div>
      </div>
    </div>
  );
}
