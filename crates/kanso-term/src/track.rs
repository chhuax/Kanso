//! 命令跟踪与 AI 轮次的状态机（terminal.md §5、§6.2）。
//!
//! 本模块是 sans-IO 的：不读系统时钟（时间由调用方以单调毫秒 `now` 传入）、不发事件（只返回 [`Effect`]），
//! 读屏幕用的是调用方传入的 `&Terminal`。本地与 SSH 会话共用它，会话层负责加锁、定时唤醒和把 `Effect` 变成事件。
//!
//! # 两个信号来源
//!
//! 活动（运行 / 空闲）的**转换**只有一个权威来源：
//! - 收到过 OSC 133 之后永远以它为准（`osc133` 置位后不再回退）；
//! - 否则靠提示符特征推断（terminal.md §5.1 第 2 条）。
//!
//! 而"用户提交了什么命令"始终靠**用户输入 + 屏幕内容**得到，两种来源共用：OSC 133 只告诉我们命令开始 / 结束，
//! 不带命令文本，AI CLI 识别（§6.1）又需要这段文本。
//!
//! # 推断的状态
//!
//! ```text
//! AtPrompt ──Enter（输入起点有效）──▶ Submitted ──读到命令文本（延迟 / 提示符返回）──▶ Running
//!    ▲                                   │ 文本为空（空回车）                            │
//!    └───────────────────────────────────┴──────── 提示符特征再次出现 / OSC 7 ─────────┘
//! ```
//!
//! - `AtPrompt`：等待用户输入。`start` 是"输入起点"（首次按键时光标的位置，也就是提示符结束处）。
//! - `Submitted`：回车已按下，命令文本要稍后再读（等远端回显到达）。
//! - `Running`：命令在运行；期间的任何用户输入都属于这条命令，不是新命令行。
//!
//! # 为什么 `armed`
//!
//! 输入起点作废（被全屏程序覆盖、被回滚裁掉、多行粘贴）之后，光标已经不在提示符末尾，
//! 此时再按键就把"当前光标"当作输入起点会得到错误的提示符特征，后果是命令永远等不到"返回"。
//! 所以作废之后要等**确定会出现新提示符**的动作（回车、Ctrl+C，或上一条命令返回）才重新"上膛"。
//!
//! # 已知局限（推断路径）
//!
//! - 提示符里带会变化的内容（`cd` 之后目录变了、时钟）时，特征对不上，命令会一直"运行中"，
//!   直到提示符恢复成原样，或远端 shell 上报了 OSC 7（此时把它当作提示符出现的信号）。
//! - tab 补全列出候选项之后，输入起点所在行不再是光标所在行，那一条命令会作废（不被跟踪）。

use kanso_types::agent::AgentKind;
use kanso_types::event::Activity;
use kanso_types::geometry::CellPoint;

use crate::agent::detect_agent;
use crate::{TermEvent, Terminal};

/// 从提交（回车）到读取命令文本的延迟：等远端把最后几个按键的回显（或 shell 擦掉自动建议的动作）送到。
pub const READ_DELAY_MS: u64 = 100;

/// AI 轮次的空闲判定：连续这么久没有输出就认为这一轮结束（terminal.md §6.2）。
pub const AGENT_IDLE_MS: u64 = 2000;

/// 命令文本的字符数上限；超过的视为误捕获的输出，丢弃（terminal.md §5.2）。
pub const MAX_COMMAND_CHARS: usize = 4096;

/// 用户输入的种类。**只有用户的按键、IME 上屏、粘贴才能调用 `on_input`**；
/// 终端自己的回应（DA、CPR、颜色查询、模式状态、焦点、鼠标报告）走绕过编码器的写入路径，
/// 不得算作输入（terminal.md §5.2）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    /// 普通按键 / IME 上屏 / 单行粘贴。
    Typing,
    /// 回车：提交当前行。
    Enter,
    /// Ctrl+C：放弃当前行。
    Interrupt,
    /// 含换行的粘贴：会执行我们没有看到用户输入的命令。
    PasteMultiline,
}

