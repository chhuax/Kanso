//! SQLite 表结构与迁移、数据读写、`.kanso` 导入导出。
//!
//! - [`schema`]：打开数据库与迁移；
//! - [`Store`]：仓库层（Profile、分组、Sender 命令、历史、设置、MCP 授权与审计）；
//! - `.kanso` 导入导出：见 [`Store::export`] 与 [`Store::import_file`]。
//!
//! ssh_config 导入（F6）依赖 `kanso-sshconfig`，不在本阶段实现。

mod exchange;
mod history;
mod mcp;
pub mod schema;
mod store;

pub use exchange::{
    APP_MARKER, EXTENSION, ExportSnapshot, FORMAT_VERSION, ImportData, ImportError, ImportOutcome,
    ProfileValidator, with_kanso_extension,
};
pub use history::{HISTORY_CAPACITY, HISTORY_MAX_CHARS, HistoryEntry};
pub use schema::{SCHEMA_VERSION, StoreError, open, open_in_memory};
pub use store::{GroupDeleteImpact, MAX_SNIPPETS, Store, visible_snippets};

#[cfg(test)]
mod tests;
