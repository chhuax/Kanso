//! 数据库打开与 schema 迁移。
//!
//! 版本号存放在 `PRAGMA user_version`：SQLite 原生支持，且与迁移 SQL 在同一事务内原子更新，
//! 不需要另建 schema_version 表。表结构说明见 `docs/schema.md`。

use std::fmt;
use std::path::Path;

use rusqlite::Connection;

/// 一次 schema 迁移。
pub struct Migration {
    /// 迁移完成后的 `user_version`；从 1 开始连续递增。
    pub version: u32,
    /// 迁移 SQL。不要在其中修改 `user_version`，由 [`migrate`] 统一设置。
    pub sql: &'static str,
}

/// 全部迁移，按版本升序。
///
/// 只能在末尾追加；已发布的迁移文件永不修改，否则已升级的库与新建的库会出现分叉。
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("../migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        sql: include_str!("../migrations/0002_history_hosts.sql"),
    },
];

/// 当前程序支持的最高 schema 版本。
pub const SCHEMA_VERSION: u32 = MIGRATIONS[MIGRATIONS.len() - 1].version;

/// 存储层错误。
#[derive(Debug)]
pub enum StoreError {
    /// SQLite 返回的错误。
    Sqlite(rusqlite::Error),
    /// 文件系统错误（创建目录、设置权限）。
    Io(std::io::Error),
    /// 数据库由更新版本的程序写过，当前程序不能打开。
    SchemaTooNew {
        /// 数据库里的版本号。
        found: u32,
        /// 当前程序支持的最高版本号。
        supported: u32,
    },
    /// Sender 命令已达 1000 条上限，拒绝新增（store-import.md §4）；更新已有的命令不受限。
    SnippetLimitReached,
    /// 数据不合法（例如设置值不是合法 JSON、导出序列化失败）。`detail` 为英文技术细节。
    InvalidData {
        /// 技术细节。
        detail: String,
    },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Sqlite(e) => write!(f, "sqlite error: {e}"),
            StoreError::Io(e) => write!(f, "io error: {e}"),
            StoreError::SchemaTooNew { found, supported } => write!(
                f,
                "database schema version {found} is newer than supported version {supported}"
            ),
            StoreError::SnippetLimitReached => f.write_str("snippet limit reached"),
            StoreError::InvalidData { detail } => write!(f, "invalid data: {detail}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Sqlite(e) => Some(e),
            StoreError::Io(e) => Some(e),
            StoreError::SchemaTooNew { .. }
            | StoreError::SnippetLimitReached
            | StoreError::InvalidData { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sqlite(e)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}

/// 把数据库迁移到最新版本，返回迁移后的版本号。
///
/// 数据库版本高于 [`SCHEMA_VERSION`] 时拒绝，且不做任何修改：新版本程序写过的库，
/// 旧程序不能乱动。每个待执行的迁移在单独的事务里执行，并在同一事务内设置 `user_version`，
/// 失败时只回滚当前这一步。
pub fn migrate(conn: &mut Connection) -> Result<u32, StoreError> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }
    for m in MIGRATIONS.iter().filter(|m| m.version > current) {
        let tx = conn.transaction()?;
        tx.execute_batch(m.sql)?;
        // PRAGMA 不支持参数绑定；version 来自编译期常量，直接格式化是安全的。
        tx.execute_batch(&format!("PRAGMA user_version = {}", m.version))?;
        tx.commit()?;
    }
    Ok(SCHEMA_VERSION)
}

/// 打开（必要时创建）数据库并迁移到最新版本。
///
/// 父目录不存在时会创建。Unix 上数据库文件及其 `-wal`、`-shm` 文件权限设为 `0600`，
/// 因为命令历史可能含敏感内容（store-import.md §5、§8）。
pub fn open(path: &Path) -> Result<Connection, StoreError> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    precreate_private(path)?;
    let mut conn = Connection::open(path)?;
    // WAL：读写互不阻塞；
    // synchronous = NORMAL：WAL 模式下仍能保证崩溃后数据库一致（最多丢最后几次提交）；
    // busy_timeout：偶发锁竞争时等待而不是直接报 SQLITE_BUSY。
    // journal_mode 会返回一行结果，必须用 query_row 读取，不能用 execute。
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    debug_assert!(mode.eq_ignore_ascii_case("wal"));
    conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;")?;
    migrate(&mut conn)?;
    restrict_permissions(path)?;
    Ok(conn)
}

/// 打开内存数据库并迁移到最新版本，供测试使用。
pub fn open_in_memory() -> Result<Connection, StoreError> {
    let mut conn = Connection::open_in_memory()?;
    migrate(&mut conn)?;
    Ok(conn)
}

