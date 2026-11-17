//! 窗口根视图：左侧竖向标签栏 + 分屏区。持有应用控制器（工作区模型），
//! 每个会话对应一个 [`TerminalView`] 实体；核心事件在这里统一接收并分发给对应终端。
//!
//! 行为对照 mac 版 `MainWindowController` / `Shell/` 与 `docs/spec/ui-shell.md`：
//! - 每个分屏（pane）里有若干标签，只显示活动标签；侧栏按分屏分组列出所有标签；
//! - 点击（鼠标按下）切换标签；分隔条可拖动；活动分屏有强调色边框，其余分屏压暗；
//! - 应用级快捷键查 `kanso_gpui::app::shortcuts` 的键位表，在捕获阶段拦截，
//!   其余按键照常进终端；
//! - 会话正常结束（退出码 0）自动关标签，异常结束保留并提示；最后一个标签关闭后退出应用。
//!
//! 菜单与对话框（控件在 `widgets/`，内容在 `kanso_gpui::app::{menus, confirm}`）：
//! - 终端右键、侧栏标签右键、主窗口顶栏的菜单按钮分别弹出终端菜单、标签菜单、应用菜单，
//!   菜单项都解码成 [`AppCommand`] 走 `run_command`，与快捷键同一条路径；
//! - 关闭 / 退出时若有仍在连接 / 运行的会话，弹确认对话框（批量关闭只确认一次）；
//! - 底部状态栏在 `status_bar.rs`。
//!
//! 主机密钥 / 密码提问的对话框由提示框模块负责，这里只留 TODO。

use kanso_gpui::app::i18n::{label, t};
use kanso_types::message::Message;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    Context, CursorStyle, Entity, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Render, SharedString, Subscription, Window, WindowHandle, div,
    prelude::*, px, rgba,
};
use kanso_core::Core;
use kanso_gpui::app::blocks::BlockPart;
use kanso_gpui::app::confirm::{CloseRequest, close_dialog_spec, is_confirmed};
use kanso_gpui::app::controller::{AppController, Effects, FALLBACK_GRID};
use kanso_gpui::app::gutter::{GutterPolicy, ToggleOutcome, core_mode};
use kanso_gpui::app::layout::{LayoutDivider, PaneId, Rect, SplitAxis};
use kanso_gpui::app::menus::{
    AppMenuState, TabMenuContext, app_menu, decode, tab_menu, terminal_menu,
};
use kanso_gpui::app::settings::DisplaySettings;
use kanso_gpui::app::shell::{DIVIDER_THICKNESS, DividerDrag, min_fraction};
use kanso_gpui::app::shortcuts::{AppCommand, lookup};
use kanso_gpui::app::status_bar::{clock_text, ms_until_next_minute};
use kanso_gpui::app::tab::{CloseNote, TabConnection};
use kanso_gpui::app::workspace::{CloseConfirmation, CloseScope};
use kanso_gpui::keys::KeyInput;
use kanso_gpui::widgets::dialog::DialogResult;
use kanso_gpui::widgets::menu::MenuItem;
use kanso_gpui::widgets::style::{Appearance, color, number};
use kanso_types::{CoreEvent, PromptId, SessionId};

use super::display;
use super::launcher::LauncherView;
use super::session_editor::SessionEditorView;
use super::settings::{self, SettingsEvent, SettingsModel, SettingsView};
use super::shell_selftest::ShellSelfTest;
use super::view::{TerminalEvent, TerminalView};
use super::widgets::dialog::{Dialog, DialogEvent};
use super::widgets::menu::{ContextMenu, MenuEvent};
use super::{EXIT_CODE, log, tz};

/// 状态栏高度（逻辑像素，取自设计 token `statusBar.height`）。
pub(crate) fn status_bar_h() -> f32 {
    number("spacing", "statusBar.height")
}

/// 现在的 Unix 毫秒。
pub(super) fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// 一帧内最多合并处理多少个核心事件，避免事件风暴时长时间占住 UI 线程。
const MAX_EVENT_BATCH: usize = 256;

/// 当前外观下的界面配色（取自 `design/tokens.json`，与 mac 前端同一份）。
#[derive(Clone, Copy)]
pub(crate) struct Theme {
    pub appearance: Appearance,
}

impl Theme {
    pub(crate) fn c(&self, name: &str) -> Hsla {
        rgba(color(name, self.appearance).to_u32()).into()
    }

    /// 带不透明度的颜色（0..=255）。
    pub(crate) fn ca(&self, name: &str, alpha: u8) -> Hsla {
        rgba(color(name, self.appearance).with_alpha(alpha).to_u32()).into()
    }
}

/// 根视图。
pub(crate) struct ShellView {
    pub(super) core: Arc<Core>,
    pub(super) ctl: AppController<Arc<Core>>,
    pub(super) terminals: HashMap<SessionId, Entity<TerminalView>>,
    subs: HashMap<SessionId, Subscription>,
    _window_subs: Vec<Subscription>,
    pub(super) sidebar_visible: bool,
    pub(super) tab_reorder: Option<kanso_gpui::app::tab::TabReorder>,
    pub(super) palette: Option<super::toolbar::Palette>,
    pub(super) path_filter: Entity<super::widgets::text_input::TextInput>,
    drag: Option<DividerDrag>,
    /// 当前弹出的右键 / 应用菜单（最上层，同一时间只有一个）。
    pub(super) menu: Option<Entity<ContextMenu>>,
    pub(super) menu_sub: Option<Subscription>,
    /// 当前的关闭 / 退出确认对话框，以及它确认后要执行的关闭请求。
    pub(super) dialog: Option<Entity<Dialog>>,
    pub(super) dialog_sub: Option<Subscription>,
    pending_close: Option<CloseRequest>,
    /// “恢复默认设置”的确认框正在显示。
    pub(super) pending_reset: bool,
    /// 右键行为：弹菜单 / 复制或粘贴（terminal.md §8.1）。
    pub(super) right_click_menu: bool,
    /// 状态栏时钟文字（整分钟刷新）。
    pub(super) clock: String,
    /// 底部的临时提示（文字、到期时刻）。
    notice: Option<(String, Instant)>,
    dark: bool,
    /// 显示设置模型（字体 / 外观 / 时间轴），设置窗口与外壳共用。
    pub(super) settings: Entity<SettingsModel>,
    settings_window: Option<WindowHandle<SettingsView>>,
    window_active: bool,
    /// 最近一次通知核心“有输入焦点”的会话。
    focused: Option<SessionId>,
    last_title: String,
    /// `--log-input` 下上一次写进日志的布局（变化才再写）。
    last_layout_log: String,
    /// `--selftest`：终端阶段的超时；由第一个终端取走。
    pending_terminal_selftest: Option<Duration>,
    pub(super) selftest_timeout: Option<Duration>,
    pub(super) shell_selftest: Option<ShellSelfTest>,
    /// 会话启动器浮层（`ui/shell_launcher.rs`）。
    pub(super) launcher: Option<Entity<LauncherView>>,
    pub(super) launcher_sub: Option<Subscription>,
    /// 会话编辑对话框。
    pub(super) editor: Option<Entity<SessionEditorView>>,
    pub(super) editor_sub: Option<Subscription>,
    /// 正在显示的连接提示框（主机密钥 / 密码 / 二次验证）。
    pub(super) prompt: Option<(PromptId, Entity<Dialog>)>,
    /// 时间戳 / 行号栏模式：全局设置 + AI 会话的临时覆盖（terminal.md §2）。
    pub(super) gutter_policy: GutterPolicy,
    pub(super) prompt_sub: Option<Subscription>,
    /// 底部 Sender 面板（`ui/shell_sender.rs`）。
    pub(super) sender: super::shell_sender::SenderState,
    /// 命令补全（`ui/shell_completion.rs`）。
    pub(super) completion: super::shell_completion::CompletionCtl,
    /// 导入 OpenSSH 配置对话框（`ui/ssh_import.rs`）。
    pub(super) ssh_import: Option<Entity<super::ssh_import::SshImportView>>,
    pub(super) ssh_import_sub: Option<Subscription>,
    /// 数据导入 / 导出流程里等待结果的对话框（`ui/shell_data.rs`）。
    pub(super) pending_data: Option<super::shell_data::PendingData>,
    /// 文件功能（辅助 Filer、纯 SFTP 双栏、终端内传输）的状态，见 `shell_filer.rs`。
    pub(super) filer: super::shell_filer::FilerState,
    /// Git 改动面板（终端右侧），见 `shell_git.rs`。
    pub(super) git: super::shell_git::GitState,
}

