import { useCallback, useMemo, useState, type MouseEvent } from "react";
import { ask } from "@tauri-apps/plugin-dialog";

import { openSession } from "../../actions";
import * as api from "../../api";
import { importSshConfig } from "../../dataTransfer";
import { byName, effectiveGroupId, sortedGroups } from "../../sessionGroups";
import { useStore } from "../../store";
import {
  colorForSession,
  type SavedCommand,
  type SessionGroup,
  type SessionProfile,
} from "../../types";
import { ContextMenu, type MenuItem } from "../ContextMenu";
import { DeleteProfileDialog } from "../DeleteProfileDialog";
import { GroupNameDialog } from "../GroupNameDialog";
import { Icon } from "../icons";
import { PanelTabs, type PanelTabsProps } from "../PanelTabs";

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

/**
 * Whether a session answers the filter box. It matches what the row shows and
 * what the user knows a host by — the name it was saved under, and the host
 * and user it connects as — since an imported session is often named by its
 * alias rather than by the machine it reaches.
 */
function matchesFilter(profile: SessionProfile, needle: string): boolean {
  return [profile.name, profile.host, profile.username].some((field) =>
    field?.toLowerCase().includes(needle),
  );
}

type Row =
  | {
      type: "group";
      group: SessionGroup;
      /** Sessions the group holds. */
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
  const [filter, setFilter] = useState("");
  /**
   * Explicit open / closed state of the list, keyed `group:<id>`. Anything
   * not in here is at its default — see `isOpen`.
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
  /** The group being renamed, if the name prompt is up. */
  const [groupDialog, setGroupDialog] = useState<SessionGroup | null>(null);

  const closeMenu = useCallback(() => setMenu(null), []);

  const filtering = filter.trim().length > 0;

  // Groups first, then the sessions that are in none, both A→Z. A group is
  // one level deep, so its own members are listed straight under it; nothing
  // is pre-grouped by session kind, and a group holds any of them.
  const tree = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const byGroup = new Map<string | null, SessionProfile[]>();
    for (const profile of profiles) {
      if (needle && !matchesFilter(profile, needle)) continue;
      const groupId = effectiveGroupId(groups, profile);
      byGroup.set(groupId, [...(byGroup.get(groupId) ?? []), profile]);
    }
    for (const members of byGroup.values()) members.sort(byName);

    const rows: Row[] = [];
    let count = 0;
    for (const group of sortedGroups(groups)) {
      const members = byGroup.get(group.id) ?? [];
      // While filtering, a group with no match is dropped and collapse state
      // is ignored so every match is on screen.
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
    const insideProfiles = profiles.filter(
      (p) => effectiveGroupId(groups, p) === group.id,
    );
    const insideIds = new Set(insideProfiles.map((p) => p.id));
    // Everything scoped to the group goes with it: commands of the group and
    // commands of the sessions in it.
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

  // Right-clicking the list is how a session is added: the header's own
  // buttons are gone, and a group is still made from the New Session dialog.
  const panelMenu = (): MenuItem[] => [
    {
      label: "New Session…",
      icon: "add",
      action: onNewSession,
    },
    "separator",
    // The hosts people already `ssh` to, in bulk.
    {
      label: "Import OpenSSH Config…",
      icon: "cloud-download",
      action: () => void importSshConfig(),
    },
  ];

  const groupMenu = (group: SessionGroup): MenuItem[] => [
    {
      label: "Rename Group…",
      icon: "rename",
      action: () => setGroupDialog(group),
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
    // The tooltip doubles as the hint that a menu exists here.
    <div
      key={`group:${row.group.id}`}
      className="row is-group"
      style={{ paddingLeft: 6 }}
      onMouseDown={(event) => {
        if (event.button === 0) toggle(`group:${row.group.id}`);
      }}
      onContextMenu={(event) => openMenu(event, groupMenu(row.group))}
      title={`${row.group.name} · right-click for options`}
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
        {tree.count === 0 && (
          <div className="panel-empty">
            {filtering
              ? `No sessions match “{filter.trim()}”.`
              : "Right-click here to add a session."}
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

      {groupDialog && (
        <GroupNameDialog
          title="Rename Group"
          initialName={groupDialog.name}
          submitLabel="Rename"
          onSubmit={async (name) => {
            await upsertGroup({ ...groupDialog, name });
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
