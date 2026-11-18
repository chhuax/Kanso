//! Git 改动面板的纯逻辑（docs/spec/filer.md §10）：宽度与显隐、跟随哪个标签、刷新节流、
//! 面板状态机（摘要 / 展开项 / 差异 / 错误 / 写操作）、差异行的展示数据（行内高亮、跳过的行数）。
//!
//! 数据来自 `kanso_core::git`（与 mac 版经 FFI 调用的是同一份实现）；这里用自己的普通数据类型
//! 接住它，便于在三个平台上单元测试。不依赖窗口系统。

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;

use kanso_core::git::{GitDiff, GitSummary};
use kanso_types::SessionId;
use kanso_types::message::Message;

use crate::app::filer_layout::MIN_TERMINAL_WIDTH;
use crate::app::filer_state::cwd_host_is_local;
use crate::app::tab::{TabConnection, TabItem, TabKind};

/// 初始宽度（D8）。
pub const DEFAULT_WIDTH: f32 = 480.0;
/// 允许的最小宽度。
pub const MIN_WIDTH: f32 = 280.0;
/// 允许的最大宽度。
pub const MAX_WIDTH: f32 = 760.0;
/// 面板被挤到比这更窄就干脆不显示。
const MIN_USABLE: f32 = 200.0;
/// 提交说明的字节上限（filer.md §10.5）。
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;
/// 自动刷新的最小间隔（毫秒）：命令连续结束时合并成一次。
pub const MIN_REFRESH_INTERVAL_MS: u64 = 800;

// ---- 布局 ----

/// 面板显隐与宽度。D8（2026-11-18 用户决定照 mac 版）：宽度重启后记住，显隐不记；首次默认 480。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GitLayout {
    /// 是否显示。
    pub visible: bool,
    /// 用户调整后的宽度。
    pub width: f32,
}

impl Default for GitLayout {
    fn default() -> Self {
        GitLayout {
            visible: false,
            width: DEFAULT_WIDTH,
        }
    }
}

impl GitLayout {
    /// 以保存的宽度起步（启动时恢复）：夹进 280–760，不是有限数值就用默认 480。
    pub fn with_saved_width(saved: f32) -> GitLayout {
        let base = GitLayout::default();
        GitLayout {
            width: base.dragged(saved),
            ..base
        }
    }

    /// 拖动时的新宽度：夹进 280–760。
    pub fn dragged(&self, width: f32) -> f32 {
        if width.is_finite() {
            width.clamp(MIN_WIDTH, MAX_WIDTH)
        } else {
            self.width
        }
    }

    /// 拖动：面板在右侧，鼠标向左移动让面板变宽。
    pub fn drag_width(&self, start_width: f32, start_x: f32, x: f32) -> f32 {
        self.dragged(start_width - (x - start_x))
    }

    /// 实际占用宽度：不显示为 0；`available` 是 Filer 右侧的剩余宽度，
    /// 放不下时面板让位，保证终端至少 [`MIN_TERMINAL_WIDTH`]；挤得太窄就不显示。
    pub fn effective_width(&self, available: f32) -> f32 {
        if !self.visible {
            return 0.0;
        }
        let room = (available - MIN_TERMINAL_WIDTH).max(0.0);
        let w = self.width.min(room);
        if w < MIN_USABLE { 0.0 } else { w }
    }
}

// ---- 跟随哪个标签 ----

/// 面板读取 Git 数据的对象：一个已连接本地 shell 的当前工作目录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitTarget {
    /// 会话。
    pub session: SessionId,
    /// shell 当前目录（不是 Filer 里浏览的目录）。
    pub cwd: String,
}

/// 活动标签对应的读取对象；SSH、纯 SFTP、未连接、已结束、目录未知或目录属于别的主机都是 `None`。
pub fn target_for(item: Option<&TabItem>, local_names: &[&str]) -> Option<GitTarget> {
    let item = item?;
    if item.kind != TabKind::Local || !matches!(item.connection, TabConnection::Connected) {
        return None;
    }
    let cwd = item.cwd.as_deref().filter(|c| !c.is_empty())?;
    if !cwd_host_is_local(item.cwd_host.as_deref(), local_names) {
        return None;
    }
    Some(GitTarget {
        session: item.id,
        cwd: cwd.to_owned(),
    })
}

// ---- 刷新节流 ----

