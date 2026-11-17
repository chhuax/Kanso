//! 时间戳 / 行号栏（terminal.md §2）的纯逻辑：栏宽、每行文字的格式化，以及 AI CLI 会话的临时设置。
//!
//! 对照 mac 版 `TerminalKit/TerminalRenderer` 的栏布局与 `Shell/GutterModeController` 的会话策略。
//! 核心只在帧里给出每行的时间、绝对行号与标志位，栏的宽度与文字由渲染侧决定；
//! 栏宽变化会让正文的列数变化，由视图已有的“尺寸变化 → resize”链路通知会话。

use std::collections::HashMap;

use kanso_types::SessionId;
use kanso_types::agent::AgentKind;
use kanso_types::input::TimestampMode;

use crate::app::settings::{GutterMode, TimelineSettings};

/// 时间文字的字符数：`HH:MM:SS.mmm`。
pub const TIME_CHARS: usize = 12;
/// 行号字段的字符数（右对齐，超出时只保留低位）。
pub const NUMBER_CHARS: usize = 6;

/// 栏内文字的字符数（不含留白）；关闭为 0。
pub fn text_chars(mode: GutterMode) -> usize {
    match mode {
        GutterMode::Off => 0,
        GutterMode::LineNumbers => NUMBER_CHARS,
        GutterMode::Timestamps => TIME_CHARS,
        GutterMode::Both => TIME_CHARS + 1 + NUMBER_CHARS,
    }
}

/// 栏总宽占几个栏字符格：左侧 1 格留白、右侧 2 格与正文的间隔；关闭为 0。
pub fn width_cells(mode: GutterMode) -> usize {
    match text_chars(mode) {
        0 => 0,
        n => n + 3,
    }
}

/// 核心的时间戳模式（注意核心把“只显示时间”叫 `Time`）。
pub fn core_mode(mode: GutterMode) -> TimestampMode {
    match mode {
        GutterMode::Off => TimestampMode::Off,
        GutterMode::LineNumbers => TimestampMode::LineNumbers,
        GutterMode::Timestamps => TimestampMode::Time,
        GutterMode::Both => TimestampMode::Both,
    }
}

/// 状态栏 / 菜单里模式的显示名。
pub fn mode_label(mode: GutterMode) -> &'static str {
    mode.label()
}

/// 把 Unix 毫秒按 `tz_offset_min`（东为正）格式化成 `HH:MM:SS.mmm`，不随语言变化。
/// `ms == 0` 表示没有时间，返回等宽的空白。
pub fn format_time(ms: i64, tz_offset_min: i32) -> String {
    if ms == 0 {
        return " ".repeat(TIME_CHARS);
    }
    let local = ms.div_euclid(1000) + i64::from(tz_offset_min) * 60;
    let millis = ms.rem_euclid(1000);
    let sod = local.rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        sod / 3600,
        sod / 60 % 60,
        sod % 60,
        millis
    )
}

/// 右对齐的行号，宽 [`NUMBER_CHARS`]；负数按 0，超出宽度只保留低位（栏宽固定，不能挤进正文）。
pub fn format_number(n: i64) -> String {
    let s = n.max(0).to_string();
    let s = if s.len() > NUMBER_CHARS {
        &s[s.len() - NUMBER_CHARS..]
    } else {
        &s
    };
    format!("{s:>width$}", width = NUMBER_CHARS)
}

/// 显示行号 = 绝对行号 − 基准 + 1（`clear` 之后基准前移，行号从 1 重新开始；有符号相减避免下溢）。
pub fn display_line_number(abs_line: u64, base: u64) -> i64 {
    (abs_line as i64).wrapping_sub(base as i64).wrapping_add(1)
}

/// 一行栏文字。`blank`（从未写过内容的行）返回 `None`；模式为关闭也返回 `None`。
pub fn line_text(
    mode: GutterMode,
    blank: bool,
    timestamp_ms: i64,
    abs_line: u64,
    line_number_base: u64,
    tz_offset_min: i32,
) -> Option<String> {
    if blank || mode == GutterMode::Off {
        return None;
    }
    let show_time = matches!(mode, GutterMode::Timestamps | GutterMode::Both);
    let show_number = matches!(mode, GutterMode::LineNumbers | GutterMode::Both);
    let mut out = String::with_capacity(TIME_CHARS + 1 + NUMBER_CHARS);
    if show_time {
        out.push_str(&format_time(timestamp_ms, tz_offset_min));
    }
    if show_time && show_number {
        out.push(' ');
    }
    if show_number {
        out.push_str(&format_number(display_line_number(
            abs_line,
            line_number_base,
        )));
    }
    Some(out)
}

/// AI 会话的临时设置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AgentOverride {
    /// 触发它的工具：同一工具重复上报保留用户选择，换工具则作废。
    agent: AgentKind,
    mode: GutterMode,
}

