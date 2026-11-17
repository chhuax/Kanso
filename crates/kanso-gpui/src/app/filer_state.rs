//! 文件面板（辅助 Filer 与纯 SFTP 双栏）的状态纯逻辑（docs/spec/filer.md §1–§5）：
//! 选中模型（单击 / Ctrl 点选 / Shift 范围 / 方向键）、浏览请求的代数与失败处理、
//! 定位终端目录的请求守卫、条目统计与悬停说明文字。
//!
//! 不依赖窗口系统，三个平台都能编译与测试。

use std::collections::{BTreeSet, HashMap};

use kanso_types::SessionId;

use crate::app::filer_text::{t, tf, tn};
use crate::filer::{DirModel, Row, format_mtime, format_size};

// ---- 选中 ----

/// 多选状态：选中的稳定编号集合、范围选择的锚点和键盘光标（都用条目稳定编号，分批插入不会让选中跑到别的行）。
#[derive(Clone, Debug, Default)]
pub struct Selection {
    ids: BTreeSet<u32>,
    anchor: Option<u32>,
    cursor: Option<u32>,
}

impl Selection {
    /// 清空。
    pub fn clear(&mut self) {
        self.ids.clear();
        self.anchor = None;
        self.cursor = None;
    }

    /// 选中的编号（升序）。
    pub fn ids(&self) -> &BTreeSet<u32> {
        &self.ids
    }

    /// 是否选中。
    pub fn contains(&self, id: u32) -> bool {
        self.ids.contains(&id)
    }

    /// 选中数。
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// 是否没有选中。
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// 键盘光标所在条目（最近一次点击或移动到的那一项）。
    pub fn cursor(&self) -> Option<u32> {
        self.cursor
    }

    /// 只选一项。
    pub fn select_only(&mut self, id: u32) {
        self.ids.clear();
        self.ids.insert(id);
        self.anchor = Some(id);
        self.cursor = Some(id);
    }

    /// 点击可见位置 `pos` 的行：普通点击单选；Ctrl 切换该项；Shift 选锚点到该行的范围（加 Ctrl 则并入已有选择）。
    pub fn click(&mut self, pos: usize, model: &DirModel, ctrl: bool, shift: bool) {
        let Some((id, _)) = model.visible_at(pos) else {
            return;
        };
        if shift {
            let anchor_pos = self
                .anchor
                .and_then(|a| model.visible_position_of(a))
                .unwrap_or(pos);
            let (lo, hi) = (anchor_pos.min(pos), anchor_pos.max(pos));
            if !ctrl {
                self.ids.clear();
            }
            for p in lo..=hi {
                if let Some((i, _)) = model.visible_at(p) {
                    self.ids.insert(i);
                }
            }
            self.cursor = Some(id);
            // 锚点保持不动，连续 Shift 点击以同一锚点为起点。
            if self.anchor.is_none() {
                self.anchor = Some(id);
            }
        } else if ctrl {
            if !self.ids.remove(&id) {
                self.ids.insert(id);
            }
            self.anchor = Some(id);
            self.cursor = Some(id);
        } else {
            self.select_only(id);
        }
    }

    /// 右键点在某行：该行已在选中集里就保持多选，否则单选它。
    pub fn right_click(&mut self, id: u32) {
        if !self.ids.contains(&id) {
            self.select_only(id);
        } else {
            self.cursor = Some(id);
        }
    }

    /// 方向键 / PageUp / PageDown / Home / End 移动：`target` 是新的可见位置。Shift 扩展范围。
    /// 返回实际落到的位置（用于滚动到可见）。
    pub fn move_to(&mut self, target: usize, model: &DirModel, shift: bool) -> Option<usize> {
        let len = model.visible_len();
        if len == 0 {
            return None;
        }
        let pos = target.min(len - 1);
        let (id, _) = model.visible_at(pos)?;
        if shift {
            let anchor_pos = self
                .anchor
                .and_then(|a| model.visible_position_of(a))
                .unwrap_or(pos);
            self.ids.clear();
            for p in anchor_pos.min(pos)..=anchor_pos.max(pos) {
                if let Some((i, _)) = model.visible_at(p) {
                    self.ids.insert(i);
                }
            }
            self.cursor = Some(id);
        } else {
            self.select_only(id);
        }
        Some(pos)
    }

