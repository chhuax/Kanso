//! 会话层对宿主（`kanso-core`）的最小依赖。

use kanso_types::agent::AgentKind;
use kanso_types::{CoreEvent, SessionId};

/// 会话把事件和需要通知用户的情况交给宿主的出口。
///
/// - 实现者：`kanso-core`，用 `Platform.events` 与 `Platform.notifier` 实现。
/// - 调用者：会话的读线程、关闭线程，以及调用 `open_session` 的线程。
/// - 阻塞：实现**必须立即返回**，并且**不得回调会话或管理器的任何方法**：
///   会话在持有内部的生命周期锁时调用 [`SessionHost::emit`]（用来保证同一会话的事件保序，
///   且 `Closed` 之后不再有事件），回调会话会死锁。
pub trait SessionHost: Send + Sync + 'static {
    /// 可记录的命令回显；宿主按命令建议开关决定是否异步保存。
    fn record_command(&self, _command: &str, _host: &str) {}

    /// 发出一个事件。
    fn emit(&self, event: CoreEvent);

    /// 异步询问传输路径；必须异步调用 reply，不能在本方法返回前同步回调。
    fn transfer_prompt(
        &self,
        _session: SessionId,
        _request: kanso_types::PromptRequest,
        reply: Box<dyn FnOnce(kanso_types::PromptResponse) + Send>,
    ) {
        std::thread::spawn(move || reply(kanso_types::PromptResponse::Cancel));
    }

    /// 取消该会话中尚未回答的传输提问。
    fn transfer_prompt_dismiss(&self, _session: SessionId) {}

    /// 会话**不可见**（`set_visible(false)`）时一条命令结束了，宿主应发系统通知
    /// （`Notification::CommandFinished`）。可见时会话不调用它。
    fn command_finished_in_background(&self, session: SessionId, exit_code: Option<i32>);

    /// 会话不可见时，AI 工具的一个轮次结束了（等待用户输入），宿主应发 agent 完成通知
    /// （`Notification::AgentTurnDone`）。可见时会话不调用它。
    ///
    /// 默认实现退化为普通"命令完成"通知，保证只实现了上面两个方法的宿主仍能提醒用户。
    fn agent_turn_done_in_background(&self, session: SessionId, _agent: AgentKind) {
        self.command_finished_in_background(session, None);
    }
}

/// 无界面会话（MCP 的 `mcp_connect`）使用的 host 包装：只放行传输事件，其余全部丢弃。
///
/// 无界面会话不出现在会话列表里，UI 不该收到它的任何会话类事件（状态、标题、通知等）；
/// 但它发起的传输要照常出现在传输列表里（spec/mcp.md §3.5）。
/// 即使是纯 SFTP 机制的会话，终端传输之类的路径理论上也会经过这里，所以按事件类型白名单过滤，
/// 而不是依赖"无界面会话不会产生这些事件"。
pub(crate) struct HeadlessHost(pub(crate) std::sync::Arc<dyn SessionHost>);

impl SessionHost for HeadlessHost {
    fn emit(&self, event: CoreEvent) {
        if matches!(
            event,
            CoreEvent::TransferStarted { .. }
                | CoreEvent::TransferProgress { .. }
                | CoreEvent::TransferFinished { .. }
        ) {
            self.0.emit(event);
        }
    }

    // 无界面会话没有"后台标签"，不发系统通知。
    fn command_finished_in_background(&self, _session: SessionId, _exit_code: Option<i32>) {}
}
