//! Sender（底部命令发送面板，sender.md）的纯逻辑：草稿与编辑、保存作用域、分页、重复发送参数、
//! 菜单项编码、状态文字。对照 mac 版 `apple/Kanso/App/Sender/SenderModel.swift`。
//!
//! 调度本身（逐行发送、等提示符、重复、跳过被占用的终端）在 `kanso_core::sender`，这里只负责把
//! 界面状态翻译成 [`SenderRequest`]、把 [`SenderStatus`] 翻译成文字。GPUI 视图在 `ui/sender.rs`。
//!
//! 本文件不依赖窗口系统，三个平台都能编译与测试。

use kanso_core::sender::{SenderRequest, SenderStatus};
use kanso_types::profile::{Group, LineEnding, Profile, Snippet, SnippetKindScope, SnippetScope};
use kanso_types::{Message, ProfileId, SessionId, SnippetId};

use crate::app::launcher::render_message;
use crate::app::tab::TabKind;
use crate::widgets::menu::MenuItem;

// ---- 常量（sender.md §1、§3、§5）----

/// 已保存命令每页条数。
pub const PAGE_SIZE: usize = 24;
/// 命令库容量上限。
pub const MAX_SNIPPETS: usize = 1000;
/// 名称最长字符数。
pub const NAME_MAX_CHARS: usize = 80;
/// 面板默认高度（逻辑像素，不含分隔条）。
pub const PANEL_DEFAULT_HEIGHT: f32 = 160.0;
/// 面板最小高度。
pub const PANEL_MIN_HEIGHT: f32 = 80.0;
/// 面板最大高度。
pub const PANEL_MAX_HEIGHT: f32 = 400.0;
/// 面板顶部拖动条的厚度。
pub const HANDLE_THICKNESS: f32 = 5.0;
/// 重复间隔范围（秒）与默认值。
pub const INTERVAL_MIN: u32 = 1;
/// 重复间隔上限（秒）。
pub const INTERVAL_MAX: u32 = 86_400;
/// 默认重复间隔（秒）。
pub const INTERVAL_DEFAULT: u32 = 60;
/// 重复次数上限，0 表示一直重复。
pub const COUNT_MAX: u32 = 100_000;
/// 设置键：重复间隔。
pub const SETTING_INTERVAL: &str = "sender.interval";
/// 设置键：重复次数。
pub const SETTING_COUNT: &str = "sender.count";
/// 设置键：上次保存时选择的作用域层级。
pub const SETTING_SCOPE_LEVEL: &str = "sender.scopeLevel";
/// 菜单项 id 前缀，外壳据此把菜单结果转给 Sender 面板。
pub const MENU_PREFIX: &str = "sender:";

/// 渲染一条界面文字。
pub fn t(message: Message) -> String {
    render_message(&message)
}

/// 把面板高度夹进允许范围。
pub fn clamp_panel_height(height: f32) -> f32 {
    if height.is_nan() {
        PANEL_DEFAULT_HEIGHT
    } else {
        height.clamp(PANEL_MIN_HEIGHT, PANEL_MAX_HEIGHT)
    }
}

/// 把拖动结果换算成面板高度：拖动条向上移动（`delta_y` 为负）面板变高。
pub fn dragged_height(start_height: f32, delta_y: f32) -> f32 {
    clamp_panel_height(start_height - delta_y)
}

// ---- 数字输入（D8：只接受整数，越界夹到边界）----

/// 解析整数输入框：只含 ASCII 数字才算有效；超出范围夹到边界，过长的数字当作上限。空串返回 `None`。
pub fn parse_clamped(text: &str, min: u32, max: u32) -> Option<u32> {
    let t = text.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value = t.parse::<u64>().unwrap_or(u64::MAX);
    Some(value.clamp(u64::from(min), u64::from(max)) as u32)
}

/// 间隔输入失焦时的最终值：无效输入退回 `fallback`。
pub fn commit_interval(text: &str, fallback: u32) -> u32 {
    parse_clamped(text, INTERVAL_MIN, INTERVAL_MAX)
        .unwrap_or(fallback.clamp(INTERVAL_MIN, INTERVAL_MAX))
}

/// 次数输入失焦时的最终值：无效输入退回 `fallback`。
pub fn commit_count(text: &str, fallback: u32) -> u32 {
    parse_clamped(text, 0, COUNT_MAX).unwrap_or(fallback.min(COUNT_MAX))
}

/// 读取设置里保存的整数（缺失或不是整数时用默认值）。
pub fn stored_number(json: Option<&str>, default: u32, min: u32, max: u32) -> u32 {
    json.and_then(|j| j.trim().parse::<i64>().ok())
        .map_or(default, |v| v.clamp(i64::from(min), i64::from(max)) as u32)
}

// ---- 命令文本 ----