    /// 按光标当前位置相对移动 `delta` 行（没有光标时向下从头、向上从尾开始）。
    pub fn step(&mut self, delta: isize, model: &DirModel, shift: bool) -> Option<usize> {
        let len = model.visible_len();
        if len == 0 {
            return None;
        }
        let cur = self.cursor.and_then(|c| model.visible_position_of(c));
        let target = match cur {
            Some(p) => p.saturating_add_signed(delta).min(len - 1),
            None if delta < 0 => len - 1,
            None => 0,
        };
        self.move_to(target, model, shift)
    }

    /// 全选当前可见条目（列表内 Ctrl+A）。
    pub fn select_all(&mut self, model: &DirModel) {
        self.ids.clear();
        for p in 0..model.visible_len() {
            if let Some((i, _)) = model.visible_at(p) {
                self.ids.insert(i);
            }
        }
        self.anchor = self.ids.iter().next().copied();
        self.cursor = self.ids.iter().next_back().copied();
    }

    /// 丢掉已经不在模型里的编号（隐藏过滤改变后调用）。
    pub fn retain_visible(&mut self, model: &DirModel) {
        self.ids
            .retain(|&id| model.visible_position_of(id).is_some());
        if self.cursor.is_some_and(|c| !self.ids.contains(&c)) {
            self.cursor = self.ids.iter().next().copied();
        }
        if self
            .anchor
            .is_some_and(|a| model.visible_position_of(a).is_none())
        {
            self.anchor = self.cursor;
        }
    }
}

// ---- 浏览请求 ----

/// 面板所处阶段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// 还没有请求（空面板）。
    Idle,
    /// 会话连接中。
    Connecting,
    /// 正在读取目录（分批到达，未完）。
    Loading,
    /// 读取完成。
    Ready,
    /// 最近一次读取失败（原因在 [`Browse::error`]）；之前的列表可能仍保留。
    Failed,
    /// 远端连接已断开：保留身份、最后目录与已过期的列表，停止写入。
    Disconnected,
}

/// 一次读取失败。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowseError {
    /// 想进入的目录。
    pub target: String,
    /// 给用户看的原因。
    pub text: String,
}

/// 目录浏览的请求状态机：只采纳最后一次请求的结果（filer.md §4.3）。
#[derive(Clone, Debug)]
pub struct Browse {
    /// 列表实际属于的目录（路径栏显示它）。
    pub path: String,
    /// 正在请求的目录。
    pub pending: Option<String>,
    /// 当前请求的代数。
    pub generation: u64,
    /// 阶段。
    pub phase: Phase,
    /// 最近一次失败。
    pub error: Option<BrowseError>,
    /// 新请求的第一批数据到达时才清掉旧列表，失败时旧列表仍可看（并标明它属于原目录）。
    clear_pending: bool,
}

impl Default for Browse {
    fn default() -> Self {
        Browse {
            path: String::new(),
            pending: None,
            generation: 0,
            phase: Phase::Idle,
            error: None,
            clear_pending: false,
        }
    }
}

impl Browse {
    /// 发起新请求，返回它的代数。之前请求的一切结果从此作废。
    pub fn begin(&mut self, target: &str) -> u64 {
        self.generation += 1;
        self.pending = Some(target.to_owned());
        self.phase = Phase::Loading;
        self.error = None;
        self.clear_pending = true;
        self.generation
    }

    /// 是否是当前请求。
    pub fn is_current(&self, generation: u64) -> bool {
        generation == self.generation && self.pending.is_some()
    }

