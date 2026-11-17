# 数据库 schema

SQLite 数据库的表结构说明。技术方案见 `architecture.md` §8，行为规则见 `spec/store-import.md`。
实现位于 `crates/kanso-store`，迁移文件位于 `crates/kanso-store/migrations/`。当前版本：**v3**（v2 给 `history` 加 `hosts` 列，v3 加 MCP 的三张表）。

## 1. 通用约定

- **同步候选表**（`groups`、`profiles`、`snippets`）：
  - `id` 是 `TEXT` 形式的 UUID（带连字符小写）；
  - `updated_at INTEGER NOT NULL`，Unix 毫秒；
  - `deleted INTEGER NOT NULL DEFAULT 0`，墓碑：删除时置 1 并更新 `updated_at`，不物理删除；
  - 为将来 iCloud 同步留余地。**查询时调用方必须过滤 `deleted = 0`。**
- **不建外键约束**：会话引用的分组可能已被删除（规则是放回顶层，`store-import.md` §3），同步时记录也可能乱序到达。引用一致性由 kanso-store 的代码负责；需要按引用查询的列建索引。
- **任何表都不存密码、口令、私钥**。凭据走 SecretStore，按 profile id 关联，数据库里没有引用列。
- 枚举值存小写 kebab 文本，并用 `CHECK` 约束限定取值。
- 版本号使用 `PRAGMA user_version`，不另建 schema_version 表。

## 2. 表

### 2.1 `groups`（`store-import.md` §1、§3）

| 列 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `id` | TEXT | PRIMARY KEY | 分组 UUID |
| `name` | TEXT | NOT NULL | 显示名 |
| `sort_key` | INTEGER | NOT NULL DEFAULT 0 | 排序键 |
| `updated_at` | INTEGER | NOT NULL | 最后修改时间（Unix 毫秒） |
| `deleted` | INTEGER | NOT NULL DEFAULT 0 | 墓碑 |

### 2.2 `profiles`（`store-import.md` §1、§3）

| 列 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `id` | TEXT | PRIMARY KEY | 会话 UUID |
| `name` | TEXT | NOT NULL | 显示名 |
| `kind` | TEXT | NOT NULL，CHECK IN ('local','ssh','sftp') | 会话类型 |
| `group_id` | TEXT | NULL | 所属分组；NULL 表示顶层，指向的分组可能已不存在 |
| `color` | TEXT | NULL | `#rrggbb`；NULL 表示不着色 |
| `sort_key` | INTEGER | NOT NULL DEFAULT 0 | 排序键 |
| `config` | TEXT | NOT NULL | JSON，`kanso_types::ProfileKind` 的 serde 序列化 |
| `updated_at` | INTEGER | NOT NULL | 最后修改时间 |
| `deleted` | INTEGER | NOT NULL DEFAULT 0 | 墓碑 |

索引：`idx_profiles_group_id (group_id)`。

类型相关字段放进 JSON 列，是因为三种类型字段差异很大、后续会频繁加字段，JSON 列免迁移；只有需要查询的公共字段（`name`、`kind`、`group_id`）才单独成列。`config` 内的 `jump`（跳板引用）失效时由代码清除。`kind` 列与 `config` 的类型标签必须一致，由写入代码保证。

### 2.3 `snippets`（`store-import.md` §4）

| 列 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `id` | TEXT | PRIMARY KEY | 命令 UUID |
| `name` | TEXT | NOT NULL | 名称 |
| `text` | TEXT | NOT NULL | 发送的文本 |
| `line_ending` | TEXT | NOT NULL，CHECK IN ('none','lf','crlf') | 行尾 |
| `scope_kind` | TEXT | NOT NULL，CHECK IN ('profile','group','kind','global') | 作用域种类 |
| `scope_ref` | TEXT | NULL | profile / group 的 id，或 `local` / `ssh`；global 时为 NULL |
| `sort_key` | INTEGER | NOT NULL DEFAULT 0 | 排序键 |
| `updated_at` | INTEGER | NOT NULL | 最后修改时间 |
| `deleted` | INTEGER | NOT NULL DEFAULT 0 | 墓碑 |

表级 CHECK：`scope_kind = 'global'` 时 `scope_ref IS NULL`，否则 `scope_ref IS NOT NULL`。
索引：`idx_snippets_scope (scope_kind, scope_ref)`。总数上限 1000 条由代码保证。

### 2.4 `history`（`store-import.md` §5）

| 列 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `command` | TEXT | PRIMARY KEY | 命令文本 |
| `last_used_at` | INTEGER | NOT NULL | 最近使用时间 |
| `use_count` | INTEGER | NOT NULL DEFAULT 1 | 使用次数 |
| `hosts` | TEXT | NOT NULL DEFAULT '[]' | 使用过该命令的主机名 JSON 数组；同文本共用一条记录 |

`WITHOUT ROWID`；索引：`idx_history_last_used_at (last_used_at)`。每执行一条命令只 upsert 一行；满 5000 条时按 `last_used_at` 淘汰最久没用的；超过 500 字符和以空格开头的命令不记录。不参与同步，没有墓碑。文件权限限制见 §4。

### 2.5 `settings`（`store-import.md` §9）

