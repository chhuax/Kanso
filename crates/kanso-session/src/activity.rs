//! 会话与命令跟踪状态机（`kanso_term::CommandTracker`）之间的粘合层，本地与 SSH 会话共用。
//!
//! 状态机是 sans-IO 的：它需要"当前屏幕"（`&Terminal`）与"当前时刻"，产出 `Effect`。
//! 这里负责三件事：加锁（顺序见下）、把 `Effect` 变成事件 / 通知、给"2 秒无输出"与"延迟读命令文本"
//! 这类**没有任何输入或输出触发**的待办提供定时唤醒。
//!
//! # 锁顺序
//!
//! `Terminal` 锁 → tracker 锁 →（生命周期锁，仅在发事件时）。三者从不反向嵌套：
//! - 调用本模块任何方法时**不得持有 `Terminal` 锁**（方法内部会自己取）；
//! - `Shared::finish` 持有生命周期锁，因此它必须在**释放**生命周期锁之后才调用 [`ActivityHub::close`]。
//!
//! 事件在持有 tracker 锁时发出：多个线程（读线程 / 输入线程 / 定时线程）都会产生活动变化，
//! 持锁发出才能保证事件顺序与状态机的转换顺序一致。`SessionHost::emit` 的契约要求它立即返回、
//! 不回调会话，所以不会死锁。
//!
//! # 时钟
//!
//! 状态机的时间是相对会话创建时刻的单调毫秒数（`Instant`），不受系统时间回拨影响；
//! 状态机本身的测试用手动推进的数字，不依赖这里的时钟。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use kanso_term::{CommandTracker, Effect, InputKind, Registered, TermEvent, Terminal};
use kanso_types::agent::AgentKind;
use kanso_types::event::Activity;

use crate::local::lock;

/// 定时线程没有待办时的轮询间隔：只用来发现会话对象已经被丢弃（正常路径靠 [`ActivityHub::close`] 立即唤醒）。
const IDLE_POLL: Duration = Duration::from_secs(5);

/// 会话要提供给 [`ActivityHub`] 的能力。
pub(crate) trait ActivityHost: Send + Sync + 'static {
    /// 终端。
    fn term(&self) -> &Mutex<Terminal>;
    /// 发 `ActivityChanged` 事件。
    fn emit_activity(&self, activity: Activity);
    /// 一条命令（`agent` 为 `None`）/ 一个 AI 轮次（`agent` 为该工具）结束了；会话不可见时应发对应的后台完成通知。
    fn command_completed(&self, exit_code: Option<i32>, agent: Option<AgentKind>);
    fn command_submitted(&self, text: &str);
}

/// 每个会话一个。见模块文档。
pub(crate) struct ActivityHub {
    tracker: Mutex<CommandTracker>,
    /// 状态变化（命令返回、会话结束）或待办时刻变化时唤醒：定时线程与 `wait_idle` 的等待者都在它上面等。
    cond: Condvar,
    epoch: Instant,
    closed: AtomicBool,
}

impl ActivityHub {
    pub(crate) fn new() -> Arc<ActivityHub> {
        Arc::new(ActivityHub {
            tracker: Mutex::new(CommandTracker::new()),
            cond: Condvar::new(),
            epoch: Instant::now(),
            closed: AtomicBool::new(false),
        })
    }