    /// 第一批数据到达：返回 `true` 表示调用方此刻应先清空旧列表（只返回一次）；
    /// 请求已过期返回 `None`（整批丢弃）。
    pub fn on_data(&mut self, generation: u64) -> Option<bool> {
        if !self.is_current(generation) {
            return None;
        }
        let clear = std::mem::take(&mut self.clear_pending);
        if clear {
            // 旧列表被替换，路径栏跟着换成新目录。
            if let Some(p) = &self.pending {
                self.path = p.clone();
            }
        }
        Some(clear)
    }

    /// 读取完成。返回 `Some(是否需要清空旧列表)`（空目录没有任何数据批次，此时要清空）；过期返回 `None`。
    pub fn on_done(&mut self, generation: u64) -> Option<bool> {
        if !self.is_current(generation) {
            return None;
        }
        let clear = std::mem::take(&mut self.clear_pending);
        if let Some(p) = self.pending.take() {
            self.path = p;
        }
        self.phase = Phase::Ready;
        Some(clear)
    }

    /// 读取失败。过期返回 `false`。失败时旧列表保留，路径仍是原目录；
    /// 若已有部分数据换进了列表，则路径属于新目录。
    pub fn on_failed(&mut self, generation: u64, text: String) -> bool {
        if !self.is_current(generation) {
            return false;
        }
        let target = self.pending.take().unwrap_or_default();
        if !self.clear_pending {
            self.path = target.clone();
        }
        self.clear_pending = false;
        self.error = Some(BrowseError { target, text });
        self.phase = Phase::Failed;
        true
    }

    /// 标记远端已断开：保留一切，只改阶段。
    pub fn on_disconnected(&mut self) {
        self.generation += 1;
        self.pending = None;
        self.phase = Phase::Disconnected;
    }

    /// 能否发起写入（新建 / 删除 / 重命名 / 上传）：目录读取中、断开、没有目录时都不行。
    pub fn can_write(&self) -> bool {
        matches!(self.phase, Phase::Ready | Phase::Failed) && !self.path.is_empty()
    }
}

// ---- 定位终端目录的请求守卫 ----

/// 定位请求的守卫（filer.md §2）：请求期间切换标签或关闭原标签，迟到的结果不能抢走当前面板。
#[derive(Clone, Debug, Default)]
pub struct Locator {
    next: u64,
    latest: HashMap<SessionId, u64>,
}

impl Locator {
    /// 为某个会话发起一次定位，返回票据序号（同一会话只认最新一次）。
    pub fn begin(&mut self, session: SessionId) -> u64 {
        self.next += 1;
        self.latest.insert(session, self.next);
        self.next
    }

    /// 票据是否仍是该会话的最新定位请求。
    pub fn is_current(&self, session: SessionId, ticket: u64) -> bool {
        self.latest.get(&session) == Some(&ticket)
    }

    /// 会话关闭后丢弃它的记录（迟到的结果随之作废）。
    pub fn forget(&mut self, session: SessionId) {
        self.latest.remove(&session);
    }
}

/// 本地 shell 上报的目录所属主机是不是本机：空、`localhost`、本机名都算（不区分大小写）。
/// 通过本地 shell 手工登录另一台机器后，远端上报的目录不能被当成本机路径。
pub fn cwd_host_is_local(host: Option<&str>, local_names: &[&str]) -> bool {
    match host.map(str::trim) {
        None | Some("") => true,
        Some(h) => {
            let h = h.to_ascii_lowercase();
            h == "localhost"
                || h == "127.0.0.1"
                || h == "::1"
                || local_names
                    .iter()
                    .any(|n| !n.is_empty() && n.to_ascii_lowercase() == h)
        }
    }
}

// ---- 文字 ----

/// 条目统计：按目录、普通文件、符号链接分别计数（链接不在普通文件里重复计数）。
pub fn counts(model: &DirModel) -> (usize, usize, usize) {
    let mut dirs = 0;
    let mut files = 0;
    let mut links = 0;
    for pos in 0..model.visible_len() {
        if let Some((_, r)) = model.visible_at(pos) {
            if r.is_symlink {
                links += 1;
            } else if r.is_dir {
                dirs += 1;
            } else {
                files += 1;
            }
        }
    }
    (dirs, files, links)
}

