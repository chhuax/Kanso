import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type MouseEvent,
} from "react";
import { createPortal } from "react-dom";
import { ask } from "@tauri-apps/plugin-dialog";

import { openLocalShell, openSession } from "../actions";
import * as api from "../api";
import { importSshConfig } from "../dataTransfer";
import { byName, effectiveGroupId, sortedGroups } from "../sessionGroups";
import { useStore } from "../store";
import {
  colorForSession,
  type SavedCommand,
  type SessionGroup,
  type SessionProfile,
} from "../types";
import { ContextMenu, type MenuItem } from "./ContextMenu";
import { DeleteProfileDialog } from "./DeleteProfileDialog";
import { GroupNameDialog } from "./GroupNameDialog";
import { Icon } from "./icons";

/** 会话缩进一个层级，保留其与所属分组的视觉关系。 */
const INDENT = 18;

/** 连接目标用于区分同名会话，并在删除前帮助用户核对。 */
function describeProfile(
  profile: SessionProfile,
  jumpHost?: SessionProfile,
): string {
  const via = jumpHost ? ` via ${jumpHost.name}` : "";
  switch (profile.kind) {
    case "ssh":
      return `${profile.username ?? ""}@${profile.host ?? ""}:${profile.port ?? 22}${via}`;
    case "sftp":
      return `${profile.username ?? ""}@${profile.host ?? ""}:${profile.port ?? 22}${via}`;
    default:
      return profile.shell ?? "default shell";
  }
}

/** 分组默认收起，避免大型连接库挤占浮层。 */
function isOpen(open: Record<string, boolean>, key: string): boolean {
  return open[key] ?? false;
}

/** 同时搜索别名和连接目标，兼容从 SSH 配置导入的命名方式。 */
function matchesFilter(profile: SessionProfile, needle: string): boolean {
  return [profile.name, profile.host, profile.username].some((field) =>
    field?.toLowerCase().includes(needle),
  );
}

type Row =
  | {
      type: "group";
      group: SessionGroup;
      count: number;
      collapsed: boolean;
    }
  | { type: "profile"; profile: SessionProfile; depth: number };

interface MenuState {
  x: number;
  y: number;
  items: MenuItem[];
}

interface Props {
  anchor: { x: number; y: number } | null;
  onClose: () => void;
  onEditProfile: (profile: SessionProfile) => void;
  onNewSession: () => void;
}