    fn now(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// 取终端锁与 tracker 锁，执行 `f`，然后（释放终端锁之后、仍持有 tracker 锁）应用效果。
    fn with_tracker<H: ActivityHost, R>(
        &self,
        host: &H,
        f: impl FnOnce(&mut CommandTracker, &Terminal, u64) -> (R, Vec<Effect>),
    ) -> R {
        let term = lock(host.term());
        let mut tracker = lock(&self.tracker);
        let (result, effects) = f(&mut tracker, &term, self.now());
        drop(term);
        for effect in effects {
            match effect {
                Effect::Activity(a) => host.emit_activity(a),
                Effect::Completed { exit_code, agent } => host.command_completed(exit_code, agent),
                Effect::Submitted {
                    text,
                    history: true,
                } => host.command_submitted(&text),
                Effect::Submitted { history: false, .. } => {}
            }
        }
        drop(tracker);
        // 状态可能变了（命令返回、待办时刻变化）：叫醒等待者与定时线程。
        self.cond.notify_all();
        result
    }

    /// 用户输入（按键、IME 上屏、粘贴）。**必须在输入字节写出去之前调用**：
    /// 输入起点要读的是"回显还没到达"时的光标位置，写出之后回显可能抢在读取之前改变光标。
    ///
    /// 终端自己的回应（DA、CPR 等）不得经过这里（terminal.md §5.2）。
    pub(crate) fn on_input<H: ActivityHost>(&self, host: &H, kind: InputKind) {
        self.with_tracker(host, |t, term, now| ((), t.on_input(kind, term, now)));
    }

    /// 一批输出已经喂进终端。`events` 是这一批的 `take_events`；没有事件也要调用（轮次靠输出续命）。
    pub(crate) fn on_output<H: ActivityHost>(&self, host: &H, events: &[TermEvent]) {
        self.with_tracker(host, |t, term, now| ((), t.on_output(events, term, now)));
    }

    /// 定时唤醒。
    fn on_tick<H: ActivityHost>(&self, host: &H) {
        self.with_tracker(host, |t, term, now| ((), t.on_tick(term, now)));
    }

    /// 当前正在运行的 AI 工具。
    pub(crate) fn agent(&self) -> Option<AgentKind> {
        lock(&self.tracker).agent()
    }

    pub(crate) fn completion_input<H: ActivityHost>(&self, host: &H) -> Option<(String, u16)> {
        let term = lock(host.term());
        lock(&self.tracker).completion_input(&term)
    }

    /// Sender 发送前登记，见 `CommandTracker::register_send`。
    pub(crate) fn register_send<H: ActivityHost>(&self, host: &H, text: &str) -> Registered {
        self.with_tracker(host, |t, term, now| t.register_send(text, term, now))
    }

    /// 撤销登记（发送失败时）。
    pub(crate) fn cancel_send<H: ActivityHost>(&self, host: &H) {
        self.with_tracker(host, |t, _, _| ((), t.cancel_send()));
    }

    /// 等到没有命令在运行，最多 `timeout`；返回等待结束时是否已空闲。
    /// 没有命令在运行时立即返回 `true`；会话结束（`close`）也会立即唤醒。
    pub(crate) fn wait_idle(&self, timeout: Duration) -> bool {
        let guard = lock(&self.tracker);
        let (guard, _) = self
            .cond
            .wait_timeout_while(guard, timeout, |t| t.is_command_running())
            .unwrap_or_else(|e| e.into_inner());
        !guard.is_command_running()
    }

    /// 会话结束：重置全部跟踪状态、唤醒等待者、让定时线程退出。
    /// 调用时**不得持有生命周期锁**（见模块文档）。
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        lock(&self.tracker).reset();
        self.cond.notify_all();
    }

    /// 启动定时线程：到点后调用 `on_tick`。线程只持有会话的弱引用，会话被丢弃后自行退出。
    pub(crate) fn spawn_timer<H: ActivityHost>(
        self: &Arc<Self>,
        host: Weak<H>,
        name: String,
    ) -> Option<JoinHandle<()>> {
        let hub = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name(name)
            .spawn(move || hub.timer_loop(&host));
        match spawned {
            Ok(h) => Some(h),
            Err(e) => {
                // 没有定时线程时状态机仍然随输入 / 输出推进，只是"2 秒无输出"要等到下一次输入或输出才会判定。
                tracing::warn!(error = %e, "failed to spawn activity timer thread");
                None
            }
        }
    }

    fn timer_loop<H: ActivityHost>(&self, host: &Weak<H>) {
        loop {
            {
                let mut guard = lock(&self.tracker);
                loop {
                    if self.closed.load(Ordering::Acquire) {
                        return;
                    }
                    let wait = match guard.next_deadline() {
                        Some(deadline) => {
                            let now = self.now();
                            if deadline <= now {
                                break;
                            }
                            Duration::from_millis(deadline - now)
                        }
                        None => {
                            if host.strong_count() == 0 {
                                return;
                            }
                            IDLE_POLL
                        }
                    };
                    guard = self
                        .cond
                        .wait_timeout(guard, wait)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            }
            // 等待期间不持有会话的强引用；到点才升级，用完立刻放手。
            let Some(host) = host.upgrade() else { return };
            self.on_tick(&*host);
        }
    }
}
