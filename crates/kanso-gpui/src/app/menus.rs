//! 菜单内容（ui-shell.md §6、terminal.md §8.1、ui-shell.md §2 右键标签行）：终端右键菜单、标签右键菜单、
//! 窗口级应用菜单。每一项的 `id` 都由 [`encode`] 从 [`AppCommand`] 生成、[`decode`] 还原，
//! 视图收到 `MenuEvent::Activate(id)` 后解码成命令交给 `run_command`，菜单与快捷键走同一条路径。
//!
//! 本文件不依赖窗口系统，三个平台都能编译与测试。

use kanso_types::SessionId;

use crate::app::shortcuts::{AppCommand, shortcut_text};
use crate::app::workspace::CloseScope;
use crate::widgets::menu::MenuItem;

/// 命令 → 菜单项 `id`。
pub fn encode(command: AppCommand) -> String {
    match command {
        AppCommand::NewLocalTab => "new-local-tab".into(),
        AppCommand::CloseSession => "close-session".into(),
        AppCommand::NextTab => "next-tab".into(),
        AppCommand::PrevTab => "prev-tab".into(),
        AppCommand::NextPane => "next-pane".into(),
        AppCommand::PrevPane => "prev-pane".into(),
        AppCommand::SplitRight => "split-right".into(),
        AppCommand::SplitDown => "split-down".into(),
        AppCommand::SelectTab(i) => format!("select-tab:{i}"),
        AppCommand::ToggleSidebar => "toggle-sidebar".into(),
        AppCommand::Copy => "copy".into(),
        AppCommand::Paste => "paste".into(),
        AppCommand::SelectAll => "select-all".into(),
        AppCommand::ClearBuffer => "clear-buffer".into(),
        AppCommand::CloseTab(s) => format!("close-tab:{}", s.raw()),
        AppCommand::CloseGroup(scope, s) => {
            format!("close-group:{}:{}", scope_name(scope), s.raw())
        }
        AppCommand::EditSession(s) => format!("edit-session:{}", s.raw()),
        AppCommand::ToggleRightClickMenu => "toggle-right-click-menu".into(),
        AppCommand::Quit => "quit".into(),
        AppCommand::OpenLauncher => "open-launcher".into(),
        AppCommand::OpenSettings => "open-settings".into(),
    }
}

fn scope_name(scope: CloseScope) -> &'static str {
    match scope {
        CloseScope::Others => "others",
        CloseScope::Above => "above",
        CloseScope::Below => "below",
        CloseScope::All => "all",
    }
}

fn scope_from(name: &str) -> Option<CloseScope> {
    Some(match name {
        "others" => CloseScope::Others,
        "above" => CloseScope::Above,
        "below" => CloseScope::Below,
        "all" => CloseScope::All,
        _ => return None,
    })
}

/// 菜单项 `id` → 命令；不认识的返回 `None`。
pub fn decode(id: &str) -> Option<AppCommand> {
    let session = |s: &str| s.parse::<u64>().ok().map(SessionId::from_raw);
    if let Some(rest) = id.strip_prefix("select-tab:") {
        return rest.parse().ok().map(AppCommand::SelectTab);
    }
    if let Some(rest) = id.strip_prefix("close-tab:") {
        return session(rest).map(AppCommand::CloseTab);
    }
    if let Some(rest) = id.strip_prefix("edit-session:") {
        return session(rest).map(AppCommand::EditSession);
    }
    if let Some(rest) = id.strip_prefix("close-group:") {
        let (scope, sid) = rest.split_once(':')?;
        return Some(AppCommand::CloseGroup(scope_from(scope)?, session(sid)?));
    }
    Some(match id {
        "new-local-tab" => AppCommand::NewLocalTab,
        "close-session" => AppCommand::CloseSession,
        "next-tab" => AppCommand::NextTab,
        "prev-tab" => AppCommand::PrevTab,
        "next-pane" => AppCommand::NextPane,
        "prev-pane" => AppCommand::PrevPane,
        "split-right" => AppCommand::SplitRight,
        "split-down" => AppCommand::SplitDown,
        "toggle-sidebar" => AppCommand::ToggleSidebar,
        "copy" => AppCommand::Copy,
        "paste" => AppCommand::Paste,
        "select-all" => AppCommand::SelectAll,
        "clear-buffer" => AppCommand::ClearBuffer,
        "toggle-right-click-menu" => AppCommand::ToggleRightClickMenu,
        "quit" => AppCommand::Quit,
        "open-launcher" => AppCommand::OpenLauncher,
        "open-settings" => AppCommand::OpenSettings,
        _ => return None,
    })
}

