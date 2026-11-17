//! 文件传输队列（docs/spec/filer.md §6）：一批源项按顺序逐项处理，每次只传一项；
//! 同名冲突统一询问（覆盖 / 跳过 / 取消，可应用于本批其余冲突）；当前项失败或取消时停止余下项目；
//! 进度条文字（方向、名称、已传 / 总字节、最近约一秒的速率）与完成后的收起时机。
//!
//! 队列只做状态推进，不调用核心：界面按 [`Action`] 去调用 `Core::upload / download`
//! 或本地复制，再把事件喂回 [`TransferQueue::on_finished`] 等方法。时间一律由调用方以毫秒传入，便于测试。
//!
//! 冲突流程（核心 `ConflictPolicy` 的约定）：先用 `Fail` 发起，收到 `already_exists` 失败后询问用户，
//! 再用 `Overwrite` 或 `Skip` 重新发起同一项。

use std::collections::VecDeque;

use kanso_types::TransferId;
use kanso_types::transfer::ConflictPolicy;

use crate::app::filer_text::{t, tf};
use crate::filer::format_size;

/// 传输方向 / 操作类型（进度条文字据此区分）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 本地到远端。
    Upload,
    /// 远端到本地。
    Download,
    /// 本地到本地的复制（拖入本地 Filer）。
    Copy,
}

impl Kind {
    /// 进度条上的操作文字。
    pub fn label(self) -> String {
        t(match self {
            Kind::Upload => "ui-filer-op-upload",
            Kind::Download => "ui-filer-op-download",
            Kind::Copy => "ui-filer-op-copy",
        })
    }
}

/// 一批里的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferItem {
    /// 操作类型。
    pub kind: Kind,
    /// 来源路径（上传 / 复制是本地路径，下载是远端路径）。
    pub source: String,
    /// 名称（显示用）。
    pub name: String,
    /// 目标目录（上传是远端目录，下载 / 复制是本地目录）。**接收任务时锁定**，之后切标签或导航不改写。
    pub dest_dir: String,
    /// 源是否目录。
    pub is_dir: bool,
}

/// 队列忙：同一浏览器已有前台传输，不再接收另一批。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Busy;

impl Busy {
    /// 提示文字。
    pub fn text(self) -> String {
        t("ui-filer-busy")
    }
}

/// 一项传输的结局（界面把核心事件翻译成它）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finish {
    /// 完成；`skipped` 是按“跳过”策略略过的已存在文件数。
    Completed {
        /// 跳过的文件数。
        skipped: u32,
    },
    /// 被取消。
    Cancelled,
    /// 失败。
    Failed {
        /// 用户可见原因。
        text: String,
        /// 原因是目标已存在（且调用时策略为 `Fail`）。
        already_exists: bool,
    },
}

/// 待用户决定的同名冲突。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// 冲突的项。
    pub item: TransferItem,
    /// 这一批里还没开始的项数（决定是否提供“应用于其余”）。
    pub remaining: usize,
}

/// 用户对冲突的决定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// 覆盖。`all` 表示应用于本批其余冲突。
    Overwrite {
        /// 应用于其余。
        all: bool,
    },
    /// 跳过。
    Skip {
        /// 应用于其余。
        all: bool,
    },
    /// 取消整批。
    Cancel,
}

/// 界面下一步该做什么。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// 以给定策略发起这一项（拿到核心返回的传输 id 后调用 [`TransferQueue::bind`]，发起失败调用 [`TransferQueue::start_failed`]）。
    Start(TransferItem, ConflictPolicy),
    /// 询问用户如何处理同名冲突，答案交给 [`TransferQueue::resolve`]。
    Ask(Conflict),
    /// 整批结束（正常、取消或失败），界面应刷新接收侧；队列已回到空闲。
    Finished(BatchResult),
    /// 还在等当前项的事件，无事可做。
    Wait,
}

/// 一批结束的概况。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchResult {
    /// 完成的项数。
    pub completed: usize,
    /// 因“跳过”策略略过的文件数。
    pub skipped: u32,
    /// 收尾方式。
    pub end: BatchEnd,
}

