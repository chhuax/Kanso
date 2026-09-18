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

/**
 * Case-insensitive, locale-aware name order. The Session list draws its
 * groups first, then the sessions outside them, each sorted with this.
 */
export const byName = (a: { name: string }, b: { name: string }) =>
  a.name.localeCompare(b.name, undefined, { sensitivity: "base" });

/** The group a profile is drawn in, or null for the top level. */
export function effectiveGroupId(
  groups: readonly SessionGroup[],
  profile: SessionProfile,
): string | null {
  const groupId = profile.groupId ?? null;
  if (groupId === null) return null;
  return groups.some((g) => g.id === groupId) ? groupId : null;
}

/** The groups, A→Z. They are one level deep; there is no tree to walk. */
export function sortedGroups(
  groups: readonly SessionGroup[],
): SessionGroup[] {
  return [...groups].sort(byName);
}