/// 菜单里显示的快捷键文字：先查应用级键位表；复制 / 粘贴 / 全选由终端按键路径处理（不在表里），写死默认值。
pub fn menu_shortcut(command: AppCommand) -> Option<String> {
    if let Some(t) = shortcut_text(command) {
        return Some(t);
    }
    match command {
        AppCommand::Copy => Some("Ctrl+Shift+C".into()),
        AppCommand::Paste => Some("Ctrl+Shift+V".into()),
        AppCommand::SelectAll => Some("Ctrl+Shift+A".into()),
        _ => None,
    }
}

/// 带快捷键提示的动作项。
fn item(label: &str, command: AppCommand) -> MenuItem {
    let it = MenuItem::action(encode(command), label);
    match menu_shortcut(command) {
        Some(t) => it.shortcut(t),
        None => it,
    }
}

/// 终端右键菜单（terminal.md §8.1：复制、粘贴、全选、清除缓冲区；再加拆分）。
/// 没有选区时“复制”禁用。（“在 Filer 中显示工作目录”等 Filer 接进外壳后再加。）
pub fn terminal_menu(has_selection: bool) -> Vec<MenuItem> {
    let copy = item("复制", AppCommand::Copy);
    vec![
        if has_selection { copy } else { copy.disabled() },
        item("粘贴", AppCommand::Paste),
        item("全选", AppCommand::SelectAll),
        item("清除缓冲区", AppCommand::ClearBuffer),
        MenuItem::separator(),
        item("向右拆分", AppCommand::SplitRight),
        item("向下拆分", AppCommand::SplitDown),
    ]
}

/// 标签右键菜单所需的上下文。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabMenuContext {
    /// 目标标签。
    pub session: SessionId,
    /// 标签来自已保存的会话（才显示“编辑会话”）。
    pub from_profile: bool,
    /// 同组里是否有可关闭的“其他”标签。
    pub has_others: bool,
    /// 同组里它上方（菜单叫左侧）是否有标签。
    pub has_above: bool,
    /// 同组里它下方（右侧）是否有标签。
    pub has_below: bool,
}

/// 标签右键菜单（ui-shell.md §2.4）：编辑已保存的会话（仅来自已保存会话的标签）、关闭、
/// 关闭其他 / 左侧 / 右侧 / 全部（只在同一分屏组内生效，没有可关闭的标签时禁用）。
pub fn tab_menu(ctx: TabMenuContext) -> Vec<MenuItem> {
    let s = ctx.session;
    let group = |label: &str, scope: CloseScope, enabled: bool| {
        let it = MenuItem::action(encode(AppCommand::CloseGroup(scope, s)), label);
        if enabled { it } else { it.disabled() }
    };
    let mut items = Vec::new();
    if ctx.from_profile {
        items.push(MenuItem::action(
            encode(AppCommand::EditSession(s)),
            "编辑会话…",
        ));
        items.push(MenuItem::separator());
    }
    items.push(MenuItem::action(encode(AppCommand::CloseTab(s)), "关闭"));
    items.push(MenuItem::separator());
    items.push(group("关闭其他", CloseScope::Others, ctx.has_others));
    items.push(group("关闭左侧", CloseScope::Above, ctx.has_above));
    items.push(group("关闭右侧", CloseScope::Below, ctx.has_below));
    items.push(group("全部关闭", CloseScope::All, true));
    items
}

/// 应用菜单的开关状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppMenuState {
    /// 标签栏是否显示。
    pub sidebar_visible: bool,
    /// 右键是否弹菜单（否则为“复制或粘贴”）。
    pub right_click_menu: bool,
}