/// 自动刷新的决定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Throttle {
    /// 现在就刷新。
    Now,
    /// 过这么多毫秒后刷新一次（期间的重复请求合并进这一次）。
    After(u64),
    /// 已有一次待执行的刷新，丢弃本次。
    Skip,
}

/// 刷新节流：用户点击立即执行；自动触发（命令结束、位置变化）在最小间隔内合并。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefreshThrottle {
    last_start: Option<u64>,
    pending: bool,
}

impl RefreshThrottle {
    /// 登记一次刷新请求。`manual` 为用户点击。
    pub fn request(&mut self, now_ms: u64, manual: bool) -> Throttle {
        if manual {
            self.pending = false;
            return Throttle::Now;
        }
        match self.last_start {
            Some(last) if now_ms.saturating_sub(last) < MIN_REFRESH_INTERVAL_MS => {
                if self.pending {
                    Throttle::Skip
                } else {
                    self.pending = true;
                    Throttle::After(MIN_REFRESH_INTERVAL_MS - now_ms.saturating_sub(last))
                }
            }
            _ => Throttle::Now,
        }
    }

    /// 一次刷新开始了。
    pub fn started(&mut self, now_ms: u64) {
        self.last_start = Some(now_ms);
        self.pending = false;
    }

    /// 延后的定时器到点：返回是否仍需刷新（期间被手动刷新取代则不需要）。
    pub fn take_pending(&mut self) -> bool {
        std::mem::take(&mut self.pending)
    }
}

// ---- 数据 ----

/// 一个改动文件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    /// 仓库相对路径。
    pub path: String,
    /// 重命名 / 复制来源。
    pub original: Option<String>,
    /// 状态标记（M A D R C U ! T）。
    pub status: String,
    /// 已知新增行数。
    pub added: Option<u64>,
    /// 已知删除行数。
    pub deleted: Option<u64>,
}

/// 仓库摘要。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// 工作区根目录。
    pub root: String,
    /// 分支或游离提交。
    pub branch: String,
    /// 改动列表。
    pub files: Vec<FileRow>,
    /// 总新增。
    pub added: u64,
    /// 总删除。
    pub deleted: u64,
}

impl Summary {
    /// 从核心摘要转换。
    pub fn from_core(s: &GitSummary) -> Summary {
        Summary {
            root: s.root.clone(),
            branch: s.branch.clone(),
            files: s
                .files
                .iter()
                .map(|f| FileRow {
                    path: f.path.clone(),
                    original: f.original.clone(),
                    status: f.status.clone(),
                    added: f.added,
                    deleted: f.deleted,
                })
                .collect(),
            added: s.added,
            deleted: s.deleted,
        }
    }

    /// 仓库名称（根目录最后一段）。
    pub fn repo_name(&self) -> String {
        last_component(&self.root)
    }
}

/// 路径最后一段（兼容两种分隔符，忽略末尾分隔符）。
pub fn last_component(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_owned()
}

/// 差异行类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffKind {
    /// 新增。
    Added,
    /// 删除。
    Removed,
    /// 上下文。
    Context,
}

/// 差异正文一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    /// 类别。
    pub kind: DiffKind,
    /// 行号（新增与上下文为新版本，删除为旧版本）。
    pub number: u64,
    /// 正文。
    pub text: String,
}

/// 一个文件的差异。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffData {
    /// 正文行。
    pub lines: Vec<DiffLine>,
    /// 无正文或需要说明时的消息。
    pub notice: Option<Message>,
    /// 达到上限被截断。
    pub truncated: bool,
}

impl DiffData {
    /// 从核心差异转换。
    pub fn from_core(d: &GitDiff) -> DiffData {
        DiffData {
            lines: d
                .lines
                .iter()
                .map(|l| DiffLine {
                    kind: match l.kind.as_str() {
                        "+" => DiffKind::Added,
                        "-" => DiffKind::Removed,
                        _ => DiffKind::Context,
                    },
                    number: l.number,
                    text: l.text.clone(),
                })
                .collect(),
            notice: d.notice.clone(),
            truncated: d.truncated,
        }
    }
}

// ---- 差异行展示 ----

/// 一行的展示数据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRow {
    /// 类别。
    pub kind: DiffKind,
    /// 行号。
    pub number: u64,
    /// 正文。
    pub text: String,
    /// 与上一行之间被跳过的行数（大于 0 时显示分隔）。
    pub gap: u64,
    /// 行内变化的字节范围（成对的删除 / 新增行）。
    pub highlight: Option<Range<usize>>,
}