/// 是否有可发送 / 保存的内容（D8：纯空白与空文本同等处理）。
pub fn has_content(text: &str) -> bool {
    !text.trim().is_empty()
}

/// 命令的第一个非空白行（保留行首空格，sender.md §2.1）。
pub fn first_line(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_owned()
}

/// 保存时使用的名称：名称框内容（去掉首尾空白）为空就用第一个非空白行，最多 80 个字符。
pub fn derive_name(name: &str, text: &str) -> String {
    let name = name.trim();
    let base = if name.is_empty() {
        first_line(text)
    } else {
        name.to_owned()
    };
    base.chars().take(NAME_MAX_CHARS).collect()
}

/// 回车键的含义（sender.md §2.1）：输入法组字中归输入法，Shift+Enter 换行，其余发送。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnterAction {
    /// 发起手动发送。
    Send,
    /// 插入换行。
    Newline,
    /// 交给输入法。
    InputMethod,
}

/// 命令框里按回车时该做什么。
pub fn enter_action(shift: bool, composing: bool) -> EnterAction {
    if composing {
        EnterAction::InputMethod
    } else if shift {
        EnterAction::Newline
    } else {
        EnterAction::Send
    }
}

// ---- 行尾 ----

/// 行尾下拉框的选项顺序。
pub const ENDINGS: [LineEnding; 3] = [LineEnding::Lf, LineEnding::CrLf, LineEnding::None];

/// 行尾的显示文字。
pub fn ending_label(ending: LineEnding) -> String {
    match ending {
        LineEnding::None => t(Message::UiSenderEndingNone {}),
        LineEnding::Lf => "LF".to_owned(),
        LineEnding::CrLf => "CRLF".to_owned(),
    }
}

/// 行尾在下拉框里的序号。
pub fn ending_index(ending: LineEnding) -> usize {
    ENDINGS.iter().position(|e| *e == ending).unwrap_or(0)
}

fn ending_key(ending: LineEnding) -> &'static str {
    match ending {
        LineEnding::None => "none",
        LineEnding::Lf => "lf",
        LineEnding::CrLf => "crlf",
    }
}

fn ending_from_key(key: &str) -> Option<LineEnding> {
    Some(match key {
        "none" => LineEnding::None,
        "lf" => LineEnding::Lf,
        "crlf" => LineEnding::CrLf,
        _ => return None,
    })
}

// ---- 草稿与编辑 ----

/// 编写区的内容。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    /// 命令正文。
    pub text: String,
    /// 名称框。
    pub name: String,
    /// 行尾。
    pub ending: LineEnding,
}

impl Default for Draft {
    fn default() -> Self {
        Draft {
            text: String::new(),
            name: String::new(),
            ending: LineEnding::Lf,
        }
    }
}

/// 草稿与“正在编辑哪条已保存命令”。编辑时记住进入编辑前的草稿，取消时恢复（sender.md §3.3）。
#[derive(Clone, Debug, Default)]
pub struct Editing {
    /// 当前草稿。
    pub draft: Draft,
    editing: Option<SnippetId>,
    original: Option<Draft>,
}

impl Editing {
    /// 正在编辑的命令。
    pub fn editing(&self) -> Option<SnippetId> {
        self.editing
    }

    /// 开始（或切换到）编辑某条命令。第一次进入时保存草稿；切换条目不覆盖它。
    pub fn begin(&mut self, snippet: &Snippet) {
        if self.original.is_none() {
            self.original = Some(self.draft.clone());
        }
        self.editing = Some(snippet.id);
        self.draft = Draft {
            text: snippet.text.clone(),
            name: snippet.name.clone(),
            ending: snippet.line_ending,
        };
    }

    /// 取消编辑：恢复进入编辑前的草稿。没有在编辑时什么也不做。
    pub fn cancel(&mut self) {
        if let Some(original) = self.original.take() {
            self.draft = original;
        }
        self.editing = None;
    }

    /// 更新成功：结束编辑，命令文本留在编写区。
    pub fn finish(&mut self) {
        self.editing = None;
        self.original = None;
    }

    /// 某条命令被删除：正在编辑它就退出编辑并恢复草稿。返回是否发生了退出。
    pub fn deleted(&mut self, id: SnippetId) -> bool {
        if self.editing == Some(id) {
            self.cancel();
            return true;
        }
        false
    }

    /// 命令库被外部改动（导入、删除会话或分组）后退出编辑，避免继续覆盖已变化的条目。
    pub fn library_changed(&mut self) -> bool {
        if self.editing.is_some() {
            self.cancel();
            return true;
        }
        false
    }
}

/// 能否保存 / 更新（D8、sender.md §3.1：达到上限时不能新增，仍可更新）。
pub fn can_save(text: &str, saving: bool, editing: bool, library_len: usize) -> bool {
    has_content(text) && !saving && (editing || library_len < MAX_SNIPPETS)
}