impl ShellView {
    pub(crate) fn new(
        core: Arc<Core>,
        mut rx: UnboundedReceiver<CoreEvent>,
        selftest: Option<Duration>,
        themes_dir: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        let settings = cx.new(|_| SettingsModel::new(core.clone(), themes_dir));
        settings.read(cx).push_to_display(cx);
        let dark = display::effective_dark(window);
        display::apply_titlebar(window);
        let path_filter = cx.new(|cx| {
            super::widgets::text_input::TextInput::new(window, cx)
                .search()
                .placeholder(kanso_types::Message::UiFilterPath {})
        });
        let filter_sub = cx.subscribe_in(&path_filter, window, |this, input, ev, window, cx| {
            use super::widgets::text_input::TextInputEvent;
            match ev {
                TextInputEvent::Change(_) => cx.notify(),
                TextInputEvent::Escape => {
                    input.update(cx, |i, cx| i.set_text("", cx));
                    this.sync_focus(window, cx);
                    cx.notify();
                }
                _ => {}
            }
        });
        let sender_panel = cx.new(|cx| super::sender::SenderPanel::new(core.clone(), window, cx));
        let sender_sub = cx.subscribe_in(
            &sender_panel,
            window,
            |this, _p, ev: &super::sender::SenderEvent, window, cx| {
                this.on_sender_event(ev, window, cx)
            },
        );
        let completion = super::shell_completion::CompletionCtl::load(&core);
        let filer = super::shell_filer::new_state(&core, window, cx);
        let git = super::shell_git::new_state(window, cx);
        let mut this = ShellView {
            ctl: AppController::new(core.clone(), home),
            core,
            terminals: HashMap::new(),
            subs: HashMap::new(),
            _window_subs: Vec::new(),
            sidebar_visible: true,
            tab_reorder: None,
            palette: None,
            path_filter,
            drag: None,
            menu: None,
            menu_sub: None,
            dialog: None,
            dialog_sub: None,
            pending_close: None,
            pending_reset: false,
            right_click_menu: true,
            clock: clock_text(unix_ms(), tz::local_offset_minutes()),
            notice: None,
            dark,
            settings,
            settings_window: None,
            window_active: true,
            focused: None,
            last_title: String::new(),
            last_layout_log: String::new(),
            pending_terminal_selftest: selftest,
            selftest_timeout: selftest,
            shell_selftest: None,
            launcher: None,
            launcher_sub: None,
            editor: None,
            editor_sub: None,
            prompt: None,
            prompt_sub: None,
            gutter_policy: GutterPolicy::default(),
            sender: super::shell_sender::SenderState::new(sender_panel),
            completion,
            ssh_import: None,
            ssh_import_sub: None,
            pending_data: None,
            filer,
            git,
        };
        {
            let timeline = *this.settings.read(cx).timeline();
            this.gutter_policy.set_global(timeline);
            this.core.set_timestamp_mode(None, core_mode(timeline.mode));
        }

        // 点窗口关闭按钮：有仍在连接的会话先确认（spec §8）。
        let weak = cx.weak_entity();
        window.on_window_should_close(&*cx, move |window, cx| {
            weak.update(cx, |this, cx| this.window_close_requested(window, cx))
                .unwrap_or(true)
        });

        // 状态栏时钟：只在整分钟刷新（状态栏不显示秒，不要每秒唤醒）。
        cx.spawn(async move |view, cx| {
            loop {
                let wait = ms_until_next_minute(unix_ms()) + 20;
                cx.background_executor()
                    .timer(Duration::from_millis(wait))
                    .await;
                let ok = view
                    .update(cx, |this, cx| {
                        let text = clock_text(unix_ms(), tz::local_offset_minutes());
                        if text != this.clock {
                            this.clock = text;
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !ok {
                    break;
                }
            }
        })
        .detach();

        // 核心事件 → UI 线程。核心保证每帧最多唤醒一次（TerminalDirty 合并），这里再把已到达的事件合并成一批。
        cx.spawn_in(window, async move |view, cx| {
            while let Some(first) = rx.next().await {
                let mut batch = vec![first];
                while batch.len() < MAX_EVENT_BATCH {
                    match rx.try_recv() {
                        Ok(ev) => batch.push(ev),
                        _ => break,
                    }
                }
                let ok = view
                    .update_in(cx, |this, window, cx| {
                        this.on_core_events(batch, window, cx)
                    })
                    .is_ok();
                if !ok {
                    break;
                }
            }
        })
        .detach();

        let activation = cx.observe_window_activation(window, |this, window, cx| {
            this.window_active = window.is_window_active();
            if !this.window_active {
                this.cancel_tab_reorder(cx);
            }
            this.sync_focus(window, cx);
            let active = this.window_active;
            for t in this.terminals.values() {
                t.update(cx, |t, cx| t.on_window_activation(active, cx));
            }
        });
        // 系统深浅色变化（GPUI 在 Windows 上监听 ImmersiveColorSet）：跟随系统时整窗切换。
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            this.refresh_display(window, cx);
            log(&format!(
                "系统外观变化：系统深色={} -> {}",
                display::system_is_dark(window),
                this.describe_terminal(cx)
            ));
        });
        let settings_sub = cx.subscribe_in(
            &this.settings.clone(),
            window,
            |this, _model, ev: &SettingsEvent, window, cx| {
                let SettingsEvent::Changed(section) = ev;
                if super::LOG_INPUT.load(Ordering::Relaxed) {
                    log(&format!("设置已变更：{section:?}"));
                }
                this.settings.read(cx).push_to_display(cx);
                this.sync_gutter_global(cx);
                this.refresh_display(window, cx);
            },
        );
        this._window_subs = vec![activation, appearance, settings_sub, filter_sub, sender_sub];
        {
            let m = this.settings.read(cx);
            log(&format!(
                "设置已载入：display={:?} appearance={:?} timeline={:?}",
                m.display(),
                m.appearance(),
                m.timeline()
            ));
        }
        log(&format!(
            "外观：系统深色={} 生效深色={}",
            display::system_is_dark(window),
            this.dark
        ));

        if super::TERM_TEST.load(Ordering::Relaxed) {
            this.run_term_test(window, cx);
        }
        if super::SETTINGS_TEST.load(Ordering::Relaxed) {
            this.run_settings_test(window, cx);
        }
        if super::SENDER_TEST.load(Ordering::Relaxed) {
            this.run_sender_test(window, cx);
        }
        if super::GIT_TEST.load(Ordering::Relaxed) {
            this.run_git_selftest(window, cx);
        }
        if super::FILER_TEST.load(Ordering::Relaxed) {
            this.run_filer_selftest(window, cx);
        }

        // 第一个标签。
        match this.ctl.open_local_shell(None, FALLBACK_GRID) {
            Ok(id) => {
                log(&format!("本地会话已打开（{id:?}）"));
                this.attach_terminal(id, window, cx);
                this.after_change(window, cx);
            }
            Err(e) => {
                log(&format!("打开本地会话失败：{e}"));
                EXIT_CODE.store(1, Ordering::SeqCst);
                cx.defer(|cx| cx.quit());
            }
        }
        this
    }

    // ---- 终端实体 ----

    pub(super) fn attach_terminal(
        &mut self,
        id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let core = self.core.clone();
        let selftest = self.pending_terminal_selftest.take();
        let track = self.selftest_timeout.is_some();
        let dark = self.dark;
        let right_click_menu = self.right_click_menu;
        let term = cx.new(|cx| {
            let mut t = TerminalView::new(core, id, selftest, track, dark, window, cx);
            t.right_click_menu = right_click_menu;
            t
        });
        let palette = self.terminal_palette(cx);
        term.update(cx, |t, cx| t.set_palette(palette, cx));
        let sub = cx.subscribe_in(
            &term,
            window,
            |this, _term, ev: &TerminalEvent, window, cx| this.on_terminal_event(ev, window, cx),
        );
        self.terminals.insert(id, term);
        self.subs.insert(id, sub);
        self.apply_gutter(id, cx);
    }

    // ---- 时间戳 / 行号栏 ----

    /// 把会话当前该用的栏模式应用到它的终端视图与核心。
    fn apply_gutter(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let mode = self.gutter_policy.effective(id);
        if let Some(t) = self.terminals.get(&id) {
            t.update(cx, |t, cx| t.set_gutter_mode(mode, cx));
        }
        self.core.set_timestamp_mode(Some(id), core_mode(mode));
    }

    /// 全局时间轴设置变了（设置窗口、状态栏开关）：同步策略并刷新所有终端。
    fn sync_gutter_global(&mut self, cx: &mut Context<Self>) {
        let timeline = *self.settings.read(cx).timeline();
        self.gutter_policy.set_global(timeline);
        self.core.set_timestamp_mode(None, core_mode(timeline.mode));
        let ids: Vec<SessionId> = self.terminals.keys().copied().collect();
        for id in ids {
            self.apply_gutter(id, cx);
        }
    }

    /// 按各会话的 agent 状态更新临时设置：识别到 AI CLI 时默认隐藏栏，工具退出后恢复。
    fn sync_agents(&mut self, cx: &mut Context<Self>) {
        let states: Vec<(SessionId, Option<kanso_types::agent::AgentKind>)> = self
            .terminals
            .keys()
            .map(|&id| {
                let agent = self
                    .ctl
                    .workspace()
                    .item(id)
                    .and_then(|i| i.activity.agent());
                (id, agent)
            })
            .collect();
        for (id, agent) in states {
            if self.gutter_policy.observe_agent(id, agent).is_some() {
                self.apply_gutter(id, cx);
            }
        }
    }

    /// 状态栏“时间戳”开关：AI 会话里只作用于当前会话，否则切换全局模式。
    pub(super) fn toggle_timeline(&mut self, cx: &mut Context<Self>) {
        let active = self.ctl.workspace().active_session();
        match self.gutter_policy.toggle(active) {
            ToggleOutcome::Session(_) => {
                if let Some(id) = active {
                    self.apply_gutter(id, cx);
                }
            }
            ToggleOutcome::Global(t) => {
                self.settings.update(cx, |m, cx| {
                    m.update_timeline(cx, |tl| *tl = t);
                });
            }
        }
        cx.notify();
    }

    /// 活动会话的终端实体。
    pub(super) fn active_terminal(&self) -> Option<Entity<TerminalView>> {
        self.ctl
            .workspace()
            .active_session()
            .and_then(|s| self.terminals.get(&s))
            .cloned()
    }

    fn on_terminal_event(
        &mut self,
        ev: &TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            TerminalEvent::SelfTestFinished(ok) => self.on_terminal_selftest(*ok, window, cx),
            TerminalEvent::Notice(text) => self.show_notice(text.clone(), cx),
            TerminalEvent::ContextMenu { session, position } => {
                // 右键的终端成为活动终端，菜单里的复制 / 粘贴等就作用于它。
                self.activate_session(*session, window, cx);
                self.open_terminal_menu(*session, *position, window, cx);
            }
        }
    }

