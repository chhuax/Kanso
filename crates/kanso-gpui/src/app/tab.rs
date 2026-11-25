//! 标签（会话）的展示状态与文字描述。对照 `Workspace.swift` 的 `TabItem` 与
//! `TabPresentation.swift`、`TabDescriptions.swift`。
//!
//! 本模块不产生任何本地化文字：需要文字的地方返回枚举 / 结构，由视图按 key 查文案
//! （关闭原因里的 core 消息用 `kanso-l10n` 渲染）。

use kanso_types::agent::AgentKind;
use kanso_types::event::{Activity, CloseReason, SessionState};
use kanso_types::geometry::Rgb;
use kanso_types::message::Message;
use kanso_types::profile::SessionKind;
use kanso_types::{ProfileId, SessionId};

/// 标签的会话种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TabKind {
    /// 本地 shell。
    Local,
    /// SSH 终端。
    Ssh,
    /// 纯 SFTP。
    Sftp,
}

impl From<SessionKind> for TabKind {
    fn from(kind: SessionKind) -> Self {
        match kind {
            SessionKind::Local => TabKind::Local,
            SessionKind::Ssh => TabKind::Ssh,
            SessionKind::Sftp => TabKind::Sftp,
        }
    }
}

/// 会话结束的原因说明（用户主动关闭没有说明，见 [`TabConnection::Closed`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseNote {
    /// 进程退出；`code` 未知（被信号终止等）为 `None`。视图显示"会话已结束（退出码 N）"。
    Exited {
        /// 退出码。
        code: Option<i32>,
    },
    /// 因错误关闭；视图用 `kanso-l10n` 渲染这条消息。
    Error(Message),
}

/// 连接状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabConnection {
    /// 正在连接 / 启动。
    Connecting,
    /// 已就绪。
    Connected,
    /// 已关闭。`None` 表示用户主动关闭，不提示"会话已结束"（local-shell.md §7）。
    Closed(Option<CloseNote>),
}

impl From<&SessionState> for TabConnection {
    fn from(state: &SessionState) -> Self {
        match state {
            SessionState::Connecting => TabConnection::Connecting,
            SessionState::Connected => TabConnection::Connected,
            SessionState::Closed(reason) => TabConnection::Closed(match reason {
                CloseReason::ByUser => None,
                CloseReason::Exited { code } => Some(CloseNote::Exited { code: *code }),
                CloseReason::Error(message) => Some(CloseNote::Error(message.clone())),
            }),
        }
    }
}

impl TabConnection {
    /// 会话结束后是否自动关掉标签：用户主动关闭、shell 正常退出（退出码 0）。
    /// 非零退出码、被信号终止、出错都保留标签，让用户看到原因（local-shell.md §7：shell 自己退出要提示退出码）。
    pub fn closes_tab_automatically(&self) -> bool {
        matches!(
            self,
            TabConnection::Closed(None)
                | TabConnection::Closed(Some(CloseNote::Exited { code: Some(0) }))
        )
    }
}

/// 会话当前活动。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabActivity {
    /// 空闲，停在提示符。
    Idle,
    /// 有命令在前台运行。
    CommandRunning,
    /// agent 在等待用户输入（已完成）。
    AgentIdle(AgentKind),
    /// agent 正在工作。
    AgentWorking(AgentKind),
}

impl TabActivity {
    /// "忙"：有前台命令或 AI 在运行。
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            TabActivity::CommandRunning | TabActivity::AgentWorking(_)
        )
    }

    /// 正在 / 刚在运行的 agent。
    pub fn agent(&self) -> Option<AgentKind> {
        match self {
            TabActivity::AgentIdle(a) | TabActivity::AgentWorking(a) => Some(*a),
            _ => None,
        }
    }
}

impl From<Activity> for TabActivity {
    fn from(activity: Activity) -> Self {
        match activity {
            Activity::Idle => TabActivity::Idle,
            Activity::CommandRunning => TabActivity::CommandRunning,
            Activity::AgentIdle(a) => TabActivity::AgentIdle(a),
            Activity::AgentWorking(a) => TabActivity::AgentWorking(a),
        }
    }
}