// ---- 当前上下文 ----

/// 面板需要知道的一个终端标签。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SenderTab {
    /// 会话 id。
    pub id: SessionId,
    /// 种类。
    pub kind: TabKind,
    /// 是否已连接。
    pub connected: bool,
    /// 显示名。
    pub name: String,
    /// 对应的已保存会话。
    pub profile: Option<ProfileId>,
}

impl SenderTab {
    /// 能接收命令：有终端且已连接。
    pub fn can_receive(&self) -> bool {
        self.kind != TabKind::Sftp && self.connected
    }
}

/// 外壳每次把当前窗口状态交给面板的快照。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SenderContext {
    /// 当前活动标签（最后获得焦点的分屏里的活动标签）。
    pub active: Option<SenderTab>,
    /// 同一窗口全部标签（含其他分屏与后台标签）。
    pub tabs: Vec<SenderTab>,
}

impl SenderContext {
    /// 已连接的终端标签数（目标选择器显示的数量，不含纯 SFTP 与断线终端，D3）。
    pub fn connected_count(&self) -> usize {
        self.tabs.iter().filter(|t| t.can_receive()).count()
    }

    /// 当前会话能否接收（0 或 1）。
    pub fn current_count(&self) -> usize {
        usize::from(self.active.as_ref().is_some_and(SenderTab::can_receive))
    }

    /// 全部标签 id，交给核心调度。
    pub fn session_ids(&self) -> Vec<SessionId> {
        self.tabs.iter().map(|t| t.id).collect()
    }
}

/// 发送目标。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Target {
    /// 当前会话。
    #[default]
    Current,
    /// 全部已连接终端。
    All,
}

/// 目标下拉框的选项文字。
pub fn target_items(ctx: &SenderContext) -> [String; 2] {
    [
        t(Message::UiSenderTargetCurrent {
            count: ctx.current_count() as i64,
        }),
        t(Message::UiSenderTargetAll {
            count: ctx.connected_count() as i64,
        }),
    ]
}

/// 发送失败的原因（界面提示用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
    /// 没有可发送的内容。
    Empty,
    /// 没有可接收的会话。
    NoSessions,
}

impl SendError {
    /// 提示文字。
    pub fn text(self) -> String {
        match self {
            SendError::Empty => t(Message::SenderEmptyCommand {}),
            SendError::NoSessions => t(Message::SenderNoSessions {}),
        }
    }
}

/// 组装核心的发送请求（sender.md §4、§5）。当前会话目标要求有终端活动标签，全部会话则交给核心筛选。
pub fn build_request(
    text: &str,
    ending: LineEnding,
    target: Target,
    ctx: &SenderContext,
    repeating: bool,
    interval: u32,
    count: u32,
) -> Result<SenderRequest, SendError> {
    if !has_content(text) {
        return Err(SendError::Empty);
    }
    let fixed = match target {
        Target::All => None,
        Target::Current => match &ctx.active {
            Some(tab) if tab.kind != TabKind::Sftp => Some(tab.id),
            _ => return Err(SendError::NoSessions),
        },
    };
    Ok(SenderRequest {
        text: text.to_owned(),
        ending,
        target: fixed,
        repeating,
        interval_seconds: interval.clamp(INTERVAL_MIN, INTERVAL_MAX),
        count: count.min(COUNT_MAX),
    })
}

// ---- 保存作用域 ----

/// 保存位置选项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeChoice {
    /// 显示文字。
    pub title: String,
    /// 作用域。
    pub scope: SnippetScope,
    /// 层级：全局 0、会话类型 1、分组 2、会话 3，用来记住“上次选择”。
    pub level: u8,
}

/// 当前上下文可选的保存位置（sender.md §3.1）：全局总是有；有终端会话时加会话类型；
/// 会话来自已保存配置时再加它所在的分组（如有）和它自己。
pub fn scope_choices(
    active: Option<&SenderTab>,
    profiles: &[Profile],
    groups: &[Group],
) -> Vec<ScopeChoice> {
    let mut out = vec![ScopeChoice {
        title: t(Message::UiSenderScopeGlobal {}),
        scope: SnippetScope::Global,
        level: 0,
    }];
    let Some(tab) = active else { return out };
    match tab.kind {
        TabKind::Ssh => out.push(ScopeChoice {
            title: t(Message::UiSenderScopeSsh {}),
            scope: SnippetScope::Kind(SnippetKindScope::Ssh),
            level: 1,
        }),
        TabKind::Local => out.push(ScopeChoice {
            title: t(Message::UiSenderScopeLocal {}),
            scope: SnippetScope::Kind(SnippetKindScope::Local),
            level: 1,
        }),
        TabKind::Sftp => {}
    }
    if let Some(profile) = tab
        .profile
        .and_then(|id| profiles.iter().find(|p| p.id == id))
    {
        if let Some(group) = profile
            .group
            .and_then(|gid| groups.iter().find(|g| g.id == gid))
        {
            out.push(ScopeChoice {
                title: t(Message::UiSenderScopeGroup {
                    name: group.name.clone(),
                }),
                scope: SnippetScope::Group(group.id),
                level: 2,
            });
        }
        out.push(ScopeChoice {
            title: t(Message::UiSenderScopeProfile {
                name: profile.name.clone(),
            }),
            scope: SnippetScope::Profile(profile.id),
            level: 3,
        });
    }
    out
}