| 列 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `key` | TEXT | PRIMARY KEY | 设置键 |
| `value` | TEXT | NOT NULL | JSON，结构自带版本号；读到不认识的字段要忽略 |
| `updated_at` | INTEGER | NOT NULL | 最后修改时间 |

### 2.6 `window_state`（`store-import.md` §1）

| 列 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `id` | TEXT | PRIMARY KEY | 窗口标识 |
| `value` | TEXT | NOT NULL | JSON：窗口大小、最大化、面板布局、分屏树 |
| `updated_at` | INTEGER | NOT NULL | 最后修改时间 |

### 2.7 `known_hosts`（`ssh.md` §3）

| 列 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `host` | TEXT | PK 之一，NOT NULL | 主机名 |
| `port` | INTEGER | PK 之一，NOT NULL | 端口 |
| `key_type` | TEXT | PK 之一，NOT NULL | 密钥算法 |
| `key` | BLOB | NOT NULL | SSH wire 格式公钥 |
| `added_at` | INTEGER | NOT NULL | 记录时间 |

`WITHOUT ROWID`。只有 MAS 版使用（Direct 版直接读写 `~/.ssh/known_hosts`）。同一主机每种算法最多一条；用户“接受变更”时先删除该主机的全部行再插入。

### 2.8 MCP 的本机专用表（v3，`spec/mcp.md` §2.2、§6）

下面三张表都是**本机专用表**：不是同步候选表，没有 `updated_at` / `deleted` 墓碑，不进入 `.kanso` 导出，也不参与将来的同步。MCP 授权必须由用户在这台机器上亲手开启，导入文件或同步都不能悄悄给 AI 开权限。

`mcp_grants`：会话的访问级别。**没有记录即"关闭"**，所以新建、导入、复制出来的会话天然是关闭。

| 列 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `profile_id` | TEXT | PRIMARY KEY | 会话 UUID |
| `level` | TEXT | NOT NULL，CHECK `read-only` / `full` | 访问级别 |

删除会话（含删除分组连带删除的会话）时，在同一事务里**物理删除**对应行；墓碑复活的会话因此回到关闭。读取时与 `profiles` 联查，已删除会话一律按关闭处理。

`mcp_local`：MCP 的本机键值设置，目前只有 `enabled`（`'1'` / `'0'`，缺省为关闭）。不放进 `settings`，因为 `settings` 会随 `.kanso` 导出。

| 列 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `key` | TEXT | PRIMARY KEY | 键 |
| `value` | TEXT | NOT NULL | 值 |

`mcp_audit`：工具调用审计。只记参数摘要，**不记**终端输出、执行输出和文件内容。

| 列 | 类型 | 约束 | 含义 |
|---|---|---|---|
| `id` | INTEGER | PRIMARY KEY AUTOINCREMENT | 自增 id，分页与"保留最新 N 条"都按它 |
| `at` | INTEGER | NOT NULL | 调用开始时间（Unix 毫秒） |
| `client` | TEXT | NOT NULL | 客户端自报名称 |
| `mcp_session` | TEXT | NOT NULL | MCP 会话编号 |
| `tool` | TEXT | NOT NULL | 工具名；连接阶段被拒绝时为空串 |
| `profile_id` | TEXT | NULL | 目标会话 UUID；会话删除后保留原值 |
| `session_id` | INTEGER | NULL | 运行时 SessionId，只在同一次运行内有意义 |
| `args` | TEXT | NOT NULL | 参数摘要 |
| `error_code` | TEXT | NULL | 错误码；NULL 表示成功 |
| `duration_ms` | INTEGER | NOT NULL | 耗时 |
| `bytes` | INTEGER | NOT NULL | 返回的数据大小 |

索引：`idx_mcp_audit_at (at)`、`idx_mcp_audit_profile (profile_id)`。保留 30 天且最多 10000 条，先到者为准：先按 `at` 删过期的，再按 `id` 只保留最新 10000 条（按 `id` 而不是 `at`，因为 `at` 来自调用方时钟，可能回拨）。

## 3. 迁移规则

- 版本号存放在 `PRAGMA user_version`；新库为 0，每个迁移完成后设为该迁移的版本号。
- 迁移文件为 `migrations/NNNN_*.sql`，版本从 1 连续递增，在 `schema.rs` 的 `MIGRATIONS` 中登记（单元测试守护）。
- **只追加新迁移文件**；已发布的迁移文件永不修改。
- **版本高于程序已知最高版本的库拒绝打开**（`StoreError::SchemaTooNew`），且不做任何修改。
- **每个迁移一个事务**：SQL 与 `user_version` 更新同属一个事务，失败则回滚该步。
- 迁移 SQL 里不要写 `PRAGMA user_version`，也不要写事务控制语句。

## 4. 连接与文件

- 打开时设置 `journal_mode = WAL`（读写互不阻塞）、`synchronous = NORMAL`（WAL 下仍保证崩溃一致性）、`busy_timeout = 5000`（偶发锁竞争不直接报错）。
- Unix 上数据库文件及 `-wal`、`-shm` 文件权限为 `0600`（命令历史可能含敏感内容，`store-import.md` §5、§8）。
