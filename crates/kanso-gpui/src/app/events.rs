//! 核心事件到工作区更新的映射。对照 `Shell/CoreEventMapping.swift`。
//!
//! 事件从核心的工作线程经通道进入 UI 线程，由 [`AppController::handle_event`](super::controller::AppController::handle_event)
//! 逐个处理；本模块只放其中与具体控制器无关、可单独测试的部分：
//! - [`SessionUpdate`]：对工作区的一次更新（纯值类型）；
//! - [`TransferTracker`]：传输 id → 会话（结束事件不带会话）；
//! - [`DirtySet`]：终端变脏的会话集合，UI 每帧取一次。

use std::collections::{BTreeSet, HashMap};

use kanso_types::{CoreEvent, SessionId, TransferId};

use super::tab::{TabActivity, TabConnection};
use super::workspace::Workspace;

/// 核心事件翻译成的、对工作区的一次更新。
#[derive(Clone, Debug, PartialEq)]
pub enum SessionUpdate {
    /// 连接状态变化（含关闭原因、退出码）。
    Connection(SessionId, TabConnection),
    /// 位置变化。
    Location {
        /// 会话。
        session: SessionId,
        /// 当前目录。
        cwd: Option<String>,
        /// 目录所属主机。
        cwd_host: Option<String>,
        /// git 分支。
        git_branch: Option<String>,
    },
    /// 活动变化。
    Activity(SessionId, TabActivity),
    /// 传输开始。
    TransferStarted {
        /// 传输 id。
        transfer: TransferId,
        /// 所属会话。
        session: SessionId,
    },
    /// 传输结束。结束事件不带会话，要靠 [`TransferTracker`] 反查。
    TransferFinished {
        /// 传输 id。
        transfer: TransferId,
    },
    /// 与标签状态无关的事件（终端变脏、通知、提示等由控制器另行处理）。
    Ignored,
}

impl SessionUpdate {
    /// 翻译一个核心事件。
    pub fn from_event(event: &CoreEvent) -> SessionUpdate {
        match event {
            CoreEvent::SessionStateChanged { session, state } => {
                SessionUpdate::Connection(*session, TabConnection::from(state))
            }
            CoreEvent::LocationChanged {
                session,
                cwd,
                cwd_host,
                git_branch,
            } => SessionUpdate::Location {
                session: *session,
                cwd: cwd.clone(),
                cwd_host: cwd_host.clone(),
                git_branch: git_branch.clone(),
            },
            CoreEvent::ActivityChanged { session, activity } => {
                SessionUpdate::Activity(*session, TabActivity::from(*activity))
            }
            CoreEvent::TransferStarted {
                transfer, session, ..
            } => SessionUpdate::TransferStarted {
                transfer: *transfer,
                session: *session,
            },
            CoreEvent::TransferFinished { transfer, .. } => SessionUpdate::TransferFinished {
                transfer: *transfer,
            },
            _ => SessionUpdate::Ignored,
        }
    }
}

/// 传输 id → 会话的映射。
///
/// `TransferFinished` 事件没有 session 字段，所以必须在 `TransferStarted` 时记下来，
/// 否则没法把活动传输计数减回去。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferTracker {
    session_by_transfer: HashMap<TransferId, SessionId>,
}

impl TransferTracker {
    /// 记录开始。
    pub fn start(&mut self, transfer: TransferId, session: SessionId) {
        self.session_by_transfer.insert(transfer, session);
    }

    /// 返回该传输所属会话并移除映射；未知 id（重复的结束事件）返回 `None`。
    pub fn finish(&mut self, transfer: TransferId) -> Option<SessionId> {
        self.session_by_transfer.remove(&transfer)
    }

    /// 会话被移除时清理，避免映射无限增长。
    pub fn forget(&mut self, session: SessionId) {
        self.session_by_transfer.retain(|_, s| *s != session);
    }

    /// 进行中的传输数。
    pub fn len(&self) -> usize {
        self.session_by_transfer.len()
    }

    /// 没有进行中的传输。
    pub fn is_empty(&self) -> bool {
        self.session_by_transfer.is_empty()
    }
}

