import { useCallback, useMemo, useState, type MouseEvent } from "react";
import { ask } from "@tauri-apps/plugin-dialog";

import {
  LOCAL_SHELL_PROFILE,
  openSession,
  toggleSessionConnection,
} from "../../actions";
import * as api from "../../api";
import { importSshConfig } from "../../dataTransfer";
import {
  byName,
  childGroups,
  describeLocation,
  effectiveGroupId,
  flattenGroups,
} from "../../sessionGroups";
import { tabTitle, useStore } from "../../store";
import {
  colorForSession,
  type SavedCommand,
  type SessionGroup,
  type SessionProfile,
  type SessionState,
} from "../../types";
import { ContextMenu, type MenuItem } from "../ContextMenu";
import { DeleteProfileDialog } from "../DeleteProfileDialog";
import { GroupNameDialog } from "../GroupNameDialog";
import { Icon } from "../icons";
import { PanelTabs, type PanelTabsProps } from "../PanelTabs";

/** Tooltip of the power toggle, by the active tab's state. */
const POWER_TITLES: Record<SessionState, string> = {
  connected: "Disconnect",
  connecting: "Connecting…",
  closed: "Reconnect",
  error: "Reconnect",
};

/** Horizontal step per tree level; the kind headings sit at level 0. */
const INDENT = 18;

/** One-line connection target, used for the row tooltip and delete prompt. */
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

/**
 * Whether a group is open. Groups start closed so a long list of servers
 * folds down to its top-level folders until the user opens one.
 */
function isOpen(open: Record<string, boolean>, key: string): boolean {
  return open[key] ?? false;
}

type Row =
  | {
      type: "group";
      group: SessionGroup;
      depth: number;
      /** Profiles in the group and all of its subgroups. */
      count: number;
      collapsed: boolean;
    }
  | { type: "profile"; profile: SessionProfile; depth: number };

interface MenuState {
  x: number;
  y: number;
  items: MenuItem[];
}

type GroupDialogState =
  | { mode: "create"; parentId: string | null }
  | { mode: "rename"; group: SessionGroup };

interface Props {
  onEditProfile: (profile: SessionProfile) => void;
  onNewSession: () => void;
  /** Docked in the right sidebar: draw the Session / Filer tabs instead of
      this panel's own title. Standalone, it keeps the title. */
  tabs?: PanelTabsProps;
}

