//! 应用控制器：串起核心、事件分发、工作区与提示队列。对照 `Shell/WorkspaceController.swift`。
//!
//! 事件流：核心事件（任意线程）→ 通道 → UI 线程调用 [`AppController::handle_event`] →
//! 更新 [`Workspace`] / 提示队列 / 脏会话集合 → 返回 [`Effects`] 告诉视图哪些部分要重绘 →
//! 每帧 [`AppController::take_dirty`] 一次，对返回的会话各取一次终端帧。
//! 反向：UI 动作 → 本类方法 → 核心调用 + 工作区变更。
//!
//! 线程：全部方法只在 UI 线程调用（模型不加锁）。核心的回调线程只负责把事件塞进通道。
//!
//! 可见性：每个 pane 的活动标签算"可见"，切换时自动 `set_visible` 差量同步。改工作区请用
//! [`AppController::edit`]（或本类的专用方法），它在修改后同步可见性。

use std::collections::{BTreeSet, HashMap, VecDeque};

use kanso_core::OpenTarget;
use kanso_types::event::{LegacyHop, NoticeLevel};
use kanso_types::message::Message;
use kanso_types::{CoreEvent, GridSize, ProfileId, SessionId};

use super::backend::{AppBackend, CoreResult};
use super::events::{DirtySet, SessionUpdate, TransferTracker, apply_update};
use super::layout::{PaneId, SplitAxis};
use super::prompts::PromptQueue;
use super::tab::{TabConnection, TabItem, TabKind, TabPresentation};
use super::workspace::{CloseConfirmation, CloseScope, Workspace};

/// 拿不到目标 pane 像素尺寸（还没布局）时的兜底网格，与 mac 版一致。
pub const FALLBACK_GRID: GridSize = GridSize {
    cols: 100,
    rows: 30,
};

/// 通知队列上限：UI 来不及取时丢最旧的。
const MAX_PENDING_NOTICES: usize = 64;

/// 处理一个事件后，视图需要刷新的部分。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Effects {
    /// 有会话终端变脏：本帧调用 [`AppController::take_dirty`]。
    pub terminal: bool,
    /// 标签栏 / 状态栏 / 窗口标题等外框变化。
    pub chrome: bool,
    /// 提示队列变化：当前提示可能换了，或需要关闭对话框。
    pub prompt: bool,
    /// 有新通知：调用 [`AppController::take_notices`]。
    pub notice: bool,
}

impl Effects {
    /// 合并（按位或），一帧内处理多个事件时累加。
    pub fn merge(&mut self, other: Effects) {
        self.terminal |= other.terminal;
        self.chrome |= other.chrome;
        self.prompt |= other.prompt;
        self.notice |= other.notice;
    }

    /// 是否什么都不用刷新。
    pub fn is_none(&self) -> bool {
        *self == Effects::default()
    }
}

/// 一条用户可见通知。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// 关联会话；全局通知为 `None`。
    pub session: Option<SessionId>,
    /// 级别。
    pub level: NoticeLevel,
    /// 内容（视图用 `kanso-l10n` 渲染）。
    pub message: Message,
}

/// Legacy SSH 标记悬停文字的一行（ssh.md §2.2）：一台服务器（含跳板机）及它用的旧算法。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyLine {
    /// `host:port`。
    pub server: String,
    /// 旧算法名（英文技术名，不本地化）。
    pub algorithms: Vec<String>,
}

/// 应用控制器。`B` 是核心的后端（生产环境 `Arc<Core>`）。
pub struct AppController<B: AppBackend + Clone> {
    backend: B,
    workspace: Workspace,
    prompts: PromptQueue<B>,
    transfers: TransferTracker,
    dirty: DirtySet,
    /// 已通知核心为可见的会话。只靠它做差量，避免每次刷新都重复调用核心。
    visible: BTreeSet<SessionId>,
    legacy: HashMap<SessionId, Vec<LegacyHop>>,
    notices: VecDeque<Notice>,
    /// 已结束且应自动关掉标签的会话（见 [`TabConnection::closes_tab_automatically`]），UI 每次处理完事件后取走。
    auto_close: Vec<SessionId>,
    home_dir: String,
    shut_down: bool,
}

impl<B: AppBackend + Clone> AppController<B> {
    /// 创建。`home_dir` 是本机用户目录（标签标题把它显示成 `~`）。
    pub fn new(backend: B, home_dir: impl Into<String>) -> Self {
        AppController {
            prompts: PromptQueue::new(backend.clone()),
            backend,
            workspace: Workspace::new(),
            transfers: TransferTracker::default(),
            dirty: DirtySet::default(),
            visible: BTreeSet::new(),
            legacy: HashMap::new(),
            notices: VecDeque::new(),
            auto_close: Vec::new(),
            home_dir: home_dir.into(),
            shut_down: false,
        }
    }

    // ---- 访问 ----

    /// 工作区（只读）。
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// 只调整同一分屏内的显示顺序，当前会话与终端可见性保持一致。
    pub fn reorder_tab(&mut self, session: SessionId, index: usize) -> bool {
        self.workspace.move_tab(session, index)
    }

    /// 提示队列。视图用它读当前提示、写入输入、提交 / 取消。
    pub fn prompts(&self) -> &PromptQueue<B> {
        &self.prompts
    }

    /// 提示队列（可变）。
    pub fn prompts_mut(&mut self) -> &mut PromptQueue<B> {
        &mut self.prompts
    }

    /// 本机用户目录。
    pub fn home_dir(&self) -> &str {
        &self.home_dir
    }

    /// 标签主标题（路径里的用户目录显示成 `~`）。
    pub fn tab_title(&self, session: SessionId) -> Option<String> {
        self.workspace
            .item(session)
            .map(|item| TabPresentation::primary_title(item, &self.home_dir))
    }

