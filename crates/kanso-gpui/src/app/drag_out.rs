//! 文件面板条目拖出的纯逻辑（docs/spec/filer.md §7.2、§7.4、§7.5）。
//!
//! GPUI 0.2.2 没有"向系统发起拖放"的接口（见 `docs/windows-gpui-handoff.md` 的说明），所以拖出只能落在本应用内：
//! 终端（插入路径）。本地条目路径现成；远端条目要先下载成本机临时副本（Windows / Linux 的旧版做法，§7.4），
//! 准备期间提前松手就取消下载，准备完成时手势已结束则提示"请重新拖动"。
//!
//! [`DragPrep`] 是拖动手势与后台下载之间共享的小状态机：拖动载荷持有它，
//! 终端的落下处理、面板的"拖动结束"轮询和下载完成事件都通过它做决定，决定本身可单测。

use std::sync::Mutex;

/// 手势与准备结果合起来的结局，调用方据此做副作用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// 无事可做。
    Nothing,
    /// 取消正在进行的准备下载（并清理半成品）。
    CancelDownload,
    /// 提示"准备完成但拖动已结束，请重新拖动"。
    TooLate,
    /// 提示准备失败及原因。
    Failed(String),
}

/// 落下时的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropOutcome {
    /// 向落点插入这些本地路径。
    Insert(Vec<String>),
    /// 还没准备完：落下无效，准备已取消。
    NotReady,
    /// 准备失败，原因。
    Failed(String),
    /// 这次拖动已经落下过或已被取消，忽略。
    Ignore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    Preparing,
    Ready(Vec<String>),
    Failed(String),
    Cancelled,
}

#[derive(Debug)]
struct Inner {
    state: State,
    /// 本地条目：一开始就就绪，结束时不需要提示。
    local: bool,
    dropped: bool,
    ended: bool,
}

/// 一次拖动的准备状态（放进 `Arc` 共享）。
#[derive(Debug)]
pub struct DragPrep {
    inner: Mutex<Inner>,
}

impl DragPrep {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 本地条目：路径现成。
    pub fn local(paths: Vec<String>) -> DragPrep {
        DragPrep {
            inner: Mutex::new(Inner {
                state: State::Ready(paths),
                local: true,
                dropped: false,
                ended: false,
            }),
        }
    }

    /// 远端条目：开始准备（下载）。
    pub fn remote() -> DragPrep {
        DragPrep {
            inner: Mutex::new(Inner {
                state: State::Preparing,
                local: false,
                dropped: false,
                ended: false,
            }),
        }
    }

    /// 把一个刚建好的（远端形态）准备状态改成本地条目：路径现成。
    pub fn set_local(&self, paths: Vec<String>) {
        let mut g = self.lock();
        g.state = State::Ready(paths);
        g.local = true;
    }

    /// 是否已被取消（提前松手 / 落下时还没准备完）。
    pub fn is_cancelled(&self) -> bool {
        self.lock().state == State::Cancelled
    }

    /// 是否还在准备。
    pub fn is_preparing(&self) -> bool {
        self.lock().state == State::Preparing
    }

    /// 手势是否已结束 / 已落下（面板据此停止轮询）。
    pub fn is_over(&self) -> bool {
        let g = self.lock();
        g.ended || g.dropped
    }

    /// 下载结束（成功给出本地路径，失败给出原因）。
    pub fn complete(&self, result: Result<Vec<String>, String>) -> Effect {
        let mut g = self.lock();
        if g.state == State::Cancelled {
            return Effect::Nothing;
        }
        match result {
            Ok(paths) => {
                g.state = State::Ready(paths);
                // 手势已经结束、也没有落下：不突然启动没有鼠标手势的拖放，提示重新拖动。
                if g.ended && !g.dropped {
                    Effect::TooLate
                } else {
                    Effect::Nothing
                }
            }
            Err(why) => {
                g.state = State::Failed(why.clone());
                if g.dropped {
                    Effect::Nothing // 落下时已经报告过
                } else {
                    Effect::Failed(why)
                }
            }
        }
    }

