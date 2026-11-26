//! 菜单内容（ui-shell.md §6、terminal.md §8.1、ui-shell.md §2 右键标签行）：终端右键菜单、标签右键菜单、
//! 窗口级应用菜单。每一项的 `id` 都由 [`encode`] 从 [`AppCommand`] 生成、[`decode`] 还原，
//! 视图收到 `MenuEvent::Activate(id)` 后解码成命令交给 `run_command`，菜单与快捷键走同一条路径。
//!
//! 本文件不依赖窗口系统，三个平台都能编译与测试。

use crate::app::i18n::label;
use kanso_types::SessionId;
use kanso_types::message::Message;

use crate::app::sender::t;

use crate::app::blocks::BlockMenu;
use crate::app::filer_text::msg;
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
        AppCommand::RenameTab(s) => format!("rename-tab:{}", s.raw()),
        AppCommand::EditSession(s) => format!("edit-session:{}", s.raw()),
        AppCommand::ReconnectTab(s) => format!("reconnect-tab:{}", s.raw()),
        AppCommand::CopyMcpTab(s) => format!("copy-mcp-tab:{}", s.raw()),
        AppCommand::ToggleRightClickMenu => "toggle-right-click-menu".into(),
        AppCommand::ToggleCopyOnSelect => "toggle-copy-on-select".into(),
        AppCommand::Quit => "quit".into(),
        AppCommand::OpenPalette => "open-palette".into(),
        AppCommand::OpenLauncher => "open-launcher".into(),
        AppCommand::OpenSettings => "open-settings".into(),
        AppCommand::OpenDisplaySettings => "open-display-settings".into(),
        AppCommand::OpenKeyboardSettings => "open-keyboard-settings".into(),
        AppCommand::NewSession => "new-session".into(),
        AppCommand::ExportData => "export-data".into(),
        AppCommand::ImportData => "import-data".into(),
        AppCommand::ImportSshConfig => "import-ssh-config".into(),
        AppCommand::ZoomIn => "zoom-in".into(),
        AppCommand::ZoomOut => "zoom-out".into(),
        AppCommand::ZoomReset => "zoom-reset".into(),
        AppCommand::Find => "find".into(),
        AppCommand::FindNext => "find-next".into(),
        AppCommand::FindPrev => "find-prev".into(),
        AppCommand::PrevBlock => "prev-block".into(),
        AppCommand::NextBlock => "next-block".into(),
        AppCommand::SetThemeMode(mode) => format!("appearance:{}", mode.storage()),
        AppCommand::ToggleTimeline => "toggle-timeline".into(),
        AppCommand::SetGutterMode(mode) => format!("gutter:{}", mode.storage()),
        AppCommand::CopyBlockCommand => "copy-block-command".into(),
        AppCommand::CopyBlockOutput => "copy-block-output".into(),
        AppCommand::ToggleSender => "toggle-sender".into(),
        AppCommand::ToggleCommandSuggestions => "toggle-command-suggestions".into(),
        AppCommand::ClearCommandHistory => "clear-command-history".into(),
        AppCommand::ToggleFiler => "toggle-filer".into(),
        AppCommand::ToggleGit => "toggle-git".into(),
        AppCommand::LocateFiler => "locate-filer".into(),
        AppCommand::XmodemSend => "xmodem-send".into(),
        AppCommand::XmodemSend1K => "xmodem-send-1k".into(),
        AppCommand::XmodemReceive => "xmodem-receive".into(),
        AppCommand::CancelTerminalTransfer => "cancel-terminal-transfer".into(),
        AppCommand::About => "about".into(),
        AppCommand::OpenLicenses => "open-licenses".into(),
        AppCommand::ResetSettings => "reset-settings".into(),
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
    if let Some(rest) = id.strip_prefix("rename-tab:") {
        return session(rest).map(AppCommand::RenameTab);
    }
    if let Some(rest) = id.strip_prefix("edit-session:") {
        return session(rest).map(AppCommand::EditSession);
    }
    if let Some(rest) = id.strip_prefix("reconnect-tab:") {
        return session(rest).map(AppCommand::ReconnectTab);
    }
    if let Some(rest) = id.strip_prefix("copy-mcp-tab:") {
        return session(rest).map(AppCommand::CopyMcpTab);
    }
    if let Some(rest) = id.strip_prefix("close-group:") {
        let (scope, sid) = rest.split_once(':')?;
        return Some(AppCommand::CloseGroup(scope_from(scope)?, session(sid)?));
    }
    if let Some(mode) = id.strip_prefix("appearance:") {
        return super::settings::ThemeMode::from_storage(mode).map(AppCommand::SetThemeMode);
    }
    if let Some(mode) = id.strip_prefix("gutter:") {
        return super::settings::GutterMode::from_storage(mode).map(AppCommand::SetGutterMode);
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
        "toggle-copy-on-select" => AppCommand::ToggleCopyOnSelect,
        "quit" => AppCommand::Quit,
        "open-palette" => AppCommand::OpenPalette,
        "open-launcher" => AppCommand::OpenLauncher,
        "open-settings" => AppCommand::OpenSettings,
        "open-display-settings" => AppCommand::OpenDisplaySettings,
        "open-keyboard-settings" => AppCommand::OpenKeyboardSettings,
        "new-session" => AppCommand::NewSession,
        "export-data" => AppCommand::ExportData,
        "import-data" => AppCommand::ImportData,
        "import-ssh-config" => AppCommand::ImportSshConfig,
        "zoom-in" => AppCommand::ZoomIn,
        "zoom-out" => AppCommand::ZoomOut,
        "zoom-reset" => AppCommand::ZoomReset,
        "find" => AppCommand::Find,
        "find-next" => AppCommand::FindNext,
        "find-prev" => AppCommand::FindPrev,
        "prev-block" => AppCommand::PrevBlock,
        "next-block" => AppCommand::NextBlock,
        "toggle-timeline" => AppCommand::ToggleTimeline,
        "copy-block-command" => AppCommand::CopyBlockCommand,
        "copy-block-output" => AppCommand::CopyBlockOutput,
        "toggle-sender" => AppCommand::ToggleSender,
        "toggle-command-suggestions" => AppCommand::ToggleCommandSuggestions,
        "clear-command-history" => AppCommand::ClearCommandHistory,
        "toggle-filer" => AppCommand::ToggleFiler,
        "toggle-git" => AppCommand::ToggleGit,
        "locate-filer" => AppCommand::LocateFiler,
        "xmodem-send" => AppCommand::XmodemSend,
        "xmodem-send-1k" => AppCommand::XmodemSend1K,
        "xmodem-receive" => AppCommand::XmodemReceive,
        "cancel-terminal-transfer" => AppCommand::CancelTerminalTransfer,
        "about" => AppCommand::About,
        "open-licenses" => AppCommand::OpenLicenses,
        "reset-settings" => AppCommand::ResetSettings,
        _ => return None,
    })
}

