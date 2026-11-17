//! 命令补全弹窗（terminal.md §14、completion.md）的纯逻辑，对照 mac 版 `Shell/CompletionModel.swift`
//! 与 `CompletionController.swift`：请求代次与“只采用最新一次结果”、被动状态下的键盘状态机、
//! 弹窗尺寸与位置、开关设置、本机主机判断、Kubernetes 配置文件位置。
//!
//! 候选内容（历史、路径、内置词汇、Kubernetes 名称）完全由核心的 `Core::complete` 产生，这里不重复。
//! GPUI 一侧的接线（读取终端状态、异步取目录、画弹窗）在 `ui/shell_completion.rs`。
//!
//! 本文件不依赖窗口系统，三个平台都能编译与测试。

use std::ffi::OsStr;
use std::path::PathBuf;

use kanso_core::{CompletionCandidate, CompletionCategory, CompletionSuggestions};

/// 设置表里的键（与 mac 版同一份数据，随 `.kanso` 导出）。
pub const SETTING_KEY: &str = "completion";
/// 弹窗最多显示的候选数（completion.md §2）。
pub const MAX_CANDIDATES: usize = 8;
/// 弹窗每行高度（逻辑像素）。
pub const ROW_HEIGHT: f32 = 30.0;
/// 弹窗上下内边距合计。
pub const POPUP_PADDING: f32 = 12.0;
/// 弹窗最大宽度。
pub const POPUP_MAX_WIDTH: f32 = 460.0;
/// 弹窗与窗格边缘 / 光标行之间的留白。
pub const POPUP_MARGIN: f32 = 8.0;
/// 本地来源等待上限（毫秒，completion.md D5）。
pub const LOCAL_TIMEOUT_MS: u64 = 300;
/// 远端来源等待上限（毫秒）。
pub const REMOTE_TIMEOUT_MS: u64 = 1500;
/// 同一工作目录结果的复用时间（毫秒）。
pub const CWD_CACHE_MS: u64 = 2000;
/// 同一目录列表的复用时间（毫秒）。
pub const DIRECTORY_CACHE_MS: u64 = 1500;
/// Kubernetes 配置名称的复用时间（毫秒）。
pub const KUBE_CACHE_MS: u64 = 30_000;
/// 单个目录最多读取的条目数。
pub const MAX_DIRECTORY_ENTRIES: usize = 2000;

// ---- 开关设置 ----

/// 解码设置 JSON：缺失、损坏或类型不对都当作关闭（默认关闭）。
pub fn decode_enabled(json: Option<&str>) -> bool {
    json.and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
        .and_then(|v| v.get("enabled").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

/// 编码设置 JSON（键序固定，与 mac 版 `{"enabled":…,"v":1}` 一致）。
pub fn encode_enabled(enabled: bool) -> String {
    format!("{{\"enabled\":{enabled},\"v\":1}}")
}

// ---- 键盘状态机 ----

/// 弹窗关心的按键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// ↓。
    Down,
    /// ↑。
    Up,
    /// Esc。
    Escape,
    /// Tab。
    Tab,
    /// 其他按键。
    Other,
}

/// 按键处理结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// 照常发给 shell。
    Forward,
    /// 弹窗吃掉这个键。
    Consume,
    /// 接受某一项。
    Accept(CompletionCandidate),
}

/// 弹窗状态：请求代次、候选、选中项与“正在等待结果”。
#[derive(Clone, Debug, Default)]
pub struct CompletionModel {
    generation: u64,
    candidates: Vec<CompletionCandidate>,
    selected: Option<usize>,
    waiting: bool,
}

impl CompletionModel {
    /// 当前代次；异步结果携带它回来，不等于当前代次的一律丢弃。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 当前候选（最多 8 条）。
    pub fn candidates(&self) -> &[CompletionCandidate] {
        &self.candidates
    }

    /// 选中项（被动状态下为 `None`，只有用户按 ↓ 才进入列表）。
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// 是否有请求尚未返回（此时 Tab 要被扣住，不交给 shell）。
    pub fn waiting(&self) -> bool {
        self.waiting
    }

    /// 弹窗是否应该显示。
    pub fn visible(&self) -> bool {
        !self.candidates.is_empty()
    }

    /// 作废当前的一切（输入变了、提交、失焦、会话结束等）。
    pub fn invalidate(&mut self) {
        self.generation += 1;
        self.candidates.clear();
        self.selected = None;
        self.waiting = false;
    }

    /// 开始一次新请求，返回它的代次。
    pub fn begin(&mut self) -> u64 {
        self.invalidate();
        self.waiting = true;
        self.generation
    }