/// 底部统计文字。读取未完时显示“正在读取”，完成后才给出最终条目数。
pub fn stats_text(model: &DirModel, phase: &Phase) -> String {
    let n = model.visible_len();
    match phase {
        Phase::Connecting => t("ui-filer-connecting"),
        Phase::Loading => tn("ui-filer-loading", n),
        Phase::Idle => String::new(),
        Phase::Disconnected => tn("ui-filer-disconnected-stats", n),
        Phase::Ready | Phase::Failed => {
            if n == 0 {
                return tn("ui-filer-empty", 0);
            }
            let (d, f, l) = counts(model);
            let key = if l > 0 {
                "ui-filer-stats-links"
            } else {
                "ui-filer-stats"
            };
            let (n, d, f, l) = (n.to_string(), d.to_string(), f.to_string(), l.to_string());
            tf(
                key,
                &[
                    ("total", n.as_str()),
                    ("dirs", d.as_str()),
                    ("files", f.as_str()),
                    ("links", l.as_str()),
                ],
            )
        }
    }
}

/// 日期（年-月-日，本地时区）；未知显示破折号。
pub fn date_text(ms: Option<i64>, tz_offset_min: i32) -> String {
    ms.map_or_else(
        || "—".to_owned(),
        |ms| format_mtime(ms, tz_offset_min).chars().take(10).collect(),
    )
}

/// 日期加时分；未知显示破折号。
pub fn datetime_text(ms: Option<i64>, tz_offset_min: i32) -> String {
    ms.map_or_else(|| "—".to_owned(), |ms| format_mtime(ms, tz_offset_min))
}

/// 大小列：目录显示破折号；读不到的大小显示破折号，不冒充 0 字节。
pub fn size_text(row: &Row) -> String {
    if row.is_dir || !row.size_known() {
        "—".to_owned()
    } else {
        format_size(row.size)
    }
}