/// 菜单提示与终端捕获阶段共用当前生效的绑定。
pub fn menu_shortcut(command: AppCommand) -> Option<String> {
    shortcut_text(command)
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
///
/// `block` 不为空（右键点在有命令的命令块内，command-blocks.md §3）时在“复制”组后增加“复制命令 / 复制输出”。
pub fn terminal_menu(has_selection: bool, block: Option<BlockMenu>) -> Vec<MenuItem> {
    let copy = item(label(Message::UiCopy {}), AppCommand::Copy);
    let mut items = vec![if has_selection { copy } else { copy.disabled() }];
    if let Some(b) = block {
        let cmd = item(
            label(Message::UiCopyCommand {}),
            AppCommand::CopyBlockCommand,
        );
        items.push(if b.copy_command { cmd } else { cmd.disabled() });
        items.push(item(
            label(Message::UiCopyOutput {}),
            AppCommand::CopyBlockOutput,
        ));
    }
    items.extend([
        item(label(Message::UiPaste {}), AppCommand::Paste),
        item(label(Message::UiSelectAll {}), AppCommand::SelectAll),
        item(label(Message::UiClearBuffer {}), AppCommand::ClearBuffer),
        MenuItem::separator(),
        item(&msg(Message::UiFilerMenuLocate {}), AppCommand::LocateFiler),
        MenuItem::separator(),
        item(label(Message::UiSplitRight {}), AppCommand::SplitRight),
        item(label(Message::UiSplitDown {}), AppCommand::SplitDown),
    ]);
    items
}

/// 标签右键菜单所需的上下文。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabMenuContext {
    /// 目标标签。
    pub session: SessionId,
    /// 标签来自已保存的会话（才显示“编辑会话”）。
    pub from_profile: bool,
    /// 标签已断开（才显示“重新连接”；且需要来自已保存会话）。
    pub disconnected: bool,
    /// MCP 服务正在运行且标签所属会话已授权。
    pub mcp_addressable: bool,
    /// 同组里是否有可关闭的“其他”标签。
    pub has_others: bool,
    /// 同组里它上方（菜单叫左侧）是否有标签。
    pub has_above: bool,
    /// 同组里它下方（右侧）是否有标签。
    pub has_below: bool,
}

