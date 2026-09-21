import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";

import { duplicateSession } from "../actions";
import { IS_MAC } from "../platform";
import { tabTitle, useStore, type Tab } from "../store";
import {
  colorForSession,
  type SessionKind,
  type SessionProfile,
} from "../types";

import { ContextMenu, type MenuItem } from "./ContextMenu";
import { Icon, type IconName } from "./icons";
import { SessionLauncher } from "./SessionLauncher";
import { useAccelerator } from "./TerminalPane";

/** 会话类型图标；本地终端使用不带外框的提示符，避免与圆形底色叠出双重边框。 */
const KIND_ICONS: Record<SessionKind, IconName> = {
  local: "terminal-prompt",
  ssh: "server",
  sftp: "folder",
};

/** 纵向移动超过该距离后才进入拖拽，普通点击不会误触排序。 */
const TAB_DRAG_THRESHOLD = 5;

interface RailDrag {
  id: string;
  paneId: string;
  pointerId: number;
  startY: number;
  currentY: number;
  startScrollTop: number;
  fromIndex: number;
  dropIndex: number;
  active: boolean;
  row: HTMLElement;
}

interface RailDragView {
  id: string;
  paneId: string;
  fromIndex: number;
  dropIndex: number;
}

/** 搜索只使用可确认的本地路径，SSH/SFTP 不拿连接名称冒充文件路径。 */
function tabPath(tab: Tab): string {
  return tab.info.kind === "local" ? (tab.cwd ?? tab.profile.cwd ?? "") : "";
}

/** 路径搜索忽略大小写，并兼容用户混用 Windows 与 Unix 分隔符。 */
function normalizePathSearch(value: string): string {
  return value.trim().replace(/\\/g, "/").toLocaleLowerCase();
}

/**
 * 根据其余标签的实时中点计算最终插入位置。被拖标签不参与测量，因此列表不会因
 * 自身位移反复改变基准，也不会产生累计漂移。
 */
function dropIndexAt(
  rail: HTMLElement,
  paneId: string,
  draggedId: string,
  clientY: number,
): number {
  let index = 0;
  for (const row of rail.querySelectorAll<HTMLElement>(".tab-row")) {
    if (row.dataset.paneId !== paneId || row.dataset.tabId === draggedId) {
      continue;
    }
    const rect = row.getBoundingClientRect();
    if (clientY > rect.top + rect.height / 2) index += 1;
  }
  return index;
}

interface Props {
  /** Opens the New Session dialog: the rail's own way to a host by hand. */
  onNewSession: () => void;
  /** 直接编辑左侧选中的已保存会话，由应用统一承载编辑对话框。 */
  onEditProfile: (profile: SessionProfile) => void;
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
  return tabPath(tab) || tabTitle(tab);
}

/**
 * The line under a row's name: the branch its directory is on, and nothing at
 * all outside a repository — a directory repeated under itself would be the
 * same fact twice.
 */
function rowMeta(tab: Tab): string | null {
  return tab.info.kind === "local" ? (tab.branch ?? null) : null;
}