/// 一批的收尾方式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchEnd {
    /// 全部处理完。
    Done,
    /// 用户取消。
    Cancelled,
    /// 某项失败（余下项目没有开始）。
    Failed(String),
}

/// 速率计：只看最近约一秒的样本，不是从任务建立开始的终身平均。
#[derive(Clone, Debug, Default)]
pub struct SpeedMeter {
    samples: VecDeque<(u64, u64)>,
}

/// 速率统计窗口（毫秒）。
pub const SPEED_WINDOW_MS: u64 = 1000;

impl SpeedMeter {
    /// 记一个样本：(时刻毫秒, 累计字节)。
    pub fn push(&mut self, now_ms: u64, done: u64) {
        // 累计字节倒退（新的一项开始）时重新计。
        if self.samples.back().is_some_and(|&(_, d)| done < d) {
            self.samples.clear();
        }
        self.samples.push_back((now_ms, done));
        while self.samples.len() > 2
            && self
                .samples
                .front()
                .is_some_and(|&(t, _)| now_ms.saturating_sub(t) > SPEED_WINDOW_MS)
        {
            self.samples.pop_front();
        }
    }

    /// 清空。
    pub fn reset(&mut self) {
        self.samples.clear();
    }

    /// 字节 / 秒；样本不足或时间没有前进时为 `None`。
    pub fn bytes_per_sec(&self) -> Option<f64> {
        let (t0, d0) = *self.samples.front()?;
        let (t1, d1) = *self.samples.back()?;
        if t1 <= t0 {
            return None;
        }
        Some((d1 - d0) as f64 * 1000.0 / (t1 - t0) as f64)
    }
}

/// 速率文字，如 `5.2 MB/s`。
pub fn speed_text(bytes_per_sec: f64) -> String {
    format!("{}/s", format_size(bytes_per_sec.max(0.0) as u64))
}

/// 进度条的显示内容。
#[derive(Clone, Debug, PartialEq)]
pub struct Bar {
    /// 标题：操作 + 名称，如 `上传 report.pdf（2/5）`。
    pub title: String,
    /// 完成比例；总量未知（或为 0）时为 `None`，画不确定进度。
    pub fraction: Option<f64>,
    /// `12.3 MB / 40 MB  5.2 MB/s`。
    pub detail: String,
    /// 是否显示取消按钮。
    pub cancellable: bool,
    /// 收尾状态；进行中为 `None`。
    pub finish: Option<BarFinish>,
}

/// 进度条的收尾状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarFinish {
    /// 完成（100%）。
    Done,
    /// 已取消（不当作失败）。
    Cancelled,
    /// 失败。
    Failed,
}

/// 完成 / 取消后进度条保留多久（毫秒）。
pub const DONE_LINGER_MS: u64 = 1800;
/// 失败的进度条保留多久（毫秒）。详细错误另存在 [`TransferQueue::error`]，不随它消失。
pub const FAIL_LINGER_MS: u64 = 3000;

#[derive(Clone, Debug)]
struct Current {
    item: TransferItem,
    id: Option<TransferId>,
    done: u64,
    total: Option<u64>,
    /// 等用户决定冲突。
    awaiting: bool,
}

/// 传输队列。
#[derive(Clone, Debug, Default)]
pub struct TransferQueue {
    pending: VecDeque<TransferItem>,
    current: Option<Current>,
    apply_all: Option<ConflictPolicy>,
    batch_total: usize,
    batch_done: usize,
    skipped: u32,
    speed: SpeedMeter,
    cancel_requested: bool,
    /// 收尾的进度条：(收尾状态, 时刻毫秒, 最后的标题与详情)。
    lingering: Option<(BarFinish, u64, String)>,
    /// 最近一次失败的详细原因，直到下一次操作才清除。
    error: Option<String>,
}

impl TransferQueue {
    /// 是否有前台传输（含等待冲突决定）。
    pub fn is_busy(&self) -> bool {
        self.current.is_some() || !self.pending.is_empty()
    }

    /// 最近一次失败的详细原因。
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// 用户关掉错误提示。
    pub fn clear_error(&mut self) {
        self.error = None;
    }

    /// 当前项对应的核心传输 id。
    pub fn current_id(&self) -> Option<TransferId> {
        self.current.as_ref().and_then(|c| c.id)
    }