/// AI 工具的显示名。名字是产品名，不随语言变化。
pub fn agent_name(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "Claude Code",
        AgentKind::Codex => "Codex",
        AgentKind::Gemini => "Gemini CLI",
        AgentKind::Aider => "Aider",
        AgentKind::CursorAgent => "Cursor Agent",
        AgentKind::Copilot => "GitHub Copilot CLI",
        AgentKind::Amp => "Amp",
        AgentKind::OpenCode => "OpenCode",
        AgentKind::Crush => "Crush",
        AgentKind::Goose => "Goose",
        AgentKind::Qwen => "Qwen Code",
        AgentKind::Droid => "Droid",
        AgentKind::OpenHands => "OpenHands",
        AgentKind::Grok => "Grok CLI",
    }
}

/// 一个标签的全部展示状态。
#[derive(Clone, Debug, PartialEq)]
pub struct TabItem {
    /// 会话 id（标签的唯一标识）。
    pub id: SessionId,
    /// 种类。
    pub kind: TabKind,
    /// 会话名称（core 给的显示名）。
    pub name: String,
    /// 用户或 MCP 设置的自定义标签名；`None` 表示使用默认标题。
    pub custom_title: Option<String>,
    /// 对应的已保存会话；默认本地 shell 为 `None`。
    pub profile: Option<ProfileId>,
    /// 配置里设置的会话颜色；`None` 则按 [`TabPresentation::color_index`] 稳定生成。
    pub color: Option<Rgb>,
    /// 当前目录（OSC 7 等上报）。
    pub cwd: Option<String>,
    /// 当前目录所属主机。
    pub cwd_host: Option<String>,
    /// git 分支。
    pub git_branch: Option<String>,
    /// 连接状态。
    pub connection: TabConnection,
    /// 活动。
    pub activity: TabActivity,
    /// 有未读完成：一直保留直到用户切到这个标签。
    pub unread_done: bool,
    /// 正在进行的文件传输数。
    pub active_transfers: u32,
}

impl TabItem {
    /// 以"连接中、空闲"的初始状态构造。
    pub fn new(id: SessionId, kind: TabKind, name: impl Into<String>) -> Self {
        TabItem {
            id,
            kind,
            name: name.into(),
            custom_title: None,
            profile: None,
            color: None,
            cwd: None,
            cwd_host: None,
            git_branch: None,
            connection: TabConnection::Connecting,
            activity: TabActivity::Idle,
            unread_done: false,
            active_transfers: 0,
        }
    }
}

/// 标签的展示辅助（纯函数）。
pub struct TabPresentation;

impl TabPresentation {
    /// 把本机路径里的 home 前缀替换为 `~`，并去掉末尾多余的 `/`（根目录 `/` 除外）。
    /// `/Users/ab` 不是 `/Users/a` 的子目录，不替换。Windows 路径（`C:\Users\me`）同样按
    /// `\` 或 `/` 分隔处理，home 前缀比较忽略大小写（盘符大小写常不一致）。
    pub fn display_path(cwd: &str, home_dir: &str) -> String {
        let path = trim_trailing_separators(cwd);
        let home = trim_trailing_separators(home_dir);
        // home 为空或根目录时替换没有意义（否则所有绝对路径都会变成 `~/...`）。
        if home.is_empty() || home == "/" || home == "\\" {
            return path.to_owned();
        }
        let windows_like = home.contains('\\') || home.as_bytes().get(1) == Some(&b':');
        // Windows 路径：两种分隔符等价、盘符与目录名不区分大小写。逐字节替换不改变长度，下标可共用。
        let norm = |s: &str| {
            if windows_like {
                s.replace('\\', "/").to_ascii_lowercase()
            } else {
                s.to_owned()
            }
        };
        let (np, nh) = (norm(path), norm(home));
        if np == nh {
            return "~".to_owned();
        }
        if np.len() > nh.len()
            && np.starts_with(&nh)
            && np.as_bytes()[nh.len()] == b'/'
            && path.is_char_boundary(nh.len())
        {
            return format!("~{}", &path[nh.len()..]);
        }
        path.to_owned()
    }