    /// 收到某次请求的结果；过时的忽略。`finished` 为假表示还在等外部来源。
    pub fn receive(
        &mut self,
        values: Vec<CompletionCandidate>,
        generation: u64,
        finished: bool,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.candidates = values.into_iter().take(MAX_CANDIDATES).collect();
        self.selected = None;
        self.waiting = !finished;
        true
    }

    /// 弹窗处于被动状态时的按键：只有 ↓ 进入列表，Esc 关闭，Tab 选第一项或接受当前项（terminal.md §14）。
    /// 输入法组字中一律作废并放行。
    pub fn key(&mut self, key: Key, composing: bool) -> Action {
        if composing {
            self.invalidate();
            return Action::Forward;
        }
        match key {
            Key::Escape if self.waiting || !self.candidates.is_empty() => {
                self.invalidate();
                Action::Consume
            }
            Key::Tab => {
                let index = self.selected.unwrap_or(0);
                if let Some(candidate) = self.candidates.get(index).cloned() {
                    self.invalidate();
                    return Action::Accept(candidate);
                }
                if self.waiting {
                    Action::Consume
                } else {
                    Action::Forward
                }
            }
            Key::Down if !self.candidates.is_empty() => {
                let next = self.selected.map_or(0, |s| s + 1);
                self.selected = Some(next.min(self.candidates.len() - 1));
                Action::Consume
            }
            Key::Up if self.selected.is_some() => {
                self.selected = self.selected.map(|s| s.saturating_sub(1));
                Action::Consume
            }
            _ => {
                self.selected = None;
                Action::Forward
            }
        }
    }
}

/// GPUI 按键名（`Keystroke::key`）→ 弹窗按键；带任何修饰键的都算 `Other`（Ctrl+↓ 等属于 shell）。
pub fn key_from_name(name: &str, ctrl: bool, alt: bool, shift: bool, platform: bool) -> Key {
    if ctrl || alt || shift || platform {
        return Key::Other;
    }
    match name {
        "down" => Key::Down,
        "up" => Key::Up,
        "escape" => Key::Escape,
        "tab" => Key::Tab,
        _ => Key::Other,
    }
}

/// 接受候选时要模拟的输入：先退格删掉光标前的 `delete_codepoints` 个码点，再输入替换文本。
pub fn replacement_plan(candidate: &CompletionCandidate) -> (u32, &str) {
    (candidate.delete_codepoints, candidate.replacement.as_str())
}

/// 候选类别的显示文字。
pub fn category_label(category: CompletionCategory) -> &'static str {
    match category {
        CompletionCategory::Directory => "目录",
        CompletionCategory::File => "文件",
        CompletionCategory::History => "历史",
        CompletionCategory::Kubernetes => "Kubernetes",
        CompletionCategory::Docker => "Docker",
        CompletionCategory::Git => "Git",
        CompletionCategory::Systemd => "systemd",
    }
}

/// 核心给出的结果是否还需要外部数据（目录、工作目录、家目录、Kubernetes 名称）。
pub fn needs_external(s: &CompletionSuggestions) -> bool {
    s.directory.is_some() || s.needs_kube_names || s.needs_cwd || s.needs_home
}

/// 外部来源的等待上限。
pub fn timeout_ms(remote: bool) -> u64 {
    if remote {
        REMOTE_TIMEOUT_MS
    } else {
        LOCAL_TIMEOUT_MS
    }
}

// ---- 弹窗几何 ----

/// 弹窗的窗口坐标矩形。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopupRect {
    /// 左上角 x。
    pub x: f32,
    /// 左上角 y。
    pub y: f32,
    /// 宽。
    pub width: f32,
    /// 高。
    pub height: f32,
}

/// 弹窗高度（`count` 行）。
pub fn popup_height(count: usize) -> f32 {
    count as f32 * ROW_HEIGHT + POPUP_PADDING
}

/// 弹窗位置（terminal.md §14）：水平对齐输入起点，垂直放在光标行下方，下方放不下就翻到上方。
/// `bounds` 是终端窗格的窗口坐标 `(x, y, 宽, 高)`；`line_top` / `line_bottom` 是光标行的上下边。
pub fn place_popup(
    anchor_x: f32,
    line_top: f32,
    line_bottom: f32,
    count: usize,
    bounds: (f32, f32, f32, f32),
) -> PopupRect {
    let (bx, by, bw, bh) = bounds;
    let width = POPUP_MAX_WIDTH.min((bw - 16.0).max(80.0));
    let height = popup_height(count);
    let max_x = (bx + bw - width).max(bx);
    let x = anchor_x.clamp(bx, max_x);
    let below = line_bottom + 2.0;
    let y = if below + height <= by + bh || line_top - height - 2.0 < by {
        below
    } else {
        line_top - height - 2.0
    };
    PopupRect {
        x,
        y,
        width,
        height,
    }
}