    /// 当前项（含正在等待冲突决定的）。
    pub fn current_item(&self) -> Option<&TransferItem> {
        self.current.as_ref().map(|c| &c.item)
    }

    /// 接收一批。已有前台传输时拒绝（旧版没有全局任务队列）。接收时目标目录已写在各项里，之后不再改。
    pub fn enqueue(&mut self, items: Vec<TransferItem>) -> Result<(), Busy> {
        if self.is_busy() {
            return Err(Busy);
        }
        if items.is_empty() {
            return Ok(());
        }
        self.batch_total = items.len();
        self.batch_done = 0;
        self.skipped = 0;
        self.apply_all = None;
        self.cancel_requested = false;
        self.lingering = None;
        self.error = None;
        self.speed.reset();
        self.pending = items.into();
        Ok(())
    }

    /// 推进：没有当前项时取下一项开始；整批处理完时给出结果。
    pub fn next_action(&mut self) -> Action {
        if self.current.is_some() {
            return Action::Wait;
        }
        let Some(item) = self.pending.pop_front() else {
            return Action::Wait;
        };
        let policy = self.apply_all.unwrap_or(ConflictPolicy::Fail);
        self.current = Some(Current {
            item: item.clone(),
            id: None,
            done: 0,
            total: None,
            awaiting: false,
        });
        self.speed.reset();
        Action::Start(item, policy)
    }

    /// 核心返回了传输 id。
    pub fn bind(&mut self, id: TransferId) {
        if let Some(c) = &mut self.current {
            c.id = Some(id);
        }
    }

    /// 发起这一项失败（参数类错误，核心不发任何事件）：停止整批并记下原因。
    pub fn start_failed(&mut self, text: String, now_ms: u64) -> Action {
        self.finish_current(
            Finish::Failed {
                text,
                already_exists: false,
            },
            now_ms,
        )
    }

    /// `TransferStarted` 事件。
    pub fn on_started(&mut self, id: TransferId, total: Option<u64>) {
        if let Some(c) = self.current.as_mut().filter(|c| c.id == Some(id)) {
            c.total = total;
        }
    }

    /// `TransferProgress` 事件。不属于当前项的 id 忽略（来自别的批或别的会话）。
    pub fn on_progress(&mut self, id: TransferId, done: u64, total: Option<u64>, now_ms: u64) {
        if let Some(c) = self.current.as_mut().filter(|c| c.id == Some(id)) {
            c.done = done;
            if total.is_some() {
                c.total = total;
            }
            self.speed.push(now_ms, done);
        }
    }

    /// 本地复制的进度（没有传输 id）。
    pub fn on_local_progress(&mut self, done: u64, total: u64, now_ms: u64) {
        if let Some(c) = self.current.as_mut().filter(|c| c.id.is_none()) {
            c.done = done;
            c.total = Some(total);
            self.speed.push(now_ms, done);
        }
    }

    /// `TransferFinished` 事件；不是当前项的 id 返回 `None`（忽略）。
    pub fn on_finished(&mut self, id: TransferId, finish: Finish, now_ms: u64) -> Option<Action> {
        if self.current.as_ref()?.id != Some(id) {
            return None;
        }
        Some(self.finish_current(finish, now_ms))
    }

    /// 本地复制结束（当前项没有传输 id）。
    pub fn on_local_finished(&mut self, finish: Finish, now_ms: u64) -> Option<Action> {
        if self.current.as_ref()?.id.is_some() {
            return None;
        }
        Some(self.finish_current(finish, now_ms))
    }

    fn finish_current(&mut self, finish: Finish, now_ms: u64) -> Action {
        let Some(cur) = self.current.take() else {
            return Action::Wait;
        };
        match finish {
            Finish::Completed { skipped } => {
                self.batch_done += 1;
                self.skipped += skipped;
                self.after_item(now_ms)
            }
            Finish::Cancelled => self.stop(BatchEnd::Cancelled, now_ms, &cur),
            Finish::Failed {
                already_exists: true,
                ..
            } if self.apply_all.is_none() => {
                let remaining = self.pending.len();
                let item = cur.item.clone();
                self.current = Some(Current {
                    awaiting: true,
                    id: None,
                    ..cur
                });
                Action::Ask(Conflict { item, remaining })
            }
            Finish::Failed { text, .. } => self.stop(BatchEnd::Failed(text), now_ms, &cur),
        }
    }