/// 窗口级应用菜单（Windows / Linux 没有系统菜单栏，spec §6 末段：放在应用内）。
/// 结构按 §6：会话 / 编辑 / 视图，外加设置与退出；还没有实现的功能（新建会话、设置）照常显示，
/// 命令交给对应模块处理。
pub fn app_menu(state: AppMenuState) -> Vec<MenuItem> {
    vec![
        MenuItem::submenu(
            "会话",
            vec![
                item("新建会话…", AppCommand::OpenLauncher),
                item("新建本地 Shell", AppCommand::NewLocalTab),
                MenuItem::separator(),
                item("上一个标签", AppCommand::PrevTab),
                item("下一个标签", AppCommand::NextTab),
                item("上一个分屏", AppCommand::PrevPane),
                item("下一个分屏", AppCommand::NextPane),
                MenuItem::separator(),
                item("关闭会话", AppCommand::CloseSession),
            ],
        ),
        MenuItem::submenu(
            "编辑",
            vec![
                item("复制", AppCommand::Copy),
                item("粘贴", AppCommand::Paste),
                item("全选", AppCommand::SelectAll),
                MenuItem::separator(),
                item("清除缓冲区", AppCommand::ClearBuffer),
                MenuItem::separator(),
                item("右键弹出菜单", AppCommand::ToggleRightClickMenu)
                    .checked(state.right_click_menu),
            ],
        ),
        MenuItem::submenu(
            "视图",
            vec![
                item("标签栏", AppCommand::ToggleSidebar).checked(state.sidebar_visible),
                MenuItem::separator(),
                item("向右拆分", AppCommand::SplitRight),
                item("向下拆分", AppCommand::SplitDown),
            ],
        ),
        MenuItem::separator(),
        item("设置…", AppCommand::OpenSettings),
        item("退出", AppCommand::Quit),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(n: u64) -> SessionId {
        SessionId::from_raw(n)
    }

    /// 递归收集所有动作项（id, enabled, label, checked）。
    fn actions(items: &[MenuItem], out: &mut Vec<(String, bool, String, bool)>) {
        for it in items {
            match it {
                MenuItem::Action {
                    id,
                    enabled,
                    label,
                    checked,
                    ..
                } => out.push((id.clone(), *enabled, label.clone(), *checked)),
                MenuItem::Submenu { items, .. } => actions(items, out),
                MenuItem::Separator => {}
            }
        }
    }

    fn all(items: &[MenuItem]) -> Vec<(String, bool, String, bool)> {
        let mut v = Vec::new();
        actions(items, &mut v);
        v
    }

    #[test]
    fn encode_decode_round_trip() {
        let cmds = [
            AppCommand::NewLocalTab,
            AppCommand::CloseSession,
            AppCommand::NextTab,
            AppCommand::PrevTab,
            AppCommand::NextPane,
            AppCommand::PrevPane,
            AppCommand::SplitRight,
            AppCommand::SplitDown,
            AppCommand::SelectTab(4),
            AppCommand::ToggleSidebar,
            AppCommand::Copy,
            AppCommand::Paste,
            AppCommand::SelectAll,
            AppCommand::ClearBuffer,
            AppCommand::CloseTab(sid(7)),
            AppCommand::CloseGroup(CloseScope::Others, sid(7)),
            AppCommand::CloseGroup(CloseScope::Above, sid(8)),
            AppCommand::CloseGroup(CloseScope::Below, sid(9)),
            AppCommand::CloseGroup(CloseScope::All, sid(10)),
            AppCommand::EditSession(sid(3)),
            AppCommand::ToggleRightClickMenu,
            AppCommand::Quit,
            AppCommand::OpenLauncher,
            AppCommand::OpenSettings,
        ];
        for c in cmds {
            assert_eq!(decode(&encode(c)), Some(c), "{c:?}");
        }
        assert_eq!(decode("nope"), None);
        assert_eq!(decode("close-tab:x"), None);
        assert_eq!(decode("close-group:weird:1"), None);
    }

    #[test]
    fn every_menu_item_id_decodes() {
        let ctx = TabMenuContext {
            session: sid(1),
            from_profile: true,
            has_others: true,
            has_above: true,
            has_below: true,
        };
        let state = AppMenuState {
            sidebar_visible: true,
            right_click_menu: true,
        };
        for items in [terminal_menu(true), tab_menu(ctx), app_menu(state)] {
            for (id, _, label, _) in all(&items) {
                assert!(decode(&id).is_some(), "{label} 的 id {id} 无法解码");
            }
        }
    }

    #[test]
    fn terminal_menu_copy_needs_a_selection() {
        let copy = |sel| {
            all(&terminal_menu(sel))
                .into_iter()
                .find(|a| a.0 == "copy")
                .map(|a| a.1)
        };
        assert_eq!(copy(false), Some(false));
        assert_eq!(copy(true), Some(true));
        let ids: Vec<_> = all(&terminal_menu(true)).into_iter().map(|a| a.0).collect();
        for want in [
            "paste",
            "select-all",
            "clear-buffer",
            "split-right",
            "split-down",
        ] {
            assert!(ids.iter().any(|i| i == want), "缺少 {want}");
        }
    }

    #[test]
    fn tab_menu_follows_the_spec() {
        let base = TabMenuContext {
            session: sid(5),
            from_profile: false,
            has_others: false,
            has_above: false,
            has_below: false,
        };
        let a = all(&tab_menu(base));
        // 没有已保存会话：不出现“编辑”。
        assert!(!a.iter().any(|x| x.0.starts_with("edit-session")));
        let enabled = |id: &str| a.iter().find(|x| x.0 == id).map(|x| x.1);
        assert_eq!(enabled("close-tab:5"), Some(true));
        // 没有可关闭的标签时禁用，全部关闭始终可用（至少关自己）。
        assert_eq!(enabled("close-group:others:5"), Some(false));
        assert_eq!(enabled("close-group:above:5"), Some(false));
        assert_eq!(enabled("close-group:below:5"), Some(false));
        assert_eq!(enabled("close-group:all:5"), Some(true));

        let b = all(&tab_menu(TabMenuContext {
            from_profile: true,
            has_others: true,
            has_below: true,
            ..base
        }));
        assert!(b.iter().any(|x| x.0 == "edit-session:5"));
        let enabled = |id: &str| b.iter().find(|x| x.0 == id).map(|x| x.1);
        assert_eq!(enabled("close-group:others:5"), Some(true));
        assert_eq!(enabled("close-group:above:5"), Some(false));
        assert_eq!(enabled("close-group:below:5"), Some(true));
    }

    #[test]
    fn app_menu_has_the_required_entries_and_check_states() {
        let items = app_menu(AppMenuState {
            sidebar_visible: false,
            right_click_menu: true,
        });
        let flat = all(&items);
        for want in [
            "open-launcher",
            "new-local-tab",
            "close-session",
            "copy",
            "paste",
            "select-all",
            "clear-buffer",
            "toggle-sidebar",
            "split-right",
            "open-settings",
            "quit",
        ] {
            assert!(flat.iter().any(|a| a.0 == want), "缺少 {want}");
        }
        let checked = |id: &str| flat.iter().find(|a| a.0 == id).map(|a| a.3);
        assert_eq!(checked("toggle-sidebar"), Some(false));
        assert_eq!(checked("toggle-right-click-menu"), Some(true));
    }

    #[test]
    fn shortcut_hints_come_from_the_binding_table() {
        assert_eq!(
            menu_shortcut(AppCommand::NewLocalTab).as_deref(),
            Some("Ctrl+Shift+T")
        );
        assert_eq!(
            menu_shortcut(AppCommand::Copy).as_deref(),
            Some("Ctrl+Shift+C")
        );
        assert_eq!(
            menu_shortcut(AppCommand::ClearBuffer).as_deref(),
            Some("Alt+K")
        );
        assert_eq!(menu_shortcut(AppCommand::Quit), None);
    }
}
