//! 终端内查找（terminal.md §11）的纯逻辑：查询、状态文字、随输出刷新的节流。
//!
//! 对照 mac 版 `Find/FindBarController`。匹配的高亮由核心写进帧（`CELL_MATCH` / `CELL_MATCH_CURRENT`），
//! 这里只维护查找栏自己的状态；每个会话一个 [`FindState`]，互不干扰。

use kanso_core::{SearchQuery, SearchStatus};
use kanso_types::message::Message;

use crate::app::launcher::render_message;

/// 新输出到来时刷新计数的最小间隔（毫秒）。
pub const REFRESH_INTERVAL_MS: u64 = 250;

/// 查找结果（供状态文字用）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FindResult {
    /// 没有搜索（文字为空或栏刚打开）。
    #[default]
    Idle,
    /// 正则无法编译。
    InvalidRegex,
    /// 搜索结果：匹配总数与当前项序号（从 1 开始）。
    Found {
        /// 匹配总数。
        total: u32,
        /// 当前项；当前项不在匹配里时为 `None`。
        current: Option<u32>,
    },
}

impl From<SearchStatus> for FindResult {
    fn from(s: SearchStatus) -> Self {
        if s.invalid_regex {
            return Self::InvalidRegex;
        }
        FindResult::Found {
            total: s.total,
            current: s.current,
        }
    }
}

/// 节流器要求调用方做的事。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Throttle {
    /// 现在就读一次状态，并在 `timer_ms` 毫秒后调用 [`Refresher::on_timer`]。
    RunNow {
        /// 窗口长度。
        timer_ms: u64,
    },
    /// 已在窗口内，只记了一个“尾随”标记；定时器已经在等，不用再起。
    Deferred,
}

/// 定时器到点后要求调用方做的事。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerAction {
    /// 窗口内没有新输出，什么都不做。
    Idle,
    /// 补读一次状态并再起一个窗口（任意时刻的刷新频率不超过 4 次 / 秒）。
    RunAndRearm {
        /// 窗口长度。
        timer_ms: u64,
    },
}

/// 刷新节流：窗口外立即执行一次并开启窗口；窗口内的请求只记一个尾随标记，窗口结束时补执行一次，
/// 保证最后一次输出之后的计数是准确的。
#[derive(Clone, Debug, Default)]
pub struct Refresher {
    in_window: bool,
    trailing: bool,
}

impl Refresher {
    /// 收到一次“新输出”。
    pub fn request(&mut self) -> Throttle {
        if self.in_window {
            self.trailing = true;
            Throttle::Deferred
        } else {
            self.in_window = true;
            Throttle::RunNow {
                timer_ms: REFRESH_INTERVAL_MS,
            }
        }
    }

    /// 窗口定时器到点。
    pub fn on_timer(&mut self) -> TimerAction {
        if self.trailing {
            self.trailing = false;
            // 保持 in_window：补读本身开启新窗口。
            TimerAction::RunAndRearm {
                timer_ms: REFRESH_INTERVAL_MS,
            }
        } else {
            self.in_window = false;
            TimerAction::Idle
        }
    }

    /// 取消（栏关闭或文字清空）；已排的定时器到点时会得到 `Idle`。
    pub fn cancel(&mut self) {
        self.in_window = false;
        self.trailing = false;
    }
}

/// 一个会话的查找状态。
#[derive(Clone, Debug, Default)]
pub struct FindState {
    /// 查找栏是否打开。
    pub open: bool,
    /// 搜索文字。
    pub text: String,
    /// 区分大小写。
    pub case_sensitive: bool,
    /// 按正则解释。
    pub regex: bool,
    /// 最近一次结果。
    pub result: FindResult,
    /// 随输出刷新的节流。
    pub refresher: Refresher,
}

impl FindState {
    /// 当前查询；文字为空返回 `None`（不搜索）。
    pub fn query(&self) -> Option<SearchQuery> {
        (!self.text.is_empty()).then(|| SearchQuery {
            text: self.text.clone(),
            case_sensitive: self.case_sensitive,
            regex: self.regex,
        })
    }

