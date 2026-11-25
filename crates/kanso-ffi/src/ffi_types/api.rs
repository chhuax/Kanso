//! `Core` 公开 API 的参数与返回值类型镜像（不含输入事件与 Profile，它们在各自模块）。

use kanso_core::{
    CoreConfig, DeleteImpact, Entitlement, EntitlementSource, ImportReport, OpenTarget,
    PurchaseOutcome, SearchQuery, SearchStatus, SecretKind, SessionInfo, SshConfigCandidate,
    SshConfigImportReport, SshConfigScan, XmodemBlock,
};
use kanso_types::{CursorShape, DirEntry, SessionId, TerminalOptions};

use super::event::FfiSessionState;
use super::message::FfiMessage;
use super::profile::{FfiLocalProfile, FfiSessionKind};
use super::{FfiCoreError, mirror_enum, parse_id};

/// 光标前输入及当前会话的目录上报。
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiCompletionState {
    /// 原始输入。
    pub input: String,
    /// 输入起点列。
    pub start_column: u16,
    /// 上报路径。
    pub directory: Option<String>,
    /// 上报主机。
    pub directory_host: Option<String>,
}

impl From<kanso_core::CompletionState> for FfiCompletionState {
    fn from(state: kanso_core::CompletionState) -> Self {
        Self {
            input: state.input,
            start_column: state.start_column,
            directory: state.directory,
            directory_host: state.directory_host,
        }
    }
}

/// 有限目录流中的一个条目。
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiCompletionEntry {
    /// 文件名。
    pub name: String,
    /// 是否为目录。
    pub directory: bool,
}

/// 宿主提供的外部来源。
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiCompletionSources {
    /// 可信工作目录。
    pub cwd: Option<String>,
    /// 当前机器家目录。
    pub home: Option<String>,
    /// 是否可以使用当前文件服务。
    pub allow_paths: bool,
    /// 最多两千项目录条目。
    pub entries: Vec<FfiCompletionEntry>,
    /// 本地配置内容。
    pub configs: Vec<String>,
}

impl From<FfiCompletionSources> for kanso_core::CompletionSources {
    fn from(sources: FfiCompletionSources) -> Self {
        Self {
            cwd: sources.cwd,
            home: sources.home,
            allow_paths: sources.allow_paths,
            entries: sources
                .entries
                .into_iter()
                .take(2000)
                .map(|entry| kanso_core::CompletionEntry {
                    name: entry.name,
                    directory: entry.directory,
                })
                .collect(),
            configs: sources.configs,
        }
    }
}

mirror_enum! {
    /// 候选来源类别，由界面本地化。
    FfiCompletionCategory <=> kanso_core::CompletionCategory {
        /// 目录。
        Directory,
        /// 文件。
        File,
        /// 历史。
        History,
        /// Kubernetes。
        Kubernetes,
        /// Docker。
        Docker,
        /// Git。
        Git,
        /// 系统服务与日志。
        Systemd,
    }
}

/// 通过模拟按键接受的一项建议。
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiCompletionCandidate {
    /// 替换文本。
    pub replacement: String,
    /// 来源类别。
    pub category: FfiCompletionCategory,
    /// 光标前删除的码点数。
    pub delete_codepoints: u32,
}

/// 即时候选以及适用的外部请求。
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiCompletionSuggestions {
    /// 最多八项候选。
    pub candidates: Vec<FfiCompletionCandidate>,
    /// 需读取的目录。
    pub directory: Option<String>,
    /// 是否需要本地 kubeconfig。
    pub needs_kube_names: bool,
    /// 是否需要查询可信工作目录。
    pub needs_cwd: bool,
    /// 是否需要查询家目录。
    pub needs_home: bool,
}

impl From<kanso_core::CompletionSuggestions> for FfiCompletionSuggestions {
    fn from(suggestions: kanso_core::CompletionSuggestions) -> Self {
        Self {
            candidates: suggestions
                .candidates
                .into_iter()
                .map(|candidate| FfiCompletionCandidate {
                    replacement: candidate.replacement,
                    category: candidate.category.into(),
                    delete_codepoints: candidate.delete_codepoints,
                })
                .collect(),
            directory: suggestions.directory,
            needs_kube_names: suggestions.needs_kube_names,
            needs_cwd: suggestions.needs_cwd,
            needs_home: suggestions.needs_home,
        }
    }
}