impl InputKind {
    /// 按发往会话的按键字节分类。
    pub fn of_key_bytes(bytes: &[u8]) -> InputKind {
        match bytes {
            b"\r" | b"\n" => InputKind::Enter,
            [0x03] => InputKind::Interrupt,
            _ => InputKind::Typing,
        }
    }

    /// 按 IME 上屏等直接文本输入分类。
    ///
    /// 一段文字后面带一个换行等价于"打字再按回车"；文字里有别的换行则像多行粘贴。
    pub fn of_text(text: &str) -> Vec<InputKind> {
        // 目录选择器的私有 ZLE 序列保留输入行，末尾换行只是载荷结束符。
        if text.starts_with("\x1b[9001~") {
            return Vec::new();
        }
        let body = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix(['\r', '\n']));
        match body {
            Some(b) if !b.contains(['\r', '\n']) => {
                let mut kinds = Vec::new();
                if !b.is_empty() {
                    kinds.push(InputKind::Typing);
                }
                kinds.push(InputKind::Enter);
                kinds
            }
            Some(_) => vec![InputKind::PasteMultiline],
            None if text.contains(['\r', '\n']) => vec![InputKind::PasteMultiline],
            None => vec![InputKind::Typing],
        }
    }

    /// 按粘贴文本分类。
    pub fn of_paste(text: &str) -> InputKind {
        if text.contains(['\r', '\n']) {
            InputKind::PasteMultiline
        } else {
            InputKind::Typing
        }
    }
}

/// 状态机产生的效果，由会话层转成事件 / 通知。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// 活动变化（只在变化时产生）。
    Activity(Activity),
    /// 一条命令（或 AI 的一个轮次）结束了；会话不可见时据此发"后台完成"通知。
    Completed {
        /// 退出码；推断路径与 AI 轮次拿不到，为 `None`。
        exit_code: Option<i32>,
        /// 结束的是 AI 工具的一个轮次时为该工具；普通命令为 `None`。
        agent: Option<AgentKind>,
    },
    /// 读到了提交的命令文本。供将来的历史 / 补全使用；`history` 为 `false` 表示不该入历史（空格开头）。
    Submitted {
        /// 命令文本（已按 §5.2 截掉右提示符、去掉首尾空白）。
        text: String,
        /// 是否可以记入历史。
        history: bool,
    },
}

/// [`CommandTracker::register_send`] 的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Registered {
    /// 已登记为一条命令；发送失败时必须调用 [`CommandTracker::cancel_send`]。
    Command,
    /// 当前是 AI 会话：这次发送算一个轮次，没有东西需要撤销。
    Turn,
    /// 上一条命令还没返回提示符，不能登记。
    Busy,
    /// 看不到可用的提示符（屏幕被全屏程序占用、提示符为空），无法跟踪完成；调用方按"不跟踪"处理。
    Untracked,
}

#[derive(Clone, Debug)]
struct Submitted {
    start: CellPoint,
    /// 提交瞬间读到的命令文本；稍后读不到（行已不在）时用它。
    snapshot: String,
    /// 提示符特征（去掉尾部空白）。
    sig: String,
    /// 读取命令文本的时刻；OSC 133 模式下不用定时读取（命令开始事件到来时读）。
    due: Option<u64>,
    /// 命令文本由 Sender 给定，以它为准，不读屏幕。
    registered: bool,
}

#[derive(Clone, Debug)]
enum Phase {
    AtPrompt { start: Option<CellPoint> },
    Submitted(Submitted),
    Running { sig: String, registered: bool },
}

