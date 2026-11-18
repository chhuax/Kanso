//! `Core` 门面：持有运行时、注入平台 trait，是对外唯一的公开 API。
//!
//! 模块导览：
//! - [`api`]：公开类型与 [`Core`] 的方法签名及契约文档。
//! - `mock`（私有）：纯内存的 mock 实现，`Core::new_mock` 使用。
//! - [`platform`]：宿主注入的平台 trait。
//! - [`validate`]：Profile 保存校验，mock 与真实实现共用。
//! - [`mcp`]：MCP 服务（kanso-mcp-server）使用的契约类型。

pub mod api;
pub mod git;
pub mod mcp;
mod mock;
pub mod platform;
mod real;
pub mod sender;
mod ssh;
mod sshconfig_import;
pub mod terminal_themes;
pub mod validate;

pub use api::{
    CompletionSources, CompletionState, CompletionSuggestions, Core, CoreConfig, CoreError,
    DeleteImpact, DirStream, ImportReport, OpenTarget, SearchQuery, SearchStatus, SessionInfo,
    SshConfigCandidate, SshConfigImportReport, SshConfigScan, XmodemBlock,
};
pub use kanso_complete::{
    Candidate as CompletionCandidate, Category as CompletionCategory,
    DirectoryEntry as CompletionEntry,
};
pub use kanso_license::{Entitlement, EntitlementSource, PurchaseOutcome, PurchaseProvider};
pub use kanso_secret::{SecretKind, SecretStore};
pub use kanso_ssh::HostKeyStore;
pub use platform::{
    AccessGrant, CollectingEventSink, EventSink, FileAccess, FileAccessError, NoopFileAccess,
    NoopNotifier, Notification, Notifier, Platform,
};
pub use validate::validate_profile;