    // ---- 核心事件 ----

    fn on_core_events(
        &mut self,
        batch: Vec<CoreEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filer_core_events(&batch, cx);
        let git_sessions = Self::git_event_sessions(&batch);
        let mut fx = Effects::default();
        for ev in batch {
            fx.merge(self.ctl.handle_event(ev));
        }
        self.git_after_events(&git_sessions, cx);
        if fx.terminal {
            for id in self.ctl.take_dirty() {
                if let Some(t) = self.terminals.get(&id) {
                    t.update(cx, |t, cx| t.pull_frame(cx));
                }
            }
            self.completion_refresh(window, cx);
        }
        self.sync_agents(cx);
        if fx.prompt {
            self.sync_prompt(window, cx);
        }
        if fx.notice {
            for n in self.ctl.take_notices() {
                // TODO(notice-ui)：用 kanso-l10n 渲染并显示成提示条。
                log(&format!("核心通知 {:?} {:?}", n.level, n.message));
            }
        }
        // shell 正常退出：关掉标签（异常退出的标签保留，侧栏显示退出码）。
        let finished = self.ctl.take_auto_close();
        if !finished.is_empty() {
            log(&format!("会话已结束，自动关闭标签：{finished:?}"));
            self.close_now(&finished, window, cx);
            return;
        }
        if !fx.is_none() {
            cx.notify();
        }
    }

    // ---- 命令 ----