/// `KansoCore.new` 的配置，镜像 `kanso_core::CoreConfig`。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiCoreConfig {
    /// 数据目录（数据库、shell 集成脚本、日志等）。不存在时由 core 创建。
    pub data_dir: String,
    /// 应用版本号。
    pub app_version: String,
    /// 应用 bundle id。
    pub bundle_id: String,
    /// 系统首选语言标识（如 `zh-Hans-CN`），本地 shell 推算 `LANG` 用。
    pub preferred_language: String,
    /// 系统地区代码（如 `CN`）；没有为 `nil`。
    #[uniffi(default = None)]
    pub region: Option<String>,
    /// 默认本地 shell 的命令行；`nil` 取 `$SHELL`。用于测试和将来的设置项。
    #[uniffi(default = None)]
    pub default_shell: Option<String>,
    /// 是否启用 zsh 提示符集成，默认开启。
    #[uniffi(default = true)]
    pub shell_integration: bool,
}

impl From<FfiCoreConfig> for CoreConfig {
    fn from(c: FfiCoreConfig) -> Self {
        CoreConfig {
            data_dir: c.data_dir.into(),
            app_version: c.app_version,
            bundle_id: c.bundle_id,
            preferred_language: c.preferred_language,
            region: c.region,
            default_shell: c.default_shell,
            shell_integration: c.shell_integration,
        }
    }
}

/// 打开会话的目标。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiOpenTarget {
    /// 按已保存的 Profile 打开。
    Profile {
        /// Profile id（UUID 字符串）。
        id: String,
    },
    /// 用默认配置打开本地 shell。
    DefaultLocalShell,
    /// 使用本次配置打开临时本地 shell。
    LocalShell {
        /// 本次启动配置。
        profile: FfiLocalProfile,
    },
    /// 复制标签。
    Duplicate {
        /// 被复制的会话。
        session: u64,
    },
}

impl TryFrom<FfiOpenTarget> for OpenTarget {
    type Error = FfiCoreError;

    fn try_from(t: FfiOpenTarget) -> Result<Self, FfiCoreError> {
        Ok(match t {
            FfiOpenTarget::Profile { id } => OpenTarget::Profile(parse_id(&id, "profile")?),
            FfiOpenTarget::DefaultLocalShell => OpenTarget::DefaultLocalShell,
            FfiOpenTarget::LocalShell { profile } => OpenTarget::LocalShell(profile.try_into()?),
            FfiOpenTarget::Duplicate { session } => {
                OpenTarget::Duplicate(SessionId::from_raw(session))
            }
        })
    }
}

/// 会话摘要。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiSessionInfo {
    /// 会话 id。
    pub id: u64,
    /// 对应的 Profile id（UUID 字符串）；默认本地 shell 为 `None`。
    pub profile: Option<String>,
    /// 会话种类。
    pub kind: FfiSessionKind,
    /// 默认显示名。
    pub name: String,
    /// 用户或 MCP 设置的自定义标签名；`None` 表示使用默认标题。
    pub custom_title: Option<String>,
    /// 当前状态。
    pub state: FfiSessionState,
}

impl From<&SessionInfo> for FfiSessionInfo {
    fn from(s: &SessionInfo) -> Self {
        FfiSessionInfo {
            id: s.id.raw(),
            profile: s.profile.map(|p| p.to_string()),
            kind: s.kind.into(),
            name: s.name.clone(),
            custom_title: s.custom_title.clone(),
            state: (&s.state).into(),
        }
    }
}

mirror_enum! {
    /// 默认光标形状。
    FfiCursorShape <=> CursorShape {
        /// 块状。
        Block,
        /// 下划线。
        Underline,
        /// 竖线。
        Bar,
    }
}