/// 标签右键菜单（ui-shell.md §2.4）：重命名、编辑已保存的会话（仅来自已保存会话的标签）、关闭、
/// 关闭其他 / 左侧 / 右侧 / 全部（只在同一分屏组内生效，没有可关闭的标签时禁用）。
pub fn tab_menu(ctx: TabMenuContext) -> Vec<MenuItem> {
    let s = ctx.session;
    let group = |label: &str, scope: CloseScope, enabled: bool| {
        let it = MenuItem::action(encode(AppCommand::CloseGroup(scope, s)), label);
        if enabled { it } else { it.disabled() }
    };
    let mut items = vec![MenuItem::action(
        encode(AppCommand::RenameTab(s)),
        label(Message::UiRename {}),
    )];
    if ctx.from_profile {
        items.push(MenuItem::action(
            encode(AppCommand::EditSession(s)),
            label(Message::UiEditSession2 {}),
        ));
    }
    if ctx.disconnected && ctx.from_profile {
        items.push(MenuItem::action(
            encode(AppCommand::ReconnectTab(s)),
            label(Message::UiReconnect {}),
        ));
    }
    if ctx.mcp_addressable {
        items.push(MenuItem::action(
            encode(AppCommand::CopyMcpTab(s)),
            label(Message::WinMcpCopyTab {}),
        ));
    }
    items.push(MenuItem::separator());
    items.push(MenuItem::action(
        encode(AppCommand::CloseTab(s)),
        label(Message::UiClose {}),
    ));
    items.push(MenuItem::separator());
    items.push(group(
        label(Message::UiCloseOthers {}),
        CloseScope::Others,
        ctx.has_others,
    ));
    items.push(group(
        label(Message::UiCloseTabsToLeft {}),
        CloseScope::Above,
        ctx.has_above,
    ));
    items.push(group(
        label(Message::UiCloseTabsToRight {}),
        CloseScope::Below,
        ctx.has_below,
    ));
    items.push(group(label(Message::UiCloseAll {}), CloseScope::All, true));
    items
}

/// 应用菜单的开关状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppMenuState {
    /// 标签栏是否显示。
    pub sidebar_visible: bool,
    /// 右键是否弹菜单（否则为“复制或粘贴”）。
    pub right_click_menu: bool,
    /// “选中即复制”是否开启。
    pub copy_on_select: bool,
    /// Sender 面板是否显示。
    pub sender_visible: bool,
    /// “命令建议”是否开启。
    pub command_suggestions: bool,
    /// Filer 是否显示。
    pub filer_visible: bool,
    /// Git 改动面板是否显示。
    pub git_visible: bool,
    /// 全局时间栏设置。
    pub gutter_mode: super::settings::GutterMode,
    /// 外观的设置值（包括跟随系统）。
    pub theme_mode: super::settings::ThemeMode,
    /// 可选的前九个标签数量。
    pub tab_count: usize,
}