/// 把更新应用到工作区；返回工作区是否真的变化（会话已不在工作区时为 false）。
/// 传输计数按 `tracker` 反查会话。
pub fn apply_update(
    update: &SessionUpdate,
    workspace: &mut Workspace,
    transfers: &mut TransferTracker,
) -> bool {
    match update {
        SessionUpdate::Connection(id, connection) => {
            workspace.apply_connection(*id, connection.clone())
        }
        SessionUpdate::Location {
            session,
            cwd,
            cwd_host,
            git_branch,
        } => workspace.apply_location(*session, cwd.clone(), cwd_host.clone(), git_branch.clone()),
        SessionUpdate::Activity(id, activity) => workspace.apply_activity(*id, *activity),
        SessionUpdate::TransferStarted { transfer, session } => {
            // 会话已不在工作区（刚关闭）时不记映射，否则结束事件永远不会来清理。
            if workspace.item(*session).is_none() {
                return false;
            }
            transfers.start(*transfer, *session);
            workspace.apply_transfer(*session, true)
        }
        SessionUpdate::TransferFinished { transfer } => match transfers.finish(*transfer) {
            Some(session) => workspace.apply_transfer(session, false),
            None => false,
        },
        SessionUpdate::Ignored => false,
    }
}

/// 终端变脏的会话集合。
///
/// 核心的唤醒合并约定：终端从干净变脏时只发一次 `TerminalDirty`，UI 拉取帧之后才会再次触发。
/// 所以这里用集合去重：同一帧内到达多个事件也只取一次；UI 每帧调用 [`take`](Self::take)，
/// 对返回的每个会话调用一次 `take_frame`。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirtySet {
    sessions: BTreeSet<SessionId>,
}

impl DirtySet {
    /// 标记。
    pub fn mark(&mut self, session: SessionId) {
        self.sessions.insert(session);
    }

    /// 会话关闭时丢弃，避免对已不存在的会话取帧。
    pub fn forget(&mut self, session: SessionId) {
        self.sessions.remove(&session);
    }

    /// 取走全部脏会话（按会话 id 升序），集合清空。
    pub fn take(&mut self) -> Vec<SessionId> {
        std::mem::take(&mut self.sessions).into_iter().collect()
    }

    /// 是否有脏会话。
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tab::{CloseNote, TabItem, TabKind};
    use kanso_types::agent::AgentKind;
    use kanso_types::event::{Activity, CloseReason, SessionState, TransferDirection};
    use kanso_types::message::Message;
    use kanso_types::transfer::XferProtocol;

    fn sid(n: u64) -> SessionId {
        SessionId::from_raw(n)
    }

    fn tid(n: u64) -> TransferId {
        TransferId::from_raw(n)
    }

    #[test]
    fn maps_state_changes_including_exit_codes() {
        let ev = CoreEvent::SessionStateChanged {
            session: sid(1),
            state: SessionState::Closed(CloseReason::Exited { code: Some(130) }),
        };
        assert_eq!(
            SessionUpdate::from_event(&ev),
            SessionUpdate::Connection(
                sid(1),
                TabConnection::Closed(Some(CloseNote::Exited { code: Some(130) }))
            )
        );
        let ev = CoreEvent::SessionStateChanged {
            session: sid(1),
            state: SessionState::Closed(CloseReason::ByUser),
        };
        assert_eq!(
            SessionUpdate::from_event(&ev),
            SessionUpdate::Connection(sid(1), TabConnection::Closed(None))
        );
    }

    #[test]
    fn maps_location_and_activity() {
        let ev = CoreEvent::LocationChanged {
            session: sid(2),
            cwd: Some("/a".into()),
            cwd_host: None,
            git_branch: Some("main".into()),
        };
        assert_eq!(
            SessionUpdate::from_event(&ev),
            SessionUpdate::Location {
                session: sid(2),
                cwd: Some("/a".into()),
                cwd_host: None,
                git_branch: Some("main".into())
            }
        );
        let ev = CoreEvent::ActivityChanged {
            session: sid(2),
            activity: Activity::AgentWorking(AgentKind::Claude),
        };
        assert_eq!(
            SessionUpdate::from_event(&ev),
            SessionUpdate::Activity(sid(2), TabActivity::AgentWorking(AgentKind::Claude))
        );
    }

