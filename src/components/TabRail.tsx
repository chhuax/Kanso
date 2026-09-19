import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type MouseEvent as ReactMouseEvent,
} from "react";

import { openLocalShell, openSession, splitSession } from "../actions";
import { byName, effectiveGroupId, sortedGroups } from "../sessionGroups";
import { tabTitle, useStore, type DropTarget, type Tab } from "../store";
import {
  colorForSession,
  type SessionKind,
  type SessionProfile,
} from "../types";
import { ContextMenu, type MenuItem } from "./ContextMenu";
import { Icon, type IconName } from "./icons";
import { useAccelerator } from "./TerminalPane";

/** Pointer travel before a press on a row turns into a drag. */
const DRAG_THRESHOLD = 4;
/**
 * How far in from a pane's edge, as a share of its size, a dropped row splits
 * the pane on that side rather than joining it.
 */
const SPLIT_ZONE = 0.25;

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

interface RailDrag {
  id: string;
  paneId: string;
  pointerId: number;
  /** Pointer position at pointerdown, to tell a click from a drag. */
  startX: number;
  startY: number;
  x: number;
  y: number;
  /** Set once the pointer has travelled past the threshold. */
  active: boolean;
  /**
   * Stands in for the row once the pointer has left the rail, where the row
   * itself cannot follow; null while the drag is along the rail.
   */
  ghost: HTMLElement | null;
}

const showGhost = (state: RailDrag, label: string) => {
  if (!state.ghost) {
    const ghost = document.createElement("div");
    ghost.className = "tab-drag-ghost";
    ghost.textContent = label;
    document.body.appendChild(ghost);
    state.ghost = ghost;
  }
  state.ghost.style.transform = `translate(${state.x + 14}px, ${state.y + 14}px)`;
};

const hideGhost = (state: RailDrag) => {
  state.ghost?.remove();
  state.ghost = null;
};

/**
 * The slot in `paneId`'s block of the rail a row at `y` would take: after
 * every row of that pane whose middle it has passed. Only the tab's own pane
 * is scanned — the rail's panes are fixed, and moving a tab to another one is
 * what dropping it on that pane does.
 */
const slotIndex = (rail: HTMLElement, paneId: string, y: number): number => {
  let index = 0;
  for (const row of rail.querySelectorAll<HTMLElement>(
    `.tab-row[data-pane-id="${CSS.escape(paneId)}"]`,
  )) {
    const rect = row.getBoundingClientRect();
    if (y > rect.top + rect.height / 2) index += 1;
  }
  return index;
};

/**
 * What a row dragged from `ownPaneId` would do if released at (x, y): join
 * another pane or split the pane under the pointer on the side nearest the
 * pointer. Null over the rail itself, where the drag reorders live, and
 * anywhere that takes no tab.
 *
 * `elementsFromPoint` looks through the session covering a pane, since the
 * session is placed over the pane rather than inside it (see Workspace).
 */
