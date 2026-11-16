-- Kanso 数据库 schema v1。
--
-- 通用约定：
-- * 同步候选表（groups、profiles、snippets）的 id 是 TEXT 形式的 UUID（带连字符小写），
--   带 updated_at（Unix 毫秒）和 deleted（墓碑）。删除时把 deleted 置 1 并更新 updated_at，
--   不物理删除，为将来 iCloud 同步留余地。查询时调用方必须过滤 deleted = 0。
-- * 不建外键约束：会话引用的分组可能已被删除（规则是放回顶层），同步时记录也可能乱序到达；
--   引用一致性由 kanso-store 的代码负责。需要按引用查询的列单独建索引。
-- * 任何表都不存密码、口令、私钥。凭据走 SecretStore，按 profile id 关联，这里不放引用列。
-- * 枚举值存小写 kebab 文本，并用 CHECK 约束限定取值。
-- * 版本号用 PRAGMA user_version，由迁移机制在事务内设置，本文件不要写 user_version。

-- 分组：只有一层，不嵌套。
CREATE TABLE groups (
    id         TEXT    PRIMARY KEY,
    name       TEXT    NOT NULL,
    sort_key   INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL,
    deleted    INTEGER NOT NULL DEFAULT 0
);

-- 会话配置。
-- 类型相关字段放进 config（JSON）：本地、SSH、SFTP 三种类型字段差异很大，
-- 且后续会频繁加字段，用 JSON 列可以免迁移。只有需要查询、排序的公共字段
-- （name、kind、group_id）才单独成列。
-- config 是 kanso_types::ProfileKind 的 serde 序列化。其中的 jump（跳板引用）失效时由代码清除。
CREATE TABLE profiles (
    id         TEXT    PRIMARY KEY,
    name       TEXT    NOT NULL,
    kind       TEXT    NOT NULL CHECK (kind IN ('local', 'ssh', 'sftp')),
    group_id   TEXT    NULL,                 -- 所属分组；NULL 表示顶层
    color      TEXT    NULL,                 -- '#rrggbb'；NULL 表示不着色
    sort_key   INTEGER NOT NULL DEFAULT 0,
    config     TEXT    NOT NULL,             -- JSON
    updated_at INTEGER NOT NULL,
    deleted    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_profiles_group_id ON profiles (group_id);

-- Sender 命令。总数上限 1000 条，由代码保证（store-import.md §4）。
CREATE TABLE snippets (
    id          TEXT    PRIMARY KEY,
    name        TEXT    NOT NULL,
    text        TEXT    NOT NULL,
    line_ending TEXT    NOT NULL CHECK (line_ending IN ('none', 'lf', 'crlf')),
    scope_kind  TEXT    NOT NULL CHECK (scope_kind IN ('profile', 'group', 'kind', 'global')),
    -- profile / group 时是对应的 id；kind 时是 'local' 或 'ssh'；global 时为 NULL。
    scope_ref   TEXT    NULL,
    sort_key    INTEGER NOT NULL DEFAULT 0,
    updated_at  INTEGER NOT NULL,
    deleted     INTEGER NOT NULL DEFAULT 0,
    CHECK ((scope_kind = 'global' AND scope_ref IS NULL)
        OR (scope_kind <> 'global' AND scope_ref IS NOT NULL))
);
CREATE INDEX idx_snippets_scope ON snippets (scope_kind, scope_ref);

-- 命令历史（补全用）。
-- 每执行一条命令只 upsert 一行，写入很小。满 5000 条时按 last_used_at 淘汰最久没用的
-- （不是最早记录的）；超过 500 字符和以空格开头的命令不记录（store-import.md §5）。
-- 不参与同步，所以没有墓碑。
CREATE TABLE history (
    command      TEXT    PRIMARY KEY,
    last_used_at INTEGER NOT NULL,
    use_count    INTEGER NOT NULL DEFAULT 1
) WITHOUT ROWID;
CREATE INDEX idx_history_last_used_at ON history (last_used_at);

-- 键值设置。value 是 JSON，结构自带版本号，读到不认识的字段要忽略（store-import.md §9）。
CREATE TABLE settings (
    key        TEXT    PRIMARY KEY,
    value      TEXT    NOT NULL,             -- JSON
    updated_at INTEGER NOT NULL
);

-- 窗口状态，用于恢复上次的界面。
CREATE TABLE window_state (
    id         TEXT    PRIMARY KEY,
    value      TEXT    NOT NULL,             -- JSON：窗口大小、最大化、面板布局、分屏树
    updated_at INTEGER NOT NULL
);

-- SSH 已知主机，只有 MAS 版使用（Direct 版直接读写 ~/.ssh/known_hosts）。
-- 同一主机每种算法最多一条；用户"接受变更"时先删除该主机的全部行再插入（ssh.md §3）。
CREATE TABLE known_hosts (
    host     TEXT    NOT NULL,
    port     INTEGER NOT NULL,
    key_type TEXT    NOT NULL,
    key      BLOB    NOT NULL,               -- SSH wire 格式公钥
    added_at INTEGER NOT NULL,
    PRIMARY KEY (host, port, key_type)
) WITHOUT ROWID;