/// 栏模式策略：全局模式 + 各 AI 会话的临时覆盖（terminal.md §2、ui-shell.md §9）。
///
/// 只维护状态并回答“某会话现在该用什么模式”；把结果应用到视图是外壳的事。
#[derive(Clone, Debug, Default)]
pub struct GutterPolicy {
    global: TimelineSettings,
    overrides: HashMap<SessionId, AgentOverride>,
}

/// 状态栏开关的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToggleOutcome {
    /// 只改了这个会话，新模式如下。
    Session(GutterMode),
    /// 切换的是全局模式（调用方写回设置）。
    Global(TimelineSettings),
}

impl GutterPolicy {
    /// AI 会话刚被识别时的默认模式：全屏 TUI 不需要栏。
    pub const AGENT_DEFAULT: GutterMode = GutterMode::Off;

    /// 以设置里的时间轴为全局模式构造。
    pub fn new(global: TimelineSettings) -> Self {
        GutterPolicy {
            global,
            overrides: HashMap::new(),
        }
    }

    /// 全局设置变了（设置窗口、菜单）。
    pub fn set_global(&mut self, global: TimelineSettings) {
        self.global = global;
    }

    /// 全局时间轴设置。
    pub fn global(&self) -> TimelineSettings {
        self.global
    }

    /// 该会话是否有临时设置。
    pub fn has_override(&self, session: SessionId) -> bool {
        self.overrides.contains_key(&session)
    }

    /// 该会话实际使用的模式。
    pub fn effective(&self, session: SessionId) -> GutterMode {
        self.overrides
            .get(&session)
            .map_or(self.global.mode, |o| o.mode)
    }

    /// 状态栏开关对该会话是否处于打开状态。
    pub fn is_on(&self, session: Option<SessionId>) -> bool {
        session.map_or(self.global.mode, |s| self.effective(s)) != GutterMode::Off
    }

    /// 会话的 agent 状态：`Some(agent)` 为 agent 活动（忙或闲都算），`None` 为空闲 / 普通命令。
    ///
    /// 返回该会话需要改用的模式；没有变化（同一工具重复上报、本来就没有临时设置）返回 `None`。
    /// - 第一次识别到 agent：临时设置为 [`AGENT_DEFAULT`](Self::AGENT_DEFAULT)；
    /// - 换成另一个工具：旧设置（含用户选择）作废，重新默认隐藏；
    /// - 工具退出：临时设置作废，回到全局模式。
    pub fn observe_agent(
        &mut self,
        session: SessionId,
        agent: Option<AgentKind>,
    ) -> Option<GutterMode> {
        match agent {
            Some(a) => {
                if self.overrides.get(&session).is_some_and(|o| o.agent == a) {
                    return None;
                }
                self.overrides.insert(
                    session,
                    AgentOverride {
                        agent: a,
                        mode: Self::AGENT_DEFAULT,
                    },
                );
                Some(Self::AGENT_DEFAULT)
            }
            None => self.overrides.remove(&session).map(|_| self.global.mode),
        }
    }

    /// 会话关闭。
    pub fn forget(&mut self, session: SessionId) {
        self.overrides.remove(&session);
    }