    /// 默认标题（不考虑自定义名）：本地且 cwd 已知 → 路径（home 前缀替换为 `~`）；其他 → `name`。
    pub fn default_title(item: &TabItem, home_dir: &str) -> String {
        let Some(cwd) = item
            .cwd
            .as_deref()
            .filter(|c| item.kind == TabKind::Local && !c.is_empty())
        else {
            return item.name.clone();
        };
        Self::display_path(cwd, home_dir)
    }

    /// 主标题：已设置自定义名 → 自定义名；否则走 [`default_title`](Self::default_title)。
    ///
    /// 返回完整字符串，过长时的截断由视图层做。
    pub fn primary_title(item: &TabItem, home_dir: &str) -> String {
        if let Some(custom) = item.custom_title.as_deref().filter(|s| !s.is_empty()) {
            return custom.to_owned();
        }
        Self::default_title(item, home_dir)
    }

    /// 副标题：
    /// - 未改名：本地且有 Git 分支 → 分支名；否则 `None`（不在仓库里不显示，也不重复显示目录）。
    /// - 已改名：保留上下文——SSH / SFTP 显示原会话名（与自定义名不同时）；
    ///   本地显示当前目录，在仓库里时连同分支一并显示（`~/work · main`）。
    pub fn subtitle(item: &TabItem, home_dir: &str) -> Option<String> {
        if let Some(custom) = item.custom_title.as_deref().filter(|s| !s.is_empty()) {
            return match item.kind {
                TabKind::Local => {
                    let dir = item
                        .cwd
                        .as_deref()
                        .filter(|c| !c.is_empty())
                        .map(|c| Self::display_path(c, home_dir));
                    let branch = item.git_branch.as_deref().filter(|b| !b.is_empty());
                    match (dir, branch) {
                        (Some(d), Some(b)) => Some(format!("{d} · {b}")),
                        (Some(d), None) => Some(d),
                        (None, Some(b)) => Some(b.to_owned()),
                        (None, None) => None,
                    }
                }
                TabKind::Ssh | TabKind::Sftp => {
                    (!item.name.is_empty() && item.name != custom).then(|| item.name.clone())
                }
            };
        }
        if item.kind != TabKind::Local {
            return None;
        }
        item.git_branch
            .as_deref()
            .filter(|b| !b.is_empty())
            .map(str::to_owned)
    }

    /// 悬停时的快捷键提示：前 9 个是 `Ctrl+1`–`Ctrl+9` 的数字，之后是序号。返回要显示的数字文字，
    /// 前缀（`Ctrl+` / `⌘`）由视图按平台拼。第二个返回值表示是否有快捷键。
    pub fn shortcut_digit(sidebar_index: usize) -> (String, bool) {
        ((sidebar_index + 1).to_string(), sidebar_index < 9)
    }

    /// 会话颜色在调色板中的下标。配置了 `color` 时调用方直接用它，不经过这里。
    ///
    /// 用 FNV-1a 64 位而不是 `DefaultHasher`：后者不保证跨版本稳定，
    /// 违反"同一个会话每次启动都是同一个颜色"。键取已保存会话的 id（没有则取名称）。
    pub fn color_index(item: &TabItem, palette_count: usize) -> usize {
        if palette_count == 0 {
            return 0;
        }
        let key = item
            .profile
            .map_or_else(|| item.name.clone(), |p| p.to_string());
        (fnv1a64(&key) % palette_count as u64) as usize
    }

    /// 路径筛选：query 去首尾空白后为空 → 匹配全部；否则只匹配本地且 cwd 已知的标签
    /// （SSH / SFTP 不拿名称冒充路径）。不区分大小写，`\` 与 `/` 视为同一分隔符，子串匹配。
    pub fn matches_path_filter(item: &TabItem, query: &str) -> bool {
        let q = query.trim();
        if q.is_empty() {
            return true;
        }
        match item.cwd.as_deref() {
            Some(cwd) if item.kind == TabKind::Local && !cwd.is_empty() => {
                normalize_for_filter(cwd).contains(&normalize_for_filter(q))
            }
            _ => false,
        }
    }
}