/// 命令跟踪与 AI 轮次状态机。每个会话一个，见模块文档。
#[derive(Debug)]
pub struct CommandTracker {
    /// 是否见过 OSC 133（见过之后以它为准）。
    osc133: bool,
    /// 能不能把下一次按键时的光标当作输入起点，见模块文档"为什么 `armed`"。
    armed: bool,
    phase: Phase,
    activity: Activity,
    /// 正在运行的 AI 工具。
    agent: Option<AgentKind>,
    /// 当前是否处在一个轮次里。
    working: bool,
    /// 轮次里最后一次输出（或按键）的时刻。
    last_active: u64,
}

impl Default for CommandTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// 把屏幕上读到的原始命令行整理成命令文本。
enum Cleaned {
    /// 空行（空回车）。
    Blank,
    /// 过长，视为误捕获的输出。
    TooLong,
    Text {
        text: String,
        history: bool,
    },
}

fn clean_command(raw: &str) -> Cleaned {
    // 以空格开头的命令不入历史（与 shell 的 ignorespace 约定一致）。
    let history = !raw.starts_with(' ');
    let body = raw.trim_start();
    // zsh 的右侧提示符和命令在同一行，中间隔着一串空格：遇到连续 3 个以上的空格就截断。
    let body = body.find("   ").map_or(body, |i| &body[..i]);
    let body = body.trim_end();
    if body.is_empty() {
        return Cleaned::Blank;
    }
    if body.chars().count() > MAX_COMMAND_CHARS {
        return Cleaned::TooLong;
    }
    Cleaned::Text {
        text: body.to_owned(),
        history,
    }
}

impl CommandTracker {
    /// 只读取有效命令行从输入起点到光标之前的内容。
    pub fn completion_input(&self, term: &Terminal) -> Option<(String, u16)> {
        let Phase::AtPrompt { start: Some(start) } = self.phase else {
            return None;
        };
        let cursor = term.cursor_point()?;
        if !term.same_logical_line(start.line, cursor.line) {
            return None;
        }
        let prompt = term.text_before(start)?;
        let before_cursor = term.text_before(cursor)?;
        Some((before_cursor.strip_prefix(&prompt)?.to_owned(), start.col))
    }

    /// 新建；初始视为停在提示符上、可以记录输入起点。
    pub fn new() -> Self {
        CommandTracker {
            osc133: false,
            armed: true,
            phase: Phase::AtPrompt { start: None },
            activity: Activity::Idle,
            agent: None,
            working: false,
            last_active: 0,
        }
    }

    /// 当前活动。
    pub fn activity(&self) -> Activity {
        self.activity
    }

    /// 正在运行的 AI 工具；不在 AI 会话里为 `None`。粘贴规则（terminal.md §8.3）据此区分。
    pub fn agent(&self) -> Option<AgentKind> {
        self.agent
    }

    /// 有命令在运行（含刚提交、还没读到文本的）。Sender 据此判断"上一条命令是否已经返回"。
    pub fn is_command_running(&self) -> bool {
        !matches!(self.phase, Phase::AtPrompt { .. })
    }