/// 两行文字的变化范围：去掉共同前缀与共同后缀，各自剩下的字节范围。
pub fn changed_ranges(old: &str, new: &str) -> (Range<usize>, Range<usize>) {
    let o: Vec<(usize, char)> = old.char_indices().collect();
    let n: Vec<(usize, char)> = new.char_indices().collect();
    let shared = o.len().min(n.len());
    let mut prefix = 0;
    while prefix < shared && o[prefix].1 == n[prefix].1 {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < shared - prefix && o[o.len() - suffix - 1].1 == n[n.len() - suffix - 1].1 {
        suffix += 1;
    }
    let range = |v: &[(usize, char)], text: &str| {
        let start = v.get(prefix).map_or(text.len(), |c| c.0);
        let end = if suffix == 0 {
            text.len()
        } else {
            v[v.len() - suffix].0
        };
        start..end.max(start)
    };
    (range(&o, old), range(&n, new))
}

/// 把差异正文变成展示行：连续的删除块与随后的新增块按顺序两两配对做行内高亮；
/// 计算每行前面被跳过的行数（按新版本行号）。
pub fn diff_rows(lines: &[DiffLine]) -> Vec<DiffRow> {
    let mut rows: Vec<DiffRow> = lines
        .iter()
        .map(|l| DiffRow {
            kind: l.kind,
            number: l.number,
            text: l.text.clone(),
            gap: 0,
            highlight: None,
        })
        .collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind != DiffKind::Removed {
            i += 1;
            continue;
        }
        let rs = i;
        while i < lines.len() && lines[i].kind == DiffKind::Removed {
            i += 1;
        }
        let as_ = i;
        while i < lines.len() && lines[i].kind == DiffKind::Added {
            i += 1;
        }
        for k in 0..(as_ - rs).min(i - as_) {
            let (a, b) = changed_ranges(&lines[rs + k].text, &lines[as_ + k].text);
            rows[rs + k].highlight = Some(a);
            rows[as_ + k].highlight = Some(b);
        }
    }
    let mut next_new: i64 = 1;
    let mut delta: i64 = 0;
    for (row, line) in rows.iter_mut().zip(lines) {
        let number = line.number as i64;
        let new_number = if line.kind == DiffKind::Removed {
            number + delta
        } else {
            number
        };
        row.gap = (new_number - next_new).max(0) as u64;
        next_new = new_number + i64::from(line.kind != DiffKind::Removed);
        match line.kind {
            DiffKind::Added => delta += 1,
            DiffKind::Removed => delta -= 1,
            DiffKind::Context => {}
        }
    }
    rows
}

/// 行首的正负标记。
pub fn kind_marker(kind: DiffKind) -> &'static str {
    match kind {
        DiffKind::Added => "+",
        DiffKind::Removed => "-",
        DiffKind::Context => " ",
    }
}

// ---- 面板状态 ----

/// 面板主体显示什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmptyState {
    /// 有改动列表，不是空状态。
    None,
    /// 正在读取。
    Loading,
    /// 不是可检测的目录（无仓库、SSH、SFTP、无标签、会话结束）。
    Unsupported,
    /// 仓库干净。
    Clean,
}

/// 展开 / 收起的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    /// 忙碌或条目不存在：不处理。
    Ignored,
    /// 已收起。
    Collapsed,
    /// 已展开，需要读取差异。
    Expand,
}

/// 丢弃确认框的种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscardKind {
    /// 新增、未跟踪或复制出的内容：永久删除。
    NewContent,
    /// 已跟踪内容（含重命名）：暂存与未暂存编辑都丢失。
    Tracked,
    /// 全部。
    All,
}

/// 丢弃单个文件的确认种类。
pub fn discard_kind(file: &FileRow) -> DiscardKind {
    if matches!(file.status.as_str(), "A" | "U" | "C") {
        DiscardKind::NewContent
    } else {
        DiscardKind::Tracked
    }
}