/// 光标起点的窗口坐标：`origin` 是网格左上角，`start_column` 输入起点列，`cursor_row` 光标行。
/// 返回 `(x, 行上边, 行下边)`。
pub fn anchor_for(
    origin: (f32, f32),
    cell_w: f32,
    cell_h: f32,
    start_column: u16,
    cursor_row: u16,
) -> (f32, f32, f32) {
    let top = origin.1 + f32::from(cursor_row) * cell_h;
    (
        origin.0 + f32::from(start_column) * cell_w,
        top,
        top + cell_h,
    )
}

// ---- 路径来源辅助 ----

/// 目录上报里的主机名是否就是本机（completion.md D1）：空、`localhost`、本机名，或同名加域后缀都算。
pub fn is_local_host(host: &str, local_names: &[String]) -> bool {
    let h = host.trim().trim_end_matches('.').to_lowercase();
    if h.is_empty() || h == "localhost" {
        return true;
    }
    local_names.iter().any(|n| {
        let n = n.trim().to_lowercase();
        !n.is_empty()
            && (h == n
                || h.split('.').next() == Some(n.as_str())
                || n.split('.').next() == Some(h.as_str()))
    })
}

/// Kubernetes 配置文件列表：`KUBECONFIG` 环境变量里的各个文件，否则 `~/.kube/config`。
pub fn kubeconfig_paths(env: Option<&OsStr>, home: Option<&str>) -> Vec<PathBuf> {
    if let Some(value) = env.filter(|v| !v.is_empty()) {
        return std::env::split_paths(value)
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
    }
    home.map(|h| vec![PathBuf::from(h).join(".kube").join("config")])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(text: &str) -> CompletionCandidate {
        CompletionCandidate {
            replacement: text.into(),
            category: CompletionCategory::History,
            delete_codepoints: 2,
        }
    }

    fn loaded(n: usize) -> (CompletionModel, u64) {
        let mut m = CompletionModel::default();
        let g = m.begin();
        m.receive((0..n).map(|i| cand(&format!("c{i}"))).collect(), g, true);
        (m, g)
    }

    #[test]
    fn setting_round_trip_and_defaults() {
        assert!(!decode_enabled(None));
        assert!(!decode_enabled(Some("not json")));
        assert!(!decode_enabled(Some("{\"enabled\":\"yes\"}")));
        assert!(decode_enabled(Some(&encode_enabled(true))));
        assert!(!decode_enabled(Some(&encode_enabled(false))));
        assert_eq!(encode_enabled(true), "{\"enabled\":true,\"v\":1}");
    }

    #[test]
    fn only_the_latest_request_is_adopted() {
        let mut m = CompletionModel::default();
        let old = m.begin();
        let new = m.begin();
        assert!(!m.receive(vec![cand("old")], old, true));
        assert!(m.candidates().is_empty());
        assert!(m.waiting());
        assert!(m.receive(vec![cand("new")], new, true));
        assert_eq!(m.candidates()[0].replacement, "new");
        assert!(!m.waiting());
    }

    #[test]
    fn at_most_eight_candidates_are_kept() {
        let (m, _) = loaded(12);
        assert_eq!(m.candidates().len(), 8);
    }

    #[test]
    fn passive_state_only_down_enters_the_list() {
        let (mut m, _) = loaded(3);
        // 被动状态：↑ 与其他键都交给 shell。
        assert_eq!(m.key(Key::Up, false), Action::Forward);
        assert_eq!(m.key(Key::Other, false), Action::Forward);
        assert_eq!(m.selected(), None);
        assert_eq!(m.key(Key::Down, false), Action::Consume);
        assert_eq!(m.selected(), Some(0));
        m.key(Key::Down, false);
        m.key(Key::Down, false);
        m.key(Key::Down, false);
        assert_eq!(m.selected(), Some(2));
        assert_eq!(m.key(Key::Up, false), Action::Consume);
        assert_eq!(m.selected(), Some(1));
        // 其他键让弹窗回到被动状态。
        assert_eq!(m.key(Key::Other, false), Action::Forward);
        assert_eq!(m.selected(), None);
    }

    #[test]
    fn tab_accepts_first_or_selected() {
        let (mut m, _) = loaded(3);
        assert_eq!(m.key(Key::Tab, false), Action::Accept(cand("c0")));
        assert!(!m.visible());
        let (mut m, _) = loaded(3);
        m.key(Key::Down, false);
        m.key(Key::Down, false);
        assert_eq!(m.key(Key::Tab, false), Action::Accept(cand("c1")));
    }

    #[test]
    fn tab_is_held_back_while_a_request_is_pending_and_forwarded_otherwise() {
        let mut m = CompletionModel::default();
        assert_eq!(m.key(Key::Tab, false), Action::Forward);
        m.begin();
        assert_eq!(m.key(Key::Tab, false), Action::Consume);
        // 没有弹窗也没有请求时 Esc 交给 shell。
        let mut idle = CompletionModel::default();
        assert_eq!(idle.key(Key::Escape, false), Action::Forward);
    }

    #[test]
    fn escape_closes_and_composing_invalidates() {
        let (mut m, g) = loaded(2);
        assert_eq!(m.key(Key::Escape, false), Action::Consume);
        assert!(!m.visible());
        assert!(m.generation() > g);
        let (mut m, _) = loaded(2);
        assert_eq!(m.key(Key::Down, true), Action::Forward);
        assert!(!m.visible());
    }

    #[test]
    fn modified_keys_are_not_completion_keys() {
        assert_eq!(key_from_name("down", false, false, false, false), Key::Down);
        assert_eq!(key_from_name("down", true, false, false, false), Key::Other);
        assert_eq!(key_from_name("tab", false, false, true, false), Key::Other);
        assert_eq!(key_from_name("a", false, false, false, false), Key::Other);
    }

    #[test]
    fn replacement_plan_counts_codepoints_to_delete() {
        let c = cand("git status");
        assert_eq!(replacement_plan(&c), (2, "git status"));
    }

    #[test]
    fn popup_prefers_below_and_flips_when_it_does_not_fit() {
        let bounds = (0.0, 0.0, 800.0, 600.0);
        let r = place_popup(100.0, 100.0, 120.0, 3, bounds);
        assert_eq!((r.x, r.y), (100.0, 122.0));
        assert_eq!(r.height, 3.0 * ROW_HEIGHT + POPUP_PADDING);
        assert_eq!(r.width, POPUP_MAX_WIDTH);
        // 光标在底部：翻到上方。
        let r = place_popup(100.0, 560.0, 580.0, 4, bounds);
        assert_eq!(r.y, 560.0 - r.height - 2.0);
        // 上下都放不下时留在下方（不越过窗格顶部）。
        let tiny = (0.0, 0.0, 800.0, 100.0);
        let r = place_popup(0.0, 50.0, 70.0, 8, tiny);
        assert_eq!(r.y, 72.0);
        // 水平方向夹在窗格内，窄窗格缩窄弹窗。
        let r = place_popup(790.0, 0.0, 20.0, 1, bounds);
        assert_eq!(r.x + r.width, 800.0);
        let r = place_popup(0.0, 0.0, 20.0, 1, (0.0, 0.0, 300.0, 300.0));
        assert_eq!(r.width, 284.0);
    }

    #[test]
    fn anchor_follows_the_cursor_cell() {
        let (x, top, bottom) = anchor_for((10.0, 20.0), 8.0, 16.0, 5, 3);
        assert_eq!((x, top, bottom), (50.0, 68.0, 84.0));
    }

    #[test]
    fn local_host_detection() {
        let names = vec!["DESKTOP-1".to_owned()];
        assert!(is_local_host("", &names));
        assert!(is_local_host("LocalHost", &names));
        assert!(is_local_host("desktop-1", &names));
        assert!(is_local_host("desktop-1.lan", &names));
        assert!(is_local_host("desktop-1.", &names));
        assert!(!is_local_host("prod-db", &names));
        assert!(!is_local_host("desktop-10", &names));
    }

    #[test]
    fn kubeconfig_sources() {
        let joined = std::env::join_paths(["/a/config", "/b/config"]).unwrap();
        assert_eq!(
            kubeconfig_paths(Some(joined.as_os_str()), Some("/home/u")),
            vec![PathBuf::from("/a/config"), PathBuf::from("/b/config")]
        );
        assert_eq!(
            kubeconfig_paths(None, Some("/home/u")),
            vec![PathBuf::from("/home/u").join(".kube").join("config")]
        );
        assert_eq!(
            kubeconfig_paths(Some(OsStr::new("")), Some("/h")),
            vec![PathBuf::from("/h").join(".kube").join("config")]
        );
        assert!(kubeconfig_paths(None, None).is_empty());
    }

    #[test]
    fn external_needs_and_timeouts() {
        let mut s = CompletionSuggestions::default();
        assert!(!needs_external(&s));
        s.needs_cwd = true;
        assert!(needs_external(&s));
        assert_eq!(timeout_ms(false), 300);
        assert_eq!(timeout_ms(true), 1500);
        assert_eq!(category_label(CompletionCategory::Directory), "目录");
    }
}