    /// 下一次需要被唤醒的时刻（单调毫秒）；没有待办返回 `None`。会话层的定时线程据此休眠。
    pub fn next_deadline(&self) -> Option<u64> {
        let read = match &self.phase {
            Phase::Submitted(s) => s.due,
            _ => None,
        };
        let idle = (self.agent.is_some() && self.working).then(|| self.last_active + AGENT_IDLE_MS);
        match (read, idle) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// 会话结束：丢弃全部跟踪状态。调用方负责唤醒等待者（见 `is_command_running`）。
    pub fn reset(&mut self) {
        *self = CommandTracker::new();
    }

    fn set_activity(&mut self, activity: Activity, fx: &mut Vec<Effect>) {
        if self.activity != activity {
            self.activity = activity;
            fx.push(Effect::Activity(activity));
        }
    }

    /// 命令文本确定，开始运行：识别 AI 工具，并设置活动。
    fn begin_command(
        &mut self,
        sig: String,
        registered: bool,
        raw: Option<&str>,
        fx: &mut Vec<Effect>,
    ) {
        let mut agent = None;
        if let Some(raw) = raw {
            match clean_command(raw) {
                Cleaned::Text { text, history } => {
                    agent = detect_agent(&text);
                    fx.push(Effect::Submitted { text, history });
                }
                Cleaned::Blank | Cleaned::TooLong => {}
            }
        }
        self.phase = Phase::Running { sig, registered };
        self.working = false;
        match agent {
            Some(kind) => {
                self.agent = Some(kind);
                // 工具刚启动、还没有用户在里面按键：不是一个轮次。
                self.set_activity(Activity::AgentIdle(kind), fx);
            }
            None => {
                self.agent = None;
                self.set_activity(Activity::CommandRunning, fx);
            }
        }
    }

    /// 命令结束，回到提示符。`notify` 为 `true` 时若这条命令（或当前轮次）在运行则产生 `Completed`。
    fn end_command(&mut self, exit_code: Option<i32>, notify: bool, fx: &mut Vec<Effect>) {
        let prev = self.activity;
        self.phase = Phase::AtPrompt { start: None };
        self.armed = true;
        self.agent = None;
        self.working = false;
        self.set_activity(Activity::Idle, fx);
        // AgentIdle 说明上一个轮次结束时已经通知过；工具退出本身不再通知一次。
        match prev {
            Activity::CommandRunning if notify => fx.push(Effect::Completed {
                exit_code,
                agent: None,
            }),
            Activity::AgentWorking(kind) if notify => fx.push(Effect::Completed {
                exit_code,
                agent: Some(kind),
            }),
            _ => {}
        }
    }

    /// Submitted 的命令文本：Sender 给定的以给定为准；否则以屏幕为准，读不到用快照。
    fn read_submitted(s: &Submitted, term: &Terminal) -> String {
        if s.registered {
            return s.snapshot.clone();
        }
        term.text_from(s.start)
            .unwrap_or_else(|| s.snapshot.clone())
    }

    /// 把 Submitted 推进到 Running（或判定为空回车而放弃）。
    fn resolve_submitted(&mut self, term: &Terminal, fx: &mut Vec<Effect>) {
        let Phase::Submitted(s) =
            std::mem::replace(&mut self.phase, Phase::AtPrompt { start: None })
        else {
            return;
        };
        let raw = Self::read_submitted(&s, term);
        if matches!(clean_command(&raw), Cleaned::Blank) {
            // 空回车：shell 随后会打出新提示符，这条不算命令。
            self.armed = true;
            return;
        }
        self.begin_command(s.sig, s.registered, Some(&raw), fx);
    }

    /// 处理到期的待办：读命令文本、结束 AI 轮次。
    fn tick_inner(&mut self, term: &Terminal, now: u64, fx: &mut Vec<Effect>) {
        if let Phase::Submitted(s) = &self.phase
            && s.due.is_some_and(|d| d <= now)
        {
            self.resolve_submitted(term, fx);
        }
        if self.agent.is_some() && self.working && now >= self.last_active + AGENT_IDLE_MS {
            self.working = false;
            if let Some(kind) = self.agent {
                self.set_activity(Activity::AgentIdle(kind), fx);
                // 与"命令完成"相同的后台通知，让标签显示"完成"。
                fx.push(Effect::Completed {
                    exit_code: None,
                    agent: Some(kind),
                });
            }
        }
    }

    /// 定时唤醒（会话层的定时线程在 `next_deadline` 到点后调用）。
    pub fn on_tick(&mut self, term: &Terminal, now: u64) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.tick_inner(term, now, &mut fx);
        fx
    }

    /// 用户输入。调用时机：按键 / IME 上屏 / 粘贴已经交给写通道之后（此时终端还没有这次输入的回显）。
    pub fn on_input(&mut self, kind: InputKind, term: &Terminal, now: u64) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.tick_inner(term, now, &mut fx);