/// 面板的数据状态。异步结果由视图按请求代次过滤后再交给这里。
#[derive(Clone, Debug, Default)]
pub struct GitModel {
    /// 当前读取对象。
    pub target: Option<GitTarget>,
    /// 摘要。
    pub summary: Option<Summary>,
    /// 展开的路径。
    pub expanded: BTreeSet<String>,
    /// 已读取的差异。
    pub diffs: HashMap<String, DiffData>,
    /// 差异读取失败的原因。
    pub diff_errors: HashMap<String, String>,
    /// 面板级错误（读取失败、写操作失败）。
    pub error: Option<String>,
    /// 正在读取摘要。
    pub loading: bool,
    /// 正在执行写操作。
    pub writing: bool,
    /// 写操作期间收到的新目标（`Some(None)` 表示变成不可用）。
    pending: Option<Option<GitTarget>>,
}

impl GitModel {
    /// 跟随活动标签。目标变化时清空旧仓库的全部内容并返回 `true`（调用方应刷新）；
    /// 写操作进行中先记下，写完再应用。
    pub fn follow(&mut self, next: Option<GitTarget>) -> bool {
        if self.writing {
            self.pending = Some(next);
            return false;
        }
        if next == self.target {
            return false;
        }
        self.target = next;
        self.clear();
        true
    }

    fn clear(&mut self) {
        self.summary = None;
        self.expanded.clear();
        self.diffs.clear();
        self.diff_errors.clear();
        self.error = None;
        self.loading = false;
    }

    /// 写操作开始。
    pub fn begin_write(&mut self) {
        self.writing = true;
        self.error = None;
    }

    /// 写操作结束：返回等待中的目标变化（应用后需要刷新则 `Some(changed)`）。
    pub fn finish_write(&mut self) -> Option<bool> {
        self.writing = false;
        self.pending.take().map(|t| self.follow(t))
    }

    /// 开始读取摘要。
    pub fn begin_refresh(&mut self, keep_error: bool) {
        if !keep_error {
            self.error = None;
        }
        self.loading = true;
    }

    /// 摘要读回：展开项只保留仍有改动的，并返回需要重新读取差异的路径
    /// （每次新摘要都重读，等长替换后行数相同也不能沿用旧正文）。
    pub fn apply_summary(&mut self, summary: Option<Summary>) -> Vec<String> {
        self.loading = false;
        self.diffs.clear();
        self.diff_errors.clear();
        if let Some(s) = &summary {
            self.expanded
                .retain(|p| s.files.iter().any(|f| &f.path == p));
        } else {
            self.expanded.clear();
        }
        self.summary = summary;
        self.expanded.iter().cloned().collect()
    }

    /// 摘要读取失败。
    pub fn apply_error(&mut self, text: String) {
        self.loading = false;
        self.summary = None;
        self.expanded.clear();
        self.diffs.clear();
        self.diff_errors.clear();
        self.error = Some(text);
    }

    /// 点击一行。
    pub fn toggle(&mut self, path: &str) -> Toggle {
        if self.writing || self.loading {
            return Toggle::Ignored;
        }
        if self.expanded.remove(path) {
            self.diffs.remove(path);
            self.diff_errors.remove(path);
            return Toggle::Collapsed;
        }
        if !self
            .summary
            .as_ref()
            .is_some_and(|s| s.files.iter().any(|f| f.path == path))
        {
            return Toggle::Ignored;
        }
        self.expanded.insert(path.to_owned());
        Toggle::Expand
    }

    /// 差异读回（条目已收起则丢弃）。
    pub fn apply_diff(&mut self, path: &str, result: Result<DiffData, String>) {
        if !self.expanded.contains(path) {
            return;
        }
        match result {
            Ok(d) => {
                self.diff_errors.remove(path);
                self.diffs.insert(path.to_owned(), d);
            }
            Err(e) => {
                self.diffs.remove(path);
                self.diff_errors.insert(path.to_owned(), e);
            }
        }
    }

    /// 有改动、不在读取也不在写：可以提交或丢弃。
    pub fn can_write(&self) -> bool {
        !self.loading && !self.writing && self.summary.as_ref().is_some_and(|s| !s.files.is_empty())
    }

    /// 提交说明去首尾空白后非空且不超过 16 KiB。
    pub fn can_commit(&self, message: &str) -> bool {
        let t = message.trim();
        self.can_write() && !t.is_empty() && t.len() <= MAX_MESSAGE_BYTES
    }

    /// 空状态。
    pub fn empty_state(&self) -> EmptyState {
        match &self.summary {
            Some(s) if s.files.is_empty() => EmptyState::Clean,
            Some(_) => EmptyState::None,
            None if self.loading => EmptyState::Loading,
            None => EmptyState::Unsupported,
        }
    }

