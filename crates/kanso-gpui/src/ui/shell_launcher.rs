//! `ShellView` 与启动器 / 会话编辑对话框 / 连接提示框的接线。
//!
//! - 启动器（`LauncherView`）与编辑对话框（`SessionEditorView`）是窗口内的浮层实体，同一时间只开一个；
//!   启动器里选"新建 / 编辑"时先关启动器再开编辑对话框，保存后可顺带连接。
//! - 连接提示框：核心的提问（主机密钥变更、密码 / 私钥口令、keyboard-interactive）排在
//!   `ctl.prompts()` 队列里，这里把队首映射成模态对话框（`app::prompt_dialogs`），按结果回答核心，再显示下一个。
//!   文件传输类提问（需要系统文件选择器）暂不支持，直接按取消回答，免得核心空等到超时。
//! - 浮层打开期间不处理应用级快捷键（键盘归浮层）。

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{Context, Window, prelude::*};
use kanso_core::Core;
use kanso_gpui::app::controller::FALLBACK_GRID;
use kanso_gpui::app::i18n::{label, t};
use kanso_gpui::app::launcher::core_error_text;
use kanso_gpui::app::menus::QuickSessionItem;
use kanso_gpui::app::prompt_dialogs::{response_for, spec_for};
use kanso_gpui::app::tab::TabPresentation;
use kanso_gpui::widgets::menu::MenuBadgeTone;
use kanso_types::ProfileId;
use kanso_types::message::Message;
use kanso_types::profile::ProfileKind;

use super::launcher::{LauncherEvent, LauncherView};
use super::log;
use super::session_editor::{EditorEvent, SessionEditorView};
use super::shell::ShellView;
use super::sidebar::SESSION_COLORS;
use super::widgets::dialog::{Dialog, DialogEvent};

impl ShellView {
    /// 有浮层（启动器、编辑对话框、提示框）在显示：应用级快捷键让路。
    pub(super) fn modal_open(&self, cx: &gpui::App) -> bool {
        self.palette.is_some()
            || self.launcher.is_some()
            || self.editor.is_some()
            || self.prompt.is_some()
            || self.ssh_import.is_some()
            || self.dialog.is_some()
            || self.filer.dialog.is_some()
            || self.git.dialog.is_some()
            || self.menu.is_some()
            || self
                .sidebar_sessions
                .as_ref()
                .is_some_and(|v| v.read(cx).has_overlay())
    }

    // ---- 全宽会话库与启动器 ----