    /// 栏上显示的状态文字：无结果、`第几个/总数`，当前项不在匹配里时用 en dash 占位。
    pub fn status_text(&self) -> String {
        match self.result {
            FindResult::Idle => String::new(),
            FindResult::InvalidRegex => render_message(&Message::UiFindInvalidRegex {}),
            FindResult::Found { total: 0, .. } => render_message(&Message::UiFindNoResults {}),
            FindResult::Found {
                total,
                current: Some(c),
            } => format!("{c}/{total}"),
            FindResult::Found {
                total,
                current: None,
            } => format!("–/{total}"),
        }
    }

    /// 状态文字是否用错误色。
    pub fn is_error(&self) -> bool {
        self.result == FindResult::InvalidRegex
    }

    /// 是否需要随新输出刷新计数（栏打开且有文字）。
    pub fn wants_refresh(&self) -> bool {
        self.open && !self.text.is_empty()
    }

    /// 关闭：清结果与节流（核心侧的高亮由调用方 `search_close` 清除）。
    pub fn close(&mut self) {
        self.open = false;
        self.result = FindResult::Idle;
        self.refresher.cancel();
    }

    /// 文字清空时的收尾：不显示“无结果”。
    pub fn clear_result(&mut self) {
        self.result = FindResult::Idle;
        self.refresher.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(total: u32, current: Option<u32>) -> FindResult {
        FindResult::Found { total, current }
    }

    #[test]
    fn query_is_none_for_empty_text() {
        let mut s = FindState::default();
        assert!(s.query().is_none());
        s.text = "abc".into();
        s.case_sensitive = true;
        let q = s.query().unwrap();
        assert_eq!(q.text, "abc");
        assert!(q.case_sensitive);
        assert!(!q.regex);
        s.regex = true;
        assert!(s.query().unwrap().regex);
    }

    #[test]
    fn status_text_cases() {
        let mut s = FindState::default();
        assert_eq!(s.status_text(), "");
        s.result = found(0, None);
        assert_eq!(s.status_text(), "无结果");
        s.result = found(12, Some(3));
        assert_eq!(s.status_text(), "3/12");
        s.result = found(12, None);
        assert_eq!(s.status_text(), "–/12");
        s.result = FindResult::InvalidRegex;
        assert_eq!(s.status_text(), "无效的正则表达式");
        assert!(s.is_error());
        s.result = found(1, Some(1));
        assert!(!s.is_error());
    }

    #[test]
    fn result_converts_from_core_status() {
        let r: FindResult = SearchStatus {
            invalid_regex: false,
            total: 5,
            current: Some(2),
        }
        .into();
        assert_eq!(r, found(5, Some(2)));
        assert_eq!(
            FindResult::from(SearchStatus {
                invalid_regex: true,
                total: 0,
                current: None
            }),
            FindResult::InvalidRegex
        );
    }

    #[test]
    fn refresh_runs_immediately_then_coalesces() {
        let mut r = Refresher::default();
        assert_eq!(
            r.request(),
            Throttle::RunNow {
                timer_ms: REFRESH_INTERVAL_MS
            }
        );
        // 窗口内的多次请求合并成一个尾随。
        assert_eq!(r.request(), Throttle::Deferred);
        assert_eq!(r.request(), Throttle::Deferred);
        assert_eq!(
            r.on_timer(),
            TimerAction::RunAndRearm {
                timer_ms: REFRESH_INTERVAL_MS
            }
        );
        // 新窗口内没有新输出：窗口结束。
        assert_eq!(r.on_timer(), TimerAction::Idle);
        // 之后又能立即执行。
        assert!(matches!(r.request(), Throttle::RunNow { .. }));
    }

    #[test]
    fn cancel_ends_the_window() {
        let mut r = Refresher::default();
        r.request();
        r.request();
        r.cancel();
        assert_eq!(r.on_timer(), TimerAction::Idle);
        assert!(matches!(r.request(), Throttle::RunNow { .. }));
    }

    #[test]
    fn close_resets_everything() {
        let mut s = FindState {
            open: true,
            text: "x".into(),
            result: found(1, Some(1)),
            ..Default::default()
        };
        assert!(s.wants_refresh());
        s.close();
        assert!(!s.open);
        assert_eq!(s.result, FindResult::Idle);
        assert!(!s.wants_refresh());
        // 文字保留，下次打开可直接重搜。
        assert_eq!(s.text, "x");
    }
}