    pub(super) fn run_command(
        &mut self,
        command: AppCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 菜单或对话框打开时是模态的：快捷键不穿透。
        if self.modal_open() {
            return;
        }
        match command {
            AppCommand::NewLocalTab => self.open_tab(window, cx),
            AppCommand::OpenPalette => self.open_palette(window, cx),
            AppCommand::OpenLauncher => self.open_launcher(window, cx),
            AppCommand::NewSession => self.open_editor(None, window, cx),
            AppCommand::CloseSession => {
                if let Some(id) = self.ctl.workspace().active_session() {
                    self.request_close(id, window, cx);
                }
            }
            AppCommand::NextTab => self.navigate(window, cx, |w| w.select_next_tab_in_pane()),
            AppCommand::PrevTab => self.navigate(window, cx, |w| w.select_previous_tab_in_pane()),
            AppCommand::NextPane => self.navigate(window, cx, |w| w.select_next_pane()),
            AppCommand::PrevPane => self.navigate(window, cx, |w| w.select_previous_pane()),
            AppCommand::SelectTab(i) => self.navigate(window, cx, move |w| {
                w.select_tab_at_sidebar_index(i);
            }),
            AppCommand::SplitRight => self.split(SplitAxis::Horizontal, window, cx),
            AppCommand::SplitDown => self.split(SplitAxis::Vertical, window, cx),
            AppCommand::ToggleSidebar => {
                self.sidebar_visible = !self.sidebar_visible;
                self.sync_focus(window, cx);
                cx.notify();
            }
            AppCommand::Copy => self.with_active_terminal(cx, |t, cx| t.copy(cx)),
            AppCommand::Paste => self.with_active_terminal(cx, |t, cx| t.paste(cx)),
            AppCommand::SelectAll => self.with_active_terminal(cx, |t, _| t.select_all()),
            AppCommand::ClearBuffer => self.with_active_terminal(cx, |t, _| t.clear_buffer()),
            AppCommand::Find => {
                if let Some(t) = self.active_terminal() {
                    t.update(cx, |t, cx| t.open_find(window, cx));
                }
            }
            AppCommand::FindNext | AppCommand::FindPrev => {
                let backwards = command == AppCommand::FindPrev;
                if let Some(t) = self.active_terminal() {
                    t.update(cx, |t, cx| t.find_step(backwards, window, cx));
                }
            }
            AppCommand::PrevBlock => self.with_active_terminal(cx, |t, cx| t.jump_block(false, cx)),
            AppCommand::NextBlock => self.with_active_terminal(cx, |t, cx| t.jump_block(true, cx)),
            AppCommand::ToggleTimeline => self.toggle_timeline(cx),
            AppCommand::SetThemeMode(mode) => self
                .settings
                .update(cx, |m, cx| m.update_appearance(cx, |a| a.mode = mode)),
            AppCommand::SetGutterMode(mode) => self
                .settings
                .update(cx, |m, cx| m.update_timeline(cx, |t| t.set_mode(mode))),
            AppCommand::CopyBlockCommand => {
                self.with_active_terminal(cx, |t, cx| t.copy_menu_block(BlockPart::Command, cx))
            }
            AppCommand::CopyBlockOutput => {
                self.with_active_terminal(cx, |t, cx| t.copy_menu_block(BlockPart::Output, cx))
            }
            AppCommand::CloseTab(id) => self.request_close(id, window, cx),
            AppCommand::CloseGroup(scope, id) => self.close_group(scope, id, window, cx),
            AppCommand::ToggleRightClickMenu => {
                self.right_click_menu = !self.right_click_menu;
                let on = self.right_click_menu;
                for t in self.terminals.values() {
                    t.update(cx, |t, _| t.right_click_menu = on);
                }
                cx.notify();
            }
            AppCommand::Quit => self.request_quit(window, cx),
            AppCommand::EditSession(id) => {
                let profile = self.ctl.workspace().item(id).and_then(|i| i.profile);
                if let Some(p) = profile {
                    self.open_editor(Some(p), window, cx);
                }
            }
            AppCommand::OpenSettings => self.open_settings(cx),
            AppCommand::OpenDisplaySettings | AppCommand::OpenKeyboardSettings => {
                self.open_settings(cx);
                if let Some(handle) = self.settings_window {
                    let page = if command == AppCommand::OpenDisplaySettings {
                        1
                    } else {
                        2
                    };
                    let _ = handle.update(cx, |settings, _, cx| settings.select_page(page, cx));
                }
            }
            AppCommand::ResetSettings => self.request_reset_settings(window, cx),
            AppCommand::ToggleSender => self.toggle_sender(window, cx),
            AppCommand::ToggleCommandSuggestions => self.toggle_completion(window, cx),
            AppCommand::ClearCommandHistory => self.request_clear_history(window, cx),
            AppCommand::ExportData => self.export_data(window, cx),
            AppCommand::ImportData => self.import_data(window, cx),
            AppCommand::ImportSshConfig => self.open_ssh_import(window, cx),
            AppCommand::ToggleFiler => self.toggle_filer(window, cx),
            AppCommand::ToggleGit => self.toggle_git(window, cx),
            AppCommand::LocateFiler => self.locate_terminal_dir(true, window, cx),
            AppCommand::XmodemSend => self.run_xmodem(Some(kanso_core::XmodemBlock::Bytes128), cx),
            AppCommand::XmodemSend1K => self.run_xmodem(Some(kanso_core::XmodemBlock::Bytes1K), cx),
            AppCommand::XmodemReceive => self.run_xmodem(None, cx),
            AppCommand::CancelTerminalTransfer => self.cancel_term_transfer(),
            AppCommand::ZoomIn => self.zoom(1.0, cx),
            AppCommand::ZoomOut => self.zoom(-1.0, cx),
            AppCommand::ZoomReset => self.settings.update(cx, |m, cx| {
                m.update_display(cx, |d| d.font_size = DisplaySettings::DEFAULT_FONT_SIZE)
            }),
        }
    }

    /// 打开设置窗口（已打开就带到前台）。
    pub(super) fn open_settings(&mut self, cx: &mut Context<Self>) {
        let model = self.settings.clone();
        self.settings_window = settings::open_window(model, self.settings_window, cx);
    }

    /// 终端文字放大 / 缩小：直接改显示设置里的终端字号（所有终端一起变）。
    fn zoom(&mut self, delta: f64, cx: &mut Context<Self>) {
        self.settings.update(cx, |m, cx| {
            m.update_display(cx, |d| d.font_size = d.zoomed(delta))
        });
    }

    /// 当前外观下的终端配色。
    fn terminal_palette(&self, cx: &gpui::App) -> kanso_gpui::theme::Palette {
        self.settings.read(cx).terminal_palette(self.dark)
    }

    /// 一行描述第一个终端的度量与配色（自检日志用）。
    fn describe_terminal(&self, cx: &gpui::App) -> String {
        match self.terminals.values().next() {
            Some(t) => {
                let t = t.read(cx);
                format!(
                    "dark={} 单元格 {:.2}x{:.2} 字号 {} 背景 {:06X} 行列 {}x{}",
                    self.dark,
                    t.metrics.cell_w,
                    t.metrics.cell_h,
                    t.metrics.font_size,
                    t.palette.background.to_u32(),
                    t.size.cols,
                    t.size.rows
                )
            }
            None => "无终端".to_owned(),
        }
    }