/// 预选项：不超过上次层级的最后一项，即“最近的更宽范围”（无法使用上次层级时往宽处退）。
pub fn default_scope_index(choices: &[ScopeChoice], last_level: u8) -> usize {
    choices
        .iter()
        .rposition(|c| c.level <= last_level)
        .unwrap_or(0)
}

/// 已保存命令所属范围的显示文字（悬停提示用）。
pub fn scope_title(scope: &SnippetScope, profiles: &[Profile], groups: &[Group]) -> String {
    match scope {
        SnippetScope::Global => t(Message::UiSenderScopeGlobal {}),
        SnippetScope::Kind(SnippetKindScope::Ssh) => t(Message::UiSenderScopeSsh {}),
        SnippetScope::Kind(SnippetKindScope::Local) => t(Message::UiSenderScopeLocal {}),
        SnippetScope::Group(id) => t(Message::UiSenderScopeGroup {
            name: groups
                .iter()
                .find(|g| g.id == *id)
                .map_or_else(|| id.to_string(), |g| g.name.clone()),
        }),
        SnippetScope::Profile(id) => t(Message::UiSenderScopeProfile {
            name: profiles
                .iter()
                .find(|p| p.id == *id)
                .map_or_else(|| id.to_string(), |p| p.name.clone()),
        }),
    }
}

// ---- 列表与分页 ----

/// 按名称（忽略大小写）排序，名称相同保持原顺序。
pub fn sort_by_name(snippets: &mut [Snippet]) {
    snippets.sort_by_key(|s| s.name.to_lowercase());
}

/// 页数（至少 1 页）。
pub fn page_count(len: usize) -> usize {
    len.div_ceil(PAGE_SIZE).max(1)
}

/// 把页码夹进有效范围（切换会话导致页数减少时落在仍有效的页）。
pub fn clamp_page(page: usize, len: usize) -> usize {
    page.min(page_count(len) - 1)
}

/// 某一页的条目。
pub fn page_items<T>(items: &[T], page: usize) -> &[T] {
    let start = (page * PAGE_SIZE).min(items.len());
    let end = (start + PAGE_SIZE).min(items.len());
    &items[start..end]
}

/// 悬停提示：所属范围加完整正文（过长截断）。
pub fn snippet_tooltip(snippet: &Snippet, profiles: &[Profile], groups: &[Group]) -> String {
    const MAX: usize = 600;
    let mut body: String = snippet.text.chars().take(MAX).collect();
    if snippet.text.chars().count() > MAX {
        body.push('…');
    }
    format!(
        "{}\n{}",
        scope_title(&snippet.scope, profiles, groups),
        body
    )
}

// ---- 进度与提示文字 ----

/// 重复任务运行条的文字：首个非空白行、间隔、目标、进度。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RepeatInfo {
    /// 命令首个非空白行。
    pub title: String,
    /// 间隔秒数。
    pub interval: u32,
    /// 目标说明（“全部会话”或“当前会话 · 名称”）。
    pub target: String,
}

/// 运行条的一行文字（sender.md §5.3）。
pub fn repeat_bar_text(info: &RepeatInfo, status: &SenderStatus) -> String {
    t(Message::UiSenderRepeatBar {
        title: info.title.clone(),
        interval: i64::from(info.interval),
        dest: info.target.clone(),
        progress: progress_text(status),
    })
}

/// 进度：已走轮次（有限次显示总数）加倒计时 / 正在发送。
pub fn progress_text(status: &SenderStatus) -> String {
    let rounds = if status.count == 0 {
        t(Message::UiSenderRounds {
            rounds: i64::from(status.rounds),
        })
    } else {
        t(Message::UiSenderRoundsOf {
            rounds: i64::from(status.rounds),
            count: i64::from(status.count),
        })
    };
    if status.next_in_ms == 0 {
        t(Message::UiSenderSending { progress: rounds })
    } else {
        t(Message::UiSenderNextIn {
            progress: rounds,
            wait: status.next_in_ms.div_ceil(1000) as i64,
        })
    }
}

