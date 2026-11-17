//! 应用级快捷键表（Windows / Linux 默认值，来自 `ui-shell.md` §7）。
//!
//! 所有应用级按键集中在 [`DEFAULT_BINDINGS`] 一张表里，视图只调用 [`lookup`]。
//! 约定（`ui-shell.md` §7、`terminal.md` §9）：普通的 Ctrl+字母 / Alt+字母不拦截，
//! 除非表里明确写了；匹配要求修饰键**完全一致**（Ctrl+Shift+T 不会被 Ctrl+T 命中）。
//! 复制 / 粘贴不在这张表里，仍由 [`crate::keys::classify_key`] 处理。
//!
//! TODO(rebind)：spec 要求可在“键盘快捷键”对话框重新绑定；对话框做好后，把这张表改成运行时可替换，
//! 查找函数的签名不用变。

use kanso_types::SessionId;

use crate::app::workspace::CloseScope;
use crate::keys::KeyInput;

/// 应用级命令。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppCommand {
    /// 在当前分屏新开一个本地 shell 标签。
    NewLocalTab,
    /// 打开会话启动器浮层（＋ 按钮同款）。
    OpenLauncher,
    /// 打开新建会话对话框（ui-shell.md §7：Alt+N）。
    NewSession,
    /// 关闭当前会话（本地 shell 有前台命令时需要确认）。
    CloseSession,
    /// 当前分屏的下一个标签（循环）。
    NextTab,
    /// 当前分屏的上一个标签（循环）。
    PrevTab,
    /// 下一个分屏（循环）。
    NextPane,
    /// 上一个分屏（循环）。
    PrevPane,
    /// 向右拆分（新分屏在右边）。
    SplitRight,
    /// 向下拆分（新分屏在下边）。
    SplitDown,
    /// 切到侧栏顺序的第 N 个标签（从 0 开始）。
    SelectTab(usize),
    /// 显示 / 隐藏标签栏。
    ToggleSidebar,
    /// 复制活动终端的选区。
    Copy,
    /// 把剪贴板内容粘贴进活动终端。
    Paste,
    /// 全选活动终端（含回滚缓冲区）。
    SelectAll,
    /// 清空活动终端的缓冲区（Alt+K）。
    ClearBuffer,
    /// 关闭指定标签（侧栏右键菜单；需要确认时弹对话框）。
    CloseTab(SessionId),
    /// 在指定标签所在分屏组内批量关闭（只确认一次）。
    CloseGroup(CloseScope, SessionId),
    /// 编辑指定标签来源的已保存会话（打开会话编辑对话框）。
    EditSession(SessionId),
    /// 切换右键行为：弹出菜单 / 复制或粘贴（terminal.md §8.1）。
    ToggleRightClickMenu,
    /// 退出应用（有仍在连接的会话时先确认）。
    Quit,
    /// 打开设置（显示设置）。
    OpenSettings,
    /// 放大终端文字（改终端字号，步长 1）。
    ZoomIn,
    /// 缩小终端文字。
    ZoomOut,
    /// 终端字号恢复默认。
    ZoomReset,
    /// 显示 / 隐藏 Filer（视图菜单；用已记住的浏览位置）。
    ToggleFiler,
    /// 在 Filer 中显示活动 shell 的工作目录（Ctrl+Shift+J，filer.md §2）。
    LocateFiler,
    /// 用 XMODEM（128 字节块）发送文件。
    XmodemSend,
    /// 用 XMODEM-1K 发送文件。
    XmodemSend1K,
    /// 用 XMODEM 接收文件。
    XmodemReceive,
    /// 取消活动会话正在进行的终端内传输（ZMODEM / XMODEM）。
    CancelTerminalTransfer,
}

impl AppCommand {
    /// 长按时是否重复执行：只有“切换”类可以，新建 / 关闭 / 拆分不行（否则按住不放会开出一堆）。
    pub fn repeatable(self) -> bool {
        matches!(
            self,
            AppCommand::NextTab
                | AppCommand::PrevTab
                | AppCommand::NextPane
                | AppCommand::PrevPane
                | AppCommand::ZoomIn
                | AppCommand::ZoomOut
        )
    }
}

/// 一条按键绑定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    /// Ctrl。
    pub ctrl: bool,
    /// Alt。
    pub alt: bool,
    /// Shift。
    pub shift: bool,
    /// GPUI 的 `key`（命名键小写英文名，字符键为键帽字符）。
    pub key: &'static str,
    /// 命令。
    pub command: AppCommand,
}

const fn b(ctrl: bool, alt: bool, shift: bool, key: &'static str, command: AppCommand) -> Binding {
    Binding {
        ctrl,
        alt,
        shift,
        key,
        command,
    }
}

