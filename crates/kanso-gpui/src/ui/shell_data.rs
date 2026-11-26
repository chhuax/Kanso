//! `ShellView` 与数据导入 / 导出、OpenSSH 配置导入、首次启动空状态页的接线。
//!
//! - 会话 → 导出数据…：系统"另存为"面板（默认 `Kanso-YYYYMMDD.kanso`，自动补扩展名）→ `Core::export_data` → 结果提示；
//! - 会话 → 导入数据…：系统打开面板 → 确认框（说明合并规则）→ `Core::import_data` → 结果页（新增 / 更新 / 跳过及原因）；
//! - 导入 OpenSSH 配置…：弹出 `SshImportView`（逐台勾选、选分组）；
//! - 空状态页（ui-shell.md §11）：没有任何标签时显示三个入口。
//!
//! 文字与对话框规格在 `kanso_gpui::app::data_transfer`（三平台可测）。

use kanso_types::message::Message;
use std::path::{Path, PathBuf};

use gpui::{Context, MouseButton, MouseDownEvent, PathPromptOptions, Window, div, prelude::*, px};
use kanso_gpui::app::data_transfer::{
    export_default_name, export_done_spec, failure_spec, import_confirm_spec, import_confirmed,
    import_result, import_result_spec, with_extension,
};
use kanso_gpui::app::i18n::{label, t};
use kanso_gpui::app::launcher::core_error_text;
use kanso_gpui::app::shortcuts::{AppCommand, shortcut_text};
use kanso_gpui::widgets::dialog::{ButtonSpec, DialogResult, DialogSpec};
use kanso_gpui::widgets::style::number;

use super::shell::{ShellView, Theme, unix_ms};
use super::ssh_import::{SshImportEvent, SshImportView};
use super::widgets::dialog::{Dialog, DialogEvent};
use super::{log, tz};

/// 数据流程里正在等结果的对话框。
pub(crate) enum PendingData {
    /// 只有"好"的提示。
    Info,
    /// 关于窗口允许打开项目源码。
    About,
    /// 导入前的确认，携带要导入的文件。
    ImportConfirm(PathBuf),
}

fn home_dir() -> String {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default()
}