    /// 入口按钮上的统计；没有摘要或没有改动时 `None`（只显示图标）。
    pub fn button_stats(&self) -> Option<(u64, u64)> {
        self.summary
            .as_ref()
            .filter(|s| s.added + s.deleted > 0)
            .map(|s| (s.added, s.deleted))
    }
}

/// 行数文字：已知为 `+N` / `−M`，未知为破折号。
pub fn count_text(added: Option<u64>, deleted: Option<u64>) -> (String, String) {
    (
        added.map_or("—".into(), |n| format!("+{n}")),
        deleted.map_or("—".into(), |n| format!("−{n}")),
    )
}

/// 入口按钮的统计文字。
pub fn totals_text(added: u64, deleted: u64) -> (String, String) {
    (format!("+{added}"), format!("−{deleted}"))
}

/// 状态标记是否按错误色显示（冲突）。
pub fn status_is_conflict(status: &str) -> bool {
    status == "!"
}

/// 重命名 / 复制行的路径文字。
pub fn path_label(file: &FileRow) -> String {
    match &file.original {
        Some(o) => format!("{}  ←  {}", file.path, o),
        None => file.path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(n: u64) -> SessionId {
        SessionId::from_raw(n)
    }

    fn file(path: &str, status: &str) -> FileRow {
        FileRow {
            path: path.into(),
            original: None,
            status: status.into(),
            added: Some(1),
            deleted: Some(0),
        }
    }

    fn summary(paths: &[&str]) -> Summary {
        Summary {
            root: "C:\\work\\repo".into(),
            branch: "main".into(),
            files: paths.iter().map(|p| file(p, "M")).collect(),
            added: paths.len() as u64,
            deleted: 0,
        }
    }

    fn target(n: u64, cwd: &str) -> GitTarget {
        GitTarget {
            session: sid(n),
            cwd: cwd.into(),
        }
    }

    #[test]
    fn width_is_clamped_and_yields_to_terminal() {
        let l = GitLayout {
            visible: true,
            width: DEFAULT_WIDTH,
        };
        assert_eq!(l.dragged(10.0), MIN_WIDTH);
        assert_eq!(l.dragged(5000.0), MAX_WIDTH);
        assert_eq!(l.dragged(f32::NAN), DEFAULT_WIDTH);
        assert_eq!(GitLayout::with_saved_width(500.0).width, 500.0);
        assert_eq!(GitLayout::with_saved_width(10.0).width, MIN_WIDTH);
        assert_eq!(GitLayout::with_saved_width(9999.0).width, MAX_WIDTH);
        assert_eq!(GitLayout::with_saved_width(f32::NAN).width, DEFAULT_WIDTH);
        assert!(!GitLayout::with_saved_width(500.0).visible);
        assert_eq!(l.effective_width(1200.0), 480.0);
        assert_eq!(l.effective_width(600.0), 300.0);
        // 让位后太窄：不显示。
        assert_eq!(l.effective_width(450.0), 0.0);
        assert_eq!(GitLayout::default().effective_width(2000.0), 0.0);
        // 向左拖变宽。
        assert_eq!(l.drag_width(480.0, 800.0, 750.0), 530.0);
    }

    #[test]
    fn target_only_for_connected_local_shell() {
        let mut t = TabItem::new(sid(1), TabKind::Local, "shell");
        assert_eq!(target_for(Some(&t), &[]), None);
        t.connection = TabConnection::Connected;
        assert_eq!(target_for(Some(&t), &[]), None);
        t.cwd = Some("C:\\repo".into());
        assert_eq!(target_for(Some(&t), &[]), Some(target(1, "C:\\repo")));
        t.cwd_host = Some("other-box".into());
        assert_eq!(target_for(Some(&t), &["me"]), None);
        t.cwd_host = Some("ME".into());
        assert!(target_for(Some(&t), &["me"]).is_some());
        t.kind = TabKind::Ssh;
        assert_eq!(target_for(Some(&t), &["me"]), None);
        t.kind = TabKind::Sftp;
        assert_eq!(target_for(Some(&t), &["me"]), None);
        assert_eq!(target_for(None, &[]), None);
    }

    #[test]
    fn throttle_coalesces_auto_requests() {
        let mut th = RefreshThrottle::default();
        assert_eq!(th.request(1000, false), Throttle::Now);
        th.started(1000);
        assert_eq!(th.request(1200, false), Throttle::After(600));
        assert_eq!(th.request(1300, false), Throttle::Skip);
        assert!(th.take_pending());
        assert!(!th.take_pending());
        th.started(1800);
        assert_eq!(th.request(2700, false), Throttle::Now);
        // 手动刷新随时立即执行，并取代待执行的自动刷新。
        th.started(3000);
        assert_eq!(th.request(3100, false), Throttle::After(700));
        assert_eq!(th.request(3150, true), Throttle::Now);
        assert!(!th.take_pending());
    }

    #[test]
    fn follow_resets_on_change_only() {
        let mut m = GitModel::default();
        assert!(!m.follow(None));
        assert!(m.follow(Some(target(1, "a"))));
        m.apply_summary(Some(summary(&["x"])));
        assert!(!m.follow(Some(target(1, "a"))));
        assert!(m.summary.is_some());
        // 切到别的目录：不沿用上一个仓库。
        assert!(m.follow(Some(target(1, "b"))));
        assert!(m.summary.is_none());
        assert!(m.follow(None));
        assert_eq!(m.empty_state(), EmptyState::Unsupported);
    }

    #[test]
    fn follow_is_deferred_while_writing() {
        let mut m = GitModel::default();
        m.follow(Some(target(1, "a")));
        m.apply_summary(Some(summary(&["x"])));
        m.begin_write();
        assert!(!m.follow(Some(target(2, "b"))));
        assert_eq!(m.target, Some(target(1, "a")));
        assert_eq!(m.finish_write(), Some(true));
        assert_eq!(m.target, Some(target(2, "b")));
        assert_eq!(m.finish_write(), None);
    }

    #[test]
    fn summary_refresh_rereads_expanded_and_drops_gone() {
        let mut m = GitModel::default();
        m.follow(Some(target(1, "a")));
        m.apply_summary(Some(summary(&["a.rs", "b.rs"])));
        assert_eq!(m.toggle("a.rs"), Toggle::Expand);
        assert_eq!(m.toggle("b.rs"), Toggle::Expand);
        m.apply_diff(
            "a.rs",
            Ok(DiffData {
                lines: vec![],
                notice: None,
                truncated: false,
            }),
        );
        assert!(m.diffs.contains_key("a.rs"));
        m.begin_refresh(false);
        let reload = m.apply_summary(Some(summary(&["a.rs"])));
        assert_eq!(reload, vec!["a.rs".to_string()]);
        // 旧正文必须清掉，即使行数一样。
        assert!(m.diffs.is_empty());
        assert!(!m.expanded.contains("b.rs"));
    }

    #[test]
    fn toggle_rules() {
        let mut m = GitModel::default();
        m.follow(Some(target(1, "a")));
        m.begin_refresh(false);
        assert_eq!(m.toggle("a.rs"), Toggle::Ignored);
        m.apply_summary(Some(summary(&["a.rs"])));
        assert_eq!(m.toggle("nope"), Toggle::Ignored);
        assert_eq!(m.toggle("a.rs"), Toggle::Expand);
        assert_eq!(m.toggle("a.rs"), Toggle::Collapsed);
        // 收起后迟到的差异被丢弃。
        m.apply_diff("a.rs", Err("late".into()));
        assert!(m.diff_errors.is_empty());
        m.begin_write();
        assert_eq!(m.toggle("a.rs"), Toggle::Ignored);
    }

    #[test]
    fn commit_validation() {
        let mut m = GitModel::default();
        m.follow(Some(target(1, "a")));
        assert!(!m.can_commit("msg"));
        m.apply_summary(Some(summary(&["a.rs"])));
        assert!(m.can_commit("fix"));
        assert!(!m.can_commit("  \n\t "));
        assert!(m.can_commit("  trimmed  "));
        assert!(!m.can_commit(&"x".repeat(MAX_MESSAGE_BYTES + 1)));
        assert!(m.can_commit(&"x".repeat(MAX_MESSAGE_BYTES)));
        m.begin_write();
        assert!(!m.can_commit("fix"));
        m.finish_write();
        m.apply_summary(Some(summary(&[])));
        assert!(!m.can_write());
        assert_eq!(m.empty_state(), EmptyState::Clean);
    }

    #[test]
    fn error_and_states() {
        let mut m = GitModel::default();
        m.follow(Some(target(1, "a")));
        m.begin_refresh(false);
        assert_eq!(m.empty_state(), EmptyState::Loading);
        m.apply_error("git 不可用".into());
        assert_eq!(m.error.as_deref(), Some("git 不可用"));
        assert_eq!(m.empty_state(), EmptyState::Unsupported);
        m.begin_refresh(true);
        assert!(m.error.is_some());
        m.begin_refresh(false);
        assert!(m.error.is_none());
    }

    #[test]
    fn button_stats_hidden_without_changes() {
        let mut m = GitModel::default();
        assert_eq!(m.button_stats(), None);
        m.apply_summary(Some(summary(&[])));
        assert_eq!(m.button_stats(), None);
        m.apply_summary(Some(summary(&["a", "b"])));
        assert_eq!(m.button_stats(), Some((2, 0)));
    }

    #[test]
    fn count_text_uses_dash_for_unknown() {
        assert_eq!(count_text(Some(3), Some(0)), ("+3".into(), "−0".into()));
        assert_eq!(count_text(None, None), ("—".into(), "—".into()));
    }

    #[test]
    fn names_and_labels() {
        assert_eq!(last_component("C:\\work\\repo\\"), "repo");
        assert_eq!(last_component("/home/u/proj"), "proj");
        let mut f = file("new.rs", "R");
        f.original = Some("old.rs".into());
        assert_eq!(path_label(&f), "new.rs  ←  old.rs");
        assert_eq!(discard_kind(&file("a", "U")), DiscardKind::NewContent);
        assert_eq!(discard_kind(&file("a", "M")), DiscardKind::Tracked);
        assert_eq!(discard_kind(&f), DiscardKind::Tracked);
        assert!(status_is_conflict("!"));
    }

    fn dl(kind: DiffKind, number: u64, text: &str) -> DiffLine {
        DiffLine {
            kind,
            number,
            text: text.into(),
        }
    }

    #[test]
    fn changed_ranges_strip_common_edges() {
        let (a, b) = changed_ranges("let x = 1;", "let x = 22;");
        assert_eq!(&"let x = 1;"[a], "1");
        assert_eq!(&"let x = 22;"[b], "22");
        let (a, b) = changed_ranges("same", "same");
        assert!(a.is_empty() && b.is_empty());
        // 多字节字符不能被切开。
        let (a, b) = changed_ranges("你好世界", "你们世界");
        assert_eq!(&"你好世界"[a], "好");
        assert_eq!(&"你们世界"[b], "们");
        let (a, b) = changed_ranges("", "abc");
        assert_eq!(a, 0..0);
        assert_eq!(b, 0..3);
    }

    #[test]
    fn rows_pair_removed_with_added_and_report_gaps() {
        let lines = vec![
            dl(DiffKind::Context, 10, "ctx"),
            dl(DiffKind::Removed, 11, "a = 1"),
            dl(DiffKind::Added, 11, "a = 2"),
            dl(DiffKind::Context, 12, "ctx2"),
            // 下一个块从新版本第 20 行开始：跳过 7 行。
            dl(DiffKind::Context, 20, "far"),
        ];
        let rows = diff_rows(&lines);
        assert_eq!(rows[0].gap, 9);
        assert_eq!(rows[1].gap, 0);
        assert_eq!(rows[2].gap, 0);
        assert_eq!(rows[3].gap, 0);
        assert_eq!(rows[4].gap, 7);
        assert_eq!(&rows[1].text[rows[1].highlight.clone().unwrap()], "1");
        assert_eq!(&rows[2].text[rows[2].highlight.clone().unwrap()], "2");
        assert!(rows[0].highlight.is_none());
    }

    #[test]
    fn rows_without_pair_have_no_highlight() {
        let lines = vec![
            dl(DiffKind::Removed, 1, "gone"),
            dl(DiffKind::Removed, 2, "gone2"),
            dl(DiffKind::Added, 1, "new"),
        ];
        let rows = diff_rows(&lines);
        assert!(rows[0].highlight.is_some());
        assert!(rows[1].highlight.is_none());
        assert!(rows[2].highlight.is_some());
        // 删除行用旧行号加偏移换算成新行号，不会误报跳行。
        assert!(rows.iter().all(|r| r.gap == 0));
        assert_eq!(kind_marker(DiffKind::Added), "+");
    }
}