/** 左侧展示已打开终端，并提供会话浮层入口；新建与编辑由应用对话框负责。 */
export function TabRail({ onNewSession, onEditProfile }: Props) {
  const allTabs = useStore((s) => s.tabs);
  const panes = useStore((s) => s.panes);
  const profiles = useStore((s) => s.profiles);
  const activeId = useStore((s) => s.activeId);
  const activePaneId = useStore((s) => s.activePaneId);
  const setActive = useStore((s) => s.setActive);
  const setActivePane = useStore((s) => s.setActivePane);
  const requestCloseTabs = useStore((s) => s.requestCloseTabs);
  const moveTab = useStore((s) => s.moveTab);
  const closeKey = useAccelerator("closeSession");
  const railRef = useRef<HTMLDivElement>(null);
  const drag = useRef<RailDrag | null>(null);
  const [pathFilter, setPathFilter] = useState("");
  const [dragView, setDragView] = useState<RailDragView | null>(null);
  const pathNeedle = normalizePathSearch(pathFilter);

  // 搜索只派生可见列表，不修改标签顺序、活动会话或窗格状态。
  const blocks = useMemo(
    () =>
      panes
        .map((pane, paneIndex) => ({
          pane,
          paneIndex,
          tabs: allTabs.filter(
            (tab) =>
              tab.paneId === pane.id &&
              (!pathNeedle ||
                normalizePathSearch(tabPath(tab)).includes(pathNeedle)),
          ),
        }))
        .filter((block) => block.tabs.length > 0),
    [allTabs, panes, pathNeedle],
  );

  // A row's own menu: the close commands work along its pane, the way VS
  // Code's "Close to the Right" stays inside its editor group.
  const [rowMenu, setRowMenu] = useState<{
    x: number;
    y: number;
    id: string;
  } | null>(null);
  const closeRowMenu = useCallback(() => setRowMenu(null), []);

  // ＋与空白处右键共用同一个入口，避免连接和管理分散在不同侧栏。
  const [railMenu, setRailMenu] = useState<{ x: number; y: number } | null>(
    null,
  );
  const closeRailMenu = useCallback(() => setRailMenu(null), []);

  const onContextMenu = (event: ReactMouseEvent<HTMLDivElement>) => {
    if ((event.target as Element).closest("input")) return;
    const id = (event.target as Element).closest<HTMLElement>(".tab-row")
      ?.dataset.tabId;
    event.preventDefault();
    if (id) {
      setRailMenu(null);
      setRowMenu({ x: event.clientX, y: event.clientY, id });
    } else {
      setRowMenu(null);
      setRailMenu({ x: event.clientX, y: event.clientY });
    }
  };

  const rowMenuItems = (id: string): MenuItem[] => {
    const tab = allTabs.find((item) => item.info.id === id);
    const ids = allTabs
      .filter((item) => item.paneId === tab?.paneId)
      .map((item) => item.info.id);
    const index = ids.indexOf(id);
    const profile = profiles.find(
      (item) => item.id && item.id === tab?.info.profileId,
    );
    return [
      ...(profile
        ? ([
            {
              label: "Edit Saved Session…",
              icon: "edit",
              action: () => onEditProfile(profile),
            },
            "separator",
          ] as MenuItem[])
        : []),
      {
        label: "Close",
        shortcut: closeKey,
        action: () => requestCloseTabs([id]),
      },
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

  /** 双击左侧空白处复制当前高亮标签，行与按钮保留各自的行为。 */
  const onDoubleClick = (event: ReactMouseEvent<HTMLDivElement>) => {
    const target = event.target as Element;
    if (target.closest(".tab-row") || target.closest("button, input")) return;
    if (activeId) void duplicateSession(activeId, activePaneId);
  };

  const refreshDrag = (state: RailDrag, clientY: number) => {
    const rail = railRef.current;
    if (!rail) return;
    state.currentY = clientY;
    const translateY =
      clientY - state.startY + rail.scrollTop - state.startScrollTop;
    state.row.style.setProperty("--tab-drag-y", `${translateY}px`);
    const dropIndex = dropIndexAt(rail, state.paneId, state.id, clientY);
    if (dropIndex === state.dropIndex) return;
    state.dropIndex = dropIndex;
    setDragView({
      id: state.id,
      paneId: state.paneId,
      fromIndex: state.fromIndex,
      dropIndex,
    });
  };

  const handlePointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 || drag.current || pathNeedle) return;
    const target = event.target as Element;
    if (target.closest("button, input")) return;
    const row = target.closest<HTMLElement>(".tab-row");
    const id = row?.dataset.tabId;
    const paneId = row?.dataset.paneId;
    const rail = railRef.current;
    if (!row || !id || !paneId || !rail) return;
    const paneTabs = allTabs.filter((tab) => tab.paneId === paneId);
    drag.current = {
      id,
      paneId,
      pointerId: event.pointerId,
      startY: event.clientY,
      currentY: event.clientY,
      startScrollTop: rail.scrollTop,
      fromIndex: paneTabs.findIndex((tab) => tab.info.id === id),
      dropIndex: paneTabs.findIndex((tab) => tab.info.id === id),
      active: false,
      row,
    };
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const handlePointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const state = drag.current;
    if (!state || state.pointerId !== event.pointerId) return;
    if (!state.active) {
      if (Math.abs(event.clientY - state.startY) < TAB_DRAG_THRESHOLD) return;
      state.active = true;
      setRowMenu(null);
      setDragView({
        id: state.id,
        paneId: state.paneId,
        fromIndex: state.fromIndex,
        dropIndex: state.dropIndex,
      });
    }
    event.preventDefault();
    refreshDrag(state, event.clientY);
  };

  const endDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    const state = drag.current;
    if (!state || state.pointerId !== event.pointerId) return;
    drag.current = null;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    state.row.style.removeProperty("--tab-drag-y");
    setDragView(null);
    if (state.active && event.type !== "pointercancel") {
      moveTab(state.id, state.dropIndex);
    }
  };

  const handleRailScroll = () => {
    const state = drag.current;
    if (state?.active) refreshDrag(state, state.currentY);
  };

  return (
    <>
      {/* 入口固定在顶部，打开的终端列表再长也能直接创建和管理会话。 */}
      <div
        className="tab-rail-panel"
        onContextMenu={onContextMenu}
        onDoubleClick={onDoubleClick}
      >
        <div className="tab-rail-bar">
          <div className="tab-rail-path-search" role="search">
            <Icon name="search" className="tab-rail-path-search-icon" />
            <input
              type="search"
              value={pathFilter}
              onChange={(event) => setPathFilter(event.target.value)}
              onKeyDown={(event) => {
                if (event.key !== "Escape" || !pathFilter) return;
                event.stopPropagation();
                setPathFilter("");
              }}
              placeholder="Search paths…"
              aria-label="Search tabs by path"
            />
          </div>
          <button
            type="button"
            className="tab-rail-add"
            onClick={(event) => {
              const box = event.currentTarget.getBoundingClientRect();
              setRowMenu(null);
              const left =
                event.currentTarget
                  .closest(".sidebar-left")
                  ?.getBoundingClientRect().left ?? box.left;
              setRailMenu({ x: left + 8, y: box.bottom + 4 });
            }}
            title="Sessions"
            aria-label="Sessions"
            aria-haspopup="dialog"
            aria-expanded={railMenu !== null}
            aria-controls={railMenu ? "session-launcher" : undefined}
          >
            <Icon name="add" />
          </button>
        </div>

        <div
          className={`tab-rail${dragView ? " is-reordering" : ""}`}
          ref={railRef}
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
          onScroll={handleRailScroll}
        >
          {blocks.map((block) => {
            const remaining =
              dragView?.paneId === block.pane.id
                ? block.tabs.filter((tab) => tab.info.id !== dragView.id)
                : [];
            const dropBeforeId =
              dragView?.paneId === block.pane.id
                ? remaining[dragView.dropIndex]?.info.id
                : undefined;
            const dropAfterId =
              dragView?.paneId === block.pane.id &&
              dragView.dropIndex === remaining.length
                ? remaining[remaining.length - 1]?.info.id
                : undefined;
            return (
              <div className="tab-rail-block" key={block.pane.id}>
                {panes.length > 1 && (
                  <button
                    type="button"
                    className={`tab-rail-pane${
                      block.pane.id === activePaneId ? " is-active" : ""
                    }`}
                    onMouseDown={(event) => {
                      event.stopPropagation();
                      setActivePane(block.pane.id);
                    }}
                    title={`Pane ${block.paneIndex + 1}`}
                  >
                    Pane {block.paneIndex + 1}
                  </button>
                )}
                {block.tabs.map((tab, tabIndex) => {
                const active = tab.info.id === activeId;
                const shiftsUp =
                  dragView?.paneId === block.pane.id &&
                  dragView.fromIndex < dragView.dropIndex &&
                  tabIndex > dragView.fromIndex &&
                  tabIndex <= dragView.dropIndex;
                const shiftsDown =
                  dragView?.paneId === block.pane.id &&
                  dragView.fromIndex > dragView.dropIndex &&
                  tabIndex >= dragView.dropIndex &&
                  tabIndex < dragView.fromIndex;
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
                      dragView?.id === tab.info.id ? "is-dragging" : "",
                      shiftsUp ? "is-shift-up" : "",
                      shiftsDown ? "is-shift-down" : "",
                      dropBeforeId === tab.info.id ? "is-drop-before" : "",
                      dropAfterId === tab.info.id ? "is-drop-after" : "",
                    ]
                      .filter(Boolean)
                      .join(" ")}
                    style={
                      {
                        "--session-color": sessionColor,
                        ...(tab.aiTool
                          ? { "--agent-color": tab.aiTool.color }
                          : {}),
                      } as CSSProperties
                    }
                    data-tab-id={tab.info.id}
                    data-pane-id={tab.paneId}
                    onMouseDown={() => setActive(tab.info.id)}
                    title={[
                      `${tab.info.protocol} · ${tab.info.address}`,
                      // 图标负责识别工具，这里补充文字名称以便悬停核对。
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
                        <span className="tab-row-actions">
                          <span className="tab-shortcut">
                            {tab.number <= 9
                              ? `${IS_MAC ? "⌘" : "Alt+"}${tab.number}`
                              : tab.number}
                          </span>
                          <button
                            className="tab-close"
                            onMouseDown={(event) => event.stopPropagation()}
                            onClick={(event) => {
                              event.stopPropagation();
                              requestCloseTabs([tab.info.id]);
                            }}
                            title="Close session"
                            aria-label="Close session"
                          >
                            <Icon name="close" />
                          </button>
                        </span>
                      </div>
                      {meta && <div className="tab-row-meta">{meta}</div>}
                    </div>
                  </div>
                );
                })}
              </div>
            );
          })}

          {pathNeedle && blocks.length === 0 && (
            <div className="tab-rail-empty">No matching paths</div>
          )}

          {rowMenu && (
            <ContextMenu
              x={rowMenu.x}
              y={rowMenu.y}
              items={rowMenuItems(rowMenu.id)}
              onClose={closeRowMenu}
            />
          )}
        </div>
      </div>
      <SessionLauncher
        anchor={railMenu}
        onClose={closeRailMenu}
        onNewSession={onNewSession}
        onEditProfile={onEditProfile}
      />
    </>
  );
}