impl ShellView {
    fn show_data_dialog(
        &mut self,
        spec: DialogSpec,
        pending: PendingData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog.is_some() {
            return;
        }
        self.menu = None;
        self.menu_sub = None;
        let dialog = cx.new(|cx| Dialog::new(spec, window, cx));
        self.dialog_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |this, _d, DialogEvent::Result(r): &DialogEvent, window, cx| {
                this.on_dialog_result(r, window, cx)
            },
        ));
        self.dialog = Some(dialog);
        self.pending_data = Some(pending);
        cx.notify();
    }

    /// 数据流程对话框的结果（`on_dialog_result` 在有 `pending_data` 时转到这里）。
    pub(super) fn on_data_dialog(
        &mut self,
        pending: PendingData,
        r: &DialogResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(pending, PendingData::About) && r.button.as_deref() == Some("source") {
            cx.open_url("https://github.com/chhuax/Kanso");
        }
        match pending {
            PendingData::ImportConfirm(path) if import_confirmed(r) => {
                self.run_import(path, window, cx)
            }
            _ => {
                self.sync_focus(window, cx);
                cx.notify();
            }
        }
    }

    /// 显示和发行包一致的版本、项目许可和源码入口。
    pub(super) fn show_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let spec = DialogSpec::new(label(Message::UiAboutKanso {}))
            .text(&t(&Message::UiAboutVersion {
                app_version: env!("CARGO_PKG_VERSION").into(),
            }))
            .text(label(Message::UiAboutLicense {}))
            .button(ButtonSpec::secondary(
                "source",
                label(Message::UiOpenSourceCode {}),
            ))
            .button(ButtonSpec::primary("ok", label(Message::UiOk {})).cancel());
        self.show_data_dialog(spec, PendingData::About, window, cx);
    }

    /// 用 Windows 记事本打开本发行版的第三方声明，避免依赖 Markdown 文件关联。
    pub(super) fn open_licenses(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        const NAME: &str = "THIRD_PARTY_NOTICES.windows.md";
        let result = std::env::current_exe().and_then(|exe| {
            let path = exe.with_file_name(NAME);
            #[cfg(debug_assertions)]
            let path = if path.is_file() {
                path
            } else {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../..")
                    .join(NAME)
            };
            std::fs::metadata(&path)?;
            std::process::Command::new("notepad.exe").arg(path).spawn()
        });
        if let Err(error) = result {
            let reason = t(&Message::UiFilerOpenFailed {
                dest: NAME.into(),
                why: error.to_string(),
            });
            self.show_data_dialog(failure_spec(&reason), PendingData::Info, window, cx);
        }
    }

    // ---- 导出 ----

    /// 会话 → 导出数据…
    pub(super) fn export_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = export_default_name(unix_ms(), tz::local_offset_minutes());
        let rx = cx.prompt_for_new_path(Path::new(&home_dir()), Some(&name));
        let core = self.core.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else {
                return;
            };
            let path = with_extension(&path.to_string_lossy());
            let result = core.export_data(&path).await;
            this.update_in(cx, |this, window, cx| {
                let spec = match result {
                    Ok(()) => {
                        log(&format!("数据已导出：{path}"));
                        export_done_spec(&path)
                    }
                    Err(e) => failure_spec(&core_error_text(&e)),
                };
                this.show_data_dialog(spec, PendingData::Info, window, cx);
            })
            .ok();
        })
        .detach();
    }

    // ---- 导入 ----

    /// 会话 → 导入数据…
    pub(super) fn import_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(label(Message::UiChooseKansoFileToImport {}).into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update_in(cx, |this, window, cx| {
                    let spec = import_confirm_spec(&path.to_string_lossy());
                    this.show_data_dialog(spec, PendingData::ImportConfirm(path), window, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn run_import(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let core = self.core.clone();
        let text = path.to_string_lossy().into_owned();
        cx.spawn_in(window, async move |this, cx| {
            let result = core.import_data(&text).await;
            this.update_in(cx, |this, window, cx| {
                let spec = match result {
                    Ok(report) => {
                        log(&format!(
                            "数据已导入：新增 {} 更新 {} 跳过 {}",
                            report.added,
                            report.updated,
                            report.skipped.len()
                        ));
                        import_result_spec(&import_result(&report))
                    }
                    Err(e) => failure_spec(&core_error_text(&e)),
                };
                this.show_data_dialog(spec, PendingData::Info, window, cx);
            })
            .ok();
        })
        .detach();
    }

    // ---- OpenSSH 配置 ----

    /// 会话 → 导入 OpenSSH 配置…（也由启动器与空状态页调用）。
    pub(super) fn open_ssh_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ssh_import.is_some() || self.editor.is_some() || self.prompt.is_some() {
            return;
        }
        self.launcher = None;
        self.launcher_sub = None;
        let core = self.core.clone();
        let home = home_dir();
        let view = cx.new(|cx| SshImportView::new(core, &home, window, cx));
        self.ssh_import_sub = Some(cx.subscribe_in(
            &view,
            window,
            |this, _v, SshImportEvent::Close: &SshImportEvent, window, cx| {
                this.ssh_import = None;
                this.ssh_import_sub = None;
                this.reload_sidebar_sessions(cx);
                this.sync_focus(window, cx);
                cx.notify();
            },
        ));
        self.ssh_import = Some(view);
        cx.notify();
    }

    // ---- 空状态 ----

    /// 没有任何标签时的空状态页（ui-shell.md §11）：新建本地 shell、新建会话、导入 OpenSSH 配置。
    pub(super) fn empty_state(&self, th: Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let key = shortcut_text(AppCommand::NewLocalTab).unwrap_or_default();
        let entry = |id: &'static str,
                     label: &'static str,
                     primary: bool,
                     command: AppCommand,
                     cx: &mut Context<Self>| {
            div()
                .id(id)
                .w(px(220.0))
                .h(px(32.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(number("radius", "md")))
                .border_1()
                .cursor_pointer()
                .border_color(if primary {
                    th.c("accent")
                } else {
                    th.c("border.strong")
                })
                .bg(if primary {
                    th.c("accent")
                } else {
                    th.c("bg.buttonSecondary")
                })
                .text_color(if primary {
                    th.c("text.onAccent")
                } else {
                    th.c("text.primary")
                })
                .hover(|s| s.opacity(0.88))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        this.run_command(command, window, cx)
                    }),
                )
                .child(label)
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.0))
            .text_color(th.c("text.secondary"))
            .child(
                div()
                    .text_size(px(number("font", "ui.title")))
                    .text_color(th.c("text.primary"))
                    .child(label(Message::UiNoOpenSessions {})),
            )
            .child(entry(
                "empty-local",
                label(Message::UiNewLocalShell {}),
                true,
                AppCommand::NewLocalTab,
                cx,
            ))
            .child(
                div()
                    .text_size(px(number("font", "ui.caption")))
                    .text_color(th.c("text.tertiary"))
                    .child(format!("{} {key}", label(Message::UiOrPress {}))),
            )
            .child(entry(
                "empty-session",
                label(Message::UiNewSession2 {}),
                false,
                AppCommand::NewSession,
                cx,
            ))
            .child(entry(
                "empty-import",
                label(Message::UiImportOpensshConfig2 {}),
                false,
                AppCommand::ImportSshConfig,
                cx,
            ))
            .into_any_element()
    }
}