    /// `--settings-test`：不靠截图，直接驱动设置模型并记录终端的反应。
    pub(super) fn run_settings_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let step = |cx: &mut gpui::AsyncWindowContext| {
                let cx = cx.background_executor().clone();
                async move { cx.timer(Duration::from_millis(1200)).await }
            };
            let _ = this.update_in(cx, |this, _w, cx| {
                log(&format!("[test] 初始 {}", this.describe_terminal(cx)));
                this.open_settings(cx);
                log(&format!(
                    "[test] 设置窗口已打开={}",
                    this.settings_window.is_some()
                ));
            });
            step(cx).await;
            type Edit = Box<dyn Fn(&mut SettingsModel, &mut Context<SettingsModel>)>;
            let edits: Vec<(&str, Edit)> = vec![
                (
                    "字号 20",
                    Box::new(|m, cx| m.update_display(cx, |d| d.font_size = 20.0)),
                ),
                (
                    "行距 1.5",
                    Box::new(|m, cx| m.update_display(cx, |d| d.line_height = 1.5)),
                ),
                (
                    "字体 Consolas",
                    Box::new(|m, cx| m.update_display(cx, |d| d.font_family = "Consolas".into())),
                ),
                (
                    "强制浅色",
                    Box::new(|m, cx| {
                        m.update_appearance(cx, |a| {
                            a.mode = kanso_gpui::app::settings::ThemeMode::Light
                        })
                    }),
                ),
                (
                    "强制深色",
                    Box::new(|m, cx| {
                        m.update_appearance(cx, |a| {
                            a.mode = kanso_gpui::app::settings::ThemeMode::Dark
                        })
                    }),
                ),
                (
                    "跟随系统",
                    Box::new(|m, cx| {
                        m.update_appearance(cx, |a| {
                            a.mode = kanso_gpui::app::settings::ThemeMode::System
                        })
                    }),
                ),
            ];
            for (name, edit) in edits {
                let _ = this.update_in(cx, |this, _w, cx| {
                    this.settings.update(cx, |m, cx| edit(m, cx));
                });
                step(cx).await;
                let _ = this.update_in(cx, |this, _w, cx| {
                    log(&format!("[test] {name} -> {}", this.describe_terminal(cx)));
                });
            }
            let _ = this.update_in(cx, |this, _w, cx| {
                log(&format!(
                    "[test] 已写入核心 display={:?} appearance={:?}",
                    this.core.setting("display"),
                    this.core.setting("appearance")
                ));
                this.run_keys_check(cx);
                if !super::KEEP_OPEN.load(Ordering::Relaxed) {
                    cx.quit();
                }
            });
        })
        .detach();
    }

    /// 外观或显示设置变了：重新算深浅色，并让所有终端换配色、重新量字体。
    fn refresh_display(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dark = display::effective_dark(window);
        display::apply_titlebar(window);
        let palette = self.terminal_palette(cx);
        for t in self.terminals.values() {
            t.update(cx, |t, cx| {
                t.set_palette(palette.clone(), cx);
                t.apply_display(window, cx);
            });
        }
        cx.notify();
    }

    /// 在活动会话的终端上执行一个操作。
    fn with_active_terminal(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut TerminalView, &mut Context<TerminalView>),
    ) {
        if let Some(t) = self
            .ctl
            .workspace()
            .active_session()
            .and_then(|s| self.terminals.get(&s))
        {
            t.update(cx, f);
        }
    }

    // ---- 菜单 ----

    /// 弹出菜单（替换已有的）。选中项解码成命令执行，关闭后焦点还给活动终端。
    pub(super) fn open_menu(
        &mut self,
        items: Vec<MenuItem>,
        anchor: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog.is_some() {
            return;
        }
        self.menu_sub = None;
        let menu = cx.new(|cx| ContextMenu::new(items, anchor, window, cx));
        self.menu_sub =
            Some(
                cx.subscribe_in(&menu, window, |this, _m, ev: &MenuEvent, window, cx| {
                    this.menu = None;
                    this.menu_sub = None;
                    this.sync_focus(window, cx);
                    if let MenuEvent::Activate(id) = ev {
                        if id.starts_with(kanso_gpui::app::sender::MENU_PREFIX) {
                            let panel = this.sender.panel.clone();
                            panel.update(cx, |p, cx| p.on_menu_action(id, window, cx));
                            cx.notify();
                            return;
                        }
                        match decode(id) {
                            Some(command) => this.run_command(command, window, cx),
                            None => log(&format!("未知菜单项 {id}")),
                        }
                    }
                    cx.notify();
                }),
            );
        self.menu = Some(menu);
        cx.notify();
    }

    pub(super) fn open_terminal_menu(
        &mut self,
        session: SessionId,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (has_selection, block) = self.terminals.get(&session).map_or((false, None), |t| {
            let t = t.read(cx);
            (t.has_selection(), t.menu_block_menu())
        });
        self.open_menu(terminal_menu(has_selection, block), position, window, cx);
    }

    pub(super) fn open_tab_menu(
        &mut self,
        session: SessionId,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ws = self.ctl.workspace();
        let has = |scope| !ws.close_targets(scope, session).is_empty();
        let ctx = TabMenuContext {
            session,
            from_profile: ws.item(session).is_some_and(|i| i.profile.is_some()),
            has_others: has(CloseScope::Others),
            has_above: has(CloseScope::Above),
            has_below: has(CloseScope::Below),
        };
        self.open_menu(tab_menu(ctx), position, window, cx);
    }

    pub(super) fn app_menu_state(&self, cx: &gpui::App) -> AppMenuState {
        AppMenuState {
            sidebar_visible: self.sidebar_visible,
            right_click_menu: self.right_click_menu,
            sender_visible: self.sender.visible,
            command_suggestions: self.completion.enabled,
            filer_visible: self.filer.visible(),
            git_visible: self.git.visible(),
            gutter_mode: self.gutter_policy.global().mode,
            theme_mode: self.settings.read(cx).appearance().mode,
            tab_count: self.ctl.workspace().sidebar_order().len(),
        }
    }

    pub(super) fn open_app_menu(
        &mut self,
        anchor: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items = app_menu(self.app_menu_state(cx));
        self.open_menu(items, anchor, window, cx);
    }

    fn navigate(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut kanso_gpui::app::workspace::Workspace),
    ) {
        self.ctl.edit(f);
        self.sync_focus(window, cx);
        cx.notify();
    }

    pub(super) fn open_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.ctl.open_local_shell(None, FALLBACK_GRID) {
            Ok(id) => {
                self.attach_terminal(id, window, cx);
                self.after_change(window, cx);
            }
            Err(e) => self.show_notice(
                t(&Message::UiCantOpenLocalShell {
                    detail: e.to_string(),
                }),
                cx,
            ),
        }
    }

    fn split(&mut self, axis: SplitAxis, window: &mut Window, cx: &mut Context<Self>) {
        match self.ctl.split(axis, FALLBACK_GRID) {
            Ok(id) => {
                self.attach_session(id, window, cx);
                self.after_change(window, cx);
            }
            Err(e) => self.show_notice(
                t(&Message::UiCantSplit {
                    detail: e.to_string(),
                }),
                cx,
            ),
        }
    }

    fn request_close(&mut self, id: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        self.request_close_many(vec![id], window, cx);
    }

    /// 批量关闭同组标签（只确认一次，列出仍在连接的会话）。
    fn close_group(
        &mut self,
        scope: CloseScope,
        relative_to: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets = self.ctl.close_targets(scope, relative_to);
        self.request_close_many(targets, window, cx);
    }

    /// 关闭一批会话：不需要确认就直接关，否则弹确认对话框（spec §8）。
    fn request_close_many(
        &mut self,
        sessions: Vec<SessionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if sessions.is_empty() {
            return;
        }
        match self.ctl.close_confirmation(&sessions) {
            None => self.close_now(&sessions, window, cx),
            Some(c) => {
                let req = CloseRequest {
                    sessions,
                    quit: false,
                };
                self.show_close_dialog(req, &c, window, cx);
            }
        }
    }

    /// 退出应用：有仍在连接的会话先确认。
    fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.confirm_quit_if_needed(window, cx) {
            cx.quit();
        }
    }

    /// 点窗口关闭按钮：返回 `true` 表示可以直接关闭，`false` 表示已弹出确认框（或确认框已在）。
    fn window_close_requested(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.dialog.is_some() {
            return false;
        }
        !self.confirm_quit_if_needed(window, cx)
    }

    /// 需要确认时弹出退出确认框并返回 `true`。
    fn confirm_quit_if_needed(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let all = self.ctl.workspace().sidebar_order();
        let Some(c) = self.ctl.close_confirmation(&all) else {
            return false;
        };
        let req = CloseRequest {
            sessions: Vec::new(),
            quit: true,
        };
        self.show_close_dialog(req, &c, window, cx);
        true
    }

    /// 弹出关闭 / 退出确认框，结果由 [`Self::on_dialog_result`] 处理。已有对话框时忽略。
    pub(super) fn show_close_dialog(
        &mut self,
        req: CloseRequest,
        c: &CloseConfirmation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog.is_some() {
            return;
        }
        self.menu = None;
        self.menu_sub = None;
        let titles: Vec<String> = c
            .sessions
            .iter()
            .map(|s| {
                self.ctl
                    .tab_title(*s)
                    .unwrap_or_else(|| label(Message::UiSession {}).to_owned())
            })
            .collect();
        let spec = close_dialog_spec(&titles, c.interrupts_transfers, req.quit);
        let dialog = cx.new(|cx| Dialog::new(spec, window, cx));
        self.dialog_sub =
            Some(
                cx.subscribe_in(&dialog, window, |this, _d, ev: &DialogEvent, window, cx| {
                    let DialogEvent::Result(r) = ev;
                    this.on_dialog_result(r, window, cx);
                }),
            );
        self.dialog = Some(dialog);
        self.pending_close = Some(req);
        log(&format!("关闭确认：{} 个会话需要确认", c.sessions.len()));
        cx.notify();
    }

    pub(super) fn on_dialog_result(
        &mut self,
        r: &DialogResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dialog = None;
        self.dialog_sub = None;
        if let Some(p) = self.pending_data.take() {
            self.on_data_dialog(p, r, window, cx);
            return;
        }
        let pending = self.pending_close.take();
        let confirmed = is_confirmed(r);
        if std::mem::take(&mut self.pending_reset) {
            if confirmed {
                self.reset_settings(cx);
            }
            self.sync_focus(window, cx);
            cx.notify();
            return;
        }
        if std::mem::take(&mut self.completion.pending_clear) {
            if confirmed {
                self.clear_history(cx);
            }
            self.sync_focus(window, cx);
            cx.notify();
            return;
        }
        log(&format!("关闭确认结果：确认={confirmed} {}", r.log_line()));
        match pending {
            Some(req) if confirmed && req.quit => cx.quit(),
            Some(req) if confirmed => self.close_now(&req.sessions, window, cx),
            _ => {
                self.sync_focus(window, cx);
                cx.notify();
            }
        }
    }

    /// 立即关闭（确认已完成）。
    pub(super) fn close_now(
        &mut self,
        sessions: &[SessionId],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let closed = self.ctl.close_sessions(sessions);
        self.completion_reset();
        for id in &closed {
            self.filer_forget(*id, cx);
        }
        for id in closed {
            self.terminals.remove(&id);
            self.subs.remove(&id);
            self.gutter_policy.forget(id);
            if self.focused == Some(id) {
                self.focused = None;
            }
        }
        self.after_change(window, cx);
    }

    /// 工作区结构变化后的收尾：空了就退出，否则把焦点交给活动终端并重绘。
    pub(super) fn after_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ctl.workspace().is_empty() {
            // 没有标签时显示空状态页（ui-shell.md §11）；自检运行时仍按旧行为退出。
            if self.selftest_timeout.is_some() || self.shell_selftest.is_some() {
                log("最后一个标签已关闭，退出应用");
                cx.quit();
                return;
            }
            self.sync_focus(window, cx);
            cx.notify();
            return;
        }
        self.sync_focus(window, cx);
        cx.notify();
    }

    /// 点击某个标签（侧栏）或分屏：切换活动会话。
    pub(super) fn activate_session(
        &mut self,
        id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ctl.workspace().active_session() == Some(id) {
            // 已经是活动标签：只确保焦点在它的终端里。
            self.sync_focus(window, cx);
            return;
        }
        self.ctl.edit(|w| w.activate(id));
        self.sync_focus(window, cx);
        cx.notify();
    }

    pub(super) fn activate_pane(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ctl.edit(|w| w.activate_pane(pane));
        self.sync_focus(window, cx);
        cx.notify();
    }

    pub(super) fn close_tab_clicked(
        &mut self,
        id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_close(id, window, cx);
    }

    pub(super) fn new_tab_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // ui-shell.md §2.1：＋ 按钮打开会话启动器（里面有「本地 Shell」）。
        self.run_command(AppCommand::OpenLauncher, window, cx);
    }

    /// 键盘焦点与核心的“有焦点”状态都跟随活动会话。
    pub(super) fn sync_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_filer_session(window, cx);
        self.sync_git(cx);
        // 纯 SFTP 标签：键盘焦点交给它的双栏面板。
        if let Some(panel) = self
            .ctl
            .workspace()
            .active_session()
            .and_then(|s| self.sftp_panel(s))
        {
            let handle = panel.read(cx).focus.clone();
            if !handle.is_focused(window) {
                window.focus(&handle);
            }
        }
        let active = self.ctl.workspace().active_session();
        let want = if self.window_active { active } else { None };
        if self.focused != want {
            if let Some(old) = self.focused {
                let _ = self.core.focus(old, false);
            }
            if let Some(new) = want {
                let _ = self.core.focus(new, true);
            }
            self.focused = want;
        }
        if self.modal_open() {
            return;
        }
        if let Some(handle) = active
            .and_then(|a| self.terminals.get(&a))
            .map(|t| t.read(cx).focus.clone())
            && !handle.is_focused(window)
        {
            window.focus(&handle);
        }
    }

    /// 同 [`show_notice`](Self::show_notice)，接受 `&str`。
    pub(super) fn show_notice_str(&mut self, text: &str, cx: &mut Context<Self>) {
        self.show_notice(text.to_owned(), cx);
    }

    pub(super) fn show_notice(&mut self, text: String, cx: &mut Context<Self>) {
        log(&format!("提示：{text}"));
        self.notice = Some((text, Instant::now() + Duration::from_secs(3)));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(3100))
                .await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    // ---- 键盘 ----

    /// 捕获阶段：命中应用级快捷键就执行并吞掉按键，终端收不到。
    fn on_capture_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // 浮层（启动器、编辑对话框、提示框）打开时键盘归浮层。
        if self.modal_open() {
            return;
        }
        let ks = &ev.keystroke;
        let input = KeyInput {
            key: &ks.key,
            key_char: ks.key_char.as_deref(),
            ctrl: ks.modifiers.control,
            alt: ks.modifiers.alt,
            shift: ks.modifiers.shift,
            platform: ks.modifiers.platform,
            is_held: ev.is_held,
        };
        let Some(command) = lookup(&input) else {
            // 不是应用级快捷键：先问命令补全弹窗要不要这个键。
            if self.completion_intercept(ev, window, cx) {
                cx.stop_propagation();
            }
            return;
        };
        if matches!(
            command,
            AppCommand::Copy | AppCommand::Paste | AppCommand::SelectAll
        ) {
            let Some(term) = self.active_terminal() else {
                return;
            };
            let term = term.read(cx);
            if !term.focus.is_focused(window) {
                return;
            }
            // Ctrl+字母用作复制时，没有选区仍交给终端程序，例如 Ctrl+C 中断。
            if command == AppCommand::Copy
                && input.ctrl
                && !input.alt
                && !input.shift
                && input.key.len() == 1
                && input.key.as_bytes()[0].is_ascii_alphabetic()
                && !term.has_selection()
                && term.block_sel.selected().is_none()
            {
                return;
            }
        }
        cx.stop_propagation();
        if ev.is_held && !command.repeatable() {
            return;
        }
        if super::LOG_INPUT.load(Ordering::Relaxed) {
            log(&format!("shortcut {command:?}"));
        }
        self.run_command(command, window, cx);
    }

    // ---- 分隔条 ----

    /// 侧栏占用的宽度（隐藏时为 0；窄窗口最多占 40%）。
    pub(super) fn sidebar_width(&self, window: &Window) -> f32 {
        if self.sidebar_visible {
            number("spacing", "sidebar.defaultWidth")
                .min(f32::from(window.viewport_size().width) * 0.4)
        } else {
            0.0
        }
    }

    /// 分屏区矩形（窗口坐标）。
    pub(super) fn split_area(&self, window: &Window) -> Rect {
        let size = window.viewport_size();
        let x = f64::from(self.sidebar_width(window) + self.filer_width(window));
        // 右侧的 Git 面板也从分屏区里扣掉。
        let w = f64::from(f32::from(size.width)) - f64::from(self.git_width(window));
        // 顶部留给标题栏，底部留给状态栏与发送区。
        let top = super::toolbar::HEIGHT;
        let h =
            f64::from(f32::from(size.height) - top - status_bar_h() - self.sender_total_height());
        Rect::new(x, f64::from(top), (w - x).max(0.0), h.max(0.0))
    }

    fn start_drag(&mut self, divider: &LayoutDivider, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let mouse = axis_coord(divider.axis, ev.position);
        self.drag = Some(DividerDrag::start(divider, mouse));
        cx.stop_propagation();
        cx.notify();
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(drag) = self.tab_reorder.as_mut() {
            if ev.pressed_button != Some(MouseButton::Left) {
                self.cancel_tab_reorder(cx);
            } else if drag.update(ev.position.y.into()) {
                cx.notify();
            }
            return;
        }
        if self.sender_mouse_move(ev, cx) {
            return;
        }
        if self.filer_drag_move(ev, cx) {
            return;
        }
        if self.git_drag_move(ev, cx) {
            return;
        }
        let Some(drag) = self.drag.clone() else {
            return;
        };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            cx.notify();
            return;
        }
        let area = self.split_area(window);
        let dividers = self
            .ctl
            .workspace()
            .layout()
            .dividers(area, DIVIDER_THICKNESS);
        let Some(current) = dividers.iter().find(|d| drag.is(d)) else {
            self.drag = None;
            return;
        };
        let delta = drag.delta(current, axis_coord(drag.axis, ev.position));
        if delta != 0.0 {
            let (path, index, parent) = (
                current.split_path.clone(),
                current.index,
                current.parent_length,
            );
            self.ctl
                .edit(|w| w.move_divider(&path, index, delta, min_fraction(parent)));
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, ev: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mut drag) = self.tab_reorder.take() {
            drag.update(ev.position.y.into());
            if let (Some(index), Some(pane)) =
                (drag.insertion, self.ctl.workspace().pane_of(drag.session))
            {
                let same_group = self
                    .ctl
                    .workspace()
                    .tabs_in(pane)
                    .into_iter()
                    .map(|item| item.id)
                    .eq(drag.order);
                if same_group {
                    self.ctl.reorder_tab(drag.session, index);
                }
            }
            cx.notify();
        }
        self.sender.drag = None;
        self.filer_drag_end();
        self.git_drag_end();
        if self.drag.take().is_some() {
            cx.notify();
        }
    }

    /// 模拟一次分隔条拖动（自检用，走与鼠标相同的换算）。
    pub(super) fn drag_divider_by(&mut self, window: &Window, px_delta: f64) -> bool {
        let area = self.split_area(window);
        let Some(d) = self
            .ctl
            .workspace()
            .layout()
            .dividers(area, DIVIDER_THICKNESS)
            .into_iter()
            .next()
        else {
            return false;
        };
        let start = axis_coord_f(d.axis, d.rect.x, d.rect.y) + 1.0;
        let drag = DividerDrag::start(&d, start);
        let delta = drag.delta(&d, start + px_delta);
        self.ctl
            .edit(|w| w.move_divider(&d.split_path, d.index, delta, min_fraction(d.parent_length)));
        true
    }

    /// 自动化验证用：把窗格与分隔条的几何（逻辑像素，窗口坐标）写进日志。
    fn log_layout(&mut self, area: Rect, scale: f32) {
        let ws = self.ctl.workspace();
        let mut frames: Vec<_> = ws
            .layout()
            .frames(area, DIVIDER_THICKNESS)
            .into_iter()
            .collect();
        frames.sort_by_key(|(p, _)| *p);
        let text = format!(
            "scale={scale} sidebar={} active_pane={:?} tabs={} panes={:?} dividers={:?}",
            self.sidebar_visible,
            ws.active_pane(),
            ws.tab_count(),
            frames
                .iter()
                .map(|(p, r)| format!(
                    "{}:{:.0},{:.0},{:.0},{:.0}",
                    p.0, r.x, r.y, r.width, r.height
                ))
                .collect::<Vec<_>>(),
            ws.layout()
                .dividers(area, DIVIDER_THICKNESS)
                .iter()
                .map(|d| format!(
                    "{:.0},{:.0},{:.0},{:.0}",
                    d.rect.x, d.rect.y, d.rect.width, d.rect.height
                ))
                .collect::<Vec<_>>(),
        );
        if text != self.last_layout_log {
            log(&format!("layout {text}"));
            self.last_layout_log = text;
        }
    }

    // ---- 绘制 ----

    fn theme(&self) -> Theme {
        Theme {
            appearance: if self.dark {
                Appearance::Dark
            } else {
                Appearance::Light
            },
        }
    }

    fn render_panes(&self, area: Rect, th: Theme, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let ws = self.ctl.workspace();
        let frames = ws.layout().frames(area, DIVIDER_THICKNESS);
        let dividers = ws.layout().dividers(area, DIVIDER_THICKNESS);
        let active_pane = ws.active_pane();
        let multiple = frames.len() > 1;
        let mut out: Vec<gpui::AnyElement> = Vec::new();

        for (pane, r) in frames {
            let session = ws.active_tab_in(pane);
            let is_active = pane == active_pane;
            let mut el = div()
                .id(("pane", pane.0 as usize))
                .absolute()
                .left(px(r.x as f32))
                .top(px(r.y as f32))
                .w(px(r.width as f32))
                .h(px(r.height as f32))
                .overflow_hidden()
                .bg(th.c("bg.window"));
            if let Some(panel) = session.and_then(|s| self.sftp_panel(s)) {
                el = el.child(panel);
                out.push(el.into_any_element());
                continue;
            }
            match session.and_then(|s| self.terminals.get(&s).map(|t| (s, t.clone()))) {
                Some((sid, term)) => {
                    el = el
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                this.activate_session(sid, window, cx)
                            }),
                        )
                        .child(term)
                        .children(self.render_term_bar(sid, th));
                    // 会话已异常结束：窗格底部给出原因（退出码 / 断开），标签保留到用户手动关闭。
                    if let Some(item) = ws.item(sid)
                        && let Some(text) = closed_banner(&item.connection)
                    {
                        el = el.child(
                            div()
                                .absolute()
                                .left_0()
                                .bottom_0()
                                .w_full()
                                .px(px(10.0))
                                .py(px(4.0))
                                .bg(th.ca("bg.panel", 0xE6))
                                .text_size(px(12.0))
                                .text_color(th.c("status.warning"))
                                .child(text),
                        );
                    }
                }
                None => {
                    el = el.child(self.empty_state(th, cx));
                }
            }
            if multiple {
                // 焦点一眼可见：活动分屏描强调色边框，其余分屏压暗。
                el = el.child(if is_active {
                    div()
                        .absolute()
                        .size_full()
                        .border_2()
                        .border_color(th.c("accent"))
                } else {
                    div().absolute().size_full().bg(th.ca("bg.window", 0x59))
                });
            }
            out.push(el.into_any_element());
        }

        for (i, d) in dividers.into_iter().enumerate() {
            let dragging = self.drag.as_ref().is_some_and(|x| x.is(&d));
            let horizontal = d.axis == SplitAxis::Horizontal;
            let rect = d.rect;
            let start = d.clone();
            let el = div()
                .id(("divider", i))
                .absolute()
                .left(px(rect.x as f32))
                .top(px(rect.y as f32))
                .w(px(rect.width as f32))
                .h(px(rect.height as f32))
                .bg(if dragging {
                    th.c("accent")
                } else {
                    th.c("border.strong")
                })
                .hover(|s| s.bg(th.c("accent")))
                .cursor(if horizontal {
                    CursorStyle::ResizeLeftRight
                } else {
                    CursorStyle::ResizeUpDown
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                        this.start_drag(&start, ev, cx)
                    }),
                );
            out.push(el.into_any_element());
        }
        out
    }
}

