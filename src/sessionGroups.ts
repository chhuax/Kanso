import type { SessionGroup, SessionKind, SessionProfile } from "./types";

/**
 * 各会话类型的显示名称，例如 Sender 的类型作用域说明。左侧会话浮层本身
 * 直接展示分组与会话，不使用这些名称。
 */
export const KIND_LABELS: Record<SessionKind, string> = {
  ssh: "SSH Sessions",
  sftp: "SFTP Sessions",
  local: "Shell Sessions",
};

/**
 * 忽略大小写并遵循本地化规则的名称排序。会话列表先展示分组，再展示未分组
 * 会话，两部分都使用这个顺序。
 */
export const byName = (a: { name: string }, b: { name: string }) =>
  a.name.localeCompare(b.name, undefined, { sensitivity: "base" });

/** 返回配置实际所属的分组；顶层会话返回 null。 */
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