    fn after_item(&mut self, now_ms: u64) -> Action {
        if self.pending.is_empty() {
            return self.conclude(BatchEnd::Done, now_ms, None);
        }
        self.next_action()
    }

    fn stop(&mut self, end: BatchEnd, now_ms: u64, cur: &Current) -> Action {
        self.pending.clear();
        self.conclude(end, now_ms, Some(cur))
    }

    fn conclude(&mut self, end: BatchEnd, now_ms: u64, cur: Option<&Current>) -> Action {
        let title = match (&end, cur) {
            (BatchEnd::Done, _) => tf(
                "ui-filer-batch-done",
                &[("count", &self.batch_done.to_string())],
            ),
            (BatchEnd::Cancelled, _) => t("ui-filer-cancelled"),
            (BatchEnd::Failed(why), Some(c)) => tf(
                "ui-filer-op-failed",
                &[("op", &c.item.kind.label()), ("reason", why)],
            ),
            (BatchEnd::Failed(why), None) => why.clone(),
        };
        let finish = match &end {
            BatchEnd::Done => BarFinish::Done,
            BatchEnd::Cancelled => BarFinish::Cancelled,
            BatchEnd::Failed(why) => {
                // 详细原因留在错误区域，短提示消失后也不丢。
                self.error = Some(match cur {
                    Some(c) => tf(
                        "ui-filer-op-error",
                        &[
                            ("op", &c.item.kind.label()),
                            ("name", &c.item.name),
                            ("reason", why),
                        ],
                    ),
                    None => why.clone(),
                });
                BarFinish::Failed
            }
        };
        self.lingering = Some((finish, now_ms, title));
        self.current = None;
        self.apply_all = None;
        self.cancel_requested = false;
        Action::Finished(BatchResult {
            completed: self.batch_done,
            skipped: self.skipped,
            end,
        })
    }

    /// 用户对冲突的决定。
    pub fn resolve(&mut self, decision: Decision, now_ms: u64) -> Action {
        let Some(cur) = self.current.take().filter(|c| c.awaiting) else {
            return Action::Wait;
        };
        match decision {
            Decision::Overwrite { all } => {
                if all {
                    self.apply_all = Some(ConflictPolicy::Overwrite);
                }
                let item = cur.item.clone();
                self.current = Some(Current {
                    awaiting: false,
                    done: 0,
                    total: None,
                    id: None,
                    ..cur
                });
                self.speed.reset();
                Action::Start(item, ConflictPolicy::Overwrite)
            }
            Decision::Skip { all } => {
                if all {
                    self.apply_all = Some(ConflictPolicy::Skip);
                }
                // 这一项整体跳过：算处理过，但不计入完成的文件数。
                self.skipped += 1;
                self.batch_done += 1;
                self.after_item(now_ms)
            }
            Decision::Cancel => self.stop(BatchEnd::Cancelled, now_ms, &cur),
        }
    }

    /// 用户点了取消：返回要取消的核心传输 id（本地复制 / 等待冲突时没有 id，调用方另行处理）。
    /// 余下项目随当前项被取消而停止，不会自动重新开始。
    pub fn request_cancel(&mut self) -> Option<TransferId> {
        self.cancel_requested = true;
        self.pending.clear();
        self.current.as_ref().and_then(|c| c.id)
    }

    /// 用户是否已请求取消（本地复制的后台线程据此停止）。
    pub fn cancel_requested(&self) -> bool {
        self.cancel_requested
    }

