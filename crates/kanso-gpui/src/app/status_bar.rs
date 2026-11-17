//! 状态栏（ui-shell.md §9）的纯逻辑，对照 mac 版 `Shell/StatusBarView.swift`。
//!
//! 内容：左侧依次是 Filer 开关、时间戳栏开关、当前目录、活动会话的状态文字（连接中 / 已结束 / 断开原因）；
//! 右侧依次是 Legacy SSH 标记（当前会话有任何一跳用了旧算法时显示，悬停每台服务器一行）和时钟。
//! 布局优先级：时钟与 Legacy 标记按自然宽度固定；左侧空间不足时状态文字先被截断到消失，
//! 然后才压缩当前目录（截断开头、保留末尾），两个开关始终完整显示。
//! 时钟只在整分钟刷新（状态栏不显示秒，每秒唤醒只是白白耗电）。
//!
//! 本文件不依赖窗口系统，三个平台都能编译与测试。

use crate::app::controller::LegacyLine;
use crate::app::tab::{CloseNote, TabDescriptions, TabItem, TabStateLine, truncate_keep_tail};
use crate::filer::format_mtime;

/// 状态文字的语气，决定颜色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusTone {
    /// 普通（次要文字色）。
    Normal,
    /// 需要注意（会话已结束且退出码非 0）。
    Warning,
    /// 错误（连接断开 / 失败）。
    Error,
}

/// 状态行文字（侧栏副标题与状态栏共用）。
// TODO(l10n)：用 kanso-l10n 渲染错误消息。
pub fn state_text(line: &TabStateLine) -> String {
    match line {
        TabStateLine::Connecting => "连接中…".to_owned(),
        TabStateLine::Closed(Some(CloseNote::Exited { code: Some(c) })) => {
            format!("会话已结束（退出码 {c}）")
        }
        TabStateLine::Closed(Some(CloseNote::Exited { code: None })) => "会话已结束".to_owned(),
        TabStateLine::Closed(Some(CloseNote::Error(_))) => "会话已断开".to_owned(),
        TabStateLine::Closed(None) => "会话已结束".to_owned(),
        TabStateLine::CommandRunning => "命令运行中".to_owned(),
        TabStateLine::AgentRunning => "AI 运行中".to_owned(),
        TabStateLine::AgentFinished => "AI 已完成".to_owned(),
        TabStateLine::UnreadDone => "命令已完成，点击查看".to_owned(),
    }
}

/// 状态文字的语气。
pub fn state_tone(line: &TabStateLine) -> StatusTone {
    match line {
        TabStateLine::Closed(Some(CloseNote::Error(_))) => StatusTone::Error,
        TabStateLine::Closed(Some(CloseNote::Exited { code })) if *code != Some(0) => {
            StatusTone::Warning
        }
        _ => StatusTone::Normal,
    }
}

/// Legacy SSH 标记的悬停文字：每台服务器一行，列出它用的旧算法。
pub fn legacy_tooltip(lines: &[LegacyLine]) -> Vec<String> {
    lines
        .iter()
        .map(|l| format!("{}: {}", l.server, l.algorithms.join(", ")))
        .collect()
}

/// 状态栏要显示的全部内容（视图每帧由它生成）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusBarModel {
    /// 状态文字与语气；正常连接时为 `None`（位置信息已由“当前目录”表达）。
    pub status: Option<(String, StatusTone)>,
    /// 当前目录条目；没有可显示的目录时为 `None`（条目隐藏）。
    pub directory: Option<String>,
    /// Legacy SSH 标记的悬停行；没有旧算法为 `None`（标记隐藏）。
    pub legacy: Option<Vec<String>>,
    /// 时钟文字。
    pub clock: String,
}