/// 终端显示选项，镜像 `kanso_types::TerminalOptions`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiTerminalOptions {
    /// 回滚行数上限。
    pub scrollback_lines: u32,
    /// 默认光标形状。
    pub cursor_shape: FfiCursorShape,
    /// 默认光标是否闪烁。
    pub cursor_blink: bool,
    /// 是否启用语义着色。
    pub semantic_coloring: bool,
}

impl From<FfiTerminalOptions> for TerminalOptions {
    fn from(o: FfiTerminalOptions) -> Self {
        TerminalOptions {
            scrollback_lines: o.scrollback_lines,
            cursor_shape: o.cursor_shape.into(),
            cursor_blink: o.cursor_blink,
            semantic_coloring: o.semantic_coloring,
        }
    }
}

/// 删除分组会连带删除的内容。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiDeleteImpact {
    /// 将被删除的 Profile 数。
    pub profiles: u32,
    /// 将被删除的 Sender 命令数。
    pub snippets: u32,
}

impl From<DeleteImpact> for FfiDeleteImpact {
    fn from(d: DeleteImpact) -> Self {
        FfiDeleteImpact {
            profiles: d.profiles,
            snippets: d.snippets,
        }
    }
}

/// 搜索请求。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiSearchQuery {
    /// 搜索文本；`regex` 为真时是正则。
    pub text: String,
    /// 是否区分大小写。
    pub case_sensitive: bool,
    /// 是否按正则解释。
    pub regex: bool,
}

impl From<FfiSearchQuery> for SearchQuery {
    fn from(q: FfiSearchQuery) -> Self {
        SearchQuery {
            text: q.text,
            case_sensitive: q.case_sensitive,
            regex: q.regex,
        }
    }
}

/// 搜索结果状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiSearchStatus {
    /// 匹配总数。
    pub total: u32,
    /// 当前匹配项，从 1 开始；没有匹配为 `None`。
    pub current: Option<u32>,
}

impl From<SearchStatus> for FfiSearchStatus {
    fn from(s: SearchStatus) -> Self {
        FfiSearchStatus {
            total: s.total,
            current: s.current,
        }
    }
}

mirror_enum! {
    /// XMODEM 块大小。
    FfiXmodemBlock <=> XmodemBlock {
        /// 128 字节块。
        Bytes128,
        /// 1K 块（XMODEM-1K）。
        Bytes1K,
    }
}

mirror_enum! {
    /// 凭据种类。
    FfiSecretKind <=> SecretKind {
        /// 登录密码。
        Password,
        /// 私钥口令。
        KeyPassphrase,
    }
}

/// `.kanso` 数据文件导入结果。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiImportReport {
    /// 新增条数。
    pub added: u32,
    /// 更新条数。
    pub updated: u32,
    /// 被跳过的条目及原因。
    pub skipped: Vec<FfiMessage>,
    /// 已转换的命令及原因。
    pub converted: Vec<FfiMessage>,
}

#[derive(Clone, Debug, uniffi::Record)]
/// Sender 启动参数，正文与选项在启动时冻结。
pub struct FfiSenderRequest {
    /// 命令正文。
    pub text: String,
    /// 每行追加的行尾。
    pub ending: super::FfiLineEnding,
    /// 固定终端编号；空值表示全部。
    pub target: Option<u64>,
    /// 是否重复发送。
    pub repeating: bool,
    /// 两轮之间的秒数。
    pub interval_seconds: u32,
    /// 计划次数，零为无限。
    pub count: u32,
}

#[derive(Clone, Debug, uniffi::Record)]
/// Sender 的即时进度。
pub struct FfiSenderStatus {
    /// 手动批次是否运行。
    pub manual_running: bool,
    /// 重复任务是否运行。
    pub repeat_running: bool,
    /// 已完成轮次。
    pub rounds: u32,
    /// 计划轮次。
    pub count: u32,
    /// 下一轮倒计时毫秒数。
    pub next_in_ms: u64,
    /// 本轮已发送行数。
    pub sent: u32,
    /// 本轮计划行数。
    pub total: u32,
    /// 状态栏提示。
    pub notices: Vec<FfiMessage>,
    /// 本次更新包含发送失败或跳过的对象。
    pub has_failures: bool,
}

