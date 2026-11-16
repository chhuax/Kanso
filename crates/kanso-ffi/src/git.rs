//! Git 面板的阻塞接口；宿主从后台任务调用，代次变更本身不阻塞。

use std::sync::Arc;

use kanso_core::git::{GitDiff, GitPanel, GitSummary};

use crate::ffi_types::{FfiCoreError, FfiMessage};

/// 一个文件的净变化。
#[derive(Clone, uniffi::Record)]
pub struct FfiGitFile {
    /// 相对路径。
    pub path: String,
    /// 原路径。
    pub original: Option<String>,
    /// 界面状态。
    pub status: String,
    /// 已知新增行数。
    pub added: Option<u64>,
    /// 已知删除行数。
    pub deleted: Option<u64>,
}

/// 仓库摘要。
#[derive(uniffi::Record)]
pub struct FfiGitSummary {
    /// 根目录。
    pub root: String,
    /// 分支或游离提交。
    pub branch: String,
    /// 全部改动。
    pub files: Vec<FfiGitFile>,
    /// 已知新增合计。
    pub added: u64,
    /// 已知删除合计。
    pub deleted: u64,
}

impl From<GitSummary> for FfiGitSummary {
    fn from(summary: GitSummary) -> Self {
        Self {
            root: summary.root,
            branch: summary.branch,
            added: summary.added,
            deleted: summary.deleted,
            files: summary
                .files
                .into_iter()
                .map(|file| FfiGitFile {
                    path: file.path,
                    original: file.original,
                    status: file.status,
                    added: file.added,
                    deleted: file.deleted,
                })
                .collect(),
        }
    }
}

/// 一行差异正文。
#[derive(uniffi::Record)]
pub struct FfiGitLine {
    /// 正负或上下文前缀。
    pub kind: String,
    /// 旧版或新版行号。
    pub number: u64,
    /// 正文。
    pub text: String,
}

/// 展开项。
#[derive(uniffi::Record)]
pub struct FfiGitDiff {
    /// 正文行。
    pub lines: Vec<FfiGitLine>,
    /// 无正文时的说明。
    pub notice: Option<FfiMessage>,
    /// 达到显示上限。
    pub truncated: bool,
}

impl From<GitDiff> for FfiGitDiff {
    fn from(diff: GitDiff) -> Self {
        Self {
            lines: diff
                .lines
                .into_iter()
                .map(|line| FfiGitLine {
                    kind: line.kind,
                    number: line.number,
                    text: line.text,
                })
                .collect(),
            notice: diff.notice.as_ref().map(Into::into),
            truncated: diff.truncated,
        }
    }
}

/// 每个窗口持有一个请求上下文。
#[derive(uniffi::Object, Default)]
pub struct KansoGitPanel {
    inner: GitPanel,
}

#[uniffi::export]
impl KansoGitPanel {
    /// 创建空面板，不读文件。
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 作废之前请求。
    pub fn begin(&self) -> u64 {
        self.inner.begin()
    }

    /// 核对结果仍属于当前请求。
    pub fn is_current(&self, request: u64) -> bool {
        self.inner.is_current(request)
    }

    /// 后台读取仓库摘要。
    pub fn read(&self, request: u64, cwd: String) -> Result<Option<FfiGitSummary>, FfiCoreError> {
        self.inner
            .read(request, &cwd)
            .map(|value| value.map(Into::into))
            .map_err(Into::into)
    }

    /// 后台读取一个文件差异。
    pub fn diff(&self, request: u64, path: String) -> Result<Option<FfiGitDiff>, FfiCoreError> {
        self.inner
            .diff(request, &path)
            .map(|value| value.map(Into::into))
            .map_err(Into::into)
    }

    /// 暂存全部并提交。
    pub fn commit(&self, request: u64, message: String) -> Result<(), FfiCoreError> {
        self.inner.commit(request, &message).map_err(Into::into)
    }

    /// 丢弃指定项，或全部改动。
    pub fn discard(&self, request: u64, path: Option<String>) -> Result<(), FfiCoreError> {
        self.inner
            .discard(request, path.as_deref())
            .map_err(Into::into)
    }
}