    /// 当前要显示的进度条；没有活动、也没有仍在停留的收尾状态时为 `None`。
    pub fn bar(&self, now_ms: u64) -> Option<Bar> {
        if let Some(c) = &self.current {
            let batch = if self.batch_total > 1 {
                format!("（{}/{}）", self.batch_done + 1, self.batch_total)
            } else {
                String::new()
            };
            let title = format!("{} {}{batch}", c.item.kind.label(), c.item.name);
            if c.awaiting {
                return Some(Bar {
                    title,
                    fraction: None,
                    detail: t("ui-filer-awaiting-conflict"),
                    cancellable: true,
                    finish: None,
                });
            }
            let fraction = match c.total {
                Some(t) if t > 0 => Some((c.done as f64 / t as f64).clamp(0.0, 1.0)),
                _ => None,
            };
            let mut detail = match c.total {
                Some(t) if t > 0 => format!("{} / {}", format_size(c.done), format_size(t)),
                _ => format_size(c.done),
            };
            if let Some(bps) = self.speed.bytes_per_sec() {
                detail.push_str("  ");
                detail.push_str(&speed_text(bps));
            }
            return Some(Bar {
                title,
                fraction,
                detail,
                cancellable: true,
                finish: None,
            });
        }
        let (finish, at, title) = self.lingering.as_ref()?;
        let linger = match finish {
            BarFinish::Failed => FAIL_LINGER_MS,
            _ => DONE_LINGER_MS,
        };
        if now_ms.saturating_sub(*at) >= linger {
            return None;
        }
        Some(Bar {
            title: title.clone(),
            fraction: matches!(finish, BarFinish::Done).then_some(1.0),
            detail: if self.skipped > 0 {
                tf("ui-filer-skipped", &[("count", &self.skipped.to_string())])
            } else {
                String::new()
            },
            cancellable: false,
            finish: Some(*finish),
        })
    }

    /// 收尾状态还要停留多久（毫秒）；界面据此安排一次重绘去收起进度条。没有则 `None`。
    pub fn linger_left(&self, now_ms: u64) -> Option<u64> {
        if self.current.is_some() {
            return None;
        }
        let (finish, at, _) = self.lingering.as_ref()?;
        let linger = match finish {
            BarFinish::Failed => FAIL_LINGER_MS,
            _ => DONE_LINGER_MS,
        };
        let left = (*at + linger).saturating_sub(now_ms);
        (left > 0).then_some(left)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str) -> TransferItem {
        TransferItem {
            kind: Kind::Upload,
            source: format!("C:\\{name}"),
            name: name.into(),
            dest_dir: "/srv".into(),
            is_dir: false,
        }
    }

    fn tid(n: u64) -> TransferId {
        TransferId::from_raw(n)
    }

    fn ok() -> Finish {
        Finish::Completed { skipped: 0 }
    }

    fn exists() -> Finish {
        Finish::Failed {
            text: "已存在".into(),
            already_exists: true,
        }
    }

    #[test]
    fn items_run_one_at_a_time_in_order() {
        let mut q = TransferQueue::default();
        q.enqueue(vec![item("a"), item("b")]).unwrap();
        assert_eq!(
            q.next_action(),
            Action::Start(item("a"), ConflictPolicy::Fail)
        );
        // 当前项没结束，不会开始下一项。
        assert_eq!(q.next_action(), Action::Wait);
        q.bind(tid(1));
        let next = q.on_finished(tid(1), ok(), 100).unwrap();
        assert_eq!(next, Action::Start(item("b"), ConflictPolicy::Fail));
        q.bind(tid(2));
        match q.on_finished(tid(2), ok(), 200).unwrap() {
            Action::Finished(r) => {
                assert_eq!(r.completed, 2);
                assert_eq!(r.end, BatchEnd::Done);
            }
            other => panic!("{other:?}"),
        }
        assert!(!q.is_busy());
    }

    #[test]
    fn a_second_batch_is_refused_while_busy() {
        let mut q = TransferQueue::default();
        q.enqueue(vec![item("a")]).unwrap();
        assert_eq!(q.enqueue(vec![item("b")]), Err(Busy));
        assert!(Busy.text().contains("请等待"));
        q.next_action();
        assert_eq!(q.enqueue(vec![item("b")]), Err(Busy));
    }

