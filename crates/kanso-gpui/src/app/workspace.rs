//! 工作区：分屏布局 + 每个分屏的有序标签 + 活动状态。对照 `Workspace.swift`（ui-shell.md §2、§3、§8）。
//!
//! 纯逻辑，不含视图、不依赖核心。所有操作同步完成，对不存在的 id 安静地不做事。
//! 这里只改位置与展示状态，不关闭会话：关闭要由调用方走确认和核心，成功后再调 [`Workspace::remove_tab`]。

use std::collections::HashMap;

use kanso_types::SessionId;

use super::layout::{PaneId, SplitAxis, SplitLayout};
use super::tab::{TabActivity, TabConnection, TabItem, TabKind, TabPresentation};

/// 侧栏里的一个分屏组。
#[derive(Clone, Debug, PartialEq)]
pub struct SidebarSection {
    /// 分屏。
    pub pane: PaneId,
    /// 标题序号，从 1 开始（"分屏 1"）。
    pub title_index: usize,
    /// 组内标签（按显示顺序）。
    pub tabs: Vec<TabItem>,
}

/// 批量关闭的范围（只在同一个分屏组内生效）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseScope {
    /// 关闭其他。
    Others,
    /// 关闭上方（菜单叫"关闭左侧"）。
    Above,
    /// 关闭下方（菜单叫"关闭右侧"）。
    Below,
    /// 全部关闭。
    All,
}

/// 需要用户确认的关闭请求。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseConfirmation {
    /// 需要列在确认提示里的会话（保持传入顺序）。
    pub sessions: Vec<SessionId>,
    /// 其中有文件传输正在进行，关闭会中断传输。
    pub interrupts_transfers: bool,
}

/// 把标签拖到分屏的哪条边（ui-shell.md §2.4）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneEdge {
    /// 左边：在左侧新建分屏。
    Left,
    /// 右边。
    Right,
    /// 上边。
    Top,
    /// 下边。
    Bottom,
}

impl PaneEdge {
    fn axis_and_before(self) -> (SplitAxis, bool) {
        match self {
            PaneEdge::Left => (SplitAxis::Horizontal, true),
            PaneEdge::Right => (SplitAxis::Horizontal, false),
            PaneEdge::Top => (SplitAxis::Vertical, true),
            PaneEdge::Bottom => (SplitAxis::Vertical, false),
        }
    }
}

/// 侧栏筛选（ui-shell.md §2.1）：路径搜索框 + 过滤按钮（会话类型、活动状态）。
///
/// 只影响显示，不改顺序和活动状态。各条件之间是"且"；`kinds` 为空表示不限类型。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabFilter {
    /// 路径搜索词（规则见 [`TabPresentation::matches_path_filter`]）。
    pub path_query: String,
    /// 只显示这些类型；空表示全部。
    pub kinds: Vec<TabKind>,
    /// 只显示"运行中"（命令或 AI 在跑）。
    pub only_running: bool,
    /// 只显示"有未读完成"。
    pub only_unread: bool,
}

impl TabFilter {
    /// 没有任何条件。
    pub fn is_empty(&self) -> bool {
        self.path_query.trim().is_empty()
            && self.kinds.is_empty()
            && !self.only_running
            && !self.only_unread
    }

    /// 标签是否通过筛选。
    pub fn matches(&self, item: &TabItem) -> bool {
        TabPresentation::matches_path_filter(item, &self.path_query)
            && (self.kinds.is_empty() || self.kinds.contains(&item.kind))
            && (!self.only_running || item.activity.is_busy())
            && (!self.only_unread || item.unread_done)
    }
}

/// 工作区。
#[derive(Clone, Debug)]
pub struct Workspace {
    layout: SplitLayout,
    active_pane: PaneId,
    pane_tabs: HashMap<PaneId, Vec<SessionId>>,
    pane_active: HashMap<PaneId, SessionId>,
    items: HashMap<SessionId, TabItem>,
    next_pane: u32,
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

impl Workspace {
    /// 初始状态：一个空 pane（零个标签，UI 据此显示空状态页）。
    pub fn new() -> Self {
        let first = PaneId(1);
        Workspace {
            layout: SplitLayout::single(first),
            active_pane: first,
            pane_tabs: HashMap::from([(first, Vec::new())]),
            pane_active: HashMap::new(),
            items: HashMap::new(),
            next_pane: 2,
        }
    }

    // ---- 查询 ----

    /// 分屏布局（只读；拖动分隔条用 [`move_divider`](Self::move_divider)）。
    pub fn layout(&self) -> &SplitLayout {
        &self.layout
    }

    /// 活动 pane。
    pub fn active_pane(&self) -> PaneId {
        self.active_pane
    }

    /// 阅读顺序的全部 pane。
    pub fn all_panes(&self) -> Vec<PaneId> {
        self.layout.panes()
    }

    /// pane 内的标签（按显示顺序）。
    pub fn tabs_in(&self, pane: PaneId) -> Vec<&TabItem> {
        self.pane_tabs
            .get(&pane)
            .map(|list| list.iter().filter_map(|id| self.items.get(id)).collect())
            .unwrap_or_default()
    }