export function SessionPanel({ onEditProfile, onNewSession, tabs }: Props) {
  const profiles = useStore((s) => s.profiles);
  const groups = useStore((s) => s.groups);
  const removeProfile = useStore((s) => s.removeProfile);
  const moveProfileToGroup = useStore((s) => s.moveProfileToGroup);
  const upsertGroup = useStore((s) => s.upsertGroup);
  const removeGroup = useStore((s) => s.removeGroup);
  const setStatus = useStore((s) => s.setStatus);
  // The header's power toggle acts on the active tab, like Session →
  // Disconnect / Reconnect Session.
  const activeTab = useStore((s) =>
    s.tabs.find((tab) => tab.info.id === s.activeId),
  );

  const [filter, setFilter] = useState("");
  /**
   * Explicit open / closed state of the tree, keyed `kind:<kind>` for
   * headings and `group:<id>` for groups. Anything not in here is at its
   * default — see `isOpen`.
   */
  const [open, setOpen] = useState<Record<string, boolean>>({});
  /**
   * Profile awaiting the user's answer in the delete-confirmation dialog,
   * with how many Sender commands are scoped to it alone (they go with it,
   * and the dialog says so).
   */
  const [pendingDelete, setPendingDelete] = useState<{
    profile: SessionProfile;
    scopedCommands: number;
  } | null>(null);
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [groupDialog, setGroupDialog] = useState<GroupDialogState | null>(
    null,
  );

  const closeMenu = useCallback(() => setMenu(null), []);

  const filtering = filter.trim().length > 0;

  // One tree: the folders the user made, with the sessions inside them and the
  // ones without a folder at the top level. Nothing is pre-grouped by session
  // kind — a group holds any of them.
  const tree = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const byGroup = new Map<string | null, SessionProfile[]>();
    for (const profile of [LOCAL_SHELL_PROFILE, ...profiles]) {
      if (needle && !profile.name.toLowerCase().includes(needle)) continue;
      const groupId = effectiveGroupId(groups, profile);
      byGroup.set(groupId, [...(byGroup.get(groupId) ?? []), profile]);
    }
    for (const members of byGroup.values()) members.sort(byName);

    // Folders first, then the profiles at that level, both A→Z — the shape of
    // a file tree. While filtering, empty groups are dropped and collapse
    // state is ignored so every match is on screen.
    const walk = (
      parentId: string | null,
      depth: number,
    ): { rows: Row[]; count: number } => {
      const rows: Row[] = [];
      let count = 0;
      for (const group of childGroups(groups, parentId)) {
        const sub = walk(group.id, depth + 1);
        if (needle && sub.count === 0) continue;
        const collapsed = !needle && !isOpen(open, `group:${group.id}`);
        rows.push({ type: "group", group, depth, count: sub.count, collapsed });
        if (!collapsed) rows.push(...sub.rows);
        count += sub.count;
      }
      for (const profile of byGroup.get(parentId) ?? []) {
        rows.push({ type: "profile", profile, depth });
        count++;
      }
      return { rows, count };
    };

    return walk(null, 0);
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

  /** Opens the delete dialog, counting the Sender commands that go with it. */
  const askDeleteProfile = async (profile: SessionProfile) => {
    const commands = await listSenderCommands();
    const scopedCommands = commands.filter(
      (command) =>
        command.scope.type === "profile" && command.scope.id === profile.id,
    ).length;
    setPendingDelete({ profile, scopedCommands });
  };

  const confirmDeleteGroup = async (group: SessionGroup) => {
    const inSubtree = (groupId: string | null) => {
      let cursor = groupId;
      const seen = new Set<string>();
      while (cursor && !seen.has(cursor)) {
        if (cursor === group.id) return true;
        seen.add(cursor);
        cursor = groups.find((g) => g.id === cursor)?.parentId ?? null;
      }
      return false;
    };
    const insideProfiles = profiles.filter((p) =>
      inSubtree(effectiveGroupId(groups, p)),
    );
    const insideIds = new Set(insideProfiles.map((p) => p.id));
    // Everything scoped to the subtree goes with it: commands of the groups
    // and commands of the sessions in them.
    const scopedCommands = (await listSenderCommands()).filter(
      (command) =>
        (command.scope.type === "group" && inSubtree(command.scope.id)) ||
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
        : `Its subgroups, ${contents.join(" and ")} will be deleted with it. This cannot be undone.`;
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

  // Right-clicking the panel's empty space. A group is not made here: it is
  // made where it is used, from the New Session dialog's Group field.
  const panelMenu = (): MenuItem[] => [
    // The hosts people already `ssh` to, in bulk.
    {
      label: "Import OpenSSH Config…",
      icon: "cloud-download",
      action: () => void importSshConfig(),
    },
  ];

  const groupMenu = (group: SessionGroup): MenuItem[] => [
    {
      label: "New Subgroup…",
      icon: "new-folder",
      action: () => setGroupDialog({ mode: "create", parentId: group.id }),
    },
    "separator",
    {
      label: "Rename Group…",
      icon: "rename",
      action: () => setGroupDialog({ mode: "rename", group }),
    },
    {
      label: "Delete Group…",
      icon: "trash",
      danger: true,
      action: () => void confirmDeleteGroup(group),
    },
  ];

  const profileMenu = (profile: SessionProfile): MenuItem[] => {
    const connect: MenuItem = {
      label: "Connect",
      icon: "plug",
      action: () => void openSession(profile),
    };
    // The built-in Local Shell is not a saved profile: nothing to edit or move.
    if (!profile.id) return [connect];

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
    const nodes = flattenGroups(groups);
    if (nodes.length > 0) {
      choices.push("separator");
      for (const { group, depth } of nodes) {
        choices.push({
          label: group.name,
          indent: depth,
          checked: current === group.id,
          mark: "radio",
          action: move(group.id),
        });
      }
    } else {
      choices.push({ label: "No groups yet", disabled: true });
    }

    return [
      connect,
      { label: "Edit…", icon: "edit", action: () => onEditProfile(profile) },
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
      onDoubleClick={() => void openSession(profile)}
      onContextMenu={(event) => openMenu(event, profileMenu(profile))}
      title={describeProfile(
        profile,
        profiles.find((p) => p.id && p.id === profile.jumpProfileId),
      )}
    >
      <span
        className="row-dot"
        style={{
          background:
            profile.color ?? colorForSession(profile.id || profile.name),
        }}
      />
      <span className="row-label">{profile.name}</span>
      {profile.id && (
        <>
          <button
            className="panel-action"
            onMouseDown={(event) => {
              event.stopPropagation();
              onEditProfile(profile);
            }}
            title="Edit"
            aria-label="Edit"
          >
            <Icon name="edit" />
          </button>
          <button
            className="panel-action"
            onMouseDown={(event) => {
              event.stopPropagation();
              void askDeleteProfile(profile);
            }}
            title="Delete"
            aria-label="Delete"
          >
            <Icon name="close" />
          </button>
        </>
      )}
    </div>
  );

  const renderGroup = (row: Extract<Row, { type: "group" }>) => (
    // Location breadcrumb doubles as the hint that a menu exists here.
    <div
      key={`group:${row.group.id}`}
      className="row is-group"
      style={{ paddingLeft: 6 + INDENT * row.depth }}
      onMouseDown={(event) => {
        if (event.button === 0) toggle(`group:${row.group.id}`);
      }}
      onContextMenu={(event) => openMenu(event, groupMenu(row.group))}
      title={`${describeLocation(groups, row.group.id)} · right-click for options`}
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
    </div>
  );

  return (
    <div className="panel" style={{ flex: 1 }}>
      <div className="panel-header">
        {tabs ? (
          <PanelTabs {...tabs} />
        ) : (
          <div className="panel-title is-session">
            <Icon name="server" />
            Session
          </div>
        )}
        <button
          className={`panel-action panel-power${
            activeTab?.state === "connected" ? " is-connected" : ""
          }`}
          disabled={!activeTab || activeTab.state === "connecting"}
          onClick={() => {
            if (activeTab) toggleSessionConnection(activeTab.info.id);
          }}
          title={
            activeTab
              ? `${POWER_TITLES[activeTab.state]} · ${tabTitle(activeTab)}`
              : "No active session"
          }
          aria-label={activeTab ? POWER_TITLES[activeTab.state] : "Disconnect"}
        >
          {/* Power symbol: an open ring with a bar through the gap. */}
          <Icon name="plug" />
        </button>
        <button
          className="panel-action"
          onClick={onNewSession}
          title="New session"
          aria-label="New session"
        >
          <Icon name="add" />
        </button>
      </div>

      <div className="panel-filter">
        <span className="panel-filter-icon" aria-hidden="true">
          <Icon name="search" />
        </span>
        <input
          value={filter}
          placeholder="Filter sessions"
          aria-label="Filter sessions"
          onChange={(event) => setFilter(event.target.value)}
        />
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
        {filtering && tree.count === 0 && (
          <div className="panel-empty">
            No sessions match “{filter.trim()}”.
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

      {groupDialog &&
        (groupDialog.mode === "create" ? (
          <GroupNameDialog
            title={groupDialog.parentId ? "New Subgroup" : "New Group"}
            location={describeLocation(groups, groupDialog.parentId)}
            submitLabel="Create"
            onSubmit={async (name) => {
              const saved = await upsertGroup({
                id: "",
                name,
                parentId: groupDialog.parentId,
              });
              // A new group is empty, so make sure its parent is open.
              if (saved.parentId) {
                setOpen((prev) => ({
                  ...prev,
                  [`group:${saved.parentId}`]: true,
                }));
              }
              setGroupDialog(null);
            }}
            onCancel={() => setGroupDialog(null)}
          />
        ) : (
          <GroupNameDialog
            title="Rename Group"
            location={describeLocation(groups, groupDialog.group.parentId)}
            initialName={groupDialog.group.name}
            submitLabel="Rename"
            onSubmit={async (name) => {
              await upsertGroup({ ...groupDialog.group, name });
              setGroupDialog(null);
            }}
            onCancel={() => setGroupDialog(null)}
          />
        ))}

      {pendingDelete && (
        <DeleteProfileDialog
          profile={pendingDelete.profile}
          target={describeProfile(pendingDelete.profile)}
          scopedCommands={pendingDelete.scopedCommands}
          onConfirm={() => {
            // Dismiss first so a second Enter cannot re-enter removeProfile
            // while the backend delete is still in flight.
            setPendingDelete(null);
            void removeProfile(pendingDelete.profile.id);
          }}
          onCancel={() => setPendingDelete(null)}
        />
      )}
    </div>
  );
}