/// 默认绑定表。同一命令可以有多行（别名）。
pub const DEFAULT_BINDINGS: &[Binding] = &[
    // 标签
    b(true, false, true, "t", AppCommand::NewLocalTab),
    // 规格没有给启动器单独的键；Ctrl+Shift+O 与 Ctrl+Shift+T / W 同属「会话」组。
    b(true, false, true, "o", AppCommand::OpenLauncher),
    b(false, true, false, "n", AppCommand::NewSession),
    b(true, false, true, "w", AppCommand::CloseSession),
    b(false, true, false, "]", AppCommand::NextTab),
    b(false, true, false, "[", AppCommand::PrevTab),
    // Ctrl+Tab / Ctrl+Shift+Tab：浏览器与 Windows Terminal 的习惯，作为同一命令的别名。
    b(true, false, false, "tab", AppCommand::NextTab),
    b(true, false, true, "tab", AppCommand::PrevTab),
    // 分屏。Windows 上带 Shift 的反斜杠键，GPUI 可能报 `\` 也可能报 `|`，两种都收。
    b(true, false, true, "\\", AppCommand::SplitRight),
    b(true, false, true, "|", AppCommand::SplitRight),
    b(true, true, false, "\\", AppCommand::SplitDown),
    b(true, true, false, "]", AppCommand::NextPane),
    b(true, true, false, "[", AppCommand::PrevPane),
    // Alt+1–Alt+9：按侧栏顺序切到第 N 个标签。
    b(false, true, false, "1", AppCommand::SelectTab(0)),
    b(false, true, false, "2", AppCommand::SelectTab(1)),
    b(false, true, false, "3", AppCommand::SelectTab(2)),
    b(false, true, false, "4", AppCommand::SelectTab(3)),
    b(false, true, false, "5", AppCommand::SelectTab(4)),
    b(false, true, false, "6", AppCommand::SelectTab(5)),
    b(false, true, false, "7", AppCommand::SelectTab(6)),
    b(false, true, false, "8", AppCommand::SelectTab(7)),
    b(false, true, false, "9", AppCommand::SelectTab(8)),
    // 面板显隐（spec 里 Ctrl+Alt+← 对应标签栏）。
    b(true, true, false, "left", AppCommand::ToggleSidebar),
    b(true, true, false, "right", AppCommand::ToggleFiler),
    // 定位终端目录：mac 是 ⌘J，Windows 用 Ctrl+Shift+J（ui-shell.md §7）。
    b(true, false, true, "j", AppCommand::LocateFiler),
    // 清屏（ui-shell.md §7）。
    b(false, true, false, "k", AppCommand::ClearBuffer),
    // 设置：mac 是 ⌘,，Windows 沿用 Ctrl+,（VS Code、Windows Terminal 同样的习惯）。
    b(true, false, false, ",", AppCommand::OpenSettings),
    // 终端文字缩放（ui-shell.md §7）。Ctrl+Shift+= 在美式键盘上报 `+`，一并收。
    b(true, false, false, "=", AppCommand::ZoomIn),
    b(true, false, true, "+", AppCommand::ZoomIn),
    b(true, false, false, "-", AppCommand::ZoomOut),
    b(true, false, false, "0", AppCommand::ZoomReset),
];

/// 查表。Win 键组合属于系统，永远不命中。
pub fn lookup(input: &KeyInput<'_>) -> Option<AppCommand> {
    if input.platform {
        return None;
    }
    DEFAULT_BINDINGS
        .iter()
        .find(|bd| {
            bd.ctrl == input.ctrl
                && bd.alt == input.alt
                && bd.shift == input.shift
                && bd.key == input.key
        })
        .map(|bd| bd.command)
}

