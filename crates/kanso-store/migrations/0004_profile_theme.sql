-- schema v4：会话级的终端配色覆盖。
--
-- theme 是 Profile 顶层字段：`NULL` 表示跟随全局浅/深外观设置，
-- 非 NULL 时存配色 id（如 `xshell`），该会话固定用这套配色，不受外观切换影响。
-- 与 color（标签色）一样是可导出的字段，所以进入 profiles 表并随 .kanso 走。
-- ALTER TABLE 只新增可空列，旧库升级后所有已有会话的 theme 都是 NULL，行为不变。

ALTER TABLE profiles ADD COLUMN theme TEXT NULL;