    // ---- 事件 ----

    /// 处理一个核心事件。
    pub fn handle_event(&mut self, event: CoreEvent) -> Effects {
        let mut fx = Effects::default();
        match &event {
            CoreEvent::TerminalDirty { session } => {
                // 已关闭会话的迟到事件不记录：对不存在的会话取帧没有意义。
                if self.workspace.item(*session).is_some() {
                    self.dirty.mark(*session);
                    fx.terminal = true;
                }
                return fx;
            }
            CoreEvent::Prompt {
                prompt,
                session,
                request,
            } => {
                let title = session.and_then(|s| self.tab_title(s));
                self.prompts
                    .enqueue(*prompt, *session, request.clone(), title);
                fx.prompt = true;
                return fx;
            }
            CoreEvent::PromptDismissed { prompt } => {
                self.prompts.dismiss(*prompt);
                fx.prompt = true;
                return fx;
            }
            CoreEvent::LegacyAlgorithms { session, hops } => {
                // 已关闭会话的迟到事件不记录，否则映射只增不减。空 hops 表示清除。
                if self.workspace.item(*session).is_some() {
                    if hops.is_empty() {
                        self.legacy.remove(session);
                    } else {
                        self.legacy.insert(*session, hops.clone());
                    }
                    fx.chrome = true;
                }
                return fx;
            }
            CoreEvent::Notice {
                session,
                level,
                message,
            } => {
                if self.notices.len() >= MAX_PENDING_NOTICES {
                    self.notices.pop_front();
                }
                self.notices.push_back(Notice {
                    session: *session,
                    level: *level,
                    message: message.clone(),
                });
                fx.notice = true;
                return fx;
            }
            _ => {}
        }
        let update = SessionUpdate::from_event(&event);
        if apply_update(&update, &mut self.workspace, &mut self.transfers) {
            fx.chrome = true;
            if let SessionUpdate::Connection(id, connection) = &update
                && connection.closes_tab_automatically()
                && !self.auto_close.contains(id)
            {
                self.auto_close.push(*id);
            }
            self.sync_visibility();
        }
        fx
    }

    /// 取走“会话已正常结束、应自动关闭标签”的会话（shell 输入 `exit` 等）。
    /// 视图对返回的会话调用 [`close_sessions`](Self::close_sessions)；已被用户关掉的会话不会出现在这里。
    pub fn take_auto_close(&mut self) -> Vec<SessionId> {
        std::mem::take(&mut self.auto_close)
    }

    /// 每帧取一次：自上次以来变脏的会话（升序、去重）。
    pub fn take_dirty(&mut self) -> Vec<SessionId> {
        self.dirty.take()
    }

    /// 取走待显示的通知。
    pub fn take_notices(&mut self) -> Vec<Notice> {
        self.notices.drain(..).collect()
    }

    /// 会话最近一次上报的旧算法跳；没有为空。
    pub fn legacy_hops(&self, session: SessionId) -> &[LegacyHop] {
        self.legacy.get(&session).map_or(&[], Vec::as_slice)
    }

    /// 状态栏 Legacy SSH 标记的悬停内容：每台服务器一行；没有任何旧算法返回 `None`（标记隐藏）。
    /// 没有旧算法的跳被略过。
    pub fn legacy_summary(&self, session: SessionId) -> Option<Vec<LegacyLine>> {
        let lines: Vec<LegacyLine> = self
            .legacy_hops(session)
            .iter()
            .filter(|h| !h.algorithms.is_empty())
            .map(|h| LegacyLine {
                server: h.endpoint.host_port(),
                algorithms: h.algorithms.clone(),
            })
            .collect();
        (!lines.is_empty()).then_some(lines)
    }

    /// 活动会话的 Legacy 悬停内容。
    pub fn active_legacy_summary(&self) -> Option<Vec<LegacyLine>> {
        self.legacy_summary(self.workspace.active_session()?)
    }

    // ---- 打开会话 ----

    /// 打开默认本地 shell。`size` 由视图按目标 pane 的像素尺寸估算（拿不到用 [`FALLBACK_GRID`]）。
    pub fn open_local_shell(
        &mut self,
        pane: Option<PaneId>,
        size: GridSize,
    ) -> CoreResult<SessionId> {
        self.open_shell(OpenTarget::DefaultLocalShell, pane, size)
    }

    /// 打开本地 Shell 并登记到目标面板。
    pub fn open_shell(
        &mut self,
        target: OpenTarget,
        pane: Option<PaneId>,
        size: GridSize,
    ) -> CoreResult<SessionId> {
        let id = self.backend.open_session(target, size)?;
        self.adopt(id, pane, None, true);
        Ok(id)
    }

    /// 按已保存的会话打开。核心对不存在的会话返回错误，由调用方展示。
    pub fn open_profile(
        &mut self,
        profile: ProfileId,
        pane: Option<PaneId>,
        size: GridSize,
    ) -> CoreResult<SessionId> {
        let id = self
            .backend
            .open_session(OpenTarget::Profile(profile), size)?;
        self.adopt(id, pane, None, true);
        Ok(id)
    }

    /// 用已断开标签的原已保存会话建立一条全新会话，并排到旧标签的原位置。
    /// core 的会话 id 是一次性的，无法复活旧会话，所以这里必然是新 id。
    /// 返回新会话 id；原标签不是已保存会话或没有已保存配置时返回 None，不新建。
    /// 旧标签由调用方用完整的关闭流程收尾（清终端、文件面板与 MCP 引用）。
    pub fn reconnect_session(
        &mut self,
        old: SessionId,
        size: GridSize,
    ) -> CoreResult<Option<SessionId>> {
        let Some(item) = self.workspace.item(old).cloned() else {
            return Ok(None);
        };
        if !matches!(item.connection, TabConnection::Closed(_)) {
            return Ok(None);
        }
        let Some(profile) = item.profile else {
            return Ok(None);
        };
        let pane = self.workspace.pane_of(old);
        let index = pane
            .and_then(|p| self.workspace.tabs_in(p).iter().position(|i| i.id == old))
            .unwrap_or(0);
        let new = self.open_profile(profile, pane, size)?;
        self.workspace.move_tab(new, index);
        Ok(Some(new))
    }