    /// 状态栏“时间戳”开关：当前会话有临时设置时只改这个会话，否则切换全局模式。
    /// 重新打开时恢复“上次非关闭的模式”。全局情况下本函数**不**改内部状态，由调用方写回设置后经
    /// [`set_global`](Self::set_global) 同步。
    pub fn toggle(&mut self, session: Option<SessionId>) -> ToggleOutcome {
        let restore = self.global.last_visible;
        if let Some(o) = session.and_then(|s| self.overrides.get_mut(&s)) {
            o.mode = if o.mode == GutterMode::Off {
                restore
            } else {
                GutterMode::Off
            };
            return ToggleOutcome::Session(o.mode);
        }
        let mut g = self.global;
        let next = if g.mode == GutterMode::Off {
            restore
        } else {
            GutterMode::Off
        };
        g.set_mode(next);
        ToggleOutcome::Global(g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(n: u64) -> SessionId {
        SessionId::from_raw(n)
    }

    #[test]
    fn widths_follow_the_mode() {
        assert_eq!(text_chars(GutterMode::Off), 0);
        assert_eq!(width_cells(GutterMode::Off), 0);
        assert_eq!(text_chars(GutterMode::LineNumbers), 6);
        assert_eq!(text_chars(GutterMode::Timestamps), 12);
        assert_eq!(text_chars(GutterMode::Both), 19);
        assert_eq!(width_cells(GutterMode::Both), 22);
        assert_eq!(width_cells(GutterMode::LineNumbers), 9);
    }

    #[test]
    fn time_is_fixed_format_in_local_time() {
        // 2024-03-05 23:07:30.045 UTC。
        let ms = 1_709_680_050_045;
        assert_eq!(format_time(ms, 0), "23:07:30.045");
        assert_eq!(format_time(ms, 8 * 60), "07:07:30.045");
        assert_eq!(format_time(ms, -5 * 60), "18:07:30.045");
        assert_eq!(format_time(0, 0), " ".repeat(12));
        // 1970 年之前也不会下溢。
        assert_eq!(format_time(-1, 0), "23:59:59.999");
    }

    #[test]
    fn numbers_are_right_aligned_and_truncate_high_digits() {
        assert_eq!(format_number(7), "     7");
        assert_eq!(format_number(123456), "123456");
        assert_eq!(format_number(1_234_567), "234567");
        assert_eq!(format_number(-3), "     0");
    }

    #[test]
    fn line_number_restarts_after_base_moves() {
        assert_eq!(display_line_number(10, 10), 1);
        assert_eq!(display_line_number(15, 10), 6);
        // 缓冲区满后绝对行号继续增长，显示行号随之增长，不会停住。
        assert_eq!(display_line_number(1_000_000, 0), 1_000_001);
        // 基准在后面（不应发生）时不 panic。
        assert!(display_line_number(3, 10) <= 0);
    }

    #[test]
    fn line_text_per_mode() {
        let ms = 1_709_680_050_045;
        let t = |mode, blank| line_text(mode, blank, ms, 4, 0, 0);
        assert_eq!(t(GutterMode::Off, false), None);
        assert_eq!(t(GutterMode::Both, true), None);
        assert_eq!(t(GutterMode::LineNumbers, false).as_deref(), Some("     5"));
        assert_eq!(
            t(GutterMode::Timestamps, false).as_deref(),
            Some("23:07:30.045")
        );
        assert_eq!(
            t(GutterMode::Both, false).as_deref(),
            Some("23:07:30.045      5")
        );
        assert_eq!(t(GutterMode::Both, false).unwrap().chars().count(), 19);
    }

    #[test]
    fn core_mode_mapping() {
        assert_eq!(core_mode(GutterMode::Timestamps), TimestampMode::Time);
        assert_eq!(core_mode(GutterMode::Both), TimestampMode::Both);
        assert_eq!(core_mode(GutterMode::Off), TimestampMode::Off);
    }

    fn policy(mode: GutterMode) -> GutterPolicy {
        let mut t = TimelineSettings::default();
        t.set_mode(mode);
        GutterPolicy::new(t)
    }

    #[test]
    fn agent_session_hides_the_gutter_by_default_and_restores_on_exit() {
        let mut p = policy(GutterMode::Both);
        let s = sid(1);
        assert_eq!(p.effective(s), GutterMode::Both);
        assert_eq!(
            p.observe_agent(s, Some(AgentKind::Claude)),
            Some(GutterMode::Off)
        );
        assert_eq!(p.effective(s), GutterMode::Off);
        // 其他会话不受影响。
        assert_eq!(p.effective(sid(2)), GutterMode::Both);
        // 同一工具重复上报：不变。
        assert_eq!(p.observe_agent(s, Some(AgentKind::Claude)), None);
        // 工具退出：回到全局。
        assert_eq!(p.observe_agent(s, None), Some(GutterMode::Both));
        assert!(!p.has_override(s));
        // 本来就没有覆盖：不打扰。
        assert_eq!(p.observe_agent(s, None), None);
    }

    #[test]
    fn user_choice_survives_repeat_reports_but_not_a_tool_change() {
        let mut p = policy(GutterMode::LineNumbers);
        let s = sid(1);
        p.observe_agent(s, Some(AgentKind::Claude));
        // 重新打开时恢复全局“上次可见模式”。
        let on = GutterMode::LineNumbers;
        assert_eq!(p.toggle(Some(s)), ToggleOutcome::Session(on));
        assert_eq!(p.effective(s), on);
        assert_eq!(p.observe_agent(s, Some(AgentKind::Claude)), None);
        assert_eq!(p.effective(s), on);
        // 换工具：作废，重新隐藏。
        assert_eq!(
            p.observe_agent(s, Some(AgentKind::Codex)),
            Some(GutterMode::Off)
        );
        assert_eq!(p.toggle(Some(s)), ToggleOutcome::Session(on));
        assert_eq!(p.toggle(Some(s)), ToggleOutcome::Session(GutterMode::Off));
    }

    #[test]
    fn toggle_on_a_normal_session_flips_the_global_mode() {
        let mut p = policy(GutterMode::Off);
        // 默认 last_visible 为 Both：第一次打开显示两者。
        match p.toggle(Some(sid(1))) {
            ToggleOutcome::Global(t) => {
                assert_eq!(t.mode, GutterMode::Both);
                p.set_global(t);
            }
            other => panic!("{other:?}"),
        }
        assert!(p.is_on(Some(sid(1))));
        match p.toggle(None) {
            ToggleOutcome::Global(t) => {
                assert_eq!(t.mode, GutterMode::Off);
                // 关闭后仍记得上次的可见模式。
                assert_eq!(t.last_visible, GutterMode::Both);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn forget_drops_the_override() {
        let mut p = policy(GutterMode::Both);
        p.observe_agent(sid(1), Some(AgentKind::Claude));
        p.forget(sid(1));
        assert_eq!(p.effective(sid(1)), GutterMode::Both);
    }
}