impl StatusBarModel {
    /// 由活动标签、home 目录、活动会话的 Legacy 汇总和时钟文字生成。
    pub fn build(
        active: Option<&TabItem>,
        home_dir: &str,
        legacy: Option<Vec<LegacyLine>>,
        clock: String,
    ) -> StatusBarModel {
        StatusBarModel {
            status: TabDescriptions::status_bar_state(active)
                .map(|l| (state_text(&l), state_tone(&l))),
            directory: TabDescriptions::directory_text(active, home_dir),
            legacy: legacy.as_deref().map(legacy_tooltip),
            clock,
        }
    }
}

/// 时钟的固定格式 `YYYY/MM/DD HH:MM`（本地时间）。
///
/// 刻意不随系统语言 / 区域变化：状态栏空间固定，宽度必须可预期；也与终端时间戳栏的固定格式风格一致。
pub fn clock_text(unix_ms: i64, tz_offset_min: i32) -> String {
    // `format_mtime` 的日期部分用 `-` 分隔，时间部分没有 `-`，直接替换即可。
    format_mtime(unix_ms, tz_offset_min).replace('-', "/")
}

/// 距离下一个整分钟还有多少毫秒（刚好在整分钟上返回满 60 秒），时钟据此对齐刷新，避免漂移。
pub fn ms_until_next_minute(unix_ms: i64) -> u64 {
    (60_000 - unix_ms.rem_euclid(60_000)) as u64
}

/// 估算像素宽度：两侧内边距、条目间距。
const PAD_PX: f32 = 12.0;
const GAP_PX: f32 = 16.0;
/// Filer / 时间戳开关条目的自然宽度（图标 + 文字 + 内边距）。
const TOGGLE_PX: f32 = 84.0;
/// 时钟（16 个字符）的自然宽度。
const CLOCK_PX: f32 = 116.0;
/// Legacy SSH 标记的自然宽度。
const LEGACY_PX: f32 = 96.0;
/// 平均每个字符约占的像素宽（12px 界面字体，中英混排的经验值）。
const CELL_PX: f32 = 7.0;
/// 当前目录至少保留的字符数。
const MIN_DIR_CELLS: usize = 8;

/// 当前目录条目能放多少个字符：窗口宽度扣掉固定宽度的元素后，剩下的全给目录
/// （状态文字让位，可以被截断到 0）。过长的目录由调用方用 `truncate_keep_tail` 保留末尾。
pub fn directory_cells(window_width: f32, legacy_visible: bool) -> usize {
    let fixed = PAD_PX * 2.0
        + TOGGLE_PX * 2.0
        + CLOCK_PX
        + if legacy_visible { LEGACY_PX } else { 0.0 }
        // 开关之间、开关与目录、目录与右侧元素之间的间距。
        + GAP_PX * if legacy_visible { 5.0 } else { 4.0 };
    (((window_width - fixed) / CELL_PX).max(0.0) as usize).max(MIN_DIR_CELLS)
}