    /// pane 的活动标签。
    pub fn active_tab_in(&self, pane: PaneId) -> Option<SessionId> {
        self.pane_active.get(&pane).copied()
    }

    /// 活动 pane 的活动标签。
    pub fn active_session(&self) -> Option<SessionId> {
        self.active_tab_in(self.active_pane)
    }

    /// 标签数据。
    pub fn item(&self, id: SessionId) -> Option<&TabItem> {
        self.items.get(&id)
    }

    /// 标签数量。
    pub fn tab_count(&self) -> usize {
        self.items.len()
    }

    /// 没有任何标签（UI 显示空状态页）。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 标签所在的 pane。
    pub fn pane_of(&self, session: SessionId) -> Option<PaneId> {
        if !self.items.contains_key(&session) {
            return None;
        }
        self.pane_tabs
            .iter()
            .find(|(_, list)| list.contains(&session))
            .map(|(p, _)| *p)
    }

    /// 侧栏顺序：按 pane 阅读顺序拼接每个 pane 的标签。
    pub fn sidebar_order(&self) -> Vec<SessionId> {
        self.all_panes()
            .iter()
            .flat_map(|p| self.pane_tabs.get(p).cloned().unwrap_or_default())
            .collect()
    }

    /// 侧栏分组。
    pub fn sidebar_sections(&self) -> Vec<SidebarSection> {
        self.all_panes()
            .into_iter()
            .enumerate()
            .map(|(offset, pane)| SidebarSection {
                pane,
                title_index: offset + 1,
                tabs: self.tabs_in(pane).into_iter().cloned().collect(),
            })
            .collect()
    }

    /// 只有多个 pane 时才显示分组标题。
    pub fn shows_section_headers(&self) -> bool {
        self.all_panes().len() > 1
    }

    /// 筛选后的侧栏。空组仍然返回（`tabs` 为空），是否隐藏由视图决定。
    pub fn filtered_sidebar(&self, filter: &TabFilter) -> Vec<SidebarSection> {
        let mut sections = self.sidebar_sections();
        for s in &mut sections {
            s.tabs.retain(|t| filter.matches(t));
        }
        sections
    }

    /// 每个 pane 的活动标签：核心里只有它们算"可见"（用于 `set_visible` 差量同步）。
    pub fn visible_sessions(&self) -> Vec<SessionId> {
        self.all_panes()
            .iter()
            .filter_map(|p| self.active_tab_in(*p))
            .collect()
    }

    // ---- 标签增删与激活 ----

    /// 加到 pane（默认活动 pane）当前活动标签的后面；pane 为空时成为唯一标签。
    /// 重复 id 或 pane 不存在时忽略。`activate == false` 时不改变活动标签，
    /// 但空 pane 里的第一个标签总会成为该 pane 的活动标签（否则 pane 有标签却没有活动标签）。
    pub fn add_tab(&mut self, item: TabItem, pane: Option<PaneId>, activate: bool) {
        let target = pane.unwrap_or(self.active_pane);
        if !self.pane_tabs.contains_key(&target) || self.items.contains_key(&item.id) {
            return;
        }
        let id = item.id;
        self.items.insert(id, item);
        let list = self.pane_tabs.entry(target).or_default();
        match self
            .pane_active
            .get(&target)
            .and_then(|a| list.iter().position(|x| x == a))
        {
            Some(idx) => list.insert(idx + 1, id),
            None => list.push(id),
        }
        self.pane_active.entry(target).or_insert(id);
        if activate {
            self.activate(id);
        }
    }

    /// 设为所在 pane 的活动标签，同时把该 pane 设为活动 pane，并清除 `unread_done`。
    pub fn activate(&mut self, session: SessionId) -> bool {
        let Some(pane) = self.pane_of(session) else {
            return false;
        };
        self.pane_active.insert(pane, session);
        self.active_pane = pane;
        self.clear_unread(session);
        true
    }

    /// 切换活动 pane，同时清除该 pane 活动标签的 `unread_done`（它现在被用户看到了）。
    pub fn activate_pane(&mut self, pane: PaneId) -> bool {
        if !self.pane_tabs.contains_key(&pane) {
            return false;
        }
        self.active_pane = pane;
        if let Some(session) = self.active_tab_in(pane) {
            self.clear_unread(session);
        }
        true
    }

    fn clear_unread(&mut self, session: SessionId) {
        if let Some(item) = self.items.get_mut(&session) {
            item.unread_done = false;
        }
    }

    /// 从 pane 中移除标签（会话已关闭后调用）。
    ///
    /// 若它是该 pane 的活动标签，新的活动标签取原位置的下一个，没有则取上一个。
    /// pane 因此变空且不是唯一 pane → 从布局移除（份额按比例分给兄弟）；
    /// 若它是活动 pane，活动 pane 改为阅读顺序中的前一个（没有前一个取新的第一个）。
    /// 唯一 pane 变空则保留空 pane。
    pub fn remove_tab(&mut self, session: SessionId) -> bool {
        if !self.detach(session) {
            return false;
        }
        self.items.remove(&session);
        true
    }

