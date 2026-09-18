import type { PanelName } from "../store";
import { Icon, type IconName } from "./icons";

/**
 * What a panel docked in the right sidebar needs to draw the Session / Filer
 * switcher in place of the single title it draws on its own.
 */
export interface PanelTabsProps {
  /** The panel whose tab is shown. */
  active: PanelName;
  /** Panels the View menu currently offers; a hidden one gets no tab. */
  available: readonly PanelName[];
  onSelect: (panel: PanelName) => void;
}

interface Tab {
  id: Extract<PanelName, "sessions" | "filer">;
  label: string;
  icon: IconName;
}

/** Fixed order, so the two tabs never swap places as the choice changes. */
const TABS: Tab[] = [
  { id: "sessions", label: "Session", icon: "server" },
  { id: "filer", label: "Filer", icon: "folder" },
];

/**
 * The Session / Filer switcher heading the right sidebar. Only the chosen
 * panel is mounted, so the one that carries a badge (`local` / `ssh` / `sftp`
 * on the Filer) hands it down; the tab keeps its place either way, and the
 * trailing spacer keeps the panel's own actions at the far end of the row.
 */
export function PanelTabs({
  active,
  available,
  onSelect,
  filerBadge,
}: PanelTabsProps & { filerBadge?: string }) {
  const shown = TABS.filter((tab) => available.includes(tab.id));
  if (shown.length === 0) return null;
  return (
    <>
      {shown.map((tab) => {
        const current = tab.id === active;
        return (
          <button
            key={tab.id}
            type="button"
            role="tab"
            aria-selected={current}
            className={`panel-tab is-${tab.id}${current ? " is-active" : ""}`}
            onClick={() => onSelect(tab.id)}
          >
            <Icon name={tab.icon} />
            {tab.label}
            {tab.id === "filer" && filerBadge && (
              <span className="panel-badge">{filerBadge}</span>
            )}
          </button>
        );
      })}
      <span className="panel-tabs-spacer" />
    </>
  );
}