impl From<kanso_core::sender::SenderStatus> for FfiSenderStatus {
    fn from(status: kanso_core::sender::SenderStatus) -> Self {
        Self {
            manual_running: status.manual_running,
            repeat_running: status.repeat_running,
            rounds: status.rounds,
            count: status.count,
            next_in_ms: status.next_in_ms,
            sent: status.sent,
            total: status.total,
            notices: status.notices.iter().map(FfiMessage::from).collect(),
            has_failures: status.has_failures,
        }
    }
}

impl From<&ImportReport> for FfiImportReport {
    fn from(r: &ImportReport) -> Self {
        FfiImportReport {
            added: r.added,
            updated: r.updated,
            skipped: r.skipped.iter().map(Into::into).collect(),
            converted: r.converted.iter().map(Into::into).collect(),
        }
    }
}

/// OpenSSH 配置里扫描出的一个可导入主机。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiSshConfigCandidate {
    /// Host 别名。
    pub alias: String,
    /// 解析后的主机名。
    pub host: String,
    /// 端口。
    pub port: u16,
    /// 用户名。
    pub user: String,
    /// IdentityFile（原样路径）。
    pub identity_file: Option<String>,
    /// ProxyJump 的原始文本。
    pub proxy_jump: Option<String>,
    /// 已存在的同名 Profile id（UUID 字符串）。
    pub existing: Option<String>,
    /// 该条目的警告。
    pub warnings: Vec<FfiMessage>,
}

impl From<&SshConfigCandidate> for FfiSshConfigCandidate {
    fn from(c: &SshConfigCandidate) -> Self {
        FfiSshConfigCandidate {
            alias: c.alias.clone(),
            host: c.host.clone(),
            port: c.port,
            user: c.user.clone(),
            identity_file: c.identity_file.clone(),
            proxy_jump: c.proxy_jump.clone(),
            existing: c.existing.map(|id| id.to_string()),
            warnings: c.warnings.iter().map(Into::into).collect(),
        }
    }
}

/// OpenSSH 配置扫描结果。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiSshConfigScan {
    /// 可导入的主机。
    pub candidates: Vec<FfiSshConfigCandidate>,
    /// 文件级警告。
    pub warnings: Vec<FfiMessage>,
}

impl From<&SshConfigScan> for FfiSshConfigScan {
    fn from(s: &SshConfigScan) -> Self {
        FfiSshConfigScan {
            candidates: s.candidates.iter().map(Into::into).collect(),
            warnings: s.warnings.iter().map(Into::into).collect(),
        }
    }
}

/// OpenSSH 配置导入结果。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiSshConfigImportReport {
    /// 新增的 Profile 数。
    pub added: u32,
    /// 更新的 Profile 数。
    pub updated: u32,
    /// 为跳板机新增的 Profile 数。
    pub jumps_added: u32,
    /// 因多跳 ProxyJump 而忽略的条目数。
    pub multi_hop_ignored: u32,
    /// 警告。
    pub warnings: Vec<FfiMessage>,
}

impl From<&SshConfigImportReport> for FfiSshConfigImportReport {
    fn from(r: &SshConfigImportReport) -> Self {
        FfiSshConfigImportReport {
            added: r.added,
            updated: r.updated,
            jumps_added: r.jumps_added,
            multi_hop_ignored: r.multi_hop_ignored,
            warnings: r.warnings.iter().map(Into::into).collect(),
        }
    }
}

/// 远端文件大小与修改时间。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiFileStat {
    /// 文件大小（字节）；服务器未提供为 `None`。
    pub size: Option<u64>,
    /// 修改时间（Unix 毫秒）；服务器未提供为 `None`。
    pub mtime_ms: Option<i64>,
}

impl From<kanso_types::fs::FileStat> for FfiFileStat {
    fn from(stat: kanso_types::fs::FileStat) -> Self {
        Self {
            size: stat.size,
            mtime_ms: stat.mtime_ms,
        }
    }
}