/// 窗口级应用菜单（Windows / Linux 没有系统菜单栏，spec §6 末段：放在应用内）。
/// 结构按 §6：会话 / 编辑 / 视图，外加设置与退出；还没有实现的功能（新建会话、设置）照常显示，
/// 命令交给对应模块处理。
pub fn app_menu(state: AppMenuState) -> Vec<MenuItem> {
    vec![
        MenuItem::submenu(
            label(Message::UiSession {}),
            vec![
                item(
                    label(Message::UiKeysCmdOpenLauncher {}),
                    AppCommand::OpenLauncher,
                ),
                item(label(Message::UiNewSession2 {}), AppCommand::NewSession),
                item(label(Message::UiNewLocalShell {}), AppCommand::NewLocalTab),
                MenuItem::separator(),
                item(label(Message::UiPreviousTab {}), AppCommand::PrevTab),
                item(label(Message::UiNextTab {}), AppCommand::NextTab),
                item(label(Message::UiPreviousPane {}), AppCommand::PrevPane),
                item(label(Message::UiNextPane {}), AppCommand::NextPane),
                MenuItem::submenu(
                    label(Message::UiGoToTab {}),
                    (0..9)
                        .map(|index| {
                            let entry = item(
                                &t(Message::UiTabNumber {
                                    number: (index + 1) as i64,
                                }),
                                AppCommand::SelectTab(index),
                            );
                            if index < state.tab_count {
                                entry
                            } else {
                                entry.disabled()
                            }
                        })
                        .collect(),
                ),
                MenuItem::separator(),
                MenuItem::submenu(
                    label(Message::UiGutterMenu {}),
                    super::settings::GutterMode::ALL
                        .into_iter()
                        .map(|mode| {
                            item(mode.label(), AppCommand::SetGutterMode(mode))
                                .checked(mode == state.gutter_mode)
                        })
                        .collect(),
                ),
                MenuItem::submenu(
                    msg(Message::UiXferMenu {}),
                    vec![
                        item(&msg(Message::UiXferMenuSend {}), AppCommand::XmodemSend),
                        item(&msg(Message::UiXferMenuSend1k {}), AppCommand::XmodemSend1K),
                        item(
                            &msg(Message::UiXferMenuReceive {}),
                            AppCommand::XmodemReceive,
                        ),
                        MenuItem::separator(),
                        item(
                            &msg(Message::UiXferMenuCancel {}),
                            AppCommand::CancelTerminalTransfer,
                        ),
                    ],
                ),
                MenuItem::separator(),
                item(
                    label(Message::UiImportOpensshConfig2 {}),
                    AppCommand::ImportSshConfig,
                ),
                item(label(Message::UiExportData {}), AppCommand::ExportData),
                item(label(Message::UiImportData2 {}), AppCommand::ImportData),
                MenuItem::separator(),
                item(label(Message::UiCloseSession {}), AppCommand::CloseSession),
            ],
        ),
        MenuItem::submenu(
            label(Message::UiEdit2 {}),
            vec![
                item(label(Message::UiCopy {}), AppCommand::Copy),
                item(label(Message::UiPaste {}), AppCommand::Paste),
                item(label(Message::UiSelectAll {}), AppCommand::SelectAll),
                MenuItem::separator(),
                item(label(Message::UiClearBuffer {}), AppCommand::ClearBuffer),
                item(label(Message::UiKeysCmdPrevBlock {}), AppCommand::PrevBlock),
                item(label(Message::UiKeysCmdNextBlock {}), AppCommand::NextBlock),
                MenuItem::separator(),
                item(label(Message::UiKeysCmdFind {}), AppCommand::Find),
                item(label(Message::UiKeysCmdFindNext {}), AppCommand::FindNext),
                item(label(Message::UiKeysCmdFindPrev {}), AppCommand::FindPrev),
                MenuItem::separator(),
                item(
                    &t(Message::UiMenuCommandSuggestions {}),
                    AppCommand::ToggleCommandSuggestions,
                )
                .checked(state.command_suggestions),
                item(
                    &t(Message::UiMenuClearHistory {}),
                    AppCommand::ClearCommandHistory,
                ),
                MenuItem::separator(),
                item(
                    label(Message::UiRightClickShowsMenu {}),
                    AppCommand::ToggleRightClickMenu,
                )
                .checked(state.right_click_menu),
                item(
                    label(Message::UiCopyOnSelect {}),
                    AppCommand::ToggleCopyOnSelect,
                )
                .checked(state.copy_on_select),
            ],
        ),
        MenuItem::submenu(
            label(Message::UiView {}),
            vec![
                item(label(Message::UiTabBar {}), AppCommand::ToggleSidebar)
                    .checked(state.sidebar_visible),
                item(&t(Message::UiMenuSender {}), AppCommand::ToggleSender)
                    .checked(state.sender_visible),
                item(&msg(Message::UiFilerMenuView {}), AppCommand::ToggleFiler)
                    .checked(state.filer_visible),
                item(&msg(Message::UiGitMenuView {}), AppCommand::ToggleGit)
                    .checked(state.git_visible),
                MenuItem::separator(),
                item(
                    label(Message::UiFilerMenuLocate {}),
                    AppCommand::LocateFiler,
                ),
                item(
                    label(Message::UiKeysCmdOpenPalette {}),
                    AppCommand::OpenPalette,
                ),
                MenuItem::separator(),
                item(label(Message::UiSplitRight {}), AppCommand::SplitRight),
                item(label(Message::UiSplitDown {}), AppCommand::SplitDown),
                MenuItem::separator(),
                item(label(Message::UiKeysCmdZoomIn {}), AppCommand::ZoomIn),
                item(label(Message::UiKeysCmdZoomOut {}), AppCommand::ZoomOut),
                item(label(Message::UiKeysCmdZoomReset {}), AppCommand::ZoomReset),
                MenuItem::separator(),
                MenuItem::submenu(
                    label(Message::UiAppearance {}),
                    super::settings::ThemeMode::ALL
                        .into_iter()
                        .map(|mode| {
                            item(mode.label(), AppCommand::SetThemeMode(mode))
                                .checked(mode == state.theme_mode)
                        })
                        .collect(),
                ),
                MenuItem::separator(),
                item(
                    label(Message::UiDisplaySettingsMenu {}),
                    AppCommand::OpenDisplaySettings,
                ),
                item(
                    label(Message::UiKeyboardSettingsMenu {}),
                    AppCommand::OpenKeyboardSettings,
                ),
            ],
        ),
        MenuItem::separator(),
        item(label(Message::UiSettings2 {}), AppCommand::OpenSettings),
        MenuItem::submenu(
            label(Message::UiHelpMenu {}),
            vec![
                item(label(Message::UiAboutKanso {}), AppCommand::About),
                item(label(Message::UiOpenLicenses {}), AppCommand::OpenLicenses),
                MenuItem::separator(),
                item(label(Message::UiResetMenu {}), AppCommand::ResetSettings),
            ],
        ),
        item(label(Message::UiQuit {}), AppCommand::Quit),
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
            AppCommand::RenameTab(sid(4)),
            AppCommand::EditSession(sid(3)),
            AppCommand::ToggleRightClickMenu,
            AppCommand::ToggleCopyOnSelect,
            AppCommand::Quit,
            AppCommand::OpenLauncher,
            AppCommand::OpenSettings,
            AppCommand::OpenDisplaySettings,
            AppCommand::OpenKeyboardSettings,
            AppCommand::NewSession,
            AppCommand::ExportData,
            AppCommand::ImportData,
            AppCommand::ImportSshConfig,
            AppCommand::ZoomIn,
            AppCommand::ZoomOut,
            AppCommand::ToggleFiler,
            AppCommand::ToggleGit,
            AppCommand::LocateFiler,
            AppCommand::XmodemSend,
            AppCommand::XmodemSend1K,
            AppCommand::XmodemReceive,
            AppCommand::CancelTerminalTransfer,
            AppCommand::ZoomReset,
            AppCommand::ToggleSender,
            AppCommand::ToggleCommandSuggestions,
            AppCommand::ClearCommandHistory,
            AppCommand::Find,
            AppCommand::FindNext,
            AppCommand::FindPrev,
            AppCommand::PrevBlock,
            AppCommand::NextBlock,
            AppCommand::ToggleTimeline,
            AppCommand::CopyBlockCommand,
            AppCommand::CopyBlockOutput,
            AppCommand::OpenPalette,
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
            disconnected: true,
            mcp_addressable: true,
            has_others: true,
            has_above: true,
            has_below: true,
        };
        let state = AppMenuState {
            sidebar_visible: true,
            right_click_menu: true,
            copy_on_select: false,
            sender_visible: true,
            command_suggestions: true,
            filer_visible: false,
            git_visible: false,
            gutter_mode: super::super::settings::GutterMode::Off,
            theme_mode: super::super::settings::ThemeMode::System,
            tab_count: 2,
        };
        for items in [
            terminal_menu(true, Some(BlockMenu { copy_command: true })),
            tab_menu(ctx),
            app_menu(state),
        ] {
            for (id, _, label, _) in all(&items) {
                assert!(decode(&id).is_some(), "{label} 的 id {id} 无法解码");
            }
        }
    }

    #[test]
    fn terminal_menu_copy_needs_a_selection() {
        let copy = |sel| {
            all(&terminal_menu(sel, None))
                .into_iter()
                .find(|a| a.0 == "copy")
                .map(|a| a.1)
        };
        assert_eq!(copy(false), Some(false));
        assert_eq!(copy(true), Some(true));
        let ids: Vec<_> = all(&terminal_menu(true, None))
            .into_iter()
            .map(|a| a.0)
            .collect();
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
    fn terminal_menu_block_items_only_inside_blocks() {
        let ids = |m: Vec<MenuItem>| all(&m).into_iter().map(|a| a.0).collect::<Vec<_>>();
        assert!(!ids(terminal_menu(false, None)).contains(&"copy-block-output".to_owned()));
        let with = terminal_menu(
            false,
            Some(BlockMenu {
                copy_command: false,
            }),
        );
        let a = all(&with);
        let enabled = |id: &str| a.iter().find(|x| x.0 == id).map(|x| x.1);
        assert_eq!(enabled("copy-block-command"), Some(false));
        assert_eq!(enabled("copy-block-output"), Some(true));
    }

    #[test]
    fn tab_menu_follows_the_spec() {
        let base = TabMenuContext {
            session: sid(5),
            from_profile: false,
            disconnected: false,
            mcp_addressable: false,
            has_others: false,
            has_above: false,
            has_below: false,
        };
        let a = all(&tab_menu(base));
        // 没有已保存会话：不出现“编辑”，但始终有“重命名”。
        assert!(!a.iter().any(|x| x.0.starts_with("edit-session")));
        assert!(!a.iter().any(|x| x.0.starts_with("copy-mcp-tab")));
        let enabled = |id: &str| a.iter().find(|x| x.0 == id).map(|x| x.1);
        assert_eq!(enabled("rename-tab:5"), Some(true));
        assert_eq!(enabled("close-tab:5"), Some(true));
        // 没有可关闭的标签时禁用，全部关闭始终可用（至少关自己）。
        assert_eq!(enabled("close-group:others:5"), Some(false));
        assert_eq!(enabled("close-group:above:5"), Some(false));
        assert_eq!(enabled("close-group:below:5"), Some(false));
        assert_eq!(enabled("close-group:all:5"), Some(true));

        let b = all(&tab_menu(TabMenuContext {
            from_profile: true,
            mcp_addressable: true,
            has_others: true,
            has_below: true,
            ..base
        }));
        assert!(b.iter().any(|x| x.0 == "edit-session:5"));
        assert!(b.iter().any(|x| x.0 == "copy-mcp-tab:5"));
        // “重新连接”只在已断开的已保存会话标签上出现。
        assert!(!b.iter().any(|x| x.0 == "reconnect-tab:5"));
        let c = all(&tab_menu(TabMenuContext {
            from_profile: true,
            disconnected: true,
            ..base
        }));
        assert!(c.iter().any(|x| x.0 == "reconnect-tab:5"));
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
            copy_on_select: false,
            sender_visible: false,
            command_suggestions: true,
            filer_visible: true,
            git_visible: false,
            gutter_mode: super::super::settings::GutterMode::Off,
            theme_mode: super::super::settings::ThemeMode::System,
            tab_count: 2,
        });
        let flat = all(&items);
        for want in [
            "open-launcher",
            "new-session",
            "gutter:timestamps",
            "appearance:system",
            "find",
            "select-tab:0",
            "new-local-tab",
            "close-session",
            "export-data",
            "import-data",
            "import-ssh-config",
            "copy",
            "paste",
            "select-all",
            "clear-buffer",
            "toggle-sidebar",
            "toggle-filer",
            "xmodem-send",
            "xmodem-receive",
            "cancel-terminal-transfer",
            "split-right",
            "open-settings",
            "toggle-sender",
            "toggle-command-suggestions",
            "clear-command-history",
            "about",
            "open-licenses",
            "reset-settings",
            "quit",
        ] {
            assert!(flat.iter().any(|a| a.0 == want), "缺少 {want}");
        }
        let checked = |id: &str| flat.iter().find(|a| a.0 == id).map(|a| a.3);
        assert_eq!(checked("toggle-sidebar"), Some(false));
        assert_eq!(checked("toggle-right-click-menu"), Some(true));
        assert_eq!(checked("toggle-sender"), Some(false));
        assert_eq!(checked("toggle-command-suggestions"), Some(true));
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