    /// 在活动 pane 后面拆出新 pane，把 `item` 放进去并激活新 pane。
    /// 调用方负责先开好新会话（同一会话不能同时显示在两处）。
    /// `item.id` 已存在时不做任何事，返回当前活动 pane。
    /// 活动 pane 没有标签（空状态页）时不拆分，直接把标签放进它：否则布局里会留下一个
    /// 没有标签的 pane，而空 pane 只允许在"唯一 pane"时存在。
    pub fn split_active_pane(&mut self, axis: SplitAxis, item: TabItem) -> PaneId {
        if self.items.contains_key(&item.id) {
            return self.active_pane;
        }
        if self
            .pane_tabs
            .get(&self.active_pane)
            .is_none_or(Vec::is_empty)
        {
            let pane = self.active_pane;
            self.add_tab(item, Some(pane), true);
            return pane;
        }
        let new = PaneId(self.next_pane);
        if !self.layout.split(self.active_pane, axis, new) {
            return self.active_pane;
        }
        self.next_pane += 1;
        self.pane_tabs.insert(new, Vec::new());
        self.add_tab(item, Some(new), true);
        new
    }

    // ---- 分隔条 ----

    /// 拖动分隔条（转发给 [`SplitLayout::move_divider`]）。
    pub fn move_divider(
        &mut self,
        split_path: &[usize],
        divider_index: usize,
        delta: f64,
        min_fraction: f64,
    ) {
        self.layout
            .move_divider(split_path, divider_index, delta, min_fraction);
    }

    // ---- 切换 ----

    /// 同 pane 内下一个标签（循环）。
    pub fn select_next_tab_in_pane(&mut self) {
        self.cycle_tab(1);
    }

    /// 同 pane 内上一个标签（循环）。
    pub fn select_previous_tab_in_pane(&mut self) {
        self.cycle_tab(-1);
    }

    /// 下一个 pane（循环）。
    pub fn select_next_pane(&mut self) {
        self.cycle_pane(1);
    }

    /// 上一个 pane（循环）。
    pub fn select_previous_pane(&mut self) {
        self.cycle_pane(-1);
    }

    /// Ctrl+1–9 用：按侧栏顺序的下标选标签；越界返回 false。
    pub fn select_tab_at_sidebar_index(&mut self, index: usize) -> bool {
        match self.sidebar_order().get(index) {
            Some(id) => {
                let id = *id;
                self.activate(id)
            }
            None => false,
        }
    }

    fn cycle_tab(&mut self, step: isize) {
        let list = self
            .pane_tabs
            .get(&self.active_pane)
            .cloned()
            .unwrap_or_default();
        if list.len() < 2 {
            return;
        }
        let Some(idx) = self
            .active_session()
            .and_then(|a| list.iter().position(|x| *x == a))
        else {
            return;
        };
        let n = list.len() as isize;
        self.activate(list[((idx as isize + step).rem_euclid(n)) as usize]);
    }

    fn cycle_pane(&mut self, step: isize) {
        let panes = self.all_panes();
        if panes.len() < 2 {
            return;
        }
        let Some(idx) = panes.iter().position(|p| *p == self.active_pane) else {
            return;
        };
        let n = panes.len() as isize;
        self.activate_pane(panes[((idx as isize + step).rem_euclid(n)) as usize]);
    }

    // ---- 移动 ----

    /// 同 pane 内重排。`to_index` 是移除自身后的插入位置，夹到合法范围。不改变活动状态。
    pub fn move_tab(&mut self, session: SessionId, to_index: usize) -> bool {
        let Some(pane) = self.pane_of(session) else {
            return false;
        };
        let Some(list) = self.pane_tabs.get_mut(&pane) else {
            return false;
        };
        let Some(from) = list.iter().position(|x| *x == session) else {
            return false;
        };
        list.remove(from);
        list.insert(to_index.min(list.len()), session);
        true
    }

    /// 跨 pane 移动：会话不重建，只改位置。`index` 为移除 / 插入后目标 pane 内的位置，`None` 表示末尾。
    /// 源 pane 变空按 [`remove_tab`](Self::remove_tab) 的规则收起；目标 pane 激活该标签。
    /// 目标就是源 pane 时退化为同组重排（`None` 即移到末尾）。
    pub fn move_tab_to_pane(
        &mut self,
        session: SessionId,
        target: PaneId,
        index: Option<usize>,
    ) -> bool {
        let Some(source) = self.pane_of(session) else {
            return false;
        };
        if !self.pane_tabs.contains_key(&target) {
            return false;
        }
        if source == target {
            return self.move_tab(session, index.unwrap_or(usize::MAX));
        }
        if !self.detach(session) {
            return false;
        }
        let list = self.pane_tabs.entry(target).or_default();
        list.insert(index.unwrap_or(list.len()).min(list.len()), session);
        self.pane_active.insert(target, session);
        self.activate(session);
        true
    }

