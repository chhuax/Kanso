import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type MouseEvent as ReactMouseEvent,
} from "react";

import { openLocalShell, openSession } from "../actions";
import { byName, effectiveGroupId, sortedGroups } from "../sessionGroups";
import { tabTitle, useStore, type Tab } from "../store";
import {
  colorForSession,
  type SessionKind,
  type SessionProfile,
} from "../types";

import { ContextMenu, type MenuItem } from "./ContextMenu";
import { Icon, type IconName } from "./icons";
import { useAccelerator } from "./TerminalPane";

/** The icon a session kind is listed with. */
const KIND_ICONS: Record<SessionKind, IconName> = {
  local: "terminal",
  ssh: "server",
  sftp: "folder",
};

interface Props {
  /** Opens the New Session dialog: the rail's own way to a host by hand. */
  onNewSession: () => void;
  /** Brings the Session panel forward, where a saved session is edited. */
  onManageSessions: () => void;
}

/**
 * What a row's activity treatment is reporting, or null while there is
 * nothing to report. An agentic CLI is named for what it is: its session
 * lasts as long as the user keeps it open, so only the assistant's own turns
 * are worth a running or a finished row.
 */
function activityLabel(tab: Tab): string | null {
  if (tab.commandActivity === "idle") return null;
  const subject = tab.activityKind === "ai" ? "AI" : "command";
  return `${subject} ${tab.commandActivity === "running" ? "running" : "finished"}`;
}

/**
 * What a row is named. A local shell is named for *where* it is rather than
 * what it is — the icon in front of it already says it is a shell, and one
 * directory's shell is what tells it from the next. Everything else is named
 * for its session: its name is what says which host it reaches.
 */
function rowTitle(tab: Tab): string {
  return tab.info.kind === "local" && tab.cwd ? tab.cwd : tabTitle(tab);
}

/**
 * The line under a row's name: the branch its directory is on, and nothing at
 * all outside a repository — a directory repeated under itself would be the
 * same fact twice.
 */
function rowMeta(tab: Tab): string | null {
  return tab.info.kind === "local" ? (tab.branch ?? null) : null;
}




/**
 * The session list, docked down the left edge: one row per open tab, in pane
 * order and then in the order each pane shows them. A row is a normal
 * horizontal line — icon, number, name — rather than a rotated tab, so the
 * label stays readable; the pane a row belongs to is only called out when
 * there is more than one.
 */