/// 标签主标题过长时保留末尾（路径最有辨识度的是最后几段），前面用 `…` 代替。
/// `max_cells` 按终端单元格宽度算（中日韩字符占 2 格）；不超长原样返回。
pub fn truncate_keep_tail(s: &str, max_cells: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let width = |c: char| c.width().unwrap_or(0);
    let total: usize = s.chars().map(width).sum();
    if total <= max_cells {
        return s.to_owned();
    }
    if max_cells == 0 {
        return String::new();
    }
    // 省略号占 1 格，其余留给末尾。
    let budget = max_cells - 1;
    let mut used = 0;
    let mut start = s.len();
    for (i, c) in s.char_indices().rev() {
        let w = width(c);
        if used + w > budget {
            break;
        }
        used += w;
        start = i;
    }
    format!("…{}", &s[start..])
}

/// FNV-1a 64 位哈希（按 UTF-8 字节）。跨进程、跨版本稳定。
pub fn fnv1a64(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn normalize_for_filter(s: &str) -> String {
    s.replace('\\', "/").to_lowercase()
}

fn trim_trailing_separators(s: &str) -> &str {
    let mut t = s;
    while t.len() > 1 && (t.ends_with('/') || t.ends_with('\\')) {
        t = &t[..t.len() - 1];
    }
    t
}

/// 标签的状态行（悬停提示与状态栏共用）。文字由视图本地化。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabStateLine {
    /// "连接中…"。
    Connecting,
    /// 已关闭；`None` 为用户主动关闭。
    Closed(Option<CloseNote>),
    /// "命令运行中"。
    CommandRunning,
    /// "AI 运行中"。
    AgentRunning,
    /// "AI 已完成"。
    AgentFinished,
    /// "命令已完成，点击查看"（未读完成）。
    UnreadDone,
}

/// 悬停提示的第一行：协议与地址。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TooltipHead {
    /// 本地 shell；有 cwd 时带路径。
    Local {
        /// 当前目录。
        cwd: Option<String>,
    },
    /// "SSH · name"。
    Ssh(String),
    /// "SFTP · name"。
    Sftp(String),
}

/// 悬停提示的结构化内容，每项一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabTooltip {
    /// 协议与地址。
    pub head: TooltipHead,
    /// AI 工具名（产品名，不本地化）。
    pub agent: Option<&'static str>,
    /// git 分支。
    pub branch: Option<String>,
    /// 活动 / 连接状态或错误。
    pub state: Option<TabStateLine>,
}

/// 标签的文字描述（悬停提示、状态栏）。纯函数。
pub struct TabDescriptions;

impl TabDescriptions {
    /// 悬停提示。
    pub fn tooltip(item: &TabItem) -> TabTooltip {
        TabTooltip {
            head: match item.kind {
                TabKind::Local => TooltipHead::Local {
                    cwd: item.cwd.clone().filter(|c| !c.is_empty()),
                },
                TabKind::Ssh => TooltipHead::Ssh(item.name.clone()),
                TabKind::Sftp => TooltipHead::Sftp(item.name.clone()),
            },
            agent: item.activity.agent().map(agent_name),
            branch: item.git_branch.clone().filter(|b| !b.is_empty()),
            state: Self::state_line(item),
        }
    }

    /// 活动 / 连接状态一行；没有值得提示的状态（含用户主动关闭）返回 `None`。
    pub fn state_line(item: &TabItem) -> Option<TabStateLine> {
        match &item.connection {
            TabConnection::Connecting => return Some(TabStateLine::Connecting),
            TabConnection::Closed(Some(note)) => {
                return Some(TabStateLine::Closed(Some(note.clone())));
            }
            TabConnection::Closed(None) => return None,
            TabConnection::Connected => {}
        }
        match item.activity {
            TabActivity::CommandRunning => Some(TabStateLine::CommandRunning),
            TabActivity::AgentWorking(_) => Some(TabStateLine::AgentRunning),
            TabActivity::AgentIdle(_) => Some(TabStateLine::AgentFinished),
            TabActivity::Idle => item.unread_done.then_some(TabStateLine::UnreadDone),
        }
    }

