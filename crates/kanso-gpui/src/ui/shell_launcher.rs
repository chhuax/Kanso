//! `ShellView` 与启动器 / 会话编辑对话框 / 连接提示框的接线。
//!
//! - 启动器（`LauncherView`）与编辑对话框（`SessionEditorView`）是窗口内的浮层实体，同一时间只开一个；
//!   启动器里选"新建 / 编辑"时先关启动器再开编辑对话框，保存后可顺带连接。
//! - 连接提示框：核心的提问（主机密钥变更、密码 / 私钥口令、keyboard-interactive）排在
//!   `ctl.prompts()` 队列里，这里把队首映射成模态对话框（`app::prompt_dialogs`），按结果回答核心，再显示下一个。
//!   文件传输类提问（需要系统文件选择器）暂不支持，直接按取消回答，免得核心空等到超时。
//! - 浮层打开期间不处理应用级快捷键（键盘归浮层）。

use gpui::{Context, Window, prelude::*};
use kanso_gpui::app::controller::FALLBACK_GRID;
use kanso_gpui::app::i18n::{label, t};
use kanso_gpui::app::launcher::core_error_text;
use kanso_gpui::app::prompt_dialogs::{response_for, spec_for};
use kanso_types::ProfileId;
use kanso_types::message::Message;

use super::launcher::{LauncherEvent, LauncherView};
use super::log;
use super::session_editor::{EditorEvent, SessionEditorView};
use super::shell::ShellView;
use super::widgets::dialog::{Dialog, DialogEvent};

impl ShellView {
    /// 有浮层（启动器、编辑对话框、提示框）在显示：应用级快捷键让路。
    pub(super) fn modal_open(&self) -> bool {
        self.palette.is_some()
            || self.launcher.is_some()
            || self.editor.is_some()
            || self.prompt.is_some()
            || self.ssh_import.is_some()
            || self.dialog.is_some()
            || self.filer.dialog.is_some()
            || self.git.dialog.is_some()
            || self.menu.is_some()
    }

    // ---- 启动器 ----