    /// 复制活动标签（SSH 复用已认证的连接）；没有活动标签时退化为新本地 shell。
    pub fn duplicate_active_tab(&mut self, size: GridSize) -> CoreResult<SessionId> {
        let Some(active) = self.workspace.active_session() else {
            return self.open_local_shell(None, size);
        };
        let id = self
            .backend
            .open_session(OpenTarget::Duplicate(active), size)?;
        self.adopt(id, None, None, true);
        Ok(id)
    }

    /// 拆分活动 pane：新开会话（有活动会话时复制它，否则默认本地 shell）放进新 pane。
    /// `size` 应是拆分之后新 pane 的估算尺寸。
    pub fn split(&mut self, axis: SplitAxis, size: GridSize) -> CoreResult<SessionId> {
        let target = self
            .workspace
            .active_session()
            .map_or(OpenTarget::DefaultLocalShell, OpenTarget::Duplicate);
        let id = self.backend.open_session(target, size)?;
        self.adopt(id, None, Some(axis), true);
        Ok(id)
    }

    /// 把核心已打开的会话登记进工作区。
    ///
    /// 标签的初始连接状态取自 `session_info`：核心在 `open_session` 里已经发过 `Connecting`
    /// （甚至 `Connected`）事件，而它们要到下一次处理事件时才到达，不能依赖它们来初始化。
    fn adopt(
        &mut self,
        id: SessionId,
        pane: Option<PaneId>,
        split: Option<SplitAxis>,
        activate: bool,
    ) -> bool {
        let info = self.backend.session_info(id);
        if !activate
            && (self.workspace.item(id).is_some()
                || info
                    .as_ref()
                    .is_none_or(|i| matches!(i.state, kanso_types::event::SessionState::Closed(_))))
        {
            return false;
        }
        let mut item = TabItem::new(
            id,
            info.as_ref().map_or(TabKind::Local, |i| i.kind.into()),
            info.as_ref().map_or("", |i| i.name.as_str()),
        );
        if let Some(info) = &info {
            item.custom_title = info.custom_title.clone();
            item.connection = TabConnection::from(&info.state);
            item.profile = info.profile;
            item.color = info
                .profile
                .and_then(|p| self.backend.profile(p))
                .and_then(|p| p.color);
        }
        match split {
            Some(axis) => {
                self.workspace.split_active_pane(axis, item);
            }
            None => self.workspace.add_tab(item, pane, activate),
        }
        self.sync_visibility();
        true
    }

    /// 接管 MCP 已打开的标签，保持前台会话与输入焦点。
    pub fn adopt_mcp_session(&mut self, id: SessionId) -> bool {
        self.adopt(id, None, None, false)
    }

    /// 设置或清除标签的自定义名，并同步通知核心（MCP 等处看到同一标题）。
    pub fn rename_session(&mut self, session: SessionId, title: Option<String>) -> bool {
        let normalized = title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        self.backend.set_session_title(session, normalized.clone());
        self.workspace.set_custom_title(session, normalized)
    }

    // ---- 关闭 ----

    /// 关闭前是否需要确认（视图据此弹确认框，同意后再调 [`close_sessions`](Self::close_sessions)）。
    pub fn close_confirmation(&self, sessions: &[SessionId]) -> Option<CloseConfirmation> {
        self.workspace.close_confirmation(sessions)
    }

    /// 批量关闭的目标（只在 `session` 所在的分屏组内）。
    pub fn close_targets(&self, scope: CloseScope, relative_to: SessionId) -> Vec<SessionId> {
        self.workspace.close_targets(scope, relative_to)
    }

    /// 立即关闭会话（确认已在视图完成）。返回实际关闭的会话。
    ///
    /// 选择"立即移除"而不是等 `Closed(ByUser)` 事件：`close_session` 同步生效，立即移除标签，
    /// 之后到达的该会话事件在工作区里找不到标签，会被安静忽略，不会重复移除。
    pub fn close_sessions(&mut self, sessions: &[SessionId]) -> Vec<SessionId> {
        let mut closed = Vec::new();
        for id in sessions {
            if self.workspace.item(*id).is_none() {
                continue;
            }
            self.backend.close_session(*id);
            self.workspace.remove_tab(*id);
            self.transfers.forget(*id);
            self.legacy.remove(id);
            self.dirty.forget(*id);
            self.visible.remove(id);
            self.auto_close.retain(|x| x != id);
            closed.push(*id);
        }
        if !closed.is_empty() {
            self.sync_visibility();
        }
        closed
    }

    /// 应用退出前调用：取消全部提示，关闭所有会话。可重复调用。核心本身的 `shutdown` 由宿主调用。
    pub fn shutdown(&mut self) {
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        self.prompts.cancel_all();
        let all = self.workspace.sidebar_order();
        for id in all {
            self.backend.close_session(id);
        }
    }

    // ---- 工作区编辑 ----

    /// 修改工作区（激活、移动、拖分隔条……），修改后同步可见性。
    pub fn edit<R>(&mut self, f: impl FnOnce(&mut Workspace) -> R) -> R {
        let out = f(&mut self.workspace);
        self.sync_visibility();
        out
    }