    /// 状态栏的状态：只在非正常连接时有（连接中、已结束含退出码 / 错误原因）。
    /// 正常连接时为 `None`——位置信息已由"当前目录"条目表达。
    /// 与 [`state_line`](Self::state_line) 的区别：用户主动关闭也返回 `Closed(None)`（视图显示通用的"会话已结束"）。
    pub fn status_bar_state(item: Option<&TabItem>) -> Option<TabStateLine> {
        match &item?.connection {
            TabConnection::Connecting => Some(TabStateLine::Connecting),
            TabConnection::Closed(note) => Some(TabStateLine::Closed(note.clone())),
            TabConnection::Connected => None,
        }
    }

    /// 状态栏的"当前目录"条目文字；没有可显示的目录时为 `None`（条目隐藏）。
    ///
    /// 本地会话 home 显示为 `~`；SSH / SFTP 显示 OSC 7 上报的远端路径，原样显示——
    /// 远端 home 未知，不能用本机 home 去替换。没有上报就不显示，不用会话名充数。
    pub fn directory_text(item: Option<&TabItem>, home_dir: &str) -> Option<String> {
        let item = item?;
        let cwd = item.cwd.as_deref().filter(|c| !c.is_empty())?;
        Some(if item.kind == TabKind::Local {
            TabPresentation::display_path(cwd, home_dir)
        } else {
            cwd.to_owned()
        })
    }
}

/// 标签拖拽使用按下瞬间的行中点。预览重新排版时仍以同一组中点判定插入位置。
///
/// 坐标是一维的：竖向侧栏传 y，顶部标签条传 x（见 `horizontal`）。字段名里的 `y` 是竖向时期的命名。
#[derive(Clone, Debug)]
pub struct TabReorder {
    /// 当前拖动的会话。
    pub session: SessionId,
    /// 横向排列（顶部标签条）：调用方应把指针的 x 传给 `update`。
    pub horizontal: bool,
    /// 按下时该分屏的标签顺序；期间有标签增删则放弃提交。
    pub order: Vec<SessionId>,
    start_y: f32,
    other_midpoints: Vec<f32>,
    /// 从其余标签中计数的插入位置；尚未超过拖动阈值时为 `None`。
    pub insertion: Option<usize>,
}

impl TabReorder {
    /// 用按下位置、当前行中点和相邻行间距记录这一组最初的位置。
    pub fn start(
        session: SessionId,
        order: Vec<SessionId>,
        pointer_y: f32,
        center_y: f32,
        row_stride: f32,
    ) -> Self {
        let start = order
            .iter()
            .position(|id| *id == session)
            .expect("按下的标签属于该分屏");
        let other_midpoints = (0..order.len())
            .filter(|i| *i != start)
            .map(|i| center_y + (i as f32 - start as f32) * row_stride)
            .collect();
        Self {
            session,
            horizontal: false,
            order,
            start_y: pointer_y,
            other_midpoints,
            insertion: None,
        }
    }

    /// 改为横向：之后 `start` 时传入的坐标、`update` 的参数都按 x 理解。
    pub fn horizontal(mut self) -> Self {
        self.horizontal = true;
        self
    }

    /// 按拖动方向取指针坐标：横向取 x，竖向取 y。
    pub fn axis_coord(&self, x: f32, y: f32) -> f32 {
        if self.horizontal { x } else { y }
    }