    /// 把标签拖到 `target` 分屏的某条边：在那一侧新建分屏并放入该标签。返回新分屏。
    /// 标签就是 `target` 里唯一的标签时没有意义（拆出来源分屏会立刻收起），返回 `None` 且不改动。
    pub fn move_tab_to_edge(
        &mut self,
        session: SessionId,
        target: PaneId,
        edge: PaneEdge,
    ) -> Option<PaneId> {
        let source = self.pane_of(session)?;
        if !self.pane_tabs.contains_key(&target) {
            return None;
        }
        if source == target && self.pane_tabs.get(&target).is_some_and(|l| l.len() < 2) {
            return None;
        }
        let new = PaneId(self.next_pane);
        let (axis, before) = edge.axis_and_before();
        if !self.layout.split_at(target, axis, new, before) {
            return None;
        }
        self.next_pane += 1;
        self.pane_tabs.insert(new, Vec::new());
        self.move_tab_to_pane(session, new, None);
        Some(new)
    }

    /// 把标签从所在 pane 摘下（保留 `items`），并处理活动标签与空 pane 收起。
    fn detach(&mut self, session: SessionId) -> bool {
        let Some(pane) = self.pane_of(session) else {
            return false;
        };
        let Some(list) = self.pane_tabs.get_mut(&pane) else {
            return false;
        };
        let Some(idx) = list.iter().position(|x| *x == session) else {
            return false;
        };
        list.remove(idx);
        let now_empty = list.is_empty();
        let next = list.get(idx.min(list.len().saturating_sub(1))).copied();

        if now_empty {
            self.pane_active.remove(&pane);
            let before = self.all_panes();
            if before.len() > 1 {
                let previous = before
                    .iter()
                    .position(|p| *p == pane)
                    .and_then(|i| i.checked_sub(1))
                    .map(|i| before[i]);
                if self.layout.remove(pane) {
                    self.pane_tabs.remove(&pane);
                    if self.active_pane == pane {
                        self.active_pane = previous.unwrap_or_else(|| self.all_panes()[0]);
                        if let Some(s) = self.active_tab_in(self.active_pane) {
                            self.clear_unread(s);
                        }
                    }
                }
            }
        } else if self.pane_active.get(&pane) == Some(&session) {
            // 原位置的下一个已前移到 idx；越界说明删的是最后一个，取上一个。
            if let Some(next) = next {
                self.pane_active.insert(pane, next);
                // 活动 pane 里新露出的标签正被用户看到，清除未读。
                if pane == self.active_pane {
                    self.clear_unread(next);
                }
            }
        }
        true
    }

    // ---- 批量关闭 ----

    /// 批量关闭的目标（只在 `session` 所在的分屏组内）。只返回 id，不执行关闭。
    pub fn close_targets(&self, scope: CloseScope, relative_to: SessionId) -> Vec<SessionId> {
        let Some(list) = self
            .pane_of(relative_to)
            .and_then(|p| self.pane_tabs.get(&p))
        else {
            return Vec::new();
        };
        let Some(idx) = list.iter().position(|x| *x == relative_to) else {
            return Vec::new();
        };
        match scope {
            CloseScope::Others => list.iter().copied().filter(|x| *x != relative_to).collect(),
            CloseScope::Above => list[..idx].to_vec(),
            CloseScope::Below => list[idx + 1..].to_vec(),
            CloseScope::All => list.clone(),
        }
    }

    /// 关闭前是否需要确认（ui-shell.md §8）。
    ///
    /// - SSH / SFTP：连接中或已连接需要确认。
    /// - 本地 shell：只在有前台命令或 AI 在运行时才需要确认。
    ///   这是对"仍处于连接状态"的解释：本地 shell 空闲时也算"连接着"，
    ///   但那样每次关标签都会弹窗，体验很差，所以空闲的本地 shell 直接关。
    /// - 任何会话 `active_transfers > 0` 都需要确认，且 `interrupts_transfers = true`。
    /// - 已关闭的会话不需要确认。不存在的 id 忽略。
    ///
    /// 不需要确认返回 `None`。
    pub fn close_confirmation(&self, sessions: &[SessionId]) -> Option<CloseConfirmation> {
        let mut needed = Vec::new();
        let mut interrupts = false;
        for id in sessions {
            let Some(item) = self.items.get(id) else {
                continue;
            };
            if matches!(item.connection, TabConnection::Closed(_)) {
                continue;
            }
            let live = match item.kind {
                TabKind::Ssh | TabKind::Sftp => true,
                TabKind::Local => item.activity.is_busy(),
            };
            let transferring = item.active_transfers > 0;
            if live || transferring {
                needed.push(*id);
                interrupts |= transferring;
            }
        }
        (!needed.is_empty()).then_some(CloseConfirmation {
            sessions: needed,
            interrupts_transfers: interrupts,
        })
    }

    // ---- 事件应用 ----

    /// 设置或清除标签的自定义名（去掉首尾空白后为空视为清除）。会话不存在返回 `false`。
    pub fn set_custom_title(&mut self, session: SessionId, title: Option<String>) -> bool {
        let Some(item) = self.items.get_mut(&session) else {
            return false;
        };
        item.custom_title = title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        true
    }