    /// 同步可见性：只有每个 pane 的活动标签算可见。幂等，可随时调用。
    pub fn sync_visibility(&mut self) {
        let now: BTreeSet<SessionId> = self.workspace.visible_sessions().into_iter().collect();
        for id in self.visible.difference(&now) {
            if self.workspace.item(*id).is_some() {
                self.backend.set_visible(*id, false);
            }
        }
        for id in now.difference(&self.visible) {
            self.backend.set_visible(*id, true);
        }
        self.visible = now;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use kanso_core::{Core, CoreError, SessionInfo};
    use kanso_types::agent::AgentKind;
    use kanso_types::event::{Activity, CloseReason, SessionState};
    use kanso_types::geometry::Rgb;
    use kanso_types::message::Endpoint;
    use kanso_types::profile::{Profile, SessionKind};
    use kanso_types::prompt::{KbdQuestion, PromptRequest, PromptResponse};
    use kanso_types::{PromptId, TransferId};

    use super::*;
    use crate::app::backend::{PromptResponder, SessionBackend};
    use crate::app::prompts::PromptDialog;
    use crate::app::tab::{CloseNote, TabActivity};

    const GRID: GridSize = GridSize { cols: 80, rows: 24 };

    fn sid(n: u64) -> SessionId {
        SessionId::from_raw(n)
    }

    /// 记录调用的核心替身。
    #[derive(Default)]
    struct FakeInner {
        next: u64,
        sessions: HashMap<SessionId, SessionInfo>,
        opened: Vec<OpenTarget>,
        visible_calls: Vec<(SessionId, bool)>,
        closed: Vec<SessionId>,
        responses: Vec<(PromptId, PromptResponse)>,
        profiles: Vec<Profile>,
        fail_open: bool,
    }

    #[derive(Clone, Default)]
    struct Fake(Rc<RefCell<FakeInner>>);

    impl SessionBackend for Fake {
        fn open_session(&self, target: OpenTarget, _size: GridSize) -> CoreResult<SessionId> {
            let mut s = self.0.borrow_mut();
            if s.fail_open {
                return Err(CoreError::NotSupported);
            }
            s.opened.push(target.clone());
            s.next += 1;
            let id = sid(s.next);
            let (profile, kind, name) = match target {
                OpenTarget::DefaultLocalShell | OpenTarget::LocalShell(_) => {
                    (None, SessionKind::Local, "Local".to_owned())
                }
                OpenTarget::Profile(p) => {
                    let prof = s
                        .profiles
                        .iter()
                        .find(|x| x.id == p)
                        .ok_or(CoreError::NoSuchProfile(p))?;
                    (Some(p), prof.kind.kind(), prof.name.clone())
                }
                OpenTarget::Duplicate(src) => {
                    let i = s.sessions.get(&src).ok_or(CoreError::NoSuchSession(src))?;
                    (i.profile, i.kind, i.name.clone())
                }
            };
            s.sessions.insert(
                id,
                SessionInfo {
                    id,
                    profile,
                    kind,
                    name,
                    custom_title: None,
                    state: SessionState::Connecting,
                },
            );
            Ok(id)
        }

        fn close_session(&self, session: SessionId) {
            let mut s = self.0.borrow_mut();
            s.closed.push(session);
            s.sessions.remove(&session);
        }

        fn session_info(&self, session: SessionId) -> Option<SessionInfo> {
            self.0.borrow().sessions.get(&session).cloned()
        }

        fn set_session_title(&self, session: SessionId, title: Option<String>) {
            if let Some(info) = self.0.borrow_mut().sessions.get_mut(&session) {
                info.custom_title = title;
            }
        }

        fn set_visible(&self, session: SessionId, visible: bool) {
            self.0.borrow_mut().visible_calls.push((session, visible));
        }

        fn profile(&self, id: ProfileId) -> Option<Profile> {
            self.0
                .borrow()
                .profiles
                .iter()
                .find(|p| p.id == id)
                .cloned()
        }
    }

    impl PromptResponder for Fake {
        fn respond_prompt(&self, prompt: PromptId, response: PromptResponse) {
            self.0.borrow_mut().responses.push((prompt, response));
        }
    }

    fn controller() -> (AppController<Fake>, Fake) {
        let fake = Fake::default();
        (AppController::new(fake.clone(), "/Users/me"), fake)
    }

    fn state(session: u64, state: SessionState) -> CoreEvent {
        CoreEvent::SessionStateChanged {
            session: sid(session),
            state,
        }
    }

    #[test]
    fn open_local_shell_adds_active_tab_and_marks_visible() {
        let (mut c, fake) = controller();
        let id = c.open_local_shell(None, GRID).unwrap();
        assert_eq!(c.workspace().active_session(), Some(id));
        let item = c.workspace().item(id).unwrap();
        assert_eq!(
            (item.kind, item.name.as_str(), item.profile),
            (TabKind::Local, "Local", None)
        );
        assert_eq!(
            item.connection,
            TabConnection::Connecting,
            "初始状态取自 session_info"
        );
        assert_eq!(fake.0.borrow().visible_calls, vec![(id, true)]);
        assert_eq!(fake.0.borrow().opened, vec![OpenTarget::DefaultLocalShell]);
    }

    #[test]
    fn clean_exit_queues_automatic_close_but_failure_keeps_the_tab() {
        let (mut c, _fake) = controller();
        let a = c.open_local_shell(None, GRID).unwrap();
        let b = c.open_local_shell(None, GRID).unwrap();
        let d = c.open_local_shell(None, GRID).unwrap();
        c.handle_event(state(
            a.raw(),
            SessionState::Closed(CloseReason::Exited { code: Some(0) }),
        ));
        c.handle_event(state(
            b.raw(),
            SessionState::Closed(CloseReason::Exited { code: Some(2) }),
        ));
        c.handle_event(state(d.raw(), SessionState::Closed(CloseReason::ByUser)));
        let queued = c.take_auto_close();
        assert_eq!(queued, vec![a, d]);
        assert!(c.take_auto_close().is_empty(), "取走后清空");
        // 非零退出的标签还在，并带着退出码说明。
        assert_eq!(
            c.workspace().item(b).unwrap().connection,
            TabConnection::Closed(Some(CloseNote::Exited { code: Some(2) }))
        );
        let closed = c.close_sessions(&queued);
        assert_eq!(closed, queued);
        assert_eq!(c.workspace().tab_count(), 1);
    }

    #[test]
    fn closing_by_user_first_drops_the_pending_automatic_close() {
        let (mut c, _fake) = controller();
        let a = c.open_local_shell(None, GRID).unwrap();
        c.handle_event(state(
            a.raw(),
            SessionState::Closed(CloseReason::Exited { code: Some(0) }),
        ));
        c.close_sessions(&[a]);
        assert!(c.take_auto_close().is_empty());
        // 迟到的事件：标签已不在，不再入队。
        c.handle_event(state(a.raw(), SessionState::Closed(CloseReason::ByUser)));
        assert!(c.take_auto_close().is_empty());
    }

    #[test]
    fn open_failure_leaves_workspace_untouched() {
        let (mut c, fake) = controller();
        fake.0.borrow_mut().fail_open = true;
        assert_eq!(c.open_local_shell(None, GRID), Err(CoreError::NotSupported));
        assert!(c.workspace().is_empty());
        assert!(fake.0.borrow().visible_calls.is_empty());
    }

    #[test]
    fn open_profile_copies_profile_and_color() {
        let (mut c, fake) = controller();
        let mut p = crate::app::testutil::ssh_profile("prod", "10.0.0.1", "root");
        p.color = Some(Rgb { r: 1, g: 2, b: 3 });
        let pid = p.id;
        fake.0.borrow_mut().profiles.push(p);
        let id = c.open_profile(pid, None, GRID).unwrap();
        let item = c.workspace().item(id).unwrap();
        assert_eq!(item.kind, TabKind::Ssh);
        assert_eq!(item.profile, Some(pid));
        assert_eq!(item.color, Some(Rgb { r: 1, g: 2, b: 3 }));
        assert!(c.open_profile(ProfileId::new(), None, GRID).is_err());
        assert_eq!(c.workspace().tab_count(), 1);
    }

    #[test]
    fn reconnect_uses_profile_replaces_old_tab_and_keeps_position() {
        let (mut c, fake) = controller();
        let p = crate::app::testutil::ssh_profile("prod", "10.0.0.1", "root");
        let pid = p.id;
        fake.0.borrow_mut().profiles.push(p);
        // 两个标签，断开的是第一个：重连后新标签应留在第一位。
        let first = c.open_profile(pid, None, GRID).unwrap();
        let second = c.open_local_shell(None, GRID).unwrap();
        c.handle_event(state(
            first.raw(),
            SessionState::Closed(CloseReason::Exited { code: Some(1) }),
        ));
        assert!(matches!(
            c.workspace().item(first).unwrap().connection,
            TabConnection::Closed(_)
        ));

        let new = c.reconnect_session(first, GRID).unwrap().unwrap();
        assert_ne!(new, first, "core 的会话 id 一次性，重连必须是新会话");
        assert_eq!(c.workspace().item(new).unwrap().profile, Some(pid));
        assert_eq!(
            c.workspace().sidebar_order(),
            vec![new, first, second],
            "新标签排到旧标签原位置；旧标签由 UI 层清理"
        );
        assert_eq!(
            fake.0.borrow().opened.last(),
            Some(&OpenTarget::Profile(pid)),
            "用原 profile 全新建连，不复用旧会话"
        );
    }

    #[test]
    fn reconnect_ignores_connected_and_profileless_tabs() {
        let (mut c, _fake) = controller();
        let local = c.open_local_shell(None, GRID).unwrap();
        // 仍连接的标签不重连。
        assert_eq!(c.reconnect_session(local, GRID).unwrap(), None);
        // 断开但没有 profile 的临时本地 shell 也不重连。
        c.handle_event(state(
            local.raw(),
            SessionState::Closed(CloseReason::Exited { code: Some(1) }),
        ));
        assert_eq!(c.reconnect_session(local, GRID).unwrap(), None);
        assert!(c.workspace().item(local).is_some());
    }

    #[test]
    fn duplicate_uses_active_session_or_falls_back_to_local() {
        let (mut c, fake) = controller();
        let first = c.duplicate_active_tab(GRID).unwrap();
        assert_eq!(fake.0.borrow().opened, vec![OpenTarget::DefaultLocalShell]);
        let second = c.duplicate_active_tab(GRID).unwrap();
        assert_eq!(fake.0.borrow().opened[1], OpenTarget::Duplicate(first));
        assert_eq!(c.workspace().sidebar_order(), vec![first, second]);
        assert_eq!(c.workspace().all_panes().len(), 1, "复制进同一个 pane");
    }

    #[test]
    fn split_opens_duplicate_in_new_pane_and_syncs_visibility() {
        let (mut c, fake) = controller();
        let first = c.split(SplitAxis::Horizontal, GRID).unwrap();
        assert_eq!(c.workspace().all_panes().len(), 1, "空工作区不拆分");
        let second = c.split(SplitAxis::Horizontal, GRID).unwrap();
        assert_eq!(fake.0.borrow().opened[1], OpenTarget::Duplicate(first));
        assert_eq!(c.workspace().all_panes().len(), 2);
        assert_eq!(c.workspace().active_session(), Some(second));
        // 两个 pane 的活动标签都可见，第一个始终没有被设为不可见。
        let calls = fake.0.borrow().visible_calls.clone();
        assert!(calls.contains(&(first, true)) && calls.contains(&(second, true)));
        assert!(!calls.contains(&(first, false)));
    }

    #[test]
    fn switching_tabs_in_a_pane_flips_visibility_once() {
        let (mut c, fake) = controller();
        let a = c.open_local_shell(None, GRID).unwrap();
        let b = c.open_local_shell(None, GRID).unwrap();
        fake.0.borrow_mut().visible_calls.clear();
        c.edit(|w| w.activate(a));
        assert_eq!(fake.0.borrow().visible_calls, vec![(b, false), (a, true)]);
        fake.0.borrow_mut().visible_calls.clear();
        c.sync_visibility();
        c.edit(|w| w.activate(a));
        assert!(
            fake.0.borrow().visible_calls.is_empty(),
            "幂等：无变化不调用核心"
        );
    }

    #[test]
    fn events_update_tab_state() {
        let (mut c, _) = controller();
        let id = c.open_local_shell(None, GRID).unwrap();
        let fx = c.handle_event(state(id.raw(), SessionState::Connected));
        assert!(fx.chrome && !fx.terminal);
        assert_eq!(
            c.workspace().item(id).unwrap().connection,
            TabConnection::Connected
        );

        c.handle_event(CoreEvent::LocationChanged {
            session: id,
            cwd: Some("/Users/me/src".into()),
            cwd_host: None,
            git_branch: Some("main".into()),
        });
        assert_eq!(c.tab_title(id).as_deref(), Some("~/src"));

        c.handle_event(CoreEvent::ActivityChanged {
            session: id,
            activity: Activity::AgentWorking(AgentKind::Claude),
        });
        assert_eq!(
            c.workspace().item(id).unwrap().activity,
            TabActivity::AgentWorking(AgentKind::Claude)
        );

        c.handle_event(state(
            id.raw(),
            SessionState::Closed(CloseReason::Exited { code: Some(1) }),
        ));
        assert_eq!(
            c.workspace().item(id).unwrap().connection,
            TabConnection::Closed(Some(CloseNote::Exited { code: Some(1) })),
            "退出码保留在标签上"
        );
    }

    #[test]
    fn mcp_adoption_keeps_the_user_tab_active() {
        let (mut c, backend) = controller();
        let current = c.open_local_shell(None, GRID).unwrap();
        let opened = backend
            .open_session(OpenTarget::DefaultLocalShell, GRID)
            .unwrap();
        assert!(c.adopt_mcp_session(opened));
        assert_eq!(c.workspace().active_session(), Some(current));
        assert!(c.workspace().item(opened).is_some());
        assert!(!c.adopt_mcp_session(opened));
    }

    #[test]
    fn dirty_events_coalesce_and_are_taken_once_per_frame() {
        let (mut c, _) = controller();
        let a = c.open_local_shell(None, GRID).unwrap();
        let b = c.open_local_shell(None, GRID).unwrap();
        let mut fx = Effects::default();
        for s in [b, a, b, a] {
            fx.merge(c.handle_event(CoreEvent::TerminalDirty { session: s }));
        }
        assert!(fx.terminal && !fx.chrome);
        assert_eq!(c.take_dirty(), vec![a, b]);
        assert_eq!(c.take_dirty(), vec![]);
    }

    #[test]
    fn dirty_for_unknown_or_closed_session_is_dropped() {
        let (mut c, _) = controller();
        let a = c.open_local_shell(None, GRID).unwrap();
        assert!(
            c.handle_event(CoreEvent::TerminalDirty { session: sid(99) })
                .is_none()
        );
        c.handle_event(CoreEvent::TerminalDirty { session: a });
        c.close_sessions(&[a]);
        assert_eq!(c.take_dirty(), vec![], "关闭时丢弃脏标记");
        assert!(
            c.handle_event(CoreEvent::TerminalDirty { session: a })
                .is_none()
        );
    }

    #[test]
    fn unread_after_background_command_finishes() {
        let (mut c, _) = controller();
        let a = c.open_local_shell(None, GRID).unwrap();
        let _b = c.open_local_shell(None, GRID).unwrap();
        c.handle_event(CoreEvent::ActivityChanged {
            session: a,
            activity: Activity::CommandRunning,
        });
        c.handle_event(CoreEvent::ActivityChanged {
            session: a,
            activity: Activity::Idle,
        });
        assert!(c.workspace().item(a).unwrap().unread_done);
        c.edit(|w| w.activate(a));
        assert!(!c.workspace().item(a).unwrap().unread_done);
    }

    #[test]
    fn transfers_count_and_clear_on_close() {
        let (mut c, _) = controller();
        let a = c.open_local_shell(None, GRID).unwrap();
        c.handle_event(CoreEvent::TransferStarted {
            transfer: TransferId::from_raw(5),
            session: a,
            protocol: kanso_types::transfer::XferProtocol::Sftp,
            direction: kanso_types::event::TransferDirection::Download,
            name: "x".into(),
            total_bytes: Some(10),
        });
        assert_eq!(c.workspace().item(a).unwrap().active_transfers, 1);
        assert!(c.close_confirmation(&[a]).unwrap().interrupts_transfers);
        c.handle_event(CoreEvent::TransferFinished {
            transfer: TransferId::from_raw(5),
            outcome: kanso_types::event::TransferOutcome::Cancelled,
        });
        assert_eq!(c.workspace().item(a).unwrap().active_transfers, 0);
        assert!(c.close_confirmation(&[a]).is_none());
    }

    #[test]
    fn close_sessions_removes_tabs_and_ignores_late_events() {
        let (mut c, fake) = controller();
        let a = c.open_local_shell(None, GRID).unwrap();
        let b = c.open_local_shell(None, GRID).unwrap();
        let closed = c.close_sessions(&[a, sid(99), a]);
        assert_eq!(closed, vec![a], "不存在与重复的 id 被忽略");
        assert_eq!(fake.0.borrow().closed, vec![a]);
        assert_eq!(c.workspace().sidebar_order(), vec![b]);
        // 关闭后迟到的事件被安静忽略，也不会复活标签。
        let fx = c.handle_event(state(a.raw(), SessionState::Closed(CloseReason::ByUser)));
        assert!(fx.is_none());
        assert!(c.workspace().item(a).is_none());
    }

    #[test]
    fn closing_the_active_tab_makes_the_neighbor_visible() {
        let (mut c, fake) = controller();
        let a = c.open_local_shell(None, GRID).unwrap();
        let b = c.open_local_shell(None, GRID).unwrap();
        fake.0.borrow_mut().visible_calls.clear();
        c.close_sessions(&[b]);
        assert_eq!(
            fake.0.borrow().visible_calls,
            vec![(a, true)],
            "已关闭的会话不再收到 set_visible(false)"
        );
    }

    #[test]
    fn close_scope_flow_with_confirmation() {
        let (mut c, fake) = controller();
        let p = crate::app::testutil::ssh_profile("s", "h", "u");
        let pid = p.id;
        fake.0.borrow_mut().profiles.push(p);
        let a = c.open_local_shell(None, GRID).unwrap();
        let b = c.open_profile(pid, None, GRID).unwrap();
        let targets = c.close_targets(CloseScope::Others, a);
        assert_eq!(targets, vec![b]);
        let conf = c.close_confirmation(&targets).unwrap();
        assert_eq!(conf.sessions, vec![b], "SSH 连接中需要确认");
        c.close_sessions(&targets);
        assert_eq!(c.workspace().sidebar_order(), vec![a]);
    }

    #[test]
    fn legacy_hops_are_recorded_summarized_and_cleared() {
        let (mut c, _) = controller();
        let a = c.open_local_shell(None, GRID).unwrap();
        let hop = |host: &str, algs: &[&str]| LegacyHop {
            endpoint: Endpoint {
                host: host.into(),
                port: 22,
                via: vec![],
            },
            algorithms: algs.iter().map(|s| (*s).to_owned()).collect(),
        };
        c.handle_event(CoreEvent::LegacyAlgorithms {
            session: a,
            hops: vec![
                hop("jump", &[]),
                hop("old", &["ssh-rsa", "diffie-hellman-group1-sha1"]),
            ],
        });
        assert_eq!(
            c.active_legacy_summary(),
            Some(vec![LegacyLine {
                server: "old:22".into(),
                algorithms: vec!["ssh-rsa".into(), "diffie-hellman-group1-sha1".into()]
            }]),
            "没有旧算法的跳被略过"
        );
        c.handle_event(CoreEvent::LegacyAlgorithms {
            session: a,
            hops: vec![],
        });
        assert_eq!(c.active_legacy_summary(), None);
        // 全是空算法的跳：不显示标记。
        c.handle_event(CoreEvent::LegacyAlgorithms {
            session: a,
            hops: vec![hop("x", &[])],
        });
        assert_eq!(c.legacy_summary(a), None);
        // 未知会话不记录。
        c.handle_event(CoreEvent::LegacyAlgorithms {
            session: sid(99),
            hops: vec![hop("x", &["a"])],
        });
        assert!(c.legacy_hops(sid(99)).is_empty());
    }

    #[test]
    fn notices_are_queued_and_bounded() {
        let (mut c, _) = controller();
        for _ in 0..(MAX_PENDING_NOTICES + 10) {
            let fx = c.handle_event(CoreEvent::Notice {
                session: None,
                level: NoticeLevel::Warning,
                message: Message::SessionNotTerminal {},
            });
            assert!(fx.notice);
        }
        let n = c.take_notices();
        assert_eq!(n.len(), MAX_PENDING_NOTICES);
        assert_eq!(n[0].level, NoticeLevel::Warning);
        assert!(c.take_notices().is_empty());
    }

    fn kbd_prompt_event(prompt: u64, session: Option<SessionId>) -> CoreEvent {
        CoreEvent::Prompt {
            prompt: PromptId::from_raw(prompt),
            session,
            request: PromptRequest::KeyboardInteractive {
                endpoint: Endpoint {
                    host: "h".into(),
                    port: 22,
                    via: vec![],
                },
                user: "u".into(),
                name: String::new(),
                instruction: String::new(),
                preamble: vec![],
                questions: vec![KbdQuestion {
                    text: "code".into(),
                    echo: false,
                }],
            },
        }
    }

    #[test]
    fn prompt_events_route_to_queue_with_tab_title() {
        let (mut c, fake) = controller();
        let a = c.open_local_shell(None, GRID).unwrap();
        c.handle_event(CoreEvent::LocationChanged {
            session: a,
            cwd: Some("/Users/me/work".into()),
            cwd_host: None,
            git_branch: None,
        });
        let fx = c.handle_event(kbd_prompt_event(1, Some(a)));
        assert!(fx.prompt);
        let PromptDialog::KeyboardInteractive(m) = &c.prompts().current().unwrap().dialog else {
            panic!()
        };
        assert_eq!(m.target.session_title.as_deref(), Some("~/work"));

        c.handle_event(kbd_prompt_event(2, None));
        assert_eq!(c.prompts().queued_ids(), vec![PromptId::from_raw(2)]);

        // 核心撤销正在显示的提问：不回答，队列推进。
        c.handle_event(CoreEvent::PromptDismissed {
            prompt: PromptId::from_raw(1),
        });
        assert_eq!(c.prompts().current().unwrap().id, PromptId::from_raw(2));
        assert!(fake.0.borrow().responses.is_empty());

        if let Some(PromptDialog::KeyboardInteractive(m)) =
            c.prompts_mut().current_mut().map(|e| &mut e.dialog)
        {
            m.set_answer(0, "654321");
        }
        assert!(c.prompts_mut().submit());
        assert_eq!(
            fake.0.borrow().responses,
            vec![(
                PromptId::from_raw(2),
                PromptResponse::Answers(vec!["654321".into()])
            )]
        );
    }

    #[test]
    fn shutdown_cancels_prompts_and_closes_sessions_once() {
        let (mut c, fake) = controller();
        let a = c.open_local_shell(None, GRID).unwrap();
        let b = c.open_local_shell(None, GRID).unwrap();
        c.handle_event(kbd_prompt_event(1, Some(a)));
        c.shutdown();
        c.shutdown();
        let inner = fake.0.borrow();
        assert_eq!(inner.closed, vec![a, b]);
        assert_eq!(
            inner.responses,
            vec![(PromptId::from_raw(1), PromptResponse::Cancel)]
        );
    }

    #[test]
    fn effects_merge_and_none() {
        let mut fx = Effects::default();
        assert!(fx.is_none());
        fx.merge(Effects {
            terminal: true,
            ..Effects::default()
        });
        fx.merge(Effects {
            prompt: true,
            ..Effects::default()
        });
        assert!(fx.terminal && fx.prompt && !fx.chrome && !fx.is_none());
    }

    // ---- 与核心 mock 的联调 ----

    mod with_mock_core {
        use std::sync::Arc;

        use kanso_core::{CollectingEventSink, Platform};

        use super::*;

        struct Rig {
            core: Arc<Core>,
            sink: Arc<CollectingEventSink>,
            ctl: AppController<Arc<Core>>,
        }

        impl Rig {
            fn new() -> Self {
                let sink = Arc::new(CollectingEventSink::new());
                let core = Arc::new(Core::new_mock(Platform::for_tests(sink.clone())));
                let ctl = AppController::new(core.clone(), "/home/me");
                Rig { core, sink, ctl }
            }

            /// 把核心已发出的事件全部交给控制器（模拟 UI 线程排空通道）。
            fn pump(&mut self) -> Effects {
                let mut fx = Effects::default();
                for ev in self.sink.take() {
                    fx.merge(self.ctl.handle_event(ev));
                }
                fx
            }
        }

        #[test]
        fn local_session_becomes_connected_through_events() {
            let mut rig = Rig::new();
            let id = rig.ctl.open_local_shell(None, GRID).unwrap();
            rig.pump();
            assert_eq!(
                rig.ctl.workspace().item(id).unwrap().connection,
                TabConnection::Connected
            );
            assert!(
                rig.ctl.workspace().item(id).unwrap().cwd.is_none()
                    || rig.ctl.tab_title(id).is_some()
            );
        }

        #[test]
        fn mfa_bastion_prompt_roundtrip() {
            let mut rig = Rig::new();
            let profile =
                crate::app::testutil::ssh_profile("mfa-bastion", "bastion.example.com", "alice");
            let pid = profile.id;
            rig.core.save_profile(profile).unwrap();
            let id = rig.ctl.open_profile(pid, None, GRID).unwrap();
            let fx = rig.pump();
            assert!(
                fx.prompt,
                "mock 对 mfa-bastion 发出 keyboard-interactive 提问"
            );
            let entry = rig.ctl.prompts().current().unwrap().clone();
            assert_eq!(entry.session, Some(id));
            let PromptDialog::KeyboardInteractive(m) = &entry.dialog else {
                panic!("{entry:?}")
            };
            assert_eq!(m.user, "alice");
            assert_eq!(m.target.host_port, "bastion.example.com:22");
            assert_eq!(m.target.session_title.as_deref(), Some("mfa-bastion"));
            assert_eq!(
                rig.ctl.workspace().item(id).unwrap().connection,
                TabConnection::Connecting
            );

            if let PromptDialog::KeyboardInteractive(m) =
                &mut rig.ctl.prompts_mut().current_mut().unwrap().dialog
            {
                m.set_answer(0, "123456");
            }
            assert!(rig.ctl.prompts_mut().submit());
            rig.pump();
            assert!(rig.ctl.prompts().is_idle());
            assert_eq!(
                rig.ctl.workspace().item(id).unwrap().connection,
                TabConnection::Connected,
                "回答后核心完成连接"
            );
        }

        #[test]
        fn cancelling_prompt_and_closing_session_dismisses_other_prompts() {
            let mut rig = Rig::new();
            let profile =
                crate::app::testutil::ssh_profile("mfa-bastion", "b.example.com", "alice");
            let pid = profile.id;
            rig.core.save_profile(profile).unwrap();
            let id = rig.ctl.open_profile(pid, None, GRID).unwrap();
            rig.pump();
            assert!(rig.ctl.prompts().current().is_some());
            // 关闭会话：核心撤销该会话的提问。
            rig.ctl.close_sessions(&[id]);
            rig.pump();
            assert!(rig.ctl.prompts().is_idle());
            assert!(rig.ctl.workspace().is_empty());
        }

        #[test]
        fn duplicate_via_core_creates_second_tab() {
            let mut rig = Rig::new();
            let a = rig.ctl.open_local_shell(None, GRID).unwrap();
            let b = rig.ctl.duplicate_active_tab(GRID).unwrap();
            rig.pump();
            assert_ne!(a, b);
            assert_eq!(rig.ctl.workspace().tab_count(), 2);
            assert_eq!(rig.core.sessions().len(), 2);
        }
    }
}
