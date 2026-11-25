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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use kanso_term::{CommandTracker, Effect, Finish, InputKind, Registered, TermEvent, Terminal};
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
    /// 与 `cond` 同步递增的版本号，给 async 的等待者（MCP）用：`Condvar` 只能阻塞线程，
    /// 而 MCP 的等待在 tokio 任务里，不能占着工作线程。版本号只用来唤醒，值本身没有含义。
    tick: tokio::sync::watch::Sender<u64>,
    /// 最近一批输出到达的时刻（`now()` 的毫秒数）。`kanso_terminal_send` 的"等输出安静"据此判断。
    last_output_ms: AtomicU64,
}

/// [`ActivityHub::progress`] 的结果：MCP 的等待者每次被唤醒后据此重新判断。
#[derive(Clone, Debug)]
pub(crate) struct Progress {
    /// 会话已结束（跟踪状态已被重置，其余字段不再有意义）。
    pub closed: bool,
    /// 用户来源的输入累计次数。
    pub user_inputs: u64,
    /// 已结束的命令累计数。
    pub finished: u64,
    /// 最近一次结束的命令的结果。
    pub last_finish: Finish,
    /// 是否见过 OSC 133。
    pub osc133: bool,
    /// 当前活动。
    pub activity: Activity,
    /// 最近一批输出到达的时刻。
    pub last_output_ms: u64,
}

impl ActivityHub {
    pub(crate) fn new() -> Arc<ActivityHub> {
        Arc::new(ActivityHub {
            tracker: Mutex::new(CommandTracker::new()),
            cond: Condvar::new(),
            epoch: Instant::now(),
            closed: AtomicBool::new(false),
            tick: tokio::sync::watch::Sender::new(0),
            last_output_ms: AtomicU64::new(0),
        })
    }

    /// 会话创建以来的单调毫秒数。
    pub(crate) fn now(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// 取终端锁与 tracker 锁，执行 `f`，然后（释放终端锁之后、仍持有 tracker 锁）应用效果。
    pub(crate) fn with_tracker<H: ActivityHost, R>(
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
        self.wake();
        result
    }

    /// 只读检查 MCP 命令的终端前置条件。
    pub(crate) fn run_blocker(&self, host: &impl ActivityHost) -> Option<kanso_term::RunBlock> {
        let term = lock(host.term());
        lock(&self.tracker).run_blocker(&term)
    }

    /// 唤醒线程里等待的（`Condvar`）与 async 里等待的（`watch`）。
    fn wake(&self) {
        self.cond.notify_all();
        self.tick.send_modify(|v| *v = v.wrapping_add(1));
    }

    /// 订阅状态变化。**必须先订阅、再读状态**：订阅之后发生的变化 `changed()` 一定能看到，
    /// 反过来会在"读到旧状态"与"开始等待"之间漏掉一次唤醒。
    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.tick.subscribe()
    }

    /// 取 MCP 等待者关心的状态快照。只取 tracker 锁（不取终端锁），可以在任何不持有它们的地方调用。
    pub(crate) fn progress(&self) -> Progress {
        let t = lock(&self.tracker);
        Progress {
            closed: self.closed.load(Ordering::Acquire),
            user_inputs: t.user_inputs(),
            finished: t.finished(),
            last_finish: t.last_finish().clone(),
            osc133: t.osc133(),
            activity: t.activity(),
            last_output_ms: self.last_output_ms.load(Ordering::Acquire),
        }
    }

    /// MCP 自己的输入（不算用户输入），见 `CommandTracker::on_mcp_input`。**必须在写出之前调用**，理由同 `on_input`。
    pub(crate) fn on_mcp_input<H: ActivityHost>(&self, host: &H, kind: InputKind) {
        self.with_tracker(host, |t, term, now| ((), t.on_mcp_input(kind, term, now)));
    }

    /// 记一次不经过命令行状态机的用户输入，见 `CommandTracker::note_user_input`。
    pub(crate) fn note_user_input(&self) {
        lock(&self.tracker).note_user_input();
        self.wake();
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
        self.last_output_ms.store(self.now(), Ordering::Release);
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
        self.wake();
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