        // AI 工具里的按键开启（或延续）一个轮次，不当作命令行。
        if let Some(agent) = self.agent {
            self.last_active = now;
            if !self.working {
                self.working = true;
                self.set_activity(Activity::AgentWorking(agent), &mut fx);
            }
            return fx;
        }

        let Phase::AtPrompt { start } = &mut self.phase else {
            // 命令运行期间的输入属于这条命令（sudo 密码、REPL、top 里按 q）。
            return fx;
        };
        match kind {
            InputKind::Typing => {
                if let Some(s) = *start {
                    // 输入起点已经不在光标正在编辑的那一行：作废，否则这条命令会一直"运行中"。
                    let valid = term
                        .cursor_point()
                        .is_some_and(|c| term.same_logical_line(s.line, c.line));
                    if !valid {
                        *start = None;
                        self.armed = false;
                    }
                } else if self.armed {
                    *start = term.cursor_point();
                }
            }
            InputKind::Enter => {
                let s = start.take();
                self.armed = true;
                if let Some(s) = s {
                    self.submit(s, term, now);
                }
            }
            InputKind::Interrupt => {
                *start = None;
                self.armed = true;
            }
            InputKind::PasteMultiline => {
                *start = None;
                self.armed = false;
            }
        }
        fx
    }

    /// 回车：校验输入起点，记录提示符特征与文本快照。
    fn submit(&mut self, start: CellPoint, term: &Terminal, now: u64) {
        let Some(cursor) = term.cursor_point() else {
            return;
        };
        if !term.same_logical_line(start.line, cursor.line) {
            return;
        }
        let Some(prompt) = term.text_before(start) else {
            return;
        };
        let sig = prompt.trim_end().to_owned();
        // 空特征没有区分度：任何提示符都"相同"，无法判断命令何时返回。
        if sig.is_empty() {
            return;
        }
        let snapshot = term.text_from(start).unwrap_or_default();
        self.armed = false;
        self.phase = Phase::Submitted(Submitted {
            start,
            snapshot,
            sig,
            due: (!self.osc133).then_some(now + READ_DELAY_MS),
            registered: false,
        });
    }

    /// 一批输出（已经喂进 `term`）产生的事件。调用方在每次 `Terminal::feed` 之后调用，
    /// 事件取自 `take_events`；没有事件也要调用（轮次靠输出续命，提示符返回靠屏幕判断）。
    pub fn on_output(&mut self, events: &[TermEvent], term: &Terminal, now: u64) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.tick_inner(term, now, &mut fx);

        for event in events {
            match event {
                TermEvent::PromptStart { .. } | TermEvent::InputStart { .. } => {
                    self.osc133 = true;
                    // 正常序列里 D 先于 A；缺了 D 也要回到空闲，只是不算"完成"。
                    if self.is_command_running() {
                        self.end_command(None, false, &mut fx);
                    } else {
                        self.armed = true;
                        self.set_activity(Activity::Idle, &mut fx);
                    }
                }
                TermEvent::CommandStart { .. } => {
                    self.osc133 = true;
                    self.on_command_start(term, &mut fx);
                }
                TermEvent::CommandFinished { exit_code } => {
                    self.osc133 = true;
                    // 没有 C 的孤立 D（提示符下按 Ctrl+C）不是一条命令完成：`end_command` 只在
                    // 确实在运行时才产生 `Completed`。
                    self.end_command(*exit_code, true, &mut fx);
                }
                TermEvent::CwdChanged(_) => {
                    // 没有 OSC 133 时，远端 shell 的目录上报同时是"提示符出现"的信号（terminal.md §12），
                    // 用来弥补提示符里带目录、`cd` 之后特征对不上的情况。
                    if !self.osc133 && matches!(self.phase, Phase::Running { .. }) {
                        self.end_command(None, true, &mut fx);
                    }
                }
                TermEvent::TitleChanged | TermEvent::Bell | TermEvent::ClipboardStore(_) => {}
            }
        }

        if !self.osc133 {
            self.check_prompt_returned(term, &mut fx);
        }
        // AI 轮次靠输出续命。
        if self.agent.is_some() && self.working {
            self.last_active = now;
        }
        fx
    }

    /// OSC 133 C：命令开始执行。命令文本取自 Submitted（用户按回车记下的输入起点）。
    fn on_command_start(&mut self, term: &Terminal, fx: &mut Vec<Effect>) {
        if matches!(self.phase, Phase::Submitted(_)) {
            let Phase::Submitted(s) =
                std::mem::replace(&mut self.phase, Phase::AtPrompt { start: None })
            else {
                return;
            };
            let raw = Self::read_submitted(&s, term);
            self.begin_command(s.sig, s.registered, Some(&raw), fx);
        } else {
            // 没见过回车（例如由 Sender 之外的途径执行）：只知道有命令在运行。
            self.begin_command(String::new(), false, None, fx);
        }
    }

    /// 推断路径：光标停在与提交时相同特征的提示符末尾 = 命令返回。
    fn check_prompt_returned(&mut self, term: &Terminal, fx: &mut Vec<Effect>) {
        let sig = match &self.phase {
            Phase::Submitted(s) => &s.sig,
            Phase::Running { sig, .. } => sig,
            Phase::AtPrompt { .. } => return,
        };
        let Some(cursor) = term.cursor_point() else {
            return;
        };
        let matches = term
            .text_before(cursor)
            .is_some_and(|t| t.trim_end() == sig.as_str());
        if !matches {
            return;
        }
        if matches!(self.phase, Phase::Submitted(_)) {
            // 提示符在读取延迟内就回来了：现在回显一定到齐，立即读文本。
            self.resolve_submitted(term, fx);
        }
        if matches!(self.phase, Phase::Running { .. }) {
            self.end_command(None, true, fx);
        }
    }

    /// Sender 发送前登记"即将发送某条命令"，以当前提示符作为完成特征（terminal.md §5.3）。
    ///
    /// 调用方保证：登记之后才写会话；写失败时调用 [`CommandTracker::cancel_send`]。
    pub fn register_send(
        &mut self,
        text: &str,
        term: &Terminal,
        now: u64,
    ) -> (Registered, Vec<Effect>) {
        let mut fx = Vec::new();
        self.tick_inner(term, now, &mut fx);
        if let Some(agent) = self.agent {
            // 通过 Sender 发给工具的文字算一个轮次。
            self.last_active = now;
            if !self.working {
                self.working = true;
                self.set_activity(Activity::AgentWorking(agent), &mut fx);
            }
            return (Registered::Turn, fx);
        }
        if !matches!(self.phase, Phase::AtPrompt { .. }) {
            return (Registered::Busy, fx);
        }
        let Some(cursor) = term.cursor_point() else {
            return (Registered::Untracked, fx);
        };
        let sig = term
            .text_before(cursor)
            .map(|p| p.trim_end().to_owned())
            .unwrap_or_default();
        if sig.is_empty() {
            return (Registered::Untracked, fx);
        }
        self.armed = false;
        self.phase = Phase::Submitted(Submitted {
            start: cursor,
            snapshot: text.to_owned(),
            sig,
            due: (!self.osc133).then_some(now + READ_DELAY_MS),
            registered: true,
        });
        (Registered::Command, fx)
    }

    /// 撤销 [`CommandTracker::register_send`] 的登记（发送失败时）。
    pub fn cancel_send(&mut self) -> Vec<Effect> {
        let mut fx = Vec::new();
        let registered = match &self.phase {
            Phase::Submitted(s) => s.registered,
            Phase::Running { registered, .. } => *registered,
            Phase::AtPrompt { .. } => false,
        };
        if registered {
            self.phase = Phase::AtPrompt { start: None };
            self.armed = true;
            self.set_activity(Activity::Idle, &mut fx);
        }
        fx
    }
}

#[cfg(test)]
mod tests;