    /// 根据窗口坐标更新插入位置，有新预览需要绘制时返回 `true`。
    pub fn update(&mut self, y: f32) -> bool {
        if self.insertion.is_none() && (y - self.start_y).abs() <= 5.0 {
            return false;
        }
        let next = Some(self.other_midpoints.partition_point(|m| *m < y));
        let changed = self.insertion != next;
        self.insertion = next;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(cwd: Option<&str>) -> TabItem {
        let mut t = TabItem::new(SessionId::from_raw(1), TabKind::Local, "Local");
        t.cwd = cwd.map(str::to_owned);
        t
    }

    #[test]
    fn truncate_keeps_the_tail_with_ellipsis() {
        assert_eq!(truncate_keep_tail("short", 10), "short");
        assert_eq!(truncate_keep_tail("exactly10!", 10), "exactly10!");
        assert_eq!(truncate_keep_tail("~/work/project/src", 10), "…oject/src");
        assert_eq!(truncate_keep_tail("abcdef", 1), "…");
        assert_eq!(truncate_keep_tail("abcdef", 0), "");
        // 宽字符占 2 格，不能把一个字符劈开。
        assert_eq!(truncate_keep_tail("项目目录文档", 7), "…录文档");
        assert_eq!(truncate_keep_tail("项目目录文档", 6), "…文档");
    }

    #[test]
    fn automatic_close_rules() {
        let exited = |code| TabConnection::Closed(Some(CloseNote::Exited { code }));
        assert!(TabConnection::Closed(None).closes_tab_automatically());
        assert!(exited(Some(0)).closes_tab_automatically());
        assert!(!exited(Some(1)).closes_tab_automatically());
        assert!(!exited(None).closes_tab_automatically());
        assert!(!TabConnection::Connected.closes_tab_automatically());
        assert!(!TabConnection::Connecting.closes_tab_automatically());
    }

    fn ssh() -> TabItem {
        TabItem::new(SessionId::from_raw(2), TabKind::Ssh, "prod")
    }

    #[test]
    fn primary_title_replaces_home() {
        assert_eq!(
            TabPresentation::primary_title(&local(Some("/Users/a/work")), "/Users/a"),
            "~/work"
        );
        assert_eq!(
            TabPresentation::primary_title(&local(Some("/Users/a/")), "/Users/a/"),
            "~"
        );
        assert_eq!(
            TabPresentation::primary_title(&local(Some("/Users/ab")), "/Users/a"),
            "/Users/ab"
        );
        assert_eq!(
            TabPresentation::primary_title(&local(Some("/etc/")), "/Users/a"),
            "/etc"
        );
        assert_eq!(
            TabPresentation::primary_title(&local(Some("/")), "/Users/a"),
            "/"
        );
        assert_eq!(
            TabPresentation::primary_title(&local(Some("/x")), "/"),
            "/x"
        );
        assert_eq!(
            TabPresentation::primary_title(&local(None), "/Users/a"),
            "Local"
        );
        assert_eq!(
            TabPresentation::primary_title(&local(Some("")), "/Users/a"),
            "Local"
        );
    }

    #[test]
    fn primary_title_windows_paths() {
        let home = r"C:\Users\me";
        assert_eq!(
            TabPresentation::primary_title(&local(Some(r"c:\users\me\src")), home),
            r"~\src"
        );
        assert_eq!(
            TabPresentation::primary_title(&local(Some(r"C:\Users\me")), home),
            "~"
        );
        assert_eq!(
            TabPresentation::primary_title(&local(Some(r"C:\Users\mee")), home),
            r"C:\Users\mee"
        );
        assert_eq!(
            TabPresentation::primary_title(&local(Some("C:/Users/me/x")), home),
            "~/x"
        );
    }

    #[test]
    fn primary_title_non_local_uses_name_even_with_cwd() {
        let mut t = ssh();
        t.cwd = Some("/srv".into());
        assert_eq!(TabPresentation::primary_title(&t, "/Users/a"), "prod");
    }

    #[test]
    fn primary_title_handles_multibyte_boundary() {
        // home 长度恰好落在多字节字符中间时不能 panic。
        assert_eq!(
            TabPresentation::primary_title(&local(Some("/家家家/x")), "/abcd"),
            "/家家家/x"
        );
    }

    #[test]
    fn subtitle_only_for_local_with_branch() {
        let mut t = local(Some("/x"));
        assert_eq!(TabPresentation::subtitle(&t, "/Users/a"), None);
        t.git_branch = Some("main".into());
        assert_eq!(
            TabPresentation::subtitle(&t, "/Users/a").as_deref(),
            Some("main")
        );
        t.git_branch = Some(String::new());
        assert_eq!(TabPresentation::subtitle(&t, "/Users/a"), None);
        let mut s = ssh();
        s.git_branch = Some("main".into());
        assert_eq!(TabPresentation::subtitle(&s, "/Users/a"), None);
    }

    #[test]
    fn custom_title_overrides_primary_and_keeps_context_in_subtitle() {
        let mut t = local(Some("/Users/a/work"));
        t.custom_title = Some("构建".into());
        assert_eq!(TabPresentation::primary_title(&t, "/Users/a"), "构建");
        assert_eq!(
            TabPresentation::subtitle(&t, "/Users/a").as_deref(),
            Some("~/work")
        );
        // 状态栏当前目录不受自定义名影响。
        assert_eq!(
            TabDescriptions::directory_text(Some(&t), "/Users/a").as_deref(),
            Some("~/work")
        );
        t.git_branch = Some("main".into());
        assert_eq!(
            TabPresentation::subtitle(&t, "/Users/a").as_deref(),
            Some("~/work · main")
        );

        let mut s = ssh();
        s.custom_title = Some("日志".into());
        assert_eq!(TabPresentation::primary_title(&s, "/Users/a"), "日志");
        assert_eq!(
            TabPresentation::subtitle(&s, "/Users/a").as_deref(),
            Some("prod")
        );
        // 与原会话名相同时不重复显示副标题。
        s.custom_title = Some("prod".into());
        assert_eq!(TabPresentation::subtitle(&s, "/Users/a"), None);
    }

    #[test]
    fn shortcut_digit_first_nine() {
        assert_eq!(TabPresentation::shortcut_digit(0), ("1".into(), true));
        assert_eq!(TabPresentation::shortcut_digit(8), ("9".into(), true));
        assert_eq!(TabPresentation::shortcut_digit(9), ("10".into(), false));
    }

    #[test]
    fn color_index_is_stable_and_in_range() {
        // FNV-1a 64 的已知值：空串即偏移基数，"a" 为 0xaf63dc4c8601ec8c。
        assert_eq!(fnv1a64(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64("a"), 0xaf63_dc4c_8601_ec8c);
        let t = ssh();
        let i = TabPresentation::color_index(&t, 8);
        assert!(i < 8);
        assert_eq!(i, TabPresentation::color_index(&t.clone(), 8));
        assert_eq!(TabPresentation::color_index(&t, 0), 0);
        let mut with_profile = t.clone();
        with_profile.profile = Some(ProfileId::new());
        let key = with_profile.profile.unwrap().to_string();
        assert_eq!(
            TabPresentation::color_index(&with_profile, 16),
            (fnv1a64(&key) % 16) as usize
        );
    }

    #[test]
    fn path_filter_rules() {
        let t = local(Some(r"C:\Work\Kanso"));
        assert!(TabPresentation::matches_path_filter(&t, ""));
        assert!(TabPresentation::matches_path_filter(&t, "   "));
        assert!(TabPresentation::matches_path_filter(&t, "work/kanso"));
        assert!(TabPresentation::matches_path_filter(&t, "WORK\\KANSO"));
        assert!(!TabPresentation::matches_path_filter(&t, "other"));
        assert!(!TabPresentation::matches_path_filter(&local(None), "x"));
        assert!(TabPresentation::matches_path_filter(&local(None), ""));
        // SSH 不拿名称冒充路径。
        assert!(!TabPresentation::matches_path_filter(&ssh(), "prod"));
        assert!(TabPresentation::matches_path_filter(&ssh(), ""));
    }

    #[test]
    fn connection_from_core_state() {
        assert_eq!(
            TabConnection::from(&SessionState::Connecting),
            TabConnection::Connecting
        );
        assert_eq!(
            TabConnection::from(&SessionState::Connected),
            TabConnection::Connected
        );
        assert_eq!(
            TabConnection::from(&SessionState::Closed(CloseReason::ByUser)),
            TabConnection::Closed(None)
        );
        assert_eq!(
            TabConnection::from(&SessionState::Closed(CloseReason::Exited { code: Some(2) })),
            TabConnection::Closed(Some(CloseNote::Exited { code: Some(2) }))
        );
    }

    #[test]
    fn activity_busy() {
        assert!(TabActivity::CommandRunning.is_busy());
        assert!(TabActivity::AgentWorking(AgentKind::Claude).is_busy());
        assert!(!TabActivity::AgentIdle(AgentKind::Claude).is_busy());
        assert!(!TabActivity::Idle.is_busy());
        assert_eq!(
            TabActivity::from(Activity::AgentIdle(AgentKind::Codex)).agent(),
            Some(AgentKind::Codex)
        );
    }

    #[test]
    fn state_line_priorities() {
        let mut t = local(None);
        assert_eq!(
            TabDescriptions::state_line(&t),
            Some(TabStateLine::Connecting)
        );
        t.connection = TabConnection::Connected;
        assert_eq!(TabDescriptions::state_line(&t), None);
        t.activity = TabActivity::CommandRunning;
        assert_eq!(
            TabDescriptions::state_line(&t),
            Some(TabStateLine::CommandRunning)
        );
        t.activity = TabActivity::AgentWorking(AgentKind::Claude);
        assert_eq!(
            TabDescriptions::state_line(&t),
            Some(TabStateLine::AgentRunning)
        );
        t.activity = TabActivity::AgentIdle(AgentKind::Claude);
        assert_eq!(
            TabDescriptions::state_line(&t),
            Some(TabStateLine::AgentFinished)
        );
        t.activity = TabActivity::Idle;
        t.unread_done = true;
        assert_eq!(
            TabDescriptions::state_line(&t),
            Some(TabStateLine::UnreadDone)
        );
        t.connection = TabConnection::Closed(None);
        assert_eq!(TabDescriptions::state_line(&t), None, "用户主动关闭不提示");
        t.connection = TabConnection::Closed(Some(CloseNote::Exited { code: Some(1) }));
        assert!(matches!(
            TabDescriptions::state_line(&t),
            Some(TabStateLine::Closed(Some(_)))
        ));
    }

    #[test]
    fn tooltip_structure() {
        let mut t = local(Some("/work"));
        t.connection = TabConnection::Connected;
        t.git_branch = Some("dev".into());
        t.activity = TabActivity::AgentWorking(AgentKind::Claude);
        let tip = TabDescriptions::tooltip(&t);
        assert_eq!(
            tip.head,
            TooltipHead::Local {
                cwd: Some("/work".into())
            }
        );
        assert_eq!(tip.agent, Some("Claude Code"));
        assert_eq!(tip.branch.as_deref(), Some("dev"));
        assert_eq!(tip.state, Some(TabStateLine::AgentRunning));
        assert_eq!(
            TabDescriptions::tooltip(&ssh()).head,
            TooltipHead::Ssh("prod".into())
        );
    }

    #[test]
    fn status_bar_state_and_directory() {
        assert_eq!(TabDescriptions::status_bar_state(None), None);
        let mut t = ssh();
        assert_eq!(
            TabDescriptions::status_bar_state(Some(&t)),
            Some(TabStateLine::Connecting)
        );
        t.connection = TabConnection::Connected;
        assert_eq!(TabDescriptions::status_bar_state(Some(&t)), None);
        t.connection = TabConnection::Closed(None);
        assert_eq!(
            TabDescriptions::status_bar_state(Some(&t)),
            Some(TabStateLine::Closed(None))
        );

        assert_eq!(TabDescriptions::directory_text(None, "/h"), None);
        assert_eq!(TabDescriptions::directory_text(Some(&ssh()), "/h"), None);
        let mut s = ssh();
        s.cwd = Some("/home/u/x".into());
        assert_eq!(
            TabDescriptions::directory_text(Some(&s), "/home/u"),
            Some("/home/u/x".into())
        );
        assert_eq!(
            TabDescriptions::directory_text(Some(&local(Some("/h/x"))), "/h"),
            Some("~/x".into())
        );
    }
}

#[cfg(test)]
mod reorder_tests {
    use super::*;
    #[test]
    fn click_preview_move_back_and_group_boundaries() {
        let ids: Vec<_> = (1..=4).map(SessionId::from_raw).collect();
        let mut drag = TabReorder::start(ids[1], ids.clone(), 71.0, 66.0, 44.0);
        assert!(!drag.update(76.0));
        assert_eq!(drag.insertion, None);
        assert!(drag.update(114.0));
        assert_eq!(drag.insertion, Some(2));
        assert!(drag.update(71.0));
        assert_eq!(drag.insertion, Some(1));
        assert!(drag.update(-100.0));
        assert_eq!(drag.insertion, Some(0));
        assert!(drag.update(1000.0));
        assert_eq!(drag.insertion, Some(3));
        assert_eq!(drag.order, ids);
    }
}