/// 命令的人读快捷键文字（菜单 / 提示用），取表里第一条。没有绑定返回 `None`。
pub fn shortcut_text(command: AppCommand) -> Option<String> {
    let bd = DEFAULT_BINDINGS.iter().find(|bd| bd.command == command)?;
    let mut parts = Vec::new();
    if bd.ctrl {
        parts.push("Ctrl".to_owned());
    }
    if bd.alt {
        parts.push("Alt".to_owned());
    }
    if bd.shift {
        parts.push("Shift".to_owned());
    }
    let mut chars = bd.key.chars();
    let key = match (chars.next(), chars.next()) {
        (Some(c), None) => c.to_uppercase().to_string(),
        _ => {
            let mut s = bd.key.to_owned();
            s[..1].make_ascii_uppercase();
            s
        }
    };
    parts.push(key);
    Some(parts.join("+"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: &'static str, ctrl: bool, alt: bool, shift: bool) -> Option<AppCommand> {
        lookup(&KeyInput {
            key,
            key_char: None,
            ctrl,
            alt,
            shift,
            platform: false,
            is_held: false,
        })
    }

    #[test]
    fn spec_defaults() {
        assert_eq!(press("t", true, false, true), Some(AppCommand::NewLocalTab));
        assert_eq!(
            press("w", true, false, true),
            Some(AppCommand::CloseSession)
        );
        assert_eq!(press("\\", true, false, true), Some(AppCommand::SplitRight));
        assert_eq!(press("|", true, false, true), Some(AppCommand::SplitRight));
        assert_eq!(press("\\", true, true, false), Some(AppCommand::SplitDown));
        assert_eq!(press("]", false, true, false), Some(AppCommand::NextTab));
        assert_eq!(press("[", false, true, false), Some(AppCommand::PrevTab));
        assert_eq!(press("]", true, true, false), Some(AppCommand::NextPane));
        assert_eq!(press("[", true, true, false), Some(AppCommand::PrevPane));
        assert_eq!(
            press("1", false, true, false),
            Some(AppCommand::SelectTab(0))
        );
        assert_eq!(
            press("9", false, true, false),
            Some(AppCommand::SelectTab(8))
        );
        assert_eq!(press("n", false, true, false), Some(AppCommand::NewSession));
        assert_eq!(
            press("o", true, false, true),
            Some(AppCommand::OpenLauncher)
        );
        assert_eq!(press("tab", true, false, false), Some(AppCommand::NextTab));
        assert_eq!(press("tab", true, false, true), Some(AppCommand::PrevTab));
    }

    #[test]
    fn settings_and_zoom_defaults() {
        assert_eq!(
            press(",", true, false, false),
            Some(AppCommand::OpenSettings)
        );
        assert_eq!(press("=", true, false, false), Some(AppCommand::ZoomIn));
        assert_eq!(press("-", true, false, false), Some(AppCommand::ZoomOut));
        assert_eq!(press("0", true, false, false), Some(AppCommand::ZoomReset));
        assert!(AppCommand::ZoomIn.repeatable());
        assert!(!AppCommand::OpenSettings.repeatable());
    }

    #[test]
    fn plain_ctrl_and_alt_letters_are_never_intercepted() {
        for k in ["a", "c", "d", "t", "w", "z", "l", "b", "f"] {
            assert_eq!(press(k, true, false, false), None, "Ctrl+{k}");
            assert_eq!(press(k, false, true, false), None, "Alt+{k}");
        }
        // Alt+方向键、Alt+Shift+数字属于 shell / 程序。
        assert_eq!(press("left", false, true, false), None);
        assert_eq!(press("1", false, true, true), None);
        assert_eq!(press("1", false, false, false), None);
    }

    #[test]
    fn modifiers_must_match_exactly() {
        assert_eq!(press("t", true, true, true), None);
        assert_eq!(press("t", true, false, false), None);
        assert_eq!(press("w", false, false, true), None);
    }

    #[test]
    fn windows_key_never_matches() {
        let hit = lookup(&KeyInput {
            key: "t",
            key_char: None,
            ctrl: true,
            alt: false,
            shift: true,
            platform: true,
            is_held: false,
        });
        assert_eq!(hit, None);
    }

    #[test]
    fn no_duplicate_chords_and_no_conflict_with_copy_paste() {
        for (i, a) in DEFAULT_BINDINGS.iter().enumerate() {
            for c in &DEFAULT_BINDINGS[i + 1..] {
                assert!(
                    (a.ctrl, a.alt, a.shift, a.key) != (c.ctrl, c.alt, c.shift, c.key),
                    "重复按键 {a:?}"
                );
            }
            // Ctrl+Shift+C / V 留给复制粘贴。
            assert!(
                !(a.ctrl && a.shift && !a.alt && matches!(a.key, "c" | "v")),
                "{a:?} 与复制粘贴冲突"
            );
        }
    }

    #[test]
    fn only_switching_commands_repeat() {
        assert!(AppCommand::NextTab.repeatable());
        assert!(!AppCommand::NewLocalTab.repeatable());
        assert!(!AppCommand::SplitRight.repeatable());
        assert!(!AppCommand::CloseSession.repeatable());
    }

    #[test]
    fn shortcut_text_is_readable() {
        assert_eq!(
            shortcut_text(AppCommand::NewLocalTab).as_deref(),
            Some("Ctrl+Shift+T")
        );
        assert_eq!(
            shortcut_text(AppCommand::ToggleSidebar).as_deref(),
            Some("Ctrl+Alt+Left")
        );
        assert_eq!(
            shortcut_text(AppCommand::SelectTab(2)).as_deref(),
            Some("Alt+3")
        );
    }
}