    /// 位置变化（cwd / 主机 / 分支）。
    pub fn apply_location(
        &mut self,
        session: SessionId,
        cwd: Option<String>,
        cwd_host: Option<String>,
        git_branch: Option<String>,
    ) -> bool {
        let Some(item) = self.items.get_mut(&session) else {
            return false;
        };
        item.cwd = cwd;
        item.cwd_host = cwd_host;
        item.git_branch = git_branch;
        true
    }

    /// 连接状态变化。
    pub fn apply_connection(&mut self, session: SessionId, connection: TabConnection) -> bool {
        let Some(item) = self.items.get_mut(&session) else {
            return false;
        };
        item.connection = connection;
        true
    }

    /// 活动变化。从"忙"变到"闲"时，如果该标签不是当前活动标签，置 `unread_done`。
    /// 用户正看着的标签不需要未读提示。
    pub fn apply_activity(&mut self, session: SessionId, activity: TabActivity) -> bool {
        let active = self.active_session();
        let Some(item) = self.items.get_mut(&session) else {
            return false;
        };
        let was_busy = item.activity.is_busy();
        item.activity = activity;
        if was_busy && !activity.is_busy() && Some(session) != active {
            item.unread_done = true;
        }
        true
    }

    /// 传输计数加减，不低于 0（防止重复的结束事件把计数变成负数）。
    pub fn apply_transfer(&mut self, session: SessionId, started: bool) -> bool {
        let Some(item) = self.items.get_mut(&session) else {
            return false;
        };
        item.active_transfers = if started {
            item.active_transfers + 1
        } else {
            item.active_transfers.saturating_sub(1)
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::layout::LayoutNode;
    use kanso_types::agent::AgentKind;

    fn sid(n: u64) -> SessionId {
        SessionId::from_raw(n)
    }

    fn tab(n: u64) -> TabItem {
        let mut t = TabItem::new(sid(n), TabKind::Local, format!("t{n}"));
        t.connection = TabConnection::Connected;
        t
    }

    fn ssh_tab(n: u64) -> TabItem {
        let mut t = TabItem::new(sid(n), TabKind::Ssh, format!("s{n}"));
        t.connection = TabConnection::Connected;
        t
    }

    fn ids(ws: &Workspace, pane: PaneId) -> Vec<u64> {
        ws.tabs_in(pane).iter().map(|t| t.id.raw()).collect()
    }

    /// 两个 pane：P1 = [1, 2]，P2 = [3]；活动标签为 3。
    fn two_panes() -> (Workspace, PaneId, PaneId) {
        let mut ws = Workspace::new();
        let p1 = ws.active_pane();
        ws.add_tab(tab(1), None, true);
        ws.add_tab(tab(2), None, true);
        let p2 = ws.split_active_pane(SplitAxis::Horizontal, tab(3));
        (ws, p1, p2)
    }

    #[test]
    fn starts_empty_with_one_pane() {
        let ws = Workspace::new();
        assert!(ws.is_empty());
        assert_eq!(ws.all_panes(), vec![PaneId(1)]);
        assert_eq!(ws.active_session(), None);
        assert!(!ws.shows_section_headers());
    }

    #[test]
    fn add_tab_inserts_after_active_and_activates() {
        let mut ws = Workspace::new();
        let p = ws.active_pane();
        ws.add_tab(tab(1), None, true);
        ws.add_tab(tab(2), None, true);
        ws.activate(sid(1));
        ws.add_tab(tab(3), None, true);
        assert_eq!(ids(&ws, p), vec![1, 3, 2]);
        assert_eq!(ws.active_session(), Some(sid(3)));
    }

    #[test]
    fn add_tab_without_activate_keeps_active_but_first_tab_is_active() {
        let mut ws = Workspace::new();
        let p = ws.active_pane();
        ws.add_tab(tab(1), None, false);
        assert_eq!(
            ws.active_tab_in(p),
            Some(sid(1)),
            "空 pane 的第一个标签总是活动的"
        );
        ws.add_tab(tab(2), None, false);
        assert_eq!(ws.active_tab_in(p), Some(sid(1)));
        assert_eq!(ids(&ws, p), vec![1, 2]);
    }

    #[test]
    fn add_tab_ignores_duplicates_and_unknown_pane() {
        let mut ws = Workspace::new();
        ws.add_tab(tab(1), None, true);
        ws.add_tab(tab(1), None, true);
        ws.add_tab(tab(2), Some(PaneId(42)), true);
        assert_eq!(ws.tab_count(), 1);
    }

    #[test]
    fn split_active_pane_creates_pane_and_activates_it() {
        let (ws, p1, p2) = two_panes();
        assert_ne!(p1, p2);
        assert_eq!(ws.all_panes(), vec![p1, p2]);
        assert_eq!(ws.active_pane(), p2);
        assert_eq!(ws.active_session(), Some(sid(3)));
        assert!(ws.shows_section_headers());
        assert_eq!(ws.sidebar_order(), vec![sid(1), sid(2), sid(3)]);
        let sections = ws.sidebar_sections();
        assert_eq!(sections[1].title_index, 2);
    }

    #[test]
    fn split_in_empty_pane_just_adds_tab() {
        let mut ws = Workspace::new();
        let p = ws.split_active_pane(SplitAxis::Vertical, tab(1));
        assert_eq!(p, PaneId(1));
        assert_eq!(ws.all_panes().len(), 1, "空状态页不拆分");
        assert_eq!(ws.active_session(), Some(sid(1)));
    }

    #[test]
    fn split_with_existing_session_is_noop() {
        let (mut ws, _, p2) = two_panes();
        assert_eq!(ws.split_active_pane(SplitAxis::Vertical, tab(1)), p2);
        assert_eq!(ws.all_panes().len(), 2);
    }

    #[test]
    fn remove_active_tab_selects_next_then_previous() {
        let mut ws = Workspace::new();
        let p = ws.active_pane();
        for n in 1..=3 {
            ws.add_tab(tab(n), None, true);
        }
        ws.activate(sid(2));
        ws.remove_tab(sid(2));
        assert_eq!(ws.active_tab_in(p), Some(sid(3)), "取原位置的下一个");
        ws.remove_tab(sid(3));
        assert_eq!(ws.active_tab_in(p), Some(sid(1)), "最后一个被删则取上一个");
        ws.remove_tab(sid(1));
        assert_eq!(ws.active_tab_in(p), None);
        assert_eq!(ws.all_panes(), vec![p], "唯一 pane 变空保留");
        assert!(!ws.remove_tab(sid(1)));
    }

    #[test]
    fn remove_last_tab_collapses_pane_and_moves_focus_to_previous() {
        let (mut ws, p1, p2) = two_panes();
        assert_eq!(ws.active_pane(), p2);
        ws.remove_tab(sid(3));
        assert_eq!(ws.all_panes(), vec![p1]);
        assert_eq!(ws.active_pane(), p1);
        assert_eq!(*ws.layout().root(), LayoutNode::Pane(p1));
    }

    #[test]
    fn remove_first_pane_focuses_new_first() {
        let (mut ws, p1, p2) = two_panes();
        ws.activate_pane(p1);
        ws.remove_tab(sid(1));
        ws.remove_tab(sid(2));
        assert_eq!(ws.all_panes(), vec![p2]);
        assert_eq!(ws.active_pane(), p2);
    }

    #[test]
    fn unread_set_when_background_tab_finishes_and_cleared_on_activate() {
        let (mut ws, _, _) = two_panes(); // 活动 = 3
        ws.apply_activity(sid(1), TabActivity::CommandRunning);
        ws.apply_activity(sid(1), TabActivity::Idle);
        assert!(ws.item(sid(1)).unwrap().unread_done);
        ws.activate(sid(1));
        assert!(!ws.item(sid(1)).unwrap().unread_done);
    }

    #[test]
    fn no_unread_for_foreground_tab_or_agent_idle_to_working() {
        let (mut ws, _, _) = two_panes();
        ws.apply_activity(sid(3), TabActivity::CommandRunning);
        ws.apply_activity(sid(3), TabActivity::Idle);
        assert!(
            !ws.item(sid(3)).unwrap().unread_done,
            "正看着的标签不置未读"
        );
        // AgentIdle -> AgentWorking 不是"忙到闲"。
        ws.apply_activity(sid(1), TabActivity::AgentIdle(AgentKind::Claude));
        ws.apply_activity(sid(1), TabActivity::AgentWorking(AgentKind::Claude));
        assert!(!ws.item(sid(1)).unwrap().unread_done);
        ws.apply_activity(sid(1), TabActivity::AgentIdle(AgentKind::Claude));
        assert!(ws.item(sid(1)).unwrap().unread_done, "agent 工作完成算未读");
    }

    #[test]
    fn activate_pane_clears_unread_of_its_active_tab() {
        let (mut ws, p1, _) = two_panes();
        ws.apply_activity(sid(2), TabActivity::CommandRunning);
        ws.apply_activity(sid(2), TabActivity::Idle);
        assert!(ws.item(sid(2)).unwrap().unread_done);
        assert!(ws.activate_pane(p1));
        assert!(!ws.item(sid(2)).unwrap().unread_done);
        assert!(!ws.activate_pane(PaneId(99)));
    }

    #[test]
    fn removing_active_tab_clears_unread_of_newly_exposed_tab() {
        let mut ws = Workspace::new();
        ws.add_tab(tab(1), None, true);
        ws.add_tab(tab(2), None, true);
        ws.apply_activity(sid(1), TabActivity::CommandRunning);
        ws.apply_activity(sid(1), TabActivity::Idle);
        assert!(ws.item(sid(1)).unwrap().unread_done);
        ws.remove_tab(sid(2));
        assert!(!ws.item(sid(1)).unwrap().unread_done);
    }

    #[test]
    fn cycling_tabs_and_panes_wraps() {
        let (mut ws, p1, p2) = two_panes();
        ws.activate_pane(p1);
        ws.activate(sid(1));
        ws.select_next_tab_in_pane();
        assert_eq!(ws.active_session(), Some(sid(2)));
        ws.select_next_tab_in_pane();
        assert_eq!(ws.active_session(), Some(sid(1)));
        ws.select_previous_tab_in_pane();
        assert_eq!(ws.active_session(), Some(sid(2)));
        ws.select_next_pane();
        assert_eq!(ws.active_pane(), p2);
        ws.select_next_pane();
        assert_eq!(ws.active_pane(), p1);
        ws.select_previous_pane();
        assert_eq!(ws.active_pane(), p2);
    }

    #[test]
    fn select_by_sidebar_index() {
        let (mut ws, p1, _) = two_panes();
        assert!(ws.select_tab_at_sidebar_index(0));
        assert_eq!(ws.active_pane(), p1);
        assert_eq!(ws.active_session(), Some(sid(1)));
        assert!(ws.select_tab_at_sidebar_index(2));
        assert!(!ws.select_tab_at_sidebar_index(3));
    }

    #[test]
    fn move_tab_reorders_within_pane() {
        let mut ws = Workspace::new();
        let p = ws.active_pane();
        for n in 1..=4 {
            ws.add_tab(tab(n), None, true);
        }
        assert!(ws.move_tab(sid(1), 2));
        assert_eq!(ids(&ws, p), vec![2, 3, 1, 4]);
        assert!(ws.move_tab(sid(4), 0));
        assert_eq!(ids(&ws, p), vec![4, 2, 3, 1]);
        assert!(ws.move_tab(sid(4), 100));
        assert_eq!(ids(&ws, p), vec![2, 3, 1, 4]);
        assert!(!ws.move_tab(sid(99), 0));
        assert_eq!(ws.active_tab_in(p), Some(sid(4)), "重排不改变活动标签");
    }

    #[test]
    fn move_tab_across_panes_keeps_session_and_collapses_empty_source() {
        let (mut ws, p1, p2) = two_panes();
        assert!(ws.move_tab_to_pane(sid(3), p1, Some(1)));
        assert_eq!(ids(&ws, p1), vec![1, 3, 2]);
        assert_eq!(ws.all_panes(), vec![p1], "源 pane 变空被收起");
        assert_eq!(ws.active_session(), Some(sid(3)));
        assert_eq!(ws.pane_of(sid(3)), Some(p1));
        assert!(ws.item(sid(3)).is_some(), "会话状态保留");
        assert!(
            !ws.move_tab_to_pane(sid(3), p2, None),
            "已收起的 pane 不存在"
        );
    }

    #[test]
    fn move_tab_to_same_pane_degrades_to_reorder() {
        let (mut ws, p1, _) = two_panes();
        assert!(ws.move_tab_to_pane(sid(1), p1, None));
        assert_eq!(ids(&ws, p1), vec![2, 1]);
    }

    #[test]
    fn move_tab_to_edge_creates_pane_on_that_side() {
        let (mut ws, p1, p2) = two_panes();
        let new = ws.move_tab_to_edge(sid(2), p1, PaneEdge::Left).unwrap();
        assert_eq!(ws.all_panes(), vec![new, p1, p2]);
        assert_eq!(ids(&ws, new), vec![2]);
        assert_eq!(ids(&ws, p1), vec![1]);
        assert_eq!(ws.active_pane(), new);
        let new2 = ws.move_tab_to_edge(sid(1), p2, PaneEdge::Bottom).unwrap();
        assert_eq!(ws.all_panes(), vec![new, p2, new2]);
    }

    #[test]
    fn move_tab_to_edge_of_its_only_pane_is_noop() {
        let mut ws = Workspace::new();
        ws.add_tab(tab(1), None, true);
        assert_eq!(
            ws.move_tab_to_edge(sid(1), PaneId(1), PaneEdge::Right),
            None
        );
        assert_eq!(ws.all_panes().len(), 1);
    }

    #[test]
    fn close_targets_stay_inside_the_pane() {
        let mut ws = Workspace::new();
        for n in 1..=4 {
            ws.add_tab(tab(n), None, true);
        }
        ws.split_active_pane(SplitAxis::Horizontal, tab(9));
        assert_eq!(
            ws.close_targets(CloseScope::Others, sid(2)),
            vec![sid(1), sid(3), sid(4)]
        );
        assert_eq!(
            ws.close_targets(CloseScope::Above, sid(3)),
            vec![sid(1), sid(2)]
        );
        assert_eq!(ws.close_targets(CloseScope::Below, sid(3)), vec![sid(4)]);
        assert_eq!(ws.close_targets(CloseScope::All, sid(3)).len(), 4);
        assert_eq!(ws.close_targets(CloseScope::Below, sid(4)), vec![]);
        assert_eq!(ws.close_targets(CloseScope::All, sid(99)), vec![]);
    }

    #[test]
    fn close_confirmation_rules() {
        let mut ws = Workspace::new();
        ws.add_tab(tab(1), None, false); // 空闲本地
        ws.add_tab(ssh_tab(2), None, false);
        let mut busy = tab(3);
        busy.activity = TabActivity::CommandRunning;
        ws.add_tab(busy, None, false);
        let mut closed = ssh_tab(4);
        closed.connection = TabConnection::Closed(None);
        ws.add_tab(closed, None, false);
        let mut connecting = ssh_tab(5);
        connecting.connection = TabConnection::Connecting;
        ws.add_tab(connecting, None, false);

        assert_eq!(
            ws.close_confirmation(&[sid(1)]),
            None,
            "空闲本地 shell 直接关"
        );
        assert_eq!(ws.close_confirmation(&[sid(4)]), None, "已关闭直接关");
        assert_eq!(ws.close_confirmation(&[sid(99)]), None);
        let c = ws
            .close_confirmation(&[sid(1), sid(2), sid(3), sid(4), sid(5)])
            .unwrap();
        assert_eq!(c.sessions, vec![sid(2), sid(3), sid(5)]);
        assert!(!c.interrupts_transfers);
    }

    #[test]
    fn close_confirmation_with_transfers() {
        let mut ws = Workspace::new();
        let mut t = tab(1);
        t.active_transfers = 1;
        ws.add_tab(t, None, true);
        let c = ws.close_confirmation(&[sid(1)]).unwrap();
        assert!(c.interrupts_transfers, "本地空闲但有传输也要确认");
    }

    #[test]
    fn transfer_count_never_negative() {
        let mut ws = Workspace::new();
        ws.add_tab(ssh_tab(1), None, true);
        assert!(ws.apply_transfer(sid(1), false));
        assert_eq!(ws.item(sid(1)).unwrap().active_transfers, 0);
        ws.apply_transfer(sid(1), true);
        ws.apply_transfer(sid(1), true);
        ws.apply_transfer(sid(1), false);
        assert_eq!(ws.item(sid(1)).unwrap().active_transfers, 1);
        assert!(!ws.apply_transfer(sid(9), true));
    }

    #[test]
    fn apply_location_and_connection() {
        let mut ws = Workspace::new();
        ws.add_tab(tab(1), None, true);
        assert!(ws.apply_location(
            sid(1),
            Some("/a".into()),
            Some("h".into()),
            Some("main".into())
        ));
        let t = ws.item(sid(1)).unwrap();
        assert_eq!(
            (
                t.cwd.as_deref(),
                t.cwd_host.as_deref(),
                t.git_branch.as_deref()
            ),
            (Some("/a"), Some("h"), Some("main"))
        );
        ws.apply_location(sid(1), None, None, None);
        assert_eq!(
            ws.item(sid(1)).unwrap().git_branch,
            None,
            "离开仓库后分支清除"
        );
        assert!(ws.apply_connection(sid(1), TabConnection::Closed(None)));
        assert!(!ws.apply_connection(sid(2), TabConnection::Connected));
    }

    #[test]
    fn filtered_sidebar_does_not_change_order_or_active() {
        let mut ws = Workspace::new();
        let mut a = tab(1);
        a.cwd = Some("/work/a".into());
        let mut b = tab(2);
        b.cwd = Some("/other".into());
        ws.add_tab(a, None, true);
        ws.add_tab(b, None, true);
        ws.add_tab(ssh_tab(3), None, true);
        let f = TabFilter {
            path_query: "work".into(),
            ..Default::default()
        };
        let sections = ws.filtered_sidebar(&f);
        assert_eq!(sections[0].tabs.len(), 1);
        assert_eq!(sections[0].tabs[0].id, sid(1));
        assert_eq!(ws.sidebar_order(), vec![sid(1), sid(2), sid(3)]);
        assert_eq!(ws.active_session(), Some(sid(3)));

        let only_ssh = TabFilter {
            kinds: vec![TabKind::Ssh],
            ..Default::default()
        };
        assert_eq!(ws.filtered_sidebar(&only_ssh)[0].tabs.len(), 1);
        assert!(TabFilter::default().is_empty() && !only_ssh.is_empty());
    }

    #[test]
    fn filter_by_activity() {
        let mut busy = tab(1);
        busy.activity = TabActivity::CommandRunning;
        let mut unread = tab(2);
        unread.unread_done = true;
        let idle = tab(3);
        let running = TabFilter {
            only_running: true,
            ..Default::default()
        };
        let unread_f = TabFilter {
            only_unread: true,
            ..Default::default()
        };
        assert!(running.matches(&busy) && !running.matches(&idle) && !running.matches(&unread));
        assert!(unread_f.matches(&unread) && !unread_f.matches(&idle));
    }

    #[test]
    fn visible_sessions_are_active_tab_of_each_pane() {
        let (mut ws, p1, _) = two_panes();
        assert_eq!(ws.visible_sessions(), vec![sid(2), sid(3)]);
        ws.activate_pane(p1);
        ws.activate(sid(1));
        assert_eq!(ws.visible_sessions(), vec![sid(1), sid(3)]);
    }

    #[test]
    fn divider_moves_through_workspace() {
        let (mut ws, _, _) = two_panes();
        ws.move_divider(&[], 0, 0.2, 0.1);
        let LayoutNode::Split(_, c) = ws.layout().root() else {
            panic!()
        };
        assert!((c[0].ratio - 0.7).abs() < 1e-9);
    }
}