/// 悬停说明：完整路径、类型；普通文件附大小；有权限时显示八进制权限，属主 / 属组缺失不编造。
pub fn row_tooltip(row: &Row) -> String {
    let kind = if row.is_symlink {
        t("ui-filer-kind-link")
    } else if row.is_dir {
        t("ui-filer-kind-dir")
    } else {
        t("ui-filer-kind-file")
    };
    let mut lines = vec![
        row.path.clone(),
        tf("ui-filer-tip-kind", &[("kind", &kind)]),
    ];
    if !row.is_dir && !row.is_symlink {
        lines.push(tf("ui-filer-tip-size", &[("size", &size_text(row))]));
    }
    if let Some(p) = row.permissions {
        lines.push(tf(
            "ui-filer-tip-perm",
            &[("perm", &format!("{:04o}", p & 0o7777))],
        ));
    }
    match (&row.owner, &row.group) {
        (Some(o), Some(g)) => {
            lines.push(tf("ui-filer-tip-owner", &[("owner", &format!("{o}:{g}"))]))
        }
        (Some(o), None) => lines.push(tf("ui-filer-tip-owner", &[("owner", o)])),
        (None, Some(g)) => lines.push(tf("ui-filer-tip-group", &[("group", g)])),
        (None, None) => {}
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filer::Sort;
    use kanso_types::fs::DirEntry;

    fn entry(name: &str, dir: bool, link: bool) -> DirEntry {
        DirEntry {
            name: name.into(),
            path: format!("/x/{name}"),
            is_dir: dir,
            is_symlink: link,
            size: 3,
            mtime_ms: Some(0),
            permissions: Some(0o644),
            owner: None,
            group: None,
        }
    }

    fn model(names: &[&str]) -> DirModel {
        let mut m = DirModel::new(Sort::default());
        m.push_batch(names.iter().map(|n| entry(n, false, false)).collect());
        m
    }

    fn selected_names(sel: &Selection, m: &DirModel) -> Vec<String> {
        sel.ids()
            .iter()
            .map(|&i| m.row(i).unwrap().name.clone())
            .collect()
    }

    #[test]
    fn click_ctrl_and_shift_selection() {
        let m = model(&["a", "b", "c", "d", "e"]);
        let mut s = Selection::default();
        s.click(1, &m, false, false);
        assert_eq!(selected_names(&s, &m), vec!["b"]);
        s.click(3, &m, true, false);
        assert_eq!(selected_names(&s, &m), vec!["b", "d"]);
        s.click(1, &m, true, false);
        assert_eq!(selected_names(&s, &m), vec!["d"]);
        s.click(3, &m, false, false);
        // Shift 从锚点（d）选到 b。
        s.click(1, &m, false, true);
        assert_eq!(selected_names(&s, &m), vec!["b", "c", "d"]);
        // 连续 Shift 仍以同一锚点为起点。
        s.click(4, &m, false, true);
        assert_eq!(selected_names(&s, &m), vec!["d", "e"]);
    }

    #[test]
    fn keyboard_navigation_and_shift_extend() {
        let m = model(&["a", "b", "c", "d"]);
        let mut s = Selection::default();
        assert_eq!(s.step(1, &m, false), Some(0));
        assert_eq!(s.step(1, &m, false), Some(1));
        assert_eq!(s.step(10, &m, false), Some(3));
        assert_eq!(s.step(-1, &m, true), Some(2));
        assert_eq!(selected_names(&s, &m), vec!["c", "d"]);
        let mut s = Selection::default();
        assert_eq!(s.step(-1, &m, false), Some(3));
        s.select_all(&m);
        assert_eq!(s.len(), 4);
        assert_eq!(
            Selection::default().step(1, &DirModel::default(), false),
            None
        );
    }

    #[test]
    fn right_click_keeps_a_multi_selection_it_lands_in() {
        let m = model(&["a", "b", "c"]);
        let mut s = Selection::default();
        s.click(0, &m, false, false);
        s.click(1, &m, true, false);
        let (c_id, _) = m.visible_at(2).unwrap();
        let (b_id, _) = m.visible_at(1).unwrap();
        s.right_click(b_id);
        assert_eq!(s.len(), 2);
        s.right_click(c_id);
        assert_eq!(selected_names(&s, &m), vec!["c"]);
    }

    #[test]
    fn selection_follows_ids_when_earlier_rows_arrive() {
        let mut m = model(&["m", "n"]);
        let mut s = Selection::default();
        s.click(0, &m, false, false);
        let id = s.cursor().unwrap();
        m.push_batch(vec![entry("a", false, false)]);
        // 前面插入了新条目，选中的仍是原来的 m。
        assert_eq!(m.row(id).unwrap().name, "m");
        assert_eq!(selected_names(&s, &m), vec!["m"]);
        assert_eq!(m.visible_position_of(id), Some(1));
    }

    #[test]
    fn retain_visible_drops_filtered_rows() {
        let mut m = model(&[".a", "b"]);
        let mut s = Selection::default();
        s.select_all(&m);
        m.set_hide_hidden(true);
        s.retain_visible(&m);
        assert_eq!(selected_names(&s, &m), vec!["b"]);
    }

    #[test]
    fn only_the_latest_request_is_adopted() {
        let mut b = Browse::default();
        let g1 = b.begin("/one");
        let g2 = b.begin("/two");
        // 旧请求的数据、完成、失败都被丢弃。
        assert_eq!(b.on_data(g1), None);
        assert_eq!(b.on_done(g1), None);
        assert!(!b.on_failed(g1, "x".into()));
        assert_eq!(b.phase, Phase::Loading);
        assert_eq!(b.on_data(g2), Some(true));
        assert_eq!(b.path, "/two");
        assert_eq!(b.on_data(g2), Some(false));
        assert_eq!(b.on_done(g2), Some(false));
        assert_eq!(b.phase, Phase::Ready);
        // 完成后的重复事件无效。
        assert_eq!(b.on_done(g2), None);
    }

    #[test]
    fn empty_directory_clears_the_old_list_on_done() {
        let mut b = Browse::default();
        let g = b.begin("/empty");
        assert_eq!(b.on_done(g), Some(true));
        assert_eq!(b.path, "/empty");
    }

    #[test]
    fn failure_keeps_the_old_list_under_its_own_path() {
        let mut b = Browse::default();
        let g = b.begin("/good");
        b.on_data(g);
        b.on_done(g);
        let g = b.begin("/secret");
        assert!(b.on_failed(g, "没有权限".into()));
        assert_eq!(b.path, "/good");
        let e = b.error.clone().unwrap();
        assert_eq!(e.target, "/secret");
        assert_eq!(b.phase, Phase::Failed);
        // 部分数据已经替换了列表后失败：列表属于新目录。
        let g = b.begin("/half");
        b.on_data(g);
        assert!(b.on_failed(g, "断开".into()));
        assert_eq!(b.path, "/half");
    }

    #[test]
    fn writes_are_blocked_while_loading_or_disconnected() {
        let mut b = Browse::default();
        assert!(!b.can_write());
        let g = b.begin("/a");
        assert!(!b.can_write());
        b.on_done(g);
        assert!(b.can_write());
        b.on_disconnected();
        assert!(!b.can_write());
        assert_eq!(b.path, "/a");
    }

    #[test]
    fn stale_locate_results_are_ignored() {
        let mut l = Locator::default();
        let s1 = SessionId::from_raw(1);
        let s2 = SessionId::from_raw(2);
        let t1 = l.begin(s1);
        let t2 = l.begin(s2);
        assert!(l.is_current(s1, t1) && l.is_current(s2, t2));
        let t1b = l.begin(s1);
        assert!(!l.is_current(s1, t1));
        assert!(l.is_current(s1, t1b));
        l.forget(s2);
        assert!(!l.is_current(s2, t2));
    }

    #[test]
    fn foreign_hosts_are_not_local() {
        assert!(cwd_host_is_local(None, &[]));
        assert!(cwd_host_is_local(Some("LOCALHOST"), &[]));
        assert!(cwd_host_is_local(Some("my-pc"), &["MY-PC"]));
        assert!(!cwd_host_is_local(Some("prod.example.com"), &["my-pc"]));
    }

    #[test]
    fn stats_distinguish_loading_from_done() {
        let mut m = DirModel::new(Sort::default());
        m.push_batch(vec![
            entry("d", true, false),
            entry("f", false, false),
            entry("l", false, true),
        ]);
        assert_eq!(stats_text(&m, &Phase::Loading), "正在读取… 已收到 3 项");
        assert_eq!(
            stats_text(&m, &Phase::Ready),
            "3 项：1 个目录，1 个文件，1 个链接"
        );
        assert_eq!(stats_text(&DirModel::default(), &Phase::Ready), "0 项");
    }

    #[test]
    fn unknown_values_are_dashes_not_zero() {
        let unknown = Row::from(DirEntry {
            size: 0,
            mtime_ms: None,
            ..entry("x", false, false)
        });
        assert_eq!(size_text(&unknown), "—");
        assert_eq!(date_text(None, 0), "—");
        assert_eq!(date_text(Some(0), 0), "1970-01-01");
        assert_eq!(datetime_text(Some(0), 0), "1970-01-01 00:00");
    }

    #[test]
    fn tooltip_does_not_invent_owner() {
        let r = Row::from(entry("a", false, false));
        let t = row_tooltip(&r);
        assert!(t.contains("/x/a") && t.contains("类型：文件") && t.contains("权限：0644"));
        assert!(!t.contains("属主"));
    }
}