const dropTargetAt = (
  x: number,
  y: number,
  ownPaneId: string,
): DropTarget | null => {
  for (const element of document.elementsFromPoint(x, y)) {
    const rail = element.closest<HTMLElement>(".tab-rail-panel");
    if (rail) return null;
    const stack = element.closest<HTMLElement>(".pane-stack");
    if (stack) {
      const paneId = stack.dataset.paneId;
      if (!paneId) return null;
      const rect = stack.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) return null;
      const px = (x - rect.left) / rect.width;
      const py = (y - rect.top) / rect.height;
      const nearest = Math.min(px, 1 - px, py, 1 - py);
      if (nearest > SPLIT_ZONE) {
        return paneId === ownPaneId ? null : { paneId, zone: "center" };
      }
      const zone =
        nearest === px
          ? "left"
          : nearest === 1 - px
            ? "right"
            : nearest === py
              ? "up"
              : "down";
      return { paneId, zone };
    }
  }
  return null;
};

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
  const draggingTabId = useStore((s) => s.draggingTabId);
  const setActive = useStore((s) => s.setActive);
  const setActivePane = useStore((s) => s.setActivePane);
  const requestCloseTabs = useStore((s) => s.requestCloseTabs);
  const moveTab = useStore((s) => s.moveTab);
  const moveTabToPane = useStore((s) => s.moveTabToPane);
  const splitPane = useStore((s) => s.splitPane);
  const setTabDrag = useStore((s) => s.setTabDrag);
  const closeKey = useAccelerator("closeSession");
  const splitRightKey = useAccelerator("splitRight");
  const splitDownKey = useAccelerator("splitDown");
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
      "separator",
      {
        label: "Split Right",
        icon: "split-horizontal",
        shortcut: splitRightKey,
        action: () => void splitSession(id, "right"),
      },
      {
        label: "Split Down",
        icon: "split-vertical",
        shortcut: splitDownKey,
        action: () => void splitSession(id, "down"),
      },
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

  // Drag to reorder within a pane, or onto a pane to join or split it.
  // Pointer events rather than HTML5 drag and drop: Tauri's drag-drop handler
  // (which the Filer needs for files dropped from outside) swallows HTML5
  // drags on Windows, and pointer capture gives the dragged row a plain
  // follow-the-pointer feel with no ghost image anyway. Reordering happens
  // live as the row's centre crosses a neighbour's midpoint; over a pane the
  // row stays put, a small ghost follows the pointer and the pane draws where
  // the tab would land.
  const drag = useRef<RailDrag | null>(null);
  const dragging = draggingTabId !== null && drag.current?.active === true;

  // Re-anchor on every reorder commit: the rows moved while the pointer did
  // not, so the pane under the pointer is worth recomputing.
  useEffect(() => {
    if (!dragging) return;
    const state = drag.current;
    if (!state) return;
    setTabDrag(state.id, dropTargetAt(state.x, state.y, state.paneId));
  }, [dragging, setTabDrag, blocks]);

  const handlePointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 || drag.current) return;
    const target = event.target as Element;
    if (target.closest(".tab-close")) return;
    const row = target.closest<HTMLElement>(".tab-row");
    const id = row?.dataset.tabId;
    const paneId = row?.dataset.paneId;
    if (!row || !id || !paneId) {
      // A press on the rail's blank part focuses the pane whose rows are last
      // on screen, which is the one the eye is nearest.
      if (!target.closest("button")) setActivePane(activePaneId);
      return;
    }
    drag.current = {
      id,
      paneId,
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      x: event.clientX,
      y: event.clientY,
      active: false,
      ghost: null,
    };
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const handlePointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const state = drag.current;
    const rail = railRef.current;
    if (!state || state.pointerId !== event.pointerId || !rail) return;
    state.x = event.clientX;
    state.y = event.clientY;
    if (!state.active) {
      const travelled = Math.hypot(
        event.clientX - state.startX,
        event.clientY - state.startY,
      );
      if (travelled < DRAG_THRESHOLD) return;
      state.active = true;
      setTabDrag(state.id, null);
    }
    const target = dropTargetAt(event.clientX, event.clientY, state.paneId);
    if (target) {
      const tab = allTabs.find((item) => item.info.id === state.id);
      showGhost(state, tab ? tabTitle(tab) : "");
    } else {
      hideGhost(state);
      moveTab(state.id, slotIndex(rail, state.paneId, event.clientY));
    }
    setTabDrag(state.id, target);
  };

  const endDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    const state = drag.current;
    if (!state || state.pointerId !== event.pointerId) return;
    drag.current = null;
    const rail = event.currentTarget;
    if (rail.hasPointerCapture(event.pointerId)) {
      rail.releasePointerCapture(event.pointerId);
    }
    if (!state.active) return;
    hideGhost(state);
    const target = useStore.getState().dropTarget;
    setTabDrag(null);
    if (!target || event.type === "pointercancel") return;
    if (target.zone === "center") {
      moveTabToPane(state.id, target.paneId, Number.MAX_SAFE_INTEGER);
    } else if (target.zone !== "strip") {
      // The rail is the only "strip" left, and a drag over it reorders live
      // rather than dropping anywhere, so this arm never fires.
      splitPane(target.paneId, target.zone, state.id);
    }
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
        className={`tab-rail${dragging ? " is-reordering" : ""}`}
        ref={railRef}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
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
              return (
                <div
                  key={tab.info.id}
                  className={[
                    "tab-row",
                    active ? "is-active" : "",
                    `is-${tab.state}`,
                    `is-command-${tab.commandActivity}`,
                    tab.info.id === draggingTabId ? "is-dragging" : "",
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
                  title={`${tab.info.protocol} · ${tab.info.address} · ${
                    tab.commandActivity === "running"
                      ? activityLabel(tab)
                      : tab.commandActivity === "complete"
                        ? `${activityLabel(tab)} — select to view`
                        : (tab.message ?? tab.state)
                  }`}
                >
                  <span
                    className={`tab-row-icon${tab.aiTool ? " is-agent" : ""}`}
                  >
                    <Icon
                      name={
                        tab.aiTool
                          ? "sparkle"
                          : (KIND_ICONS[tab.info.kind] ?? "terminal")
                      }
                    />
                  </span>
                  <div className="tab-row-text">
                    <div className="tab-row-title">
                      <span className="tab-index">{tab.number}.</span>
                      <span className="tab-dot" aria-hidden="true" />
                      <span className="tab-label">{tabTitle(tab)}</span>
                    </div>
                    {tab.cwd && <div className="tab-row-path">{tab.cwd}</div>}
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