/// 数据库文件不存在时，先以 0600 创建空文件，再交给 SQLite 打开。
///
/// 不能等 SQLite 自己建文件再 chmod：那样从创建到收紧之间文件按 umask 的权限存在。
/// 另外 SQLite 的 unix VFS 创建 `-wal`、`-shm` 时沿用主库文件的权限位，
/// 所以主库一开始就是 0600，附属文件之后被重建时也是 0600。
/// 空文件对 SQLite 来说就是合法的空数据库。
#[cfg(unix)]
fn precreate_private(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::OpenOptionsExt;

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(not(unix))]
fn precreate_private(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

/// Unix 上把数据库文件及 WAL 附属文件设为 0600；其他平台为空操作。
///
/// 新库已由 [`precreate_private`] 以 0600 创建，这里兜底处理已存在、权限被放宽的旧文件。
/// `-wal`、`-shm` 不存在（例如尚未产生）时跳过。
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;

    let base = path.as_os_str().to_owned();
    for suffix in ["", "-wal", "-shm"] {
        let mut p = base.clone();
        p.push(suffix);
        match std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLES: [&str; 7] = [
        "groups",
        "profiles",
        "snippets",
        "history",
        "settings",
        "window_state",
        "known_hosts",
    ];

    fn user_version(conn: &Connection) -> u32 {
        conn.query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn migrations_are_contiguous_from_one() {
        for (i, m) in MIGRATIONS.iter().enumerate() {
            assert_eq!(m.version as usize, i + 1);
        }
    }

    #[test]
    fn fresh_database_is_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("sub/kanso.db")).unwrap();
        assert_eq!(user_version(&conn), SCHEMA_VERSION);
        for t in TABLES {
            let n: u32 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "missing table {t}");
        }
    }

    #[test]
    fn open_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kanso.db");
        {
            let conn = open(&path).unwrap();
            conn.execute(
                "INSERT INTO groups (id, name, updated_at) VALUES ('g', 'n', 1)",
                [],
            )
            .unwrap();
        }
        let conn = open(&path).unwrap();
        assert_eq!(user_version(&conn), SCHEMA_VERSION);
        let n: u32 = conn
            .query_row("SELECT count(*) FROM groups", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn version_one_upgrade_preserves_history() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("kanso.db");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(MIGRATIONS[0].sql).unwrap();
        connection.execute_batch("PRAGMA user_version = 1; INSERT INTO history(command, last_used_at, use_count) VALUES ('git status', 42, 3)").unwrap();
        drop(connection);
        let connection = open(&path).unwrap();
        let history: (String, i64, i64, String) = connection
            .query_row(
                "SELECT command, last_used_at, use_count, hosts FROM history",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(history, ("git status".into(), 42, 3, "[]".into()));
        assert_eq!(user_version(&connection), SCHEMA_VERSION);
    }

    #[test]
    fn newer_schema_is_rejected_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kanso.db");
        {
            let conn = open(&path).unwrap();
            conn.execute(
                "INSERT INTO groups (id, name, updated_at) VALUES ('g', 'n', 1)",
                [],
            )
            .unwrap();
            conn.execute_batch("PRAGMA user_version = 99").unwrap();
        }
        match open(&path) {
            Err(StoreError::SchemaTooNew { found, supported }) => {
                assert_eq!((found, supported), (99, SCHEMA_VERSION));
            }
            other => panic!("unexpected result: {other:?}"),
        }
        let conn = Connection::open(&path).unwrap();
        assert_eq!(user_version(&conn), 99);
        let n: u32 = conn
            .query_row("SELECT count(*) FROM groups", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn check_constraints_hold() {
        let conn = open_in_memory().unwrap();
        let bad_kind = conn.execute(
            "INSERT INTO profiles (id, name, kind, config, updated_at) VALUES ('p', 'n', 'ftp', '{}', 1)",
            [],
        );
        assert!(bad_kind.is_err());
        let bad_ending = conn.execute(
            "INSERT INTO snippets (id, name, text, line_ending, scope_kind, updated_at) \
             VALUES ('s', 'n', 't', 'cr', 'global', 1)",
            [],
        );
        assert!(bad_ending.is_err());
        let global_with_ref = conn.execute(
            "INSERT INTO snippets (id, name, text, line_ending, scope_kind, scope_ref, updated_at) \
             VALUES ('s', 'n', 't', 'lf', 'global', 'x', 1)",
            [],
        );
        assert!(global_with_ref.is_err());
        let group_without_ref = conn.execute(
            "INSERT INTO snippets (id, name, text, line_ending, scope_kind, updated_at) \
             VALUES ('s', 'n', 't', 'lf', 'group', 1)",
            [],
        );
        assert!(group_without_ref.is_err());
        conn.execute(
            "INSERT INTO snippets (id, name, text, line_ending, scope_kind, updated_at) \
             VALUES ('s', 'n', 't', 'lf', 'global', 1)",
            [],
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn database_file_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kanso.db");
        let conn = open(&path).unwrap();
        conn.execute(
            "INSERT INTO groups (id, name, updated_at) VALUES ('g', 'n', 1)",
            [],
        )
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