    /// 打开会话启动器（＋ 按钮、Ctrl+Shift+O）。
    pub(super) fn open_launcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        let core = self.core.clone();
        let launcher = cx.new(|cx| LauncherView::new(core, window, cx));
        self.launcher_sub = Some(cx.subscribe_in(
            &launcher,
            window,
            |this, _l, ev: &LauncherEvent, window, cx| {
                this.on_launcher_event(ev.clone(), window, cx)
            },
        ));
        self.launcher = Some(launcher);
        log("启动器已打开");
        cx.notify();
    }

    fn close_launcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.launcher = None;
        self.launcher_sub = None;
        self.sync_focus(window, cx);
        cx.notify();
    }

    fn on_launcher_event(
        &mut self,
        ev: LauncherEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_launcher(window, cx);
        match ev {
            LauncherEvent::Close => {}
            LauncherEvent::OpenSettings => self.run_command(
                kanso_gpui::app::shortcuts::AppCommand::OpenSettings,
                window,
                cx,
            ),
            LauncherEvent::OpenLocal => self.open_tab(window, cx),
            LauncherEvent::NewSession => self.open_editor(None, window, cx),
            LauncherEvent::Connect(id) => self.connect_profile(id, window, cx),
            LauncherEvent::Edit(id) => self.open_editor(Some(id), window, cx),
            LauncherEvent::ImportSshConfig => self.open_ssh_import(window, cx),
        }
    }

    // ---- 编辑对话框 ----

    /// 打开会话编辑对话框：`None` 新建，`Some(id)` 编辑已保存的会话
    /// （标签右键菜单的"编辑会话"调用它，ui-shell.md §4）。
    pub(super) fn open_editor(
        &mut self,
        profile: Option<ProfileId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editor.is_some() || self.prompt.is_some() {
            return;
        }
        let existing = match profile {
            Some(id) => match self.core.profiles().into_iter().find(|p| p.id == id) {
                Some(p) => Some(p),
                None => {
                    self.show_notice_str(label(Message::UiThisSessionNoLongerExists {}), cx);
                    return;
                }
            },
            None => None,
        };
        // 从启动器进入时它已经关闭；直接从菜单进入时关掉可能开着的启动器。
        self.launcher = None;
        self.launcher_sub = None;
        let core = self.core.clone();
        let editor = cx.new(|cx| SessionEditorView::new(core, existing, window, cx));
        self.editor_sub = Some(cx.subscribe_in(
            &editor,
            window,
            |this, editor, ev: &EditorEvent, window, cx| {
                editor.update(cx, |e, cx| e.wipe_secrets(cx));
                this.editor = None;
                this.editor_sub = None;
                match ev {
                    EditorEvent::Cancel => {}
                    EditorEvent::Saved { id, connect } => {
                        log(&format!("会话已保存 {id}"));
                        if *connect {
                            this.connect_profile(*id, window, cx);
                        }
                    }
                }
                this.sync_focus(window, cx);
                cx.notify();
            },
        ));
        self.editor = Some(editor);
        cx.notify();
    }

    // ---- 连接 ----

    /// 在新标签里连接已保存的会话。
    pub(super) fn connect_profile(
        &mut self,
        id: ProfileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self.core.profiles().into_iter().find(|p| p.id == id) else {
            self.show_notice_str(label(Message::UiThisSessionNoLongerExists {}), cx);
            return;
        };
        match self.ctl.open_profile(id, None, FALLBACK_GRID) {
            Ok(sid) => {
                log(&format!("已连接会话“{}”（{sid:?}）", profile.name));
                self.attach_session(sid, window, cx);
                self.after_change(window, cx);
            }
            Err(e) => {
                log(&format!("连接“{}”失败：{e}", profile.name));
                let text = t(&Message::UiCantConnectTo {
                    name: profile.name.to_string(),
                    detail: (core_error_text(&e)).to_string(),
                });
                self.show_notice_str(&text, cx);
            }
        }
    }

    // ---- 连接提示框 ----

    /// 让显示的提示框与提示队列的队首一致：新提问到达、核心撤销提问、回答后出下一个，都调用它。
    pub(super) fn sync_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        loop {
            let current = self
                .ctl
                .prompts()
                .current()
                .map(|e| (e.id, e.dialog.clone()));
            let Some((id, dialog)) = current else {
                if self.prompt.take().is_some() {
                    self.prompt_sub = None;
                    self.sync_focus(window, cx);
                    cx.notify();
                }
                return;
            };
            if self.prompt.as_ref().is_some_and(|(shown, _)| *shown == id) {
                return;
            }
            self.palette = None;
            // 队首换了（上一个被核心撤销）：丢掉旧的对话框。
            self.prompt = None;
            self.prompt_sub = None;
            let Some(spec) = spec_for(&dialog) else {
                // 文件传输类提问（ZMODEM / XMODEM）：系统文件选择器，选完应答。
                if let kanso_gpui::app::prompts::PromptDialog::Transfer(request) = &dialog {
                    self.begin_transfer_picker(id, request.clone(), window, cx);
                    return;
                }
                self.ctl.prompts_mut().cancel();
                continue;
            };
            let dlg = cx.new(|cx| Dialog::new(spec, window, cx));
            self.prompt_sub = Some(cx.subscribe_in(
                &dlg,
                window,
                |this, _d, DialogEvent::Result(r): &DialogEvent, window, cx| {
                    let shown = this.prompt.take().map(|(id, _)| id);
                    this.prompt_sub = None;
                    let current = this.ctl.prompts().current().cloned();
                    if let (Some(shown), Some(entry)) = (shown, current)
                        && entry.id == shown
                    {
                        let response = response_for(&entry.dialog, r);
                        this.ctl.prompts_mut().respond(response);
                    }
                    this.sync_prompt(window, cx);
                    this.sync_focus(window, cx);
                    cx.notify();
                },
            ));
            log(&format!("显示提问对话框 {id:?}"));
            self.prompt = Some((id, dlg));
            cx.notify();
            return;
        }
    }
}
