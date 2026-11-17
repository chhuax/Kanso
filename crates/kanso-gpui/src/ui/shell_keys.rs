//! “帮助 → 恢复默认设置…”（ui-shell.md §6、§7）：确认 → 重置 → 提示结果。
//!
//! 对应 mac 的 `SettingsResetFlow`：只清设置（显示、外观、时间轴、快捷键），不动会话、分组、Sender 命令和历史
//! （store-import.md §9，由核心的 `reset_settings` 保证）。重置后各分区各发一次 `Changed`，
//! 外壳与设置窗口照常刷新；已打开的“命令建议”开关回到默认（关）。

use gpui::{AppContext as _, Context, Window};
use kanso_gpui::app::completion::decode_enabled;
use kanso_gpui::app::confirm::{CANCEL_ID, CONFIRM_ID};
use kanso_gpui::app::sender::t;
use kanso_gpui::widgets::dialog::{ButtonSpec, DialogSpec};
use kanso_types::Message;

use super::log;
use super::shell::ShellView;
use super::widgets::dialog::{Dialog, DialogEvent};

impl ShellView {
    /// 确认框：取消为默认按钮。
    pub(super) fn request_reset_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        self.menu = None;
        self.menu_sub = None;
        let spec = DialogSpec::new(&t(Message::UiResetTitle {}))
            .text(&t(Message::UiResetBody {}))
            .button(
                ButtonSpec::secondary(CANCEL_ID, &t(Message::UiCancel {}))
                    .cancel()
                    .default_button(),
            )
            .button(ButtonSpec::danger(
                CONFIRM_ID,
                &t(Message::UiResetConfirm {}),
            ));
        let dialog = cx.new(|cx| Dialog::new(spec, window, cx));
        self.dialog_sub =
            Some(
                cx.subscribe_in(&dialog, window, |this, _d, ev: &DialogEvent, window, cx| {
                    let DialogEvent::Result(r) = ev;
                    this.on_dialog_result(r, window, cx);
                }),
            );
        self.dialog = Some(dialog);
        self.pending_reset = true;
        cx.notify();
    }

    /// 确认后真正重置；失败时提示原因，设置保持原样。
    pub(super) fn reset_settings(&mut self, cx: &mut Context<Self>) {
        let result = self.settings.update(cx, |m, cx| m.reset_to_defaults(cx));
        match result {
            Ok(()) => {
                // 命令建议的开关也在设置表里，核心已清空；内存里回到默认。
                self.completion.enabled = decode_enabled(None);
                log("已恢复默认设置");
                self.show_notice(t(Message::UiResetDone {}), cx);
            }
            Err(e) => {
                log(&format!("恢复默认设置失败：{e}"));
                self.show_notice(t(Message::UiResetFailed { detail: e }), cx);
            }
        }
    }

    /// 自检（挂在 `--settings-test` 末尾）：改绑立即生效且已写入核心；恢复默认后回到原样、设置表被清空。
    /// 结果以 `[test] keys PASS/FAIL` 写进日志。注意它会清空当前数据目录的设置表，只在隔离的测试数据上跑。
    pub(super) fn run_keys_check(&mut self, cx: &mut Context<Self>) {
        use kanso_gpui::app::keybindings::{Chord, SETTING_KEY};
        use kanso_gpui::app::shortcuts::{AppCommand, lookup, shortcut_text};
        use kanso_gpui::keys::KeyInput;

        let press = |key: &'static str, ctrl: bool, alt: bool, shift: bool| {
            lookup(&KeyInput {
                key,
                key_char: None,
                ctrl,
                alt,
                shift,
                platform: false,
                is_held: false,
            })
        };
        let mut fails: Vec<&str> = Vec::new();
        let mut check = |name: &'static str, ok: bool| {
            if !ok {
                fails.push(name);
            }
        };
        check(
            "默认 Ctrl+Shift+F = 查找",
            press("f", true, false, true) == Some(AppCommand::Find),
        );
        let chord = Chord::new(true, true, false, "f");
        self.settings.update(cx, |m, cx| {
            m.update_keybindings(cx, |k| {
                k.assign("find", &chord);
            })
        });
        check(
            "改绑后新键生效",
            press("f", true, true, false) == Some(AppCommand::Find),
        );
        check("改绑后旧键失效", press("f", true, false, true).is_none());
        check(
            "菜单提示更新",
            shortcut_text(AppCommand::Find).as_deref() == Some("Ctrl+Alt+F"),
        );
        check(
            "已写入核心",
            self.core
                .setting(SETTING_KEY)
                .is_some_and(|s| s.contains("ctrl+alt+f")),
        );
        let reset = self.settings.update(cx, |m, cx| m.reset_to_defaults(cx));
        check("恢复默认成功", reset.is_ok());
        check(
            "恢复后旧键回来",
            press("f", true, false, true) == Some(AppCommand::Find),
        );
        check("恢复后新键失效", press("f", true, true, false).is_none());
        check(
            "恢复后设置表已清空",
            self.core.setting(SETTING_KEY).is_none(),
        );
        if fails.is_empty() {
            log("[test] keys PASS");
        } else {
            log(&format!("[test] keys FAIL：{}", fails.join("；")));
        }
    }
}