/// 按窗口宽度截断当前目录（保留末尾）。
pub fn fit_directory(directory: &str, window_width: f32, legacy_visible: bool) -> String {
    truncate_keep_tail(directory, directory_cells(window_width, legacy_visible))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tab::{TabConnection, TabKind};
    use kanso_types::SessionId;

    fn tab(kind: TabKind) -> TabItem {
        TabItem::new(SessionId::from_raw(1), kind, "prod")
    }

    #[test]
    fn clock_uses_the_fixed_format_in_local_time() {
        // 2024-03-05 23:07:30 UTC。
        let ms = 1_709_680_050_000;
        assert_eq!(clock_text(ms, 0), "2024/03/05 23:07");
        // 东八区跨日。
        assert_eq!(clock_text(ms, 8 * 60), "2024/03/06 07:07");
        // 西部时区。
        assert_eq!(clock_text(ms, -5 * 60), "2024/03/05 18:07");
        assert_eq!(clock_text(0, 0), "1970/01/01 00:00");
        assert_eq!(clock_text(-1, 0), "1969/12/31 23:59");
    }

    #[test]
    fn clock_refresh_aligns_to_the_minute() {
        assert_eq!(ms_until_next_minute(0), 60_000);
        assert_eq!(ms_until_next_minute(1), 59_999);
        assert_eq!(ms_until_next_minute(59_999), 1);
        assert_eq!(ms_until_next_minute(60_000 * 7 + 30_000), 30_000);
        assert_eq!(ms_until_next_minute(-1), 1);
    }

    #[test]
    fn healthy_session_shows_only_the_directory() {
        let mut t = tab(TabKind::Local);
        t.connection = TabConnection::Connected;
        t.cwd = Some("/home/me/proj".into());
        let m = StatusBarModel::build(Some(&t), "/home/me", None, "2024/01/01 00:00".into());
        assert_eq!(m.status, None);
        assert_eq!(m.directory.as_deref(), Some("~/proj"));
        assert_eq!(m.legacy, None);
    }

    #[test]
    fn closed_and_connecting_sessions_show_status_with_tone() {
        let mut t = tab(TabKind::Ssh);
        t.connection = TabConnection::Connecting;
        let m = StatusBarModel::build(Some(&t), "", None, String::new());
        assert_eq!(m.status, Some(("连接中…".into(), StatusTone::Normal)));

        t.connection = TabConnection::Closed(Some(CloseNote::Exited { code: Some(2) }));
        let m = StatusBarModel::build(Some(&t), "", None, String::new());
        assert_eq!(
            m.status,
            Some(("会话已结束（退出码 2）".into(), StatusTone::Warning))
        );

        t.connection = TabConnection::Closed(Some(CloseNote::Exited { code: Some(0) }));
        let m = StatusBarModel::build(Some(&t), "", None, String::new());
        assert_eq!(m.status.map(|s| s.1), Some(StatusTone::Normal));

        t.connection = TabConnection::Closed(None);
        let m = StatusBarModel::build(Some(&t), "", None, String::new());
        assert_eq!(m.status, Some(("会话已结束".into(), StatusTone::Normal)));
    }

    #[test]
    fn no_active_tab_shows_nothing_but_the_clock() {
        let m = StatusBarModel::build(None, "/h", None, "c".into());
        assert_eq!(m.status, None);
        assert_eq!(m.directory, None);
        assert_eq!(m.clock, "c");
    }

    #[test]
    fn remote_directory_is_shown_as_reported() {
        let mut t = tab(TabKind::Ssh);
        t.cwd = Some("/home/me/srv".into());
        let m = StatusBarModel::build(Some(&t), "/home/me", None, String::new());
        assert_eq!(m.directory.as_deref(), Some("/home/me/srv"));
    }

    #[test]
    fn legacy_badge_lists_one_line_per_server() {
        let lines = vec![
            LegacyLine {
                server: "a.example:22".into(),
                algorithms: vec!["ssh-rsa".into(), "diffie-hellman-group1-sha1".into()],
            },
            LegacyLine {
                server: "jump:2222".into(),
                algorithms: vec!["ssh-dss".into()],
            },
        ];
        let m = StatusBarModel::build(None, "", Some(lines), String::new());
        assert_eq!(
            m.legacy,
            Some(vec![
                "a.example:22: ssh-rsa, diffie-hellman-group1-sha1".to_owned(),
                "jump:2222: ssh-dss".to_owned(),
            ])
        );
    }

    #[test]
    fn directory_budget_shrinks_with_the_window_and_legacy_badge() {
        let wide = directory_cells(1200.0, false);
        assert!(wide > directory_cells(800.0, false));
        assert!(directory_cells(1200.0, true) < wide);
        // 极窄时仍保留最小宽度，不会变成 0。
        assert_eq!(directory_cells(100.0, true), MIN_DIR_CELLS);
    }

    #[test]
    fn long_directory_keeps_its_tail() {
        let dir = "~/a/very/long/path/that/keeps/going/and/going/to/the/final-dir";
        let fitted = fit_directory(dir, 100.0, false);
        assert!(fitted.starts_with('…'));
        assert!(fitted.ends_with("final-dir") || fitted.ends_with("al-dir"));
        assert_eq!(fit_directory("~/x", 1200.0, false), "~/x");
    }
}