    #[test]
    fn unrelated_events_are_ignored() {
        for ev in [
            CoreEvent::TerminalDirty { session: sid(1) },
            CoreEvent::PromptDismissed {
                prompt: kanso_types::PromptId::from_raw(1),
            },
            CoreEvent::TransferProgress {
                transfer: tid(1),
                done_bytes: 1,
                total_bytes: None,
            },
            CoreEvent::Notice {
                session: None,
                level: kanso_types::event::NoticeLevel::Info,
                message: Message::SessionNotTerminal {},
            },
        ] {
            assert_eq!(SessionUpdate::from_event(&ev), SessionUpdate::Ignored);
        }
    }

    fn started(transfer: u64, session: u64) -> CoreEvent {
        CoreEvent::TransferStarted {
            transfer: tid(transfer),
            session: sid(session),
            protocol: XferProtocol::Sftp,
            direction: TransferDirection::Upload,
            name: "f".into(),
            total_bytes: None,
        }
    }

    fn workspace_with(id: u64) -> Workspace {
        let mut ws = Workspace::new();
        ws.add_tab(TabItem::new(sid(id), TabKind::Ssh, "s"), None, true);
        ws
    }

    #[test]
    fn transfer_counts_follow_start_and_finish() {
        let mut ws = workspace_with(1);
        let mut tracker = TransferTracker::default();
        let up = SessionUpdate::from_event(&started(10, 1));
        assert!(apply_update(&up, &mut ws, &mut tracker));
        assert!(apply_update(
            &SessionUpdate::from_event(&started(11, 1)),
            &mut ws,
            &mut tracker
        ));
        assert_eq!(ws.item(sid(1)).unwrap().active_transfers, 2);
        assert_eq!(tracker.len(), 2);

        let fin = SessionUpdate::TransferFinished { transfer: tid(10) };
        assert!(apply_update(&fin, &mut ws, &mut tracker));
        assert_eq!(ws.item(sid(1)).unwrap().active_transfers, 1);
        // 重复的结束事件不会把计数减成负数，也不影响另一个传输。
        assert!(!apply_update(&fin, &mut ws, &mut tracker));
        assert_eq!(ws.item(sid(1)).unwrap().active_transfers, 1);
    }

    #[test]
    fn transfer_for_unknown_session_is_not_tracked() {
        let mut ws = workspace_with(1);
        let mut tracker = TransferTracker::default();
        let up = SessionUpdate::from_event(&started(10, 99));
        assert!(!apply_update(&up, &mut ws, &mut tracker));
        assert!(tracker.is_empty());
    }

    #[test]
    fn tracker_forgets_closed_session() {
        let mut tracker = TransferTracker::default();
        tracker.start(tid(1), sid(1));
        tracker.start(tid(2), sid(2));
        tracker.forget(sid(1));
        assert_eq!(tracker.finish(tid(1)), None);
        assert_eq!(tracker.finish(tid(2)), Some(sid(2)));
    }

    #[test]
    fn updates_for_closed_sessions_are_quietly_ignored() {
        let mut ws = workspace_with(1);
        let mut tracker = TransferTracker::default();
        let up = SessionUpdate::Connection(sid(7), TabConnection::Connected);
        assert!(!apply_update(&up, &mut ws, &mut tracker));
        assert!(!apply_update(
            &SessionUpdate::Ignored,
            &mut ws,
            &mut tracker
        ));
    }

    #[test]
    fn dirty_set_dedups_and_is_taken_once_per_frame() {
        let mut d = DirtySet::default();
        assert!(d.is_empty());
        d.mark(sid(3));
        d.mark(sid(1));
        d.mark(sid(3));
        d.forget(sid(9));
        assert_eq!(d.take(), vec![sid(1), sid(3)]);
        assert!(d.is_empty());
        assert_eq!(d.take(), vec![]);
        d.mark(sid(2));
        d.forget(sid(2));
        assert!(d.is_empty());
    }
}
