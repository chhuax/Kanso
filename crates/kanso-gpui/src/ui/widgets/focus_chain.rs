//! Tab / Shift+Tab 焦点链（GPUI 一侧）。
//!
//! gpui 没有全局 Tab 键绑定，`window.focus_next()` 又依赖整窗的 tab-stop 登记；
//! 控件窗口里统一由宿主视图在按键处理里调用 [`handle_tab`]：按宿主登记的有序
//! `FocusHandle` 列表前进 / 后退，跳过 `tab_stop == false`（禁用）的项，首尾循环。
//! 顺序逻辑在 `kanso_gpui::widgets::focus_chain`（可测）。

use gpui::{FocusHandle, KeyDownEvent, Window};
use kanso_gpui::widgets::focus_chain::step;

/// 处理一次按键：若是 Tab / Shift+Tab（不带 Ctrl / Alt / Win）则移动焦点并返回 `true`
/// （调用方应 `cx.stop_propagation()`）；否则返回 `false`。
/// `handles` 必须是当前的句柄（禁用状态取自句柄的 `tab_stop`）。
pub(crate) fn handle_tab(handles: &[FocusHandle], ev: &KeyDownEvent, window: &mut Window) -> bool {
    let m = &ev.keystroke.modifiers;
    if ev.keystroke.key != "tab" || m.control || m.alt || m.platform {
        return false;
    }
    focus_step(handles, !m.shift, window);
    true
}

/// 把焦点移到链上的下一 / 上一个可停靠项。
pub(crate) fn focus_step(handles: &[FocusHandle], forward: bool, window: &mut Window) {
    let enabled: Vec<bool> = handles.iter().map(|h| h.tab_stop).collect();
    let cur = handles.iter().position(|h| h.is_focused(window));
    if let Some(i) = step(&enabled, cur, forward) {
        window.focus(&handles[i]);
    }
}