export function TabRail({ onNewSession, onManageSessions }: Props) {
  const allTabs = useStore((s) => s.tabs);
  const panes = useStore((s) => s.panes);
  const profiles = useStore((s) => s.profiles);
  const groups = useStore((s) => s.groups);
  const activeId = useStore((s) => s.activeId);
  const activePaneId = useStore((s) => s.activePaneId);
  const setActive = useStore((s) => s.setActive);
  const setActivePane = useStore((s) => s.setActivePane);
  const requestCloseTabs = useStore((s) => s.requestCloseTabs);
  const closeKey = useAccelerator("closeSession");
  const newShellKey = useAccelerator("newLocalShell");
  const newSessionKey = useAccelerator("newSession");
  const railRef = useRef<HTMLDivElement>(null);

  // One block per pane, in reading order; a pane with no tabs has no rows.
  const blocks = useMemo(
    () =>
      panes
        .map((pane) => ({
          pane,
          tabs: allTabs.filter((tab) => tab.paneId === pane.id),
        }))
        .filter((block) => block.tabs.length > 0),
    [allTabs, panes],
  );

  // A row's own menu: the close commands work along its pane, the way VS
  // Code's "Close to the Right" stays inside its editor group.
  const [rowMenu, setRowMenu] = useState<{
    x: number;
    y: number;
    id: string;
  } | null>(null);
  const closeRowMenu = useCallback(() => setRowMenu(null), []);

  // The rail's own menu, opened from the `+` or from a right-click on the
  // chrome behind the rows. It holds no tab: just where it floats.
  const [railMenu, setRailMenu] = useState<{ x: number; y: number } | null>(
    null,
  );
  const closeRailMenu = useCallback(() => setRailMenu(null), []);

  const onContextMenu = (event: ReactMouseEvent<HTMLDivElement>) => {
    const id = (event.target as Element).closest<HTMLElement>(".tab-row")
      ?.dataset.tabId;
    event.preventDefault();
    // A row closes and splits; everything behind the rows — the blank space, a
    // pane's header, the bar above them — asks for a new session, the way
    // Warp's panel does from its own chrome.
    if (id) setRowMenu({ x: event.clientX, y: event.clientY, id });
    else setRailMenu({ x: event.clientX, y: event.clientY });
  };

  /**
   * A saved session's entry. `inset` is for the ones listed at the top level,
   * where the menu's other entries are commands: the inset is what says a
   * session is the menu's content rather than a command of its own. Inside a
   * group's submenu there is nothing to tell apart.
   */
  const profileEntry = (profile: SessionProfile, inset = false): MenuItem => ({
    label: profile.name,
    icon: KIND_ICONS[profile.kind] ?? "terminal",
    ...(inset ? { indent: 1 } : {}),
    action: () => void openSession(profile),
  });

  /**
   * What the rail offers for a new session, Warp's "tab configs" menu: the two
   * commands that make a session, the saved ones between them, then the way
   * into managing them. The saved part is read from the same store the Session
   * panel lists and shaped the same way — a group is a submenu, and the
   * sessions in no group follow under their own heading — so a session added,
   * renamed or moved there is here with nothing to keep in step. A group keeps
   * the menu short no matter how many sessions are in it.
   */
  const sessionMenu = (): MenuItem[] => {
    const byGroup = new Map<string | null, SessionProfile[]>();
    for (const profile of profiles) {
      // The built-in Local Shell is the first entry, not a saved session.
      if (!profile.id) continue;
      const groupId = effectiveGroupId(groups, profile);
      byGroup.set(groupId, [...(byGroup.get(groupId) ?? []), profile]);
    }
    for (const members of byGroup.values()) members.sort(byName);

    const saved: MenuItem[] = [];
    for (const group of sortedGroups(groups)) {
      const members = byGroup.get(group.id) ?? [];
      // An empty group is left out: this is a launcher, and a group with
      // nothing in it is the panel's business rather than the menu's.
      if (members.length === 0) continue;
      saved.push({
        label: group.name,
        icon: "folder",
        children: members.map((profile) => profileEntry(profile)),
      });
    }
    const loose = byGroup.get(null) ?? [];
    if (loose.length > 0) {
      // Named only where it contrasts with groups above it; on its own the
      // inset already says these sit one level in.
      if (saved.length > 0) saved.push({ heading: "Ungrouped" });
      for (const profile of loose) saved.push(profileEntry(profile, true));
    }

    return [
      {
        label: "New Local Shell",
        icon: "terminal",
        shortcut: newShellKey,
        action: () => void openLocalShell(activePaneId),
      },
      {
        label: "New Session…",
        icon: "add",
        shortcut: newSessionKey,
        action: onNewSession,
      },
      ...(saved.length > 0 ? (["separator", ...saved] as MenuItem[]) : []),
      "separator",
      {
        label: "Manage Sessions…",
        icon: "list-selection",
        action: onManageSessions,
      },
    ];
  };

  const rowMenuItems = (id: string): MenuItem[] => {
    const tab = allTabs.find((item) => item.info.id === id);
    const ids = allTabs
      .filter((item) => item.paneId === tab?.paneId)
      .map((item) => item.info.id);
    const index = ids.indexOf(id);
    return [
      { label: "Close", shortcut: closeKey, action: () => requestCloseTabs([id]) },
      {
        label: "Close Others",
        disabled: ids.length < 2,
        action: () => requestCloseTabs(ids.filter((other) => other !== id)),
      },
      {
        label: "Close to the Left",
        disabled: index <= 0,
        action: () => requestCloseTabs(ids.slice(0, index)),
      },
      {
        label: "Close to the Right",
        disabled: index >= ids.length - 1,
        action: () => requestCloseTabs(ids.slice(index + 1)),
      },
      { label: "Close All", action: () => requestCloseTabs(ids) },
    ];
  };

  // Keep the active row visible when it changes via click, shortcut or a new
  // session being opened past the bottom edge.
  useEffect(() => {
    railRef.current
      ?.querySelector<HTMLElement>(".tab-row.is-active")
      ?.scrollIntoView({ block: "nearest" });
  }, [activeId]);

  /**
   * A double-click on the rail's blank part opens a local shell — one gesture
   * to a plain terminal, and the same thing the Session panel's Local Shell
   * row used to do. Rows and the rail's own controls keep their behaviour.
   */
  const onDoubleClick = (event: ReactMouseEvent<HTMLDivElement>) => {
    const target = event.target as Element;
    if (target.closest(".tab-row") || target.closest("button")) return;
    void openLocalShell(activePaneId);
  };

  return (
    // The `+` sits in a bar above the rows and stays put while they scroll —
    // Warp's control bar, which is where its new-tab button lives. The bar
    // carries the rail's own gestures too, so a right-click or a double-click
    // anywhere on the chrome means the same thing.
    <div
      className="tab-rail-panel"
      onContextMenu={onContextMenu}
      onDoubleClick={onDoubleClick}
    >
      <div className="tab-rail-bar">
        <button
          type="button"
          className="tab-rail-add"
          onClick={(event) => {
            const box = event.currentTarget.getBoundingClientRect();
            setRailMenu({ x: box.left, y: box.bottom + 4 });
          }}
          title="New session"
          aria-label="New session"
        >
          <Icon name="add" />
        </button>
      </div>

      <div
        className="tab-rail"
        ref={railRef}
      >
        {blocks.map((block, blockIndex) => (
          <div className="tab-rail-block" key={block.pane.id}>
            {blocks.length > 1 && (
              <button
                type="button"
                className={`tab-rail-pane${
                  block.pane.id === activePaneId ? " is-active" : ""
                }`}
                onMouseDown={(event) => {
                  event.stopPropagation();
                  setActivePane(block.pane.id);
                }}
                title={`Pane ${blockIndex + 1}`}
              >
                Pane {blockIndex + 1}
              </button>
            )}
            {block.tabs.map((tab) => {
              const active = tab.info.id === activeId;
              const sessionColor =
                tab.info.color ??
                colorForSession(tab.info.profileId ?? tab.info.name);
              const meta = rowMeta(tab);
              return (
                <div
                  key={tab.info.id}
                  className={[
                    "tab-row",
                    active ? "is-active" : "",
                    `is-${tab.state}`,
                    `is-command-${tab.commandActivity}`,
                  ]
                    .filter(Boolean)
                    .join(" ")}
                  style={
                    {
                      "--session-color": sessionColor,
                      ...(tab.aiTool ? { "--agent-color": tab.aiTool.color } : {}),
                    } as CSSProperties
                  }
                  data-tab-id={tab.info.id}
                  data-pane-id={tab.paneId}
                  onMouseDown={() => setActive(tab.info.id)}
                  title={[
                    `${tab.info.protocol} · ${tab.info.address}`,
                    // The mark says which tool; this names it in words.
                    tab.aiTool?.label,
                    tab.branch,
                    tab.commandActivity === "running"
                      ? activityLabel(tab)
                      : tab.commandActivity === "complete"
                        ? `${activityLabel(tab)} — select to view`
                        : (tab.message ?? tab.state),
                  ]
                    .filter(Boolean)
                    .join(" · ")}
                >
                  <span
                    className={`tab-row-icon${tab.aiTool ? " is-agent" : ""}`}
                  >
                    <Icon
                      name={
                        tab.aiTool
                          ? (tab.aiTool.icon ?? "sparkle")
                          : (KIND_ICONS[tab.info.kind] ?? "terminal")
                      }
                    />
                  </span>
                  <div className="tab-row-text">
                    <div className="tab-row-title">
                      <span className="tab-label">{rowTitle(tab)}</span>
                    </div>
                    {meta && <div className="tab-row-meta">{meta}</div>}
                  </div>
                  <button
                    className="tab-close"
                    onMouseDown={(event) => {
                      event.stopPropagation();
                      requestCloseTabs([tab.info.id]);
                    }}
                    title="Close session"
                    aria-label="Close session"
                  >
                    <Icon name="close" />
                  </button>
                </div>
              );
            })}
          </div>
        ))}

        {rowMenu && (
          <ContextMenu
            x={rowMenu.x}
            y={rowMenu.y}
            items={rowMenuItems(rowMenu.id)}
            onClose={closeRowMenu}
          />
        )}

        {railMenu && (
          <ContextMenu
            x={railMenu.x}
            y={railMenu.y}
            items={sessionMenu()}
            onClose={closeRailMenu}
            className="menu-scroll"
          />
        )}
      </div>
    </div>
  );
}
