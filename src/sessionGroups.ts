import type { SessionGroup, SessionKind, SessionProfile } from "./types";

/**
 * Per-kind display names, used where an individual kind is named — the
 * Sender's kind-scope description, for instance. The Session panel itself
 * lists groups and sessions without them.
 */
export const KIND_LABELS: Record<SessionKind, string> = {
  ssh: "SSH Sessions",
  sftp: "SFTP Sessions",
  local: "Shell Sessions",
};

/** A group with its nesting depth (0 = top level). */
export interface GroupNode {
  group: SessionGroup;
  depth: number;
}

/**
 * Case-insensitive, locale-aware name order. Every level of the Session tree
 * lists groups first, then profiles, each sorted with this comparator.
 */
export const byName = (a: { name: string }, b: { name: string }) =>
  a.name.localeCompare(b.name, undefined, { sensitivity: "base" });

/**
 * The parent a group is drawn under. A parent that no longer exists (a
 * hand-edited file) is treated as "none" so the group still shows up at the
 * top level instead of silently disappearing with everything in it.
 */
export function effectiveParentId(
  groups: readonly SessionGroup[],
  group: SessionGroup,
): string | null {
  const parentId = group.parentId ?? null;
  if (parentId === null || parentId === group.id) return null;
  return groups.some((g) => g.id === parentId) ? parentId : null;
}

/** The group a profile is drawn in, or null for the top level. */
export function effectiveGroupId(
  groups: readonly SessionGroup[],
  profile: SessionProfile,
): string | null {
  const groupId = profile.groupId ?? null;
  if (groupId === null) return null;
  return groups.some((g) => g.id === groupId) ? groupId : null;
}

/** Direct subgroups of `parentId` (null = top level), sorted by name. */
export function childGroups(
  groups: readonly SessionGroup[],
  parentId: string | null,
): SessionGroup[] {
  return groups
    .filter((g) => effectiveParentId(groups, g) === parentId)
    .sort(byName);
}

/** Every group, depth-first in the order the panel draws them. */
export function flattenGroups(
  groups: readonly SessionGroup[],
): GroupNode[] {
  const out: GroupNode[] = [];
  const seen = new Set<string>();
  const visit = (parentId: string | null, depth: number) => {
    for (const group of childGroups(groups, parentId)) {
      // Defensive against a cycle in a hand-edited file.
      if (seen.has(group.id)) continue;
      seen.add(group.id);
      out.push({ group, depth });
      visit(group.id, depth + 1);
    }
  };
  visit(null, 0);
  return out;
}

/** Names from the top level down to the group, e.g. ["prod", "eu"]. */
export function groupPath(
  groups: readonly SessionGroup[],
  id: string | null,
): string[] {
  const names: string[] = [];
  const seen = new Set<string>();
  let cursor = id;
  while (cursor !== null && !seen.has(cursor)) {
    seen.add(cursor);
    const group = groups.find((g) => g.id === cursor);
    if (!group) break;
    names.unshift(group.name);
    cursor = effectiveParentId(groups, group);
  }
  return names;
}

/**
 * "prod / eu" — where a group or profile lives. Reads inside a sentence
 * ("In {location}"), so the top level names itself with its article.
 */
export function describeLocation(
  groups: readonly SessionGroup[],
  groupId: string | null,
): string {
  const path = groupPath(groups, groupId);
  return path.length > 0 ? path.join(" / ") : "the top level";
}