/// 状态栏里的“重复发送运行中”提示（面板收起时显示，D7）。
pub fn running_chip_text(status: &SenderStatus) -> String {
    if status.count == 0 {
        t(Message::UiSenderChip {
            rounds: i64::from(status.rounds),
        })
    } else {
        t(Message::UiSenderChipOf {
            rounds: i64::from(status.rounds),
            count: i64::from(status.count),
        })
    }
}

/// 把核心本次更新里的提示合并成一行文字；没有提示返回 `None`。
pub fn notice_text(status: &SenderStatus) -> Option<String> {
    if status.notices.is_empty() {
        return None;
    }
    Some(
        status
            .notices
            .iter()
            .map(render_message)
            .collect::<Vec<_>>()
            .join(&t(Message::UiSenderJoin {})),
    )
}

// ---- 菜单 ----

/// 已保存命令上的右键菜单动作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnippetAction {
    /// 把命令带进编写区编辑。
    Edit(SnippetId),
    /// 用保存的正文开始重复发送。
    Repeat(SnippetId),
    /// 改行尾（直接更新该条）。
    Ending(SnippetId, LineEnding),
    /// 改作用域（序号指向 [`scope_choices`]）。
    Scope(SnippetId, usize),
    /// 删除。
    Delete(SnippetId),
    /// 保存按钮菜单：选择存放范围（序号指向 [`scope_choices`]）。
    Save(usize),
}

/// 动作 → 菜单项 id。
pub fn encode_action(action: &SnippetAction) -> String {
    match action {
        SnippetAction::Edit(id) => format!("{MENU_PREFIX}edit:{id}"),
        SnippetAction::Repeat(id) => format!("{MENU_PREFIX}repeat:{id}"),
        SnippetAction::Ending(id, e) => format!("{MENU_PREFIX}ending:{id}:{}", ending_key(*e)),
        SnippetAction::Scope(id, i) => format!("{MENU_PREFIX}scope:{id}:{i}"),
        SnippetAction::Delete(id) => format!("{MENU_PREFIX}delete:{id}"),
        SnippetAction::Save(i) => format!("{MENU_PREFIX}save:{i}"),
    }
}

/// 菜单项 id → 动作；不认识的返回 `None`。
pub fn decode_action(id: &str) -> Option<SnippetAction> {
    let rest = id.strip_prefix(MENU_PREFIX)?;
    let (kind, tail) = rest.split_once(':')?;
    let snippet = |s: &str| s.parse::<SnippetId>().ok();
    Some(match kind {
        "edit" => SnippetAction::Edit(snippet(tail)?),
        "repeat" => SnippetAction::Repeat(snippet(tail)?),
        "delete" => SnippetAction::Delete(snippet(tail)?),
        "ending" => {
            let (sid, key) = tail.split_once(':')?;
            SnippetAction::Ending(snippet(sid)?, ending_from_key(key)?)
        }
        "scope" => {
            let (sid, idx) = tail.split_once(':')?;
            SnippetAction::Scope(snippet(sid)?, idx.parse().ok()?)
        }
        "save" => SnippetAction::Save(tail.parse().ok()?),
        _ => return None,
    })
}

/// 已保存命令的右键菜单（sender.md §3.3）：编辑、重复发送、改行尾、改作用域、删除。
pub fn snippet_menu(snippet: &Snippet, choices: &[ScopeChoice]) -> Vec<MenuItem> {
    let id = snippet.id;
    let endings = ENDINGS
        .iter()
        .map(|e| {
            MenuItem::action(
                encode_action(&SnippetAction::Ending(id, *e)),
                ending_label(*e),
            )
            .checked(*e == snippet.line_ending)
        })
        .collect();
    let scopes = choices
        .iter()
        .enumerate()
        .map(|(i, c)| {
            MenuItem::action(encode_action(&SnippetAction::Scope(id, i)), c.title.clone())
                .checked(c.scope == snippet.scope)
        })
        .collect();
    vec![
        MenuItem::action(
            encode_action(&SnippetAction::Edit(id)),
            t(Message::UiSenderMenuEdit {}),
        ),
        MenuItem::action(
            encode_action(&SnippetAction::Repeat(id)),
            t(Message::UiSenderMenuRepeat {}),
        ),
        MenuItem::separator(),
        MenuItem::submenu(t(Message::UiSenderMenuEnding {}), endings),
        MenuItem::submenu(t(Message::UiSenderMenuScope {}), scopes),
        MenuItem::separator(),
        MenuItem::action(
            encode_action(&SnippetAction::Delete(id)),
            t(Message::UiSenderMenuDelete {}),
        ),
    ]
}