fn axis_coord(axis: SplitAxis, p: gpui::Point<Pixels>) -> f64 {
    axis_coord_f(axis, f64::from(f32::from(p.x)), f64::from(f32::from(p.y)))
}

fn axis_coord_f(axis: SplitAxis, x: f64, y: f64) -> f64 {
    match axis {
        SplitAxis::Horizontal => x,
        SplitAxis::Vertical => y,
    }
}

/// 异常结束的会话在窗格底部显示的文字；正常运行 / 用户主动关闭没有。
fn closed_banner(connection: &TabConnection) -> Option<String> {
    match connection {
        TabConnection::Closed(Some(CloseNote::Exited { code: Some(c) })) => {
            Some(t(&Message::UiSessionEndedExitCodePress {
                code: i64::from(*c),
            }))
        }
        TabConnection::Closed(Some(CloseNote::Exited { code: None })) => {
            Some(label(Message::UiSessionEndedPressCtrlShift {}).to_owned())
        }
        // TODO(l10n)：用 kanso-l10n 渲染错误消息。
        TabConnection::Closed(Some(CloseNote::Error(_))) => {
            Some(label(Message::UiSessionDisconnectedPressCtrlShift {}).to_owned())
        }
        _ => None,
    }
}

impl Render for ShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = self.theme();
        let area = self.split_area(window);

        // 窗口标题跟随活动标签。
        let title = self
            .ctl
            .workspace()
            .active_session()
            .and_then(|s| self.ctl.tab_title(s))
            .map_or_else(|| "Kanso".to_owned(), |t| format!("{t} - Kanso"));
        if title != self.last_title {
            window.set_window_title(&title);
            self.last_title = title;
        }

        if super::LOG_INPUT.load(Ordering::Relaxed) {
            self.log_layout(area, window.scale_factor());
        }
        let panes = self.render_panes(area, th, cx);
        let sidebar = self
            .sidebar_visible
            .then(|| self.render_sidebar(self.sidebar_width(window), th, cx));
        let status_bar = self.render_status_bar(th, window, cx);
        if self.sender.visible {
            self.push_sender_context(cx);
        }
        let sender_els = self.render_sender(area, th, cx);
        let completion_el = self.render_completion(area, th, cx);
        let filer_panels = self.render_filer(th, window, cx);
        let git_panels = self.render_git(th, window, cx);
        let notice = self
            .notice
            .as_ref()
            .filter(|(_, until)| *until > Instant::now())
            .map(|(text, _)| text.clone());

        div()
            .relative()
            // 子元素均为绝对定位，根节点需要明确尺寸，避免百分比尺寸收缩成空绘制区域。
            .w(window.viewport_size().width)
            .h(window.viewport_size().height)
            .bg(th.c("bg.window"))
            .font_family(SharedString::from(display::ui_font_family()))
            .text_size(px(display::ui_font_size()))
            .text_color(th.c("text.primary"))
            .key_context("Shell")
            .capture_key_down(cx.listener(Self::on_capture_key))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, ev, window, cx| {
                    this.cancel_tab_reorder(cx);
                    this.on_mouse_up(ev, window, cx);
                }),
            )
            .children(sidebar)
            .children(filer_panels)
            .children(git_panels)
            .children(panes)
            .child(status_bar)
            .child(self.render_toolbar(th, window, cx))
            .children(notice.map(|text| {
                div()
                    .absolute()
                    .left(px(area.x as f32))
                    .bottom(px(14.0 + status_bar_h()))
                    .w(px(area.width as f32))
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .px(px(14.0))
                            .py(px(7.0))
                            .rounded(px(number("radius", "md")))
                            .bg(th.c("bg.popover"))
                            .border_1()
                            .border_color(th.c("border.strong"))
                            .text_size(px(12.0))
                            .child(text),
                    )
            }))
            // 最上层：菜单与模态对话框（根元素铺满窗口，遮罩按窗口坐标定位）。
            .children(sender_els)
            .children(completion_el)
            .children(self.render_palette(th, window, cx))
            .children(self.menu.clone())
            .children(self.dialog.clone())
            .children(self.filer.dialog.clone())
            .children(self.git.dialog.clone())
            .children(self.launcher.clone())
            .children(self.editor.clone())
            .children(self.ssh_import.clone())
            .children(self.prompt.as_ref().map(|(_, d)| d.clone()))
            .when(!window.is_fullscreen(), |root| {
                root.child(super::toolbar::window_buttons(th, window))
            })
    }
}