/** 左侧会话入口；调用方保持组件挂载，以便收起浮层后继续完成编辑和删除确认。 */
export function SessionLauncher({
  anchor,
  onClose,
  onEditProfile,
  onNewSession,
}: Props) {
  const launcherRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const [position, setPosition] = useState({ left: 0, top: 0 });
  const activePaneId = useStore((s) => s.activePaneId);
  const panelFontSize = useStore((s) => s.panelFontSize);
  const profiles = useStore((s) => s.profiles);
  const groups = useStore((s) => s.groups);
  const removeProfile = useStore((s) => s.removeProfile);
  const moveProfileToGroup = useStore((s) => s.moveProfileToGroup);
  const upsertGroup = useStore((s) => s.upsertGroup);
  const removeGroup = useStore((s) => s.removeGroup);
  const setStatus = useStore((s) => s.setStatus);
  const [filter, setFilter] = useState("");
  // 收起浮层保留分组展开状态，下次进入时仍在原来的位置。
  const [open, setOpen] = useState<Record<string, boolean>>({});
  // 删除会连带清除专属 Sender 命令，确认框必须展示这部分影响。
  const [pendingDelete, setPendingDelete] = useState<{
    profile: SessionProfile;
    scopedCommands: number;
  } | null>(null);
  const [menu, setMenu] = useState<MenuState | null>(null);
  // 空对象表示新建，已有分组表示重命名；确认对话框独立于浮层的显示状态。
  const [groupDialog, setGroupDialog] = useState<{
    group?: SessionGroup;
  } | null>(null);

  const closeMenu = useCallback(() => setMenu(null), []);

  useLayoutEffect(() => {
    if (!anchor || !launcherRef.current) return;
    const { width, height } = launcherRef.current.getBoundingClientRect();
    setPosition({
      left: Math.max(8, Math.min(anchor.x, window.innerWidth - width - 8)),
      top: Math.max(8, Math.min(anchor.y, window.innerHeight - height - 8)),
    });
  }, [anchor, filter, open, profiles, groups, panelFontSize]);

  useLayoutEffect(() => {
    if (!anchor) return;
    setFilter("");
    searchRef.current?.focus();
  }, [anchor]);

  useEffect(() => {
    if (!anchor) return;
    const dismiss = () => {
      setMenu(null);
      onClose();
    };
    const onMouseDown = (event: globalThis.MouseEvent) => {
      if (!launcherRef.current?.contains(event.target as Node)) dismiss();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      // 二级菜单先处理 Escape，避免一次按键把整个会话入口也关掉。
      if (event.key !== "Escape" || menu) return;
      event.preventDefault();
      event.stopPropagation();
      dismiss();
      document.querySelector<HTMLButtonElement>(".tab-rail-add")?.focus();
    };
    document.addEventListener("mousedown", onMouseDown, true);
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("resize", dismiss);
    window.addEventListener("blur", dismiss);
    return () => {
      document.removeEventListener("mousedown", onMouseDown, true);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("resize", dismiss);
      window.removeEventListener("blur", dismiss);
    };
  }, [anchor, menu, onClose]);

  const connect = (profile: SessionProfile) => {
    onClose();
    void openSession(profile);
  };

  const edit = (profile: SessionProfile) => {
    onClose();
    onEditProfile(profile);
  };

  const filtering = filter.trim().length > 0;

  // 分组优先，再展示未分组会话；仅支持一层分组，不按协议重复分类。
  const tree = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const byGroup = new Map<string | null, SessionProfile[]>();
    for (const profile of profiles) {
      // 内置本地 Shell 已有固定入口，列表只管理持久化的连接配置。
      if (!profile.id) continue;
      if (needle && !matchesFilter(profile, needle)) continue;
      const groupId = effectiveGroupId(groups, profile);
      byGroup.set(groupId, [...(byGroup.get(groupId) ?? []), profile]);
    }
    for (const members of byGroup.values()) members.sort(byName);

    const rows: Row[] = [];
    let count = 0;
    for (const group of sortedGroups(groups)) {
      const members = byGroup.get(group.id) ?? [];
      // 搜索时展开匹配分组，否则收起状态会让匹配结果不可见。
      if (needle && members.length === 0) continue;
      const collapsed = !needle && !isOpen(open, `group:${group.id}`);
      rows.push({ type: "group", group, count: members.length, collapsed });
      if (!collapsed) {
        for (const profile of members) {
          rows.push({ type: "profile", profile, depth: 1 });
        }
      }
      count += members.length;
    }
    for (const profile of byGroup.get(null) ?? []) {
      rows.push({ type: "profile", profile, depth: 0 });
      count++;
    }
    return { rows, count };
  }, [profiles, groups, filter, open]);

  const toggle = (key: string) =>
    setOpen((prev) => ({ ...prev, [key]: !isOpen(prev, key) }));

  const openMenu = (event: MouseEvent, items: MenuItem[]) => {
    event.preventDefault();
    event.stopPropagation();
    setMenu({ x: event.clientX, y: event.clientY, items });
  };

  const report = (what: string, error: unknown) =>
    setStatus(`${what}: ${error}`);

  const listSenderCommands = () =>
    api.listSenderCommands().catch((): SavedCommand[] => []);

  /** 收起浮层后统计删除影响，避免对话框与浮层同时争抢焦点。 */
  const askDeleteProfile = async (profile: SessionProfile) => {
    onClose();
    const commands = await listSenderCommands();
    const scopedCommands = commands.filter(
      (command) =>
        command.scope.type === "profile" && command.scope.id === profile.id,
    ).length;
    setPendingDelete({ profile, scopedCommands });
  };

  const confirmDeleteGroup = async (group: SessionGroup) => {
    onClose();
    const insideProfiles = profiles.filter(
      (p) => effectiveGroupId(groups, p) === group.id,
    );
    const insideIds = new Set(insideProfiles.map((p) => p.id));
    // 分组删除会连带删除其会话和两级作用域下的 Sender 命令。
    const scopedCommands = (await listSenderCommands()).filter(
      (command) =>
        (command.scope.type === "group" && command.scope.id === group.id) ||
        (command.scope.type === "profile" && insideIds.has(command.scope.id)),
    ).length;
    const plural = (count: number, noun: string) =>
      `${count} ${noun}${count === 1 ? "" : "s"}`;
    const contents = [
      insideProfiles.length > 0 && plural(insideProfiles.length, "session"),
      scopedCommands > 0 && plural(scopedCommands, "saved Sender command"),
    ].filter((part): part is string => typeof part === "string");
    const consequence =
      contents.length === 0
        ? "It contains no sessions."
        : `${contents.join(" and ")} will be deleted with it. This cannot be undone.`;
    const confirmed = await ask(
      `Delete the group "${group.name}" and everything in it? ${consequence}`,
      {
        title: "Delete Group",
        kind: "warning",
        okLabel: "Delete",
        cancelLabel: "Cancel",
      },
    );
    if (!confirmed) return;
    try {
      await removeGroup(group.id);
      setStatus(`Group "${group.name}" deleted`);
    } catch (error) {
      report("Failed to delete group", error);
    }
  };

  // 分组和导入也在左侧完成，空分组不再依赖常驻面板才能维护。
  const panelMenu = (): MenuItem[] => [
    {
      label: "New Group…",
      icon: "new-folder",
      action: () => {
        onClose();
        setGroupDialog({});
      },
    },
    "separator",
    {
      label: "Import OpenSSH Config…",
      icon: "cloud-download",
      action: () => {
        onClose();
        void importSshConfig();
      },
    },
  ];

  const groupMenu = (group: SessionGroup): MenuItem[] => [
    {
      label: "Rename Group…",
      icon: "rename",
      action: () => {
        onClose();
        setGroupDialog({ group });
      },
    },
    {
      label: "Delete Group…",
      icon: "trash",
      danger: true,
      action: () => void confirmDeleteGroup(group),
    },
  ];

  const profileMenu = (profile: SessionProfile): MenuItem[] => {
    const connectItem: MenuItem = {
      label: "Connect",
      icon: "plug",
      action: () => connect(profile),
    };

    const current = effectiveGroupId(groups, profile);
    const move = (groupId: string | null) => () =>
      moveProfileToGroup(profile.id, groupId).catch((error) =>
        report("Failed to move session", error),
      );
    const choices: MenuItem[] = [
      {
        label: "Top level (no group)",
        checked: current === null,
        mark: "radio",
        action: move(null),
      },
    ];
    const nodes = sortedGroups(groups);
    if (nodes.length > 0) {
      choices.push("separator");
      for (const group of nodes) {
        choices.push({
          label: group.name,
          checked: current === group.id,
          mark: "radio",
          action: move(group.id),
        });
      }
    } else {
      choices.push({ label: "No groups yet", disabled: true });
    }

    return [
      connectItem,
      { label: "Edit…", icon: "edit", action: () => edit(profile) },
      { label: "Move to Group", icon: "move", children: choices },
      "separator",
      {
        label: "Delete…",
        icon: "trash",
        danger: true,
        action: () => void askDeleteProfile(profile),
      },
    ];
  };

  const renderProfile = (profile: SessionProfile, depth: number) => (
    <div
      key={profile.id || profile.name}
      className="row"
      style={{ paddingLeft: 6 + INDENT * depth }}
      onContextMenu={(event) => openMenu(event, profileMenu(profile))}
      title={describeProfile(
        profile,
        profiles.find((p) => p.id && p.id === profile.jumpProfileId),
      )}
    >
      <button
        type="button"
        className="session-launcher-connect"
        onClick={() => connect(profile)}
      >
        <span
          className="row-dot"
          style={{ background: profile.color ?? colorForSession(profile.id) }}
        />
        <span className="session-launcher-label">
          <span className="row-label">{profile.name}</span>
          <span className="session-launcher-target">
            {describeProfile(profile)}
          </span>
        </span>
      </button>
      <button
        type="button"
        className="panel-action"
        onClick={() => edit(profile)}
        title="Edit"
        aria-label={`Edit ${profile.name}`}
      >
        <Icon name="edit" />
      </button>
      <button
        type="button"
        className="panel-action"
        onClick={(event) => {
          const box = event.currentTarget.getBoundingClientRect();
          setMenu({ x: box.right, y: box.top, items: profileMenu(profile) });
        }}
        title="Session actions"
        aria-label={`Actions for ${profile.name}`}
      >
        <Icon name="ellipsis" />
      </button>
    </div>
  );

  const renderGroup = (row: Extract<Row, { type: "group" }>) => (
    <div
      key={`group:${row.group.id}`}
      className="row is-group"
      style={{ paddingLeft: 6 }}
      onContextMenu={(event) => openMenu(event, groupMenu(row.group))}
      title={`${row.group.name} · right-click for options`}
    >
      <button
        type="button"
        className="session-launcher-connect"
        aria-expanded={!row.collapsed}
        onClick={() => toggle(`group:${row.group.id}`)}
      >
        <span className={`row-caret${row.collapsed ? "" : " is-open"}`}>
          <Icon name="chevron-right" />
        </span>
        <Icon
          name={row.collapsed ? "folder" : "folder-opened"}
          className="row-folder"
        />
        <span className="row-label">{row.group.name}</span>
        <span className="row-meta">{row.count}</span>
      </button>
      <button
        type="button"
        className="panel-action"
        aria-label={`Actions for group ${row.group.name}`}
        onClick={(event) => {
          const box = event.currentTarget.getBoundingClientRect();
          setMenu({ x: box.right, y: box.top, items: groupMenu(row.group) });
        }}
      >
        <Icon name="ellipsis" />
      </button>
    </div>
  );

  return createPortal(
    <>
      {anchor && (
        <div
          ref={launcherRef}
          className="session-launcher"
          style={
            {
              ...position,
              "--panel-font-size": `${panelFontSize}px`,
            } as CSSProperties
          }
          role="dialog"
          aria-label="Sessions"
          id="session-launcher"
        >
          <div className="session-launcher-quick">
            <button
              type="button"
              className="menu-entry"
              onClick={() => {
                onClose();
                void openLocalShell(activePaneId);
              }}
            >
              <Icon name="terminal" />
              New Local Shell
            </button>
            <button
              type="button"
              className="menu-entry"
              onClick={() => {
                onClose();
                onNewSession();
              }}
            >
              <Icon name="add" />
              New Session…
            </button>
          </div>

          <div className="panel-filter">
            <span className="panel-filter-icon" aria-hidden="true">
              <Icon name="search" />
            </span>
            <input
              ref={searchRef}
              value={filter}
              placeholder="Search sessions"
              aria-label="Search sessions"
              onChange={(event) => setFilter(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "ArrowDown") {
                  event.preventDefault();
                  launcherRef.current
                    ?.querySelector<HTMLButtonElement>(
                      ".panel-body .session-launcher-connect",
                    )
                    ?.focus();
                } else if (event.key === "Enter" && filtering) {
                  const first = tree.rows.find((row) => row.type === "profile");
                  if (first?.type === "profile") {
                    event.preventDefault();
                    connect(first.profile);
                  }
                }
              }}
            />
          </div>

          <div className="session-launcher-heading">
            <span>Saved sessions</span>
            <button
              type="button"
              className="panel-action"
              aria-label="Session library actions"
              onClick={(event) => {
                const box = event.currentTarget.getBoundingClientRect();
                setMenu({ x: box.right, y: box.top, items: panelMenu() });
              }}
            >
              <Icon name="ellipsis" />
            </button>
          </div>

          <div
            className="panel-body"
            onContextMenu={(event) => openMenu(event, panelMenu())}
          >
            {tree.rows.map((row) =>
              row.type === "group"
                ? renderGroup(row)
                : renderProfile(row.profile, row.depth),
            )}
            {tree.count === 0 && (
              <div className="panel-empty">
                {filtering
                  ? `No sessions match “${filter.trim()}”.`
                  : "Save a new session or import your SSH config."}
              </div>
            )}
          </div>

          {menu && (
            <ContextMenu
              x={menu.x}
              y={menu.y}
              items={menu.items}
              onClose={closeMenu}
            />
          )}
        </div>
      )}

      {groupDialog && (
        <GroupNameDialog
          title={groupDialog.group ? "Rename Group" : "New Group"}
          initialName={groupDialog.group?.name}
          submitLabel={groupDialog.group ? "Rename" : "Create"}
          onSubmit={async (name) => {
            await upsertGroup({ id: groupDialog.group?.id ?? "", name });
            setGroupDialog(null);
          }}
          onCancel={() => setGroupDialog(null)}
        />
      )}

      {pendingDelete && (
        <DeleteProfileDialog
          profile={pendingDelete.profile}
          target={describeProfile(pendingDelete.profile)}
          scopedCommands={pendingDelete.scopedCommands}
          onConfirm={() => {
            // 先关闭确认框，防止连续按 Enter 重复提交删除请求。
            setPendingDelete(null);
            void removeProfile(pendingDelete.profile.id).catch((error) =>
              report("Failed to delete session", error),
            );
          }}
          onCancel={() => setPendingDelete(null)}
        />
      )}
    </>,
    document.body,
  );
}