/// 点“保存 / 更新”后选择存放范围的菜单；`preselect` 打勾。
pub fn save_menu(choices: &[ScopeChoice], preselect: usize) -> Vec<MenuItem> {
    choices
        .iter()
        .enumerate()
        .map(|(i, c)| {
            MenuItem::action(encode_action(&SnippetAction::Save(i)), c.title.clone())
                .checked(i == preselect)
        })
        .collect()
}

/// 用草稿与选定范围组装要保存的命令；更新时沿用原 id。
pub fn build_snippet(
    draft: &Draft,
    editing: Option<SnippetId>,
    scope: SnippetScope,
) -> Option<Snippet> {
    has_content(&draft.text).then(|| Snippet {
        id: editing.unwrap_or_else(SnippetId::new),
        name: derive_name(&draft.name, &draft.text),
        text: draft.text.clone(),
        line_ending: draft.ending,
        scope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::{group, local_profile, ssh_profile};

    fn snippet(name: &str, text: &str) -> Snippet {
        Snippet {
            id: SnippetId::new(),
            name: name.into(),
            text: text.into(),
            line_ending: LineEnding::Lf,
            scope: SnippetScope::Global,
        }
    }

    fn tab(raw: u64, kind: TabKind, connected: bool, profile: Option<ProfileId>) -> SenderTab {
        SenderTab {
            id: SessionId::from_raw(raw),
            kind,
            connected,
            name: format!("t{raw}"),
            profile,
        }
    }

    #[test]
    fn panel_height_and_drag() {
        assert_eq!(clamp_panel_height(10.0), 80.0);
        assert_eq!(clamp_panel_height(999.0), 400.0);
        assert_eq!(clamp_panel_height(f32::NAN), PANEL_DEFAULT_HEIGHT);
        // 拖动条向上拖 40 像素，面板长高 40。
        assert_eq!(dragged_height(160.0, -40.0), 200.0);
        assert_eq!(dragged_height(160.0, 500.0), 80.0);
    }

    #[test]
    fn numeric_inputs_accept_integers_only_and_clamp() {
        assert_eq!(parse_clamped("30", 1, 100), Some(30));
        assert_eq!(parse_clamped(" 7 ", 1, 100), Some(7));
        assert_eq!(parse_clamped("0", 1, 100), Some(1));
        assert_eq!(parse_clamped("999", 1, 100), Some(100));
        assert_eq!(parse_clamped("99999999999999999999999", 1, 100), Some(100));
        assert_eq!(parse_clamped("", 1, 100), None);
        assert_eq!(parse_clamped("1.5", 1, 100), None);
        assert_eq!(parse_clamped("-3", 1, 100), None);
        assert_eq!(parse_clamped("一二", 1, 100), None);
        assert_eq!(commit_interval("abc", 60), 60);
        assert_eq!(commit_interval("100000", 60), 86_400);
        assert_eq!(commit_count("", 5), 5);
        assert_eq!(commit_count("0", 5), 0);
        assert_eq!(commit_count("200000", 5), COUNT_MAX);
    }

    #[test]
    fn stored_numbers_fall_back_to_defaults() {
        assert_eq!(stored_number(None, 60, 1, 86400), 60);
        assert_eq!(stored_number(Some("30"), 60, 1, 86400), 30);
        assert_eq!(stored_number(Some("\"x\""), 60, 1, 86400), 60);
        assert_eq!(stored_number(Some("999999"), 60, 1, 86400), 86400);
        assert_eq!(stored_number(Some("-5"), 60, 1, 86400), 1);
    }

    #[test]
    fn names_come_from_the_first_non_blank_line() {
        assert_eq!(derive_name("  ", "\n   \n  ls -la\nnext"), "  ls -la");
        assert_eq!(derive_name("  部署  ", "ls"), "部署");
        let long = "x".repeat(200);
        assert_eq!(derive_name("", &long).chars().count(), NAME_MAX_CHARS);
        assert_eq!(first_line("   \n\t"), "");
        assert!(!has_content(" \n\t "));
        assert!(has_content(" a "));
    }

    #[test]
    fn enter_key_meaning() {
        assert_eq!(enter_action(false, false), EnterAction::Send);
        assert_eq!(enter_action(true, false), EnterAction::Newline);
        assert_eq!(enter_action(false, true), EnterAction::InputMethod);
        assert_eq!(enter_action(true, true), EnterAction::InputMethod);
    }

    #[test]
    fn cancelling_edit_restores_the_very_first_draft() {
        let mut e = Editing::default();
        e.draft.text = "draft".into();
        let a = snippet("a", "echo a");
        let b = snippet("b", "echo b");
        e.begin(&a);
        assert_eq!(e.editing(), Some(a.id));
        assert_eq!(e.draft.text, "echo a");
        // 切换到另一条：取消仍恢复第一次进入前的草稿。
        e.begin(&b);
        assert_eq!(e.draft.text, "echo b");
        e.cancel();
        assert_eq!(e.draft.text, "draft");
        assert_eq!(e.editing(), None);
        // 没有在编辑时取消不动草稿。
        e.cancel();
        assert_eq!(e.draft.text, "draft");
    }

    #[test]
    fn finishing_keeps_text_and_deleting_the_edited_item_exits() {
        let mut e = Editing::default();
        let a = snippet("a", "echo a");
        e.begin(&a);
        e.finish();
        assert_eq!(e.draft.text, "echo a");
        assert_eq!(e.editing(), None);
        e.draft.text = "mine".into();
        e.begin(&a);
        assert!(!e.deleted(SnippetId::new()));
        assert!(e.deleted(a.id));
        assert_eq!(e.draft.text, "mine");
        e.begin(&a);
        assert!(e.library_changed());
        assert!(!e.library_changed());
    }

    #[test]
    fn save_gate_follows_the_library_limit() {
        assert!(can_save("x", false, false, 999));
        assert!(!can_save("x", false, false, MAX_SNIPPETS));
        assert!(can_save("x", false, true, MAX_SNIPPETS));
        assert!(!can_save("  ", false, true, 0));
        assert!(!can_save("x", true, true, 0));
    }

    #[test]
    fn request_targets_follow_the_spec() {
        let local = tab(1, TabKind::Local, true, None);
        let sftp = tab(2, TabKind::Sftp, true, None);
        let ctx = SenderContext {
            active: Some(local.clone()),
            tabs: vec![
                local.clone(),
                sftp.clone(),
                tab(3, TabKind::Ssh, false, None),
                tab(4, TabKind::Ssh, true, None),
            ],
        };
        assert_eq!(ctx.connected_count(), 2);
        assert_eq!(ctx.current_count(), 1);
        let cur = build_request("ls", LineEnding::Lf, Target::Current, &ctx, false, 60, 0).unwrap();
        assert_eq!(cur.target, Some(local.id));
        let all = build_request("ls", LineEnding::Lf, Target::All, &ctx, true, 0, 999_999).unwrap();
        assert_eq!(all.target, None);
        assert_eq!((all.interval_seconds, all.count), (1, COUNT_MAX));
        assert_eq!(
            build_request(" \n", LineEnding::Lf, Target::All, &ctx, false, 60, 0).err(),
            Some(SendError::Empty)
        );
        // 当前标签是纯 SFTP 或没有活动标签：当前会话没有可发送的会话，不改发到别处。
        let on_sftp = SenderContext {
            active: Some(sftp),
            ..ctx.clone()
        };
        assert_eq!(
            build_request(
                "ls",
                LineEnding::Lf,
                Target::Current,
                &on_sftp,
                false,
                60,
                0
            )
            .err(),
            Some(SendError::NoSessions)
        );
        let none = SenderContext::default();
        assert_eq!(
            build_request("ls", LineEnding::Lf, Target::Current, &none, false, 60, 0).err(),
            Some(SendError::NoSessions)
        );
        assert_eq!(target_items(&ctx)[1], "全部会话（2）");
    }

    #[test]
    fn scope_choices_depend_on_the_session() {
        let grp = group("生产");
        let mut p = ssh_profile("web", "h", "u");
        p.group = Some(grp.id);
        let profiles = vec![p.clone()];
        let groups = vec![grp.clone()];
        let ssh = tab(1, TabKind::Ssh, true, Some(p.id));
        let c = scope_choices(Some(&ssh), &profiles, &groups);
        assert_eq!(
            c.iter().map(|x| x.level).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(c[1].scope, SnippetScope::Kind(SnippetKindScope::Ssh));
        assert_eq!(c[2].scope, SnippetScope::Group(grp.id));
        assert_eq!(c[3].scope, SnippetScope::Profile(p.id));
        // 快速连接：只有类型和全局。
        let quick = tab(2, TabKind::Local, true, None);
        let c = scope_choices(Some(&quick), &profiles, &groups);
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].scope, SnippetScope::Kind(SnippetKindScope::Local));
        // 没有活动会话：只有全局。
        assert_eq!(scope_choices(None, &profiles, &groups).len(), 1);
        // 未分组的已保存会话没有分组项。
        let lp = local_profile("l", "pwsh");
        let t = tab(3, TabKind::Local, true, Some(lp.id));
        let c = scope_choices(Some(&t), &[lp], &[]);
        assert_eq!(c.iter().map(|x| x.level).collect::<Vec<_>>(), vec![0, 1, 3]);
    }

    #[test]
    fn default_scope_steps_to_the_nearest_wider_level() {
        let mk = |level| ScopeChoice {
            title: String::new(),
            scope: SnippetScope::Global,
            level,
        };
        let choices = vec![mk(0), mk(1), mk(3)];
        assert_eq!(default_scope_index(&choices, 3), 2);
        assert_eq!(default_scope_index(&choices, 2), 1);
        assert_eq!(default_scope_index(&choices, 0), 0);
        assert_eq!(default_scope_index(&[], 3), 0);
    }

    #[test]
    fn paging_clamps_to_a_valid_page() {
        assert_eq!(page_count(0), 1);
        assert_eq!(page_count(24), 1);
        assert_eq!(page_count(25), 2);
        assert_eq!(clamp_page(5, 30), 1);
        assert_eq!(clamp_page(0, 0), 0);
        let items: Vec<usize> = (0..30).collect();
        assert_eq!(page_items(&items, 0).len(), 24);
        assert_eq!(page_items(&items, 1), &items[24..]);
        assert!(page_items(&items, 9).is_empty());
    }

    #[test]
    fn sorting_ignores_case_and_keeps_duplicates() {
        let mut v = vec![snippet("b", "1"), snippet("A", "2"), snippet("a", "3")];
        sort_by_name(&mut v);
        assert_eq!(
            v.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            vec!["2", "3", "1"]
        );
    }

    #[test]
    fn progress_text_shows_rounds_and_countdown() {
        let mut st = SenderStatus {
            rounds: 2,
            count: 0,
            next_in_ms: 0,
            ..Default::default()
        };
        assert_eq!(progress_text(&st), "已发 2 轮 · 正在发送");
        st.count = 5;
        st.next_in_ms = 1500;
        assert_eq!(progress_text(&st), "已发 2 / 5 轮 · 2 秒后发送");
        assert_eq!(running_chip_text(&st), "重复发送 · 2/5");
        st.count = 0;
        assert_eq!(running_chip_text(&st), "重复发送 · 2 轮");
        let info = RepeatInfo {
            title: "ls".into(),
            interval: 30,
            target: "全部会话".into(),
        };
        assert!(repeat_bar_text(&info, &st).starts_with("重复发送：ls · 每 30 秒 · 全部会话"));
    }

    #[test]
    fn notices_are_rendered_and_joined() {
        let mut st = SenderStatus::default();
        assert_eq!(notice_text(&st), None);
        st.notices.push(kanso_types::Message::SenderBusy {});
        st.notices
            .push(kanso_types::Message::SenderRepeatReplaced {});
        let text = notice_text(&st).unwrap();
        assert!(text.contains('；'), "{text}");
    }

    #[test]
    fn menu_ids_round_trip() {
        let id = SnippetId::new();
        for action in [
            SnippetAction::Edit(id),
            SnippetAction::Repeat(id),
            SnippetAction::Delete(id),
            SnippetAction::Ending(id, LineEnding::CrLf),
            SnippetAction::Ending(id, LineEnding::None),
            SnippetAction::Scope(id, 2),
            SnippetAction::Save(3),
        ] {
            assert_eq!(decode_action(&encode_action(&action)), Some(action.clone()));
        }
        assert_eq!(decode_action("sender:nope:1"), None);
        assert_eq!(decode_action("sender:edit:not-a-uuid"), None);
        assert_eq!(decode_action("other:edit"), None);
    }

    #[test]
    fn snippet_menu_marks_the_current_values() {
        let mut s = snippet("n", "x");
        s.line_ending = LineEnding::CrLf;
        let choices = scope_choices(None, &[], &[]);
        let items = snippet_menu(&s, &choices);
        let MenuItem::Submenu { items: endings, .. } = &items[3] else {
            panic!("第 4 项应是行尾子菜单")
        };
        let checked: Vec<_> = endings
            .iter()
            .filter_map(|i| match i {
                MenuItem::Action { label, checked, .. } if *checked => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(checked, vec!["CRLF".to_owned()]);
        let save = save_menu(&choices, 0);
        assert_eq!(save.len(), choices.len());
    }

    #[test]
    fn build_snippet_reuses_the_id_on_update() {
        let draft = Draft {
            text: "  top\nx".into(),
            name: String::new(),
            ending: LineEnding::CrLf,
        };
        let id = SnippetId::new();
        let s = build_snippet(&draft, Some(id), SnippetScope::Global).unwrap();
        assert_eq!((s.id, s.name.as_str()), (id, "  top"));
        assert_eq!(s.line_ending, LineEnding::CrLf);
        let fresh = build_snippet(&draft, None, SnippetScope::Global).unwrap();
        assert_ne!(fresh.id, id);
        let empty = Draft::default();
        assert!(build_snippet(&empty, None, SnippetScope::Global).is_none());
    }

    #[test]
    fn tooltip_shows_scope_and_text() {
        let s = snippet("n", "echo hi");
        let t = snippet_tooltip(&s, &[], &[]);
        assert_eq!(t, "全局\necho hi");
    }
}