/// 远端目录条目。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiDirEntry {
    /// 文件名。
    pub name: String,
    /// 完整路径。
    pub path: String,
    /// 是否目录。
    pub is_dir: bool,
    /// 是否符号链接。
    pub is_symlink: bool,
    /// 大小（字节）。
    pub size: u64,
    /// 修改时间（Unix 毫秒）；服务器没给为 `None`。
    pub mtime_ms: Option<i64>,
    /// 权限位；服务器没给为 `None`。
    pub permissions: Option<u32>,
    /// 所有者。
    pub owner: Option<String>,
    /// 所属组。
    pub group: Option<String>,
}

impl From<&DirEntry> for FfiDirEntry {
    fn from(e: &DirEntry) -> Self {
        FfiDirEntry {
            name: e.name.clone(),
            path: e.path.clone(),
            is_dir: e.is_dir,
            is_symlink: e.is_symlink,
            size: e.size,
            mtime_ms: e.mtime_ms,
            permissions: e.permissions,
            owner: e.owner.clone(),
            group: e.group.clone(),
        }
    }
}

mirror_enum! {
    /// 授权来源。
    FfiEntitlementSource <=> EntitlementSource {
        /// License Key。
        LicenseKey,
        /// App Store 购买。
        AppStore,
        /// Setapp 订阅。
        Setapp,
    }
}

/// 一份授权。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiEntitlement {
    /// 来源。
    pub source: FfiEntitlementSource,
}

impl From<Entitlement> for FfiEntitlement {
    fn from(e: Entitlement) -> Self {
        FfiEntitlement {
            source: e.source.into(),
        }
    }
}

/// 购买流程的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiPurchaseOutcome {
    /// 购买成功。
    Purchased {
        /// 新的授权。
        entitlement: FfiEntitlement,
    },
    /// 用户取消。
    Cancelled,
    /// 待处理（例如家长审批）。
    Pending,
}

impl From<PurchaseOutcome> for FfiPurchaseOutcome {
    fn from(o: PurchaseOutcome) -> Self {
        match o {
            PurchaseOutcome::Purchased(e) => FfiPurchaseOutcome::Purchased {
                entitlement: e.into(),
            },
            PurchaseOutcome::Cancelled => FfiPurchaseOutcome::Cancelled,
            PurchaseOutcome::Pending => FfiPurchaseOutcome::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanso_types::event::SessionState;
    use kanso_types::profile::SessionKind;

    #[test]
    fn open_target_parses_ids() {
        let id = kanso_types::ProfileId::new();
        assert_eq!(
            OpenTarget::try_from(FfiOpenTarget::Profile { id: id.to_string() }).unwrap(),
            OpenTarget::Profile(id)
        );
        assert!(OpenTarget::try_from(FfiOpenTarget::Profile { id: "x".into() }).is_err());
        assert_eq!(
            OpenTarget::try_from(FfiOpenTarget::Duplicate { session: 9 }).unwrap(),
            OpenTarget::Duplicate(SessionId::from_raw(9))
        );
    }

    #[test]
    fn session_info_and_license_map() {
        let info = SessionInfo {
            id: SessionId::from_raw(2),
            profile: None,
            kind: SessionKind::Sftp,
            name: "n".into(),
            custom_title: Some("custom".into()),
            state: SessionState::Connected,
        };
        let f = FfiSessionInfo::from(&info);
        assert_eq!(f.id, 2);
        assert_eq!(f.kind, FfiSessionKind::Sftp);
        assert_eq!(f.custom_title.as_deref(), Some("custom"));
        assert_eq!(f.state, FfiSessionState::Connected);

        let e = Entitlement {
            source: EntitlementSource::Setapp,
        };
        assert_eq!(
            FfiPurchaseOutcome::from(PurchaseOutcome::Purchased(e)),
            FfiPurchaseOutcome::Purchased {
                entitlement: FfiEntitlement {
                    source: FfiEntitlementSource::Setapp,
                }
            }
        );
    }
}