    /// 落点接收（松手落在可接受的目标上）。
    pub fn on_drop(&self) -> DropOutcome {
        let mut g = self.lock();
        if g.dropped || g.state == State::Cancelled {
            return DropOutcome::Ignore;
        }
        g.dropped = true;
        match g.state.clone() {
            State::Ready(p) => DropOutcome::Insert(p),
            State::Preparing => {
                g.state = State::Cancelled;
                DropOutcome::NotReady
            }
            State::Failed(why) => DropOutcome::Failed(why),
            State::Cancelled => DropOutcome::Ignore,
        }
    }

    /// 手势结束（鼠标已松开，不论落在哪里）。
    pub fn on_ended(&self) -> Effect {
        let mut g = self.lock();
        if g.ended {
            return Effect::Nothing;
        }
        g.ended = true;
        if g.dropped {
            return Effect::Nothing;
        }
        match g.state {
            State::Preparing => {
                g.state = State::Cancelled;
                Effect::CancelDownload
            }
            State::Ready(_) if !g.local => Effect::TooLate,
            State::Failed(_) | State::Ready(_) | State::Cancelled => Effect::Nothing,
        }
    }
}

/// 把拖动的条目名拼成拖动提示文字（多项只显示数量）。
pub fn chip_text(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [first, rest @ ..] => format!("{first} +{}", rest.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_drop_inserts() {
        let p = DragPrep::local(vec!["C:\\a".into()]);
        assert_eq!(p.on_drop(), DropOutcome::Insert(vec!["C:\\a".into()]));
        assert_eq!(p.on_drop(), DropOutcome::Ignore);
        assert_eq!(p.on_ended(), Effect::Nothing);
    }

    #[test]
    fn local_ended_without_drop_is_silent() {
        let p = DragPrep::local(vec!["x".into()]);
        assert_eq!(p.on_ended(), Effect::Nothing);
    }

    #[test]
    fn early_release_cancels_download() {
        let p = DragPrep::remote();
        assert_eq!(p.on_ended(), Effect::CancelDownload);
        // 取消后下载结束的事件不再产生提示。
        assert_eq!(p.complete(Err("cancelled".into())), Effect::Nothing);
    }

    #[test]
    fn ready_after_gesture_ended_asks_to_redrag() {
        let q = DragPrep::remote();
        q.complete(Ok(vec!["t".into()]));
        assert_eq!(q.on_ended(), Effect::TooLate);
    }

    #[test]
    fn drop_while_preparing_is_refused_and_cancelled() {
        let p = DragPrep::remote();
        assert_eq!(p.on_drop(), DropOutcome::NotReady);
        assert_eq!(p.complete(Ok(vec!["t".into()])), Effect::Nothing);
        assert_eq!(p.on_ended(), Effect::Nothing);
    }

    #[test]
    fn remote_ready_then_drop() {
        let p = DragPrep::remote();
        assert_eq!(p.complete(Ok(vec!["t".into()])), Effect::Nothing);
        assert_eq!(p.on_drop(), DropOutcome::Insert(vec!["t".into()]));
        assert_eq!(p.on_ended(), Effect::Nothing);
    }

    #[test]
    fn failure_is_reported_once() {
        let p = DragPrep::remote();
        assert_eq!(
            p.complete(Err("boom".into())),
            Effect::Failed("boom".into())
        );
        let q = DragPrep::remote();
        assert_eq!(q.on_drop(), DropOutcome::NotReady);
        assert_eq!(q.complete(Err("boom".into())), Effect::Nothing);
    }

    #[test]
    fn set_local_makes_ready_and_silent() {
        let p = DragPrep::remote();
        p.set_local(vec!["C:\\x".into()]);
        assert_eq!(p.on_ended(), Effect::Nothing);
        let q = DragPrep::remote();
        q.set_local(vec!["a".into()]);
        assert_eq!(q.on_drop(), DropOutcome::Insert(vec!["a".into()]));
        let r = DragPrep::remote();
        assert_eq!(r.on_drop(), DropOutcome::NotReady);
        assert!(r.is_cancelled());
    }

    #[test]
    fn chip() {
        assert_eq!(chip_text(&["a".into()]), "a");
        assert_eq!(chip_text(&["a".into(), "b".into(), "c".into()]), "a +2");
    }
}