    /// 切换全宽会话库视图。
    pub(super) fn toggle_sessions_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sessions_library_open {
            self.close_sessions_library(window, cx);
        } else {
            self.open_sessions_library(true, window, cx);
        }
    }

    /// 关闭全宽会话库，回到当前终端。
    pub(super) fn close_sessions_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = &self.sidebar_sessions {
            self.collapsed_launcher_groups = view.read(cx).collapsed_groups().clone();
        }
        if !self.sessions_library_open {
            return;
        }
        self.sessions_library_open = false;
        self.sync_focus(window, cx);
        cx.notify();
    }

    /// 打开全宽会话库，并刷新列表。
    pub(super) fn open_sessions_library(
        &mut self,
        focus_search: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = if let Some(existing) = self.sidebar_sessions.clone() {
            existing.update(cx, |v, cx| v.reload(cx));
            existing
        } else {
            let core = self.core.clone();
            let collapsed = self.collapsed_launcher_groups.clone();
            let v = cx.new(|cx| LauncherView::new_embedded(core, collapsed, window, cx));
            self.sidebar_sessions_sub =
                Some(
                    cx.subscribe_in(&v, window, |this, _l, ev: &LauncherEvent, window, cx| {
                        this.on_sidebar_sessions_event(ev.clone(), window, cx)
                    }),
                );
            self.sidebar_sessions = Some(v.clone());
            v
        };
        self.sessions_library_open = true;
        if focus_search {
            view.read(cx).focus_search(window, cx);
        }
        cx.notify();
    }

    /// 刷新已创建的会话库视图（会话新增/编辑/导入后调用）。
    pub(super) fn reload_sidebar_sessions(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = &self.sidebar_sessions {
            view.update(cx, |v, cx| v.reload(cx));
        }
    }

    fn on_sidebar_sessions_event(
        &mut self,
        ev: LauncherEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            LauncherEvent::Close => self.close_sessions_library(window, cx),
            LauncherEvent::CustomizeShell => self.present_editor(None, true, window, cx),
            LauncherEvent::OpenLocal => {
                self.open_tab(window, cx);
                self.close_sessions_library(window, cx);
            }
            LauncherEvent::NewSession => self.open_editor(None, window, cx),
            LauncherEvent::Connect(id) => {
                self.connect_profile(id, window, cx);
                self.close_sessions_library(window, cx);
            }
            LauncherEvent::Edit(id) => self.open_editor(Some(id), window, cx),
            LauncherEvent::ImportSshConfig => self.open_ssh_import(window, cx),
            LauncherEvent::LibraryChanged => {
                self.mcp.update(cx, |_, cx| cx.notify());
            }
        }
    }

    /// 打开会话管理：无论左侧或顶部标签栏，统一进入全宽会话库视图。
    pub(super) fn open_launcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open(cx) {
            return;
        }
        self.open_sessions_library(true, window, cx);
    }

    fn close_launcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(launcher) = self.launcher.take() {
            self.collapsed_launcher_groups = launcher.read(cx).collapsed_groups().clone();
        }
        self.launcher_sub = None;
        self.sync_focus(window, cx);
        cx.notify();
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
        self.present_editor(profile, false, window, cx);
    }

    fn present_editor(
        &mut self,
        profile: Option<ProfileId>,
        custom_local: bool,
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
        if self.launcher.is_some() {
            self.close_launcher(window, cx);
        }
        let core = self.core.clone();
        let editor = cx.new(|cx| SessionEditorView::new(core, existing, custom_local, window, cx));
        self.editor_sub = Some(cx.subscribe_in(
            &editor,
            window,
            |this, editor, ev: &EditorEvent, window, cx| {
                if matches!(ev, EditorEvent::LibraryChanged) {
                    this.mcp.update(cx, |_, cx| cx.notify());
                    return;
                }
                let open_local = match ev {
                    EditorEvent::OpenLocal(profile) => Some(profile.clone()),
                    _ => None,
                };
                let saved = match ev {
                    EditorEvent::Saved { id, connect } => Some((*id, *connect)),
                    _ => None,
                };
                editor.update(cx, |e, cx| e.wipe_secrets(cx));
                this.editor = None;
                this.editor_sub = None;
                if let Some(profile) = open_local {
                    this.open_shell(Some(profile), window, cx);
                }
                if let Some((id, connect)) = saved {
                    log(&format!("会话已保存 {id}"));
                    this.reload_sidebar_sessions(cx);
                    if connect {
                        this.connect_profile(id, window, cx);
                        this.close_sessions_library(window, cx);
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
                record_profile_usage(&self.core, id);
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

    /// 返回按使用频次与最近使用时间排序的前 `limit` 个非本地已保存会话（含会话专属色与主机地址）。
    pub(super) fn frequent_profiles(&self, limit: usize) -> Vec<QuickSessionItem> {
        let usage = load_profile_usage(&self.core);
        let mut profiles: Vec<_> = self
            .core
            .profiles()
            .into_iter()
            .filter(|p| !matches!(p.kind, ProfileKind::Local(_)))
            .collect();
        profiles.sort_by(|a, b| {
            let (ca, ta) = usage.get(&a.id.to_string()).copied().unwrap_or((0, 0));
            let (cb, tb) = usage.get(&b.id.to_string()).copied().unwrap_or((0, 0));
            cb.cmp(&ca)
                .then_with(|| tb.cmp(&ta))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        profiles
            .into_iter()
            .take(limit)
            .map(|p| {
                let rgb24 = match p.color {
                    Some(c) => (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b),
                    None => {
                        SESSION_COLORS
                            [TabPresentation::profile_color_index(p.id, SESSION_COLORS.len())]
                    }
                };
                let (tag, host) = match &p.kind {
                    ProfileKind::Sftp(s) => ("SFTP", s.host.as_str()),
                    ProfileKind::Ssh(s) => ("SSH", s.target.host.as_str()),
                    ProfileKind::Local(_) => ("SSH", ""),
                };
                let detail = (!host.is_empty()).then(|| host.to_owned());
                QuickSessionItem {
                    id: p.id,
                    name: p.name,
                    tag,
                    tone: MenuBadgeTone::Custom(rgb24),
                    detail,
                }
            })
            .collect()
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

const PROFILE_USAGE_KEY: &str = "ui.profile_usage";

fn load_profile_usage(core: &Core) -> HashMap<String, (u64, u64)> {
    core.setting(PROFILE_USAGE_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn record_profile_usage(core: &Core, id: ProfileId) {
    let mut usage = load_profile_usage(core);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let entry = usage.entry(id.to_string()).or_insert((0, 0));
    entry.0 = entry.0.saturating_add(1);
    entry.1 = now_ms;
    if let Ok(json) = serde_json::to_string(&usage) {
        let _ = core.set_setting(PROFILE_USAGE_KEY, json);
    }
}