    #[test]
    fn failure_stops_the_rest_and_keeps_the_error() {
        let mut q = TransferQueue::default();
        q.enqueue(vec![item("a"), item("b"), item("c")]).unwrap();
        q.next_action();
        q.bind(tid(1));
        q.on_finished(tid(1), ok(), 10);
        q.bind(tid(2));
        let r = q
            .on_finished(
                tid(2),
                Finish::Failed {
                    text: "磁盘已满".into(),
                    already_exists: false,
                },
                20,
            )
            .unwrap();
        match r {
            Action::Finished(r) => {
                assert_eq!(r.completed, 1);
                assert_eq!(r.end, BatchEnd::Failed("磁盘已满".into()));
            }
            other => panic!("{other:?}"),
        }
        assert!(!q.is_busy());
        // 短提示约 3 秒后消失，但详细错误还在。
        assert!(q.bar(2000).is_some());
        assert!(q.bar(4000).is_none());
        assert!(q.error().unwrap().contains("磁盘已满"));
        assert!(q.error().unwrap().contains('b'));
    }

    #[test]
    fn cancelled_is_not_a_failure() {
        let mut q = TransferQueue::default();
        q.enqueue(vec![item("a"), item("b")]).unwrap();
        q.next_action();
        q.bind(tid(7));
        assert_eq!(q.request_cancel(), Some(tid(7)));
        let r = q.on_finished(tid(7), Finish::Cancelled, 50).unwrap();
        assert!(matches!(
            r,
            Action::Finished(BatchResult {
                end: BatchEnd::Cancelled,
                ..
            })
        ));
        assert_eq!(q.error(), None);
        let bar = q.bar(100).unwrap();
        assert_eq!(bar.finish, Some(BarFinish::Cancelled));
        assert_eq!(bar.title, "已取消");
        assert!(q.bar(2000).is_none());
        // 取消后不会自动重新开始。
        assert_eq!(q.next_action(), Action::Wait);
    }

    #[test]
    fn conflict_asks_then_restarts_with_overwrite() {
        let mut q = TransferQueue::default();
        q.enqueue(vec![item("a"), item("b")]).unwrap();
        q.next_action();
        q.bind(tid(1));
        match q.on_finished(tid(1), exists(), 0).unwrap() {
            Action::Ask(c) => {
                assert_eq!(c.item, item("a"));
                assert_eq!(c.remaining, 1);
            }
            other => panic!("{other:?}"),
        }
        assert!(q.is_busy());
        assert!(q.bar(0).unwrap().detail.contains("同名"));
        assert_eq!(
            q.resolve(Decision::Overwrite { all: false }, 0),
            Action::Start(item("a"), ConflictPolicy::Overwrite)
        );
        q.bind(tid(2));
        // 没有“应用于其余”：下一项仍以 Fail 开始。
        assert_eq!(
            q.on_finished(tid(2), ok(), 0).unwrap(),
            Action::Start(item("b"), ConflictPolicy::Fail)
        );
    }

    #[test]
    fn apply_to_all_covers_the_remaining_items() {
        let mut q = TransferQueue::default();
        q.enqueue(vec![item("a"), item("b"), item("c")]).unwrap();
        q.next_action();
        q.bind(tid(1));
        q.on_finished(tid(1), exists(), 0);
        assert_eq!(
            q.resolve(Decision::Skip { all: true }, 0),
            Action::Start(item("b"), ConflictPolicy::Skip)
        );
        q.bind(tid(2));
        assert_eq!(
            q.on_finished(tid(2), Finish::Completed { skipped: 3 }, 0)
                .unwrap(),
            Action::Start(item("c"), ConflictPolicy::Skip)
        );
        q.bind(tid(3));
        match q.on_finished(tid(3), ok(), 0).unwrap() {
            Action::Finished(r) => {
                // a 整体跳过 1 个 + b 里跳过 3 个。
                assert_eq!(r.skipped, 4);
                assert_eq!(r.end, BatchEnd::Done);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cancelling_at_a_conflict_ends_the_batch() {
        let mut q = TransferQueue::default();
        q.enqueue(vec![item("a"), item("b")]).unwrap();
        q.next_action();
        q.bind(tid(1));
        q.on_finished(tid(1), exists(), 0);
        match q.resolve(Decision::Cancel, 0) {
            Action::Finished(r) => assert_eq!(r.end, BatchEnd::Cancelled),
            other => panic!("{other:?}"),
        }
        assert!(!q.is_busy());
    }

    #[test]
    fn events_of_other_transfers_are_ignored() {
        let mut q = TransferQueue::default();
        q.enqueue(vec![item("a")]).unwrap();
        q.next_action();
        q.bind(tid(1));
        q.on_progress(tid(99), 5, Some(10), 0);
        assert!(q.on_finished(tid(99), ok(), 0).is_none());
        assert_eq!(q.bar(0).unwrap().fraction, None);
    }

    #[test]
    fn progress_bar_text_and_indeterminate_total() {
        let mut q = TransferQueue::default();
        q.enqueue(vec![item("a.bin"), item("b.bin")]).unwrap();
        q.next_action();
        q.bind(tid(1));
        // 总量未知：不确定进度，不会显示成传完了。
        q.on_progress(tid(1), 0, None, 0);
        assert_eq!(q.bar(0).unwrap().fraction, None);
        q.on_progress(tid(1), 0, Some(0), 0);
        assert_eq!(q.bar(0).unwrap().fraction, None);
        q.on_started(tid(1), Some(2048));
        q.on_progress(tid(1), 512, Some(2048), 0);
        q.on_progress(tid(1), 1024, Some(2048), 500);
        let bar = q.bar(500).unwrap();
        assert_eq!(bar.title, "上传 a.bin（1/2）");
        assert_eq!(bar.fraction, Some(0.5));
        assert!(bar.detail.contains("1.0 KB / 2.0 KB"));
        assert!(bar.detail.contains("/s"));
        assert!(bar.cancellable);
    }

    #[test]
    fn speed_reflects_only_the_last_second() {
        let mut m = SpeedMeter::default();
        // 前 10 秒很快（100 KB/s），最近一秒很慢（10 B/s）。
        for s in 0..=10u64 {
            m.push(s * 1000, s * 100_000);
        }
        m.push(11_000, 1_000_000 + 10);
        let bps = m.bytes_per_sec().unwrap();
        assert!(bps < 150_000.0, "{bps}");
        // 窗口只剩最近的样本。
        m.push(12_000, 1_000_000 + 20);
        assert!(m.bytes_per_sec().unwrap() < 100.0);
        assert_eq!(SpeedMeter::default().bytes_per_sec(), None);
        assert_eq!(speed_text(1536.0), "1.5 KB/s");
    }

    #[test]
    fn finished_bar_shows_full_progress_then_hides() {
        let mut q = TransferQueue::default();
        q.enqueue(vec![item("a")]).unwrap();
        q.next_action();
        q.bind(tid(1));
        q.on_finished(tid(1), ok(), 1000);
        let bar = q.bar(1500).unwrap();
        assert_eq!(bar.fraction, Some(1.0));
        assert_eq!(bar.finish, Some(BarFinish::Done));
        assert!(q.bar(1000 + DONE_LINGER_MS).is_none());
        assert_eq!(q.linger_left(1500), Some(1300));
        assert_eq!(q.linger_left(5000), None);
    }

    #[test]
    fn local_copy_uses_the_same_flow_without_transfer_ids() {
        let mut q = TransferQueue::default();
        let mut it = item("a");
        it.kind = Kind::Copy;
        q.enqueue(vec![it.clone()]).unwrap();
        assert!(matches!(q.next_action(), Action::Start(_, _)));
        q.on_local_progress(10, 100, 0);
        assert_eq!(q.bar(0).unwrap().fraction, Some(0.1));
        assert!(matches!(
            q.on_local_finished(ok(), 5).unwrap(),
            Action::Finished(_)
        ));
        assert_eq!(q.bar(10).unwrap().title, "完成：1 项");
    }

    #[test]
    fn start_failure_reports_and_stops() {
        let mut q = TransferQueue::default();
        q.enqueue(vec![item("a"), item("b")]).unwrap();
        q.next_action();
        match q.start_failed("会话不存在".into(), 0) {
            Action::Finished(r) => assert!(matches!(r.end, BatchEnd::Failed(_))),
            other => panic!("{other:?}"),
        }
        assert!(q.error().unwrap().contains("会话不存在"));
        assert!(!q.is_busy());
    }
}
