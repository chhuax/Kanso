//! 底部 Sender 面板视图（sender.md）：多行命令框、行尾 / 目标选择、运行 / 停止、重复发送、
//! 已保存命令（保存、编辑、删除、改行尾 / 作用域、分页）。纯逻辑在 `kanso_gpui::app::sender`，
//! 调度在核心的 `Core::sender_*`；本文件只做界面与接线。
//!
//! 需要窗口级浮层时（右键菜单、保存位置菜单）发 [`SenderEvent::OpenMenu`]，由外壳弹出，
//! 选中结果再经 [`SenderPanel::on_menu_action`] 回到这里。

use std::sync::Arc;

use gpui::{
    App, Context, Entity, EventEmitter, MouseButton, MouseDownEvent, Pixels, Point, Render,
    SharedString, Subscription, Window, div, prelude::*, px,
};
use kanso_core::Core;
use kanso_core::sender::SenderStatus;
use kanso_gpui::app::launcher::core_error_text;
use kanso_gpui::app::sender::{
    COUNT_MAX, Draft, ENDINGS, Editing, INTERVAL_DEFAULT, INTERVAL_MAX, INTERVAL_MIN, MAX_SNIPPETS,
    RepeatInfo, SETTING_COUNT, SETTING_INTERVAL, SETTING_SCOPE_LEVEL, SnippetAction, Target,
    build_request, build_snippet, can_save, clamp_page, commit_count, commit_interval,
    decode_action, default_scope_index, ending_index, ending_label, first_line, has_content,
    notice_text, page_count, page_items, running_chip_text, save_menu, scope_choices, snippet_menu,
    snippet_tooltip, sort_by_name, stored_number, t, target_items,
};
use kanso_gpui::app::sender::{ScopeChoice, SenderContext};
use kanso_gpui::widgets::dropdown::Item;
use kanso_gpui::widgets::menu::MenuItem;
use kanso_gpui::widgets::style::number;
use kanso_types::Message;
use kanso_types::profile::{Group, LineEnding, Profile, Snippet};

use super::shell::Theme;
use super::widgets::dropdown::{Dropdown, DropdownEvent};
use super::widgets::icons::icon;
use super::widgets::text_area::{TextArea, TextAreaEvent};
use super::widgets::text_input::{TextInput, TextInputEvent};

/// 面板向外壳发出的事件。
pub(crate) enum SenderEvent {
    /// 状态栏提示（文字，是否出错）。
    Notice(String, bool),
    /// 请外壳在窗口坐标弹出菜单；选中项的 id 以 `sender:` 开头，外壳转回 `on_menu_action`。
    OpenMenu {
        items: Vec<MenuItem>,
        anchor: Point<Pixels>,
    },
    /// 有任务在运行，外壳需要开始定时推进。
    Started,
    /// 点了面板右上角的关闭按钮。
    Close,
    /// 运行状态变化（状态栏的重复发送提示文字；没有重复任务为 `None`）。
    Chip(Option<String>),
}

impl EventEmitter<SenderEvent> for SenderPanel {}

/// 悬停提示。
struct TextTooltip {
    text: String,
    th: Theme,
}

impl Render for TextTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let th = self.th;
        div()
            .max_w(px(420.0))
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(number("radius", "md")))
            .bg(th.c("bg.popover"))
            .border_1()
            .border_color(th.c("border.strong"))
            .text_size(px(12.0))
            .text_color(th.c("text.primary"))
            .child(self.text.clone())
    }
}

/// Sender 面板。
pub(crate) struct SenderPanel {
    core: Arc<Core>,
    ctx: SenderContext,
    editing: Editing,
    area: Entity<TextArea>,
    name: Entity<TextInput>,
    ending: Entity<Dropdown>,
    target: Entity<Dropdown>,
    interval_input: Entity<TextInput>,
    count_input: Entity<TextInput>,
    target_mode: Target,
    interval: u32,
    count: u32,
    library: Vec<Snippet>,
    visible: Vec<Snippet>,
    profiles: Vec<Profile>,
    groups: Vec<Group>,
    page: usize,
    last_scope_level: u8,
    status: SenderStatus,
    manual_running: bool,
    repeat_running: bool,
    repeat_info: Option<RepeatInfo>,
    saving: bool,
    appearance: kanso_gpui::widgets::style::Appearance,
    _subs: Vec<Subscription>,
}

impl SenderPanel {
    pub(crate) fn new(core: Arc<Core>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let area = cx.new(|cx| {
            TextArea::new(window, cx).placeholder(t(Message::UiSenderInputPlaceholder {}))
        });
        let name = cx.new(|cx| {
            TextInput::new(window, cx)
                .placeholder(t(Message::UiSenderNamePlaceholder {}))
                .max_len(kanso_gpui::app::sender::NAME_MAX_CHARS)
        });
        let ending_items = ENDINGS
            .iter()
            .map(|e| Item::option(ending_label(*e)))
            .collect();
        let ending = cx.new(|cx| Dropdown::new(window, cx).items(ending_items).selected(0));
        let target = cx.new(|cx| {
            Dropdown::new(window, cx)
                .items(Self::target_items(&SenderContext::default()))
                .selected(0)
        });
        let interval = stored_number(
            core.setting(SETTING_INTERVAL).as_deref(),
            INTERVAL_DEFAULT,
            INTERVAL_MIN,
            INTERVAL_MAX,
        );
        let count = stored_number(core.setting(SETTING_COUNT).as_deref(), 0, 0, COUNT_MAX);
        let last_scope_level =
            stored_number(core.setting(SETTING_SCOPE_LEVEL).as_deref(), 0, 0, 3) as u8;
        let interval_input =
            cx.new(|cx| TextInput::new(window, cx).with_text(&interval.to_string()));
        let count_input = cx.new(|cx| TextInput::new(window, cx).with_text(&count.to_string()));

        let subs = vec![
            cx.subscribe_in(&area, window, |this, _a, ev: &TextAreaEvent, window, cx| {
                match ev {
                    TextAreaEvent::Change(text) => this.editing.draft.text = text.to_string(),
                    TextAreaEvent::Submit(_) => this.run_clicked(cx),
                    TextAreaEvent::Escape => this.cancel_edit(window, cx),
                    _ => {}
                }
                cx.notify();
            }),
            cx.subscribe_in(
                &name,
                window,
                |this, _n, ev: &TextInputEvent, window, cx| {
                    match ev {
                        TextInputEvent::Change(text) => this.editing.draft.name = text.to_string(),
                        TextInputEvent::Submit(_) => {
                            // 名称框回车：打开保存位置选择，不发送命令（sender.md §2.1）。
                            let anchor = this.default_menu_anchor(window);
                            this.open_save_menu(anchor, cx);
                        }
                        TextInputEvent::Escape => this.cancel_edit(window, cx),
                        _ => {}
                    }
                    cx.notify();
                },
            ),
            cx.subscribe(&ending, |this, _d, ev: &DropdownEvent, cx| {
                if let DropdownEvent::Change(i) = ev {
                    this.editing.draft.ending = ENDINGS.get(*i).copied().unwrap_or(LineEnding::Lf);
                    cx.notify();
                }
            }),
            cx.subscribe(&target, |this, _d, ev: &DropdownEvent, cx| {
                if let DropdownEvent::Change(i) = ev {
                    this.target_mode = if *i == 0 {
                        Target::Current
                    } else {
                        Target::All
                    };
                    cx.notify();
                }
            }),
            cx.subscribe(&interval_input, |this, _i, ev: &TextInputEvent, cx| {
                if matches!(ev, TextInputEvent::Blur | TextInputEvent::Submit(_)) {
                    this.commit_numbers(cx);
                }
            }),
            cx.subscribe(&count_input, |this, _i, ev: &TextInputEvent, cx| {
                if matches!(ev, TextInputEvent::Blur | TextInputEvent::Submit(_)) {
                    this.commit_numbers(cx);
                }
            }),
        ];

        let mut panel = SenderPanel {
            core,
            ctx: SenderContext::default(),
            editing: Editing::default(),
            area,
            name,
            ending,
            target,
            interval_input,
            count_input,
            target_mode: Target::Current,
            interval,
            count,
            library: Vec::new(),
            visible: Vec::new(),
            profiles: Vec::new(),
            groups: Vec::new(),
            page: 0,
            last_scope_level,
            status: SenderStatus::default(),
            manual_running: false,
            repeat_running: false,
            repeat_info: None,
            saving: false,
            appearance: kanso_gpui::widgets::style::Appearance::Dark,
            _subs: subs,
        };
        panel.reload(false, window, cx);
        panel
    }

    fn target_items(ctx: &SenderContext) -> Vec<Item> {
        target_items(ctx).into_iter().map(Item::Option).collect()
    }

    // ---- 对外接口（外壳调用）----

    /// 外壳每次把当前窗口状态交给面板；没有变化什么也不做。
    pub(crate) fn set_context(&mut self, ctx: SenderContext, cx: &mut Context<Self>) {
        if self.ctx == ctx {
            return;
        }
        self.ctx = ctx;
        self.refresh_visible(cx);
        let items = Self::target_items(&self.ctx);
        let mode = usize::from(self.target_mode == Target::All);
        self.target.update(cx, |d, cx| {
            d.set_items(items, cx);
            d.set_selected(Some(mode), cx);
        });
        cx.notify();
    }

    /// 面板显示时重新读取命令库（导入、删除会话等可能改过）。
    pub(crate) fn on_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reload(true, window, cx);
        self.area.update(cx, |a, _| a.focus(window));
    }

    /// 是否有任务在运行。
    pub(crate) fn running(&self) -> bool {
        self.manual_running || self.repeat_running
    }

    /// 推进核心调度并刷新进度；返回是否仍有任务在运行。
    pub(crate) fn tick(&mut self, cx: &mut Context<Self>) -> bool {
        let status = self.core.sender_tick(self.ctx.session_ids());
        self.manual_running = status.manual_running;
        self.repeat_running = status.repeat_running;
        if let Some(text) = notice_text(&status) {
            cx.emit(SenderEvent::Notice(text, status.has_failures));
        }
        if !self.repeat_running {
            self.repeat_info = None;
        }
        cx.emit(SenderEvent::Chip(
            self.repeat_running.then(|| running_chip_text(&status)),
        ));
        self.status = status;
        cx.notify();
        self.running()
    }

    /// `--sender-test`：把文字放进命令框并按“运行”。
    pub(crate) fn selftest_send(&mut self, text: &str, cx: &mut Context<Self>) {
        self.area.update(cx, |a, cx| a.set_text(text, cx));
        self.editing.draft.text = text.to_owned();
        self.run_clicked(cx);
    }

    // ---- 命令库 ----

    fn notice(&self, text: String, error: bool, cx: &mut Context<Self>) {
        cx.emit(SenderEvent::Notice(text, error));
    }

    fn reload(&mut self, external: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut all = self.core.snippets();
        sort_by_name(&mut all);
        let profiles = self.core.profiles();
        let groups = self.core.groups();
        let changed = all != self.library || profiles != self.profiles || groups != self.groups;
        self.library = all;
        self.profiles = profiles;
        self.groups = groups;
        if external && changed && self.editing.library_changed() {
            self.apply_draft(window, cx);
        }
        self.refresh_visible(cx);
    }

    fn refresh_visible(&mut self, cx: &mut Context<Self>) {
        let session = self.ctx.active.as_ref().map(|t| t.id);
        match self.core.visible_snippets(session) {
            Ok(mut v) => {
                sort_by_name(&mut v);
                self.visible = v;
            }
            Err(e) => {
                self.visible.clear();
                self.notice(core_error_text(&e), true, cx);
            }
        }
        self.page = clamp_page(self.page, self.visible.len());
        cx.notify();
    }

    fn choices(&self) -> Vec<ScopeChoice> {
        scope_choices(self.ctx.active.as_ref(), &self.profiles, &self.groups)
    }

    /// 把草稿写回各输入控件。
    fn apply_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Draft { text, name, ending } = self.editing.draft.clone();
        self.area.update(cx, |a, cx| a.set_text(&text, cx));
        self.name.update(cx, |n, cx| n.set_text(&name, cx));
        self.ending
            .update(cx, |d, cx| d.set_selected(Some(ending_index(ending)), cx));
        self.area.update(cx, |a, _| a.focus(window));
        cx.notify();
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.editing().is_some() {
            self.editing.cancel();
            self.apply_draft(window, cx);
        }
    }

    // ---- 重复参数 ----

    fn commit_numbers(&mut self, cx: &mut Context<Self>) {
        let interval = commit_interval(self.interval_input.read(cx).text(), self.interval);
        let count = commit_count(self.count_input.read(cx).text(), self.count);
        self.interval = interval;
        self.count = count;
        // 失焦时把最终采用的值显示在框里（D8）。
        self.interval_input
            .update(cx, |i, cx| i.set_text(&interval.to_string(), cx));
        self.count_input
            .update(cx, |i, cx| i.set_text(&count.to_string(), cx));
    }

    // ---- 发送 ----

    fn run_clicked(&mut self, cx: &mut Context<Self>) {
        if self.manual_running {
            return;
        }
        if has_content(&self.editing.draft.text) {
            self.start(false, None, cx);
        }
    }

    fn start(&mut self, repeating: bool, snippet: Option<&Snippet>, cx: &mut Context<Self>) {
        self.commit_numbers(cx);
        let (text, ending) = match snippet {
            Some(s) => (s.text.clone(), s.line_ending),
            None => (self.editing.draft.text.clone(), self.editing.draft.ending),
        };
        let request = match build_request(
            &text,
            ending,
            self.target_mode,
            &self.ctx,
            repeating,
            self.interval,
            self.count,
        ) {
            Ok(r) => r,
            Err(e) => {
                self.notice(e.text(), true, cx);
                return;
            }
        };
        match self.core.sender_start(request, self.ctx.session_ids()) {
            Ok(()) => {
                if repeating {
                    let target = match (self.target_mode, &self.ctx.active) {
                        (Target::Current, Some(tab)) => t(Message::UiSenderTargetCurrentLabel {
                            name: tab.name.clone(),
                        }),
                        _ => t(Message::UiSenderTargetAllLabel {}),
                    };
                    self.repeat_info = Some(RepeatInfo {
                        title: first_line(&text).trim().to_owned(),
                        interval: self.interval,
                        target,
                    });
                    for (key, value) in [
                        (SETTING_INTERVAL, self.interval),
                        (SETTING_COUNT, self.count),
                    ] {
                        let _ = self.core.set_setting(key, value.to_string());
                    }
                }
                self.tick(cx);
                cx.emit(SenderEvent::Started);
            }
            Err(e) => {
                let text = t(Message::UiSenderSendFailed {
                    detail: core_error_text(&e),
                });
                self.notice(text, true, cx);
            }
        }
    }

    fn stop(&mut self, repeating: bool, cx: &mut Context<Self>) {
        self.core.sender_stop(repeating);
        self.tick(cx);
    }

    // ---- 保存 / 菜单 ----

    fn default_menu_anchor(&self, window: &Window) -> Point<Pixels> {
        let size = window.viewport_size();
        gpui::point(size.width - px(260.0), size.height - px(120.0))
    }

    fn save_allowed(&self) -> bool {
        can_save(
            &self.editing.draft.text,
            self.saving,
            self.editing.editing().is_some(),
            self.library.len(),
        )
    }

    fn open_save_menu(&mut self, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.save_allowed() {
            if has_content(&self.editing.draft.text) && self.library.len() >= MAX_SNIPPETS {
                self.notice(
                    t(Message::UiSenderLimit {
                        max: MAX_SNIPPETS as i64,
                    }),
                    true,
                    cx,
                );
            }
            return;
        }
        let choices = self.choices();
        let pre = default_scope_index(&choices, self.last_scope_level);
        cx.emit(SenderEvent::OpenMenu {
            items: save_menu(&choices, pre),
            anchor,
        });
    }

    fn do_save(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let choices = self.choices();
        let Some(choice) = choices.get(index) else {
            return;
        };
        let editing = self.editing.editing();
        let Some(snippet) = build_snippet(&self.editing.draft, editing, choice.scope.clone())
        else {
            return;
        };
        self.saving = true;
        let result = self.core.save_snippet(snippet);
        self.saving = false;
        match result {
            Ok(()) => {
                self.last_scope_level = choice.level;
                let _ = self
                    .core
                    .set_setting(SETTING_SCOPE_LEVEL, choice.level.to_string());
                self.editing.finish();
                self.reload(false, window, cx);
            }
            Err(e) => {
                let detail = core_error_text(&e);
                let text = if editing.is_some() {
                    t(Message::UiSenderUpdateFailed { detail })
                } else {
                    t(Message::UiSenderSaveFailed { detail })
                };
                self.notice(text, true, cx);
            }
        }
        cx.notify();
    }

    fn find(&self, id: kanso_types::SnippetId) -> Option<Snippet> {
        self.library.iter().find(|s| s.id == id).cloned()
    }

    fn update_snippet(&mut self, snippet: Snippet, window: &mut Window, cx: &mut Context<Self>) {
        match self.core.save_snippet(snippet) {
            Ok(()) => self.reload(false, window, cx),
            Err(e) => {
                let text = t(Message::UiSenderUpdateFailed {
                    detail: core_error_text(&e),
                });
                self.notice(text, true, cx);
            }
        }
    }

    /// 外壳转来的菜单结果（id 以 `sender:` 开头）。
    pub(crate) fn on_menu_action(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = decode_action(id) else {
            return;
        };
        match action {
            SnippetAction::Save(i) => self.do_save(i, window, cx),
            SnippetAction::Edit(sid) => {
                if let Some(s) = self.find(sid) {
                    self.editing.begin(&s);
                    self.apply_draft(window, cx);
                }
            }
            SnippetAction::Repeat(sid) => {
                if let Some(s) = self.find(sid) {
                    self.start(true, Some(&s), cx);
                }
            }
            SnippetAction::Ending(sid, ending) => {
                if let Some(mut s) = self.find(sid) {
                    s.line_ending = ending;
                    self.update_snippet(s, window, cx);
                }
            }
            SnippetAction::Scope(sid, i) => {
                let choices = self.choices();
                if let (Some(mut s), Some(c)) = (self.find(sid), choices.get(i)) {
                    s.scope = c.scope.clone();
                    self.update_snippet(s, window, cx);
                }
            }
            SnippetAction::Delete(sid) => match self.core.delete_snippet(sid) {
                Ok(()) => {
                    if self.editing.deleted(sid) {
                        self.apply_draft(window, cx);
                    }
                    self.reload(false, window, cx);
                }
                Err(e) => {
                    let text = t(Message::UiSenderDeleteFailed {
                        detail: core_error_text(&e),
                    });
                    self.notice(text, true, cx);
                }
            },
        }
    }

    fn clear_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.editing().is_some() {
            self.cancel_edit(window, cx);
        } else {
            self.name.update(cx, |n, cx| n.set_text("", cx));
            self.area.update(cx, |a, cx| a.set_text("", cx));
            self.area.update(cx, |a, _| a.focus(window));
        }
    }
}

/// 面板上的小按钮。
fn button(
    id: &'static str,
    label: String,
    th: Theme,
    enabled: bool,
    primary: bool,
) -> gpui::Stateful<gpui::Div> {
    let (bg, fg, border) = if primary {
        (th.c("accent"), gpui::white(), th.c("accent"))
    } else {
        (th.c("bg.field"), th.c("text.primary"), th.c("border.field"))
    };
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .h(px(26.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(number("radius", "md")))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_color(fg)
        .text_size(px(12.0))
        .whitespace_nowrap()
        .overflow_hidden()
        .when(enabled, |d| d.cursor_pointer())
        .when(!enabled, |d| d.opacity(0.45))
        .child(label)
}

impl Render for SenderPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.appearance = super::display::window_appearance(window);
        let th = Theme {
            appearance: self.appearance,
        };
        let content = has_content(&self.editing.draft.text);
        let editing = self.editing.editing().is_some();
        let manual = self.manual_running;

        // ---- 标题行 ----
        let labelled = |text: String, input: Entity<TextInput>| {
            div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(th.c("text.secondary"))
                        .child(text),
                )
                .child(div().w(px(64.0)).child(input))
        };
        let header = div()
            .flex_none()
            .h(px(38.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .text_color(th.c("text.secondary"))
                    .child(t(Message::UiSenderTitle {})),
            )
            .child(div().w(px(120.0)).child(self.ending.clone()))
            .child(div().w(px(150.0)).child(self.target.clone()))
            .child(labelled(
                t(Message::UiSenderInterval {}),
                self.interval_input.clone(),
            ))
            .child(labelled(
                t(Message::UiSenderCount {}),
                self.count_input.clone(),
            ))
            .child(div().flex_1())
            .child(
                div()
                    .id("sender-close")
                    .p(px(4.0))
                    .rounded(px(number("radius", "md")))
                    .cursor_pointer()
                    .hover(|s| s.bg(th.ca("text.primary", 0x18)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _: &MouseDownEvent, _, cx| cx.emit(SenderEvent::Close)),
                    )
                    .child(icon("x", 14.0, th.c("text.secondary"))),
            );

        // ---- 重复任务运行条 ----
        let repeat_bar = (self.repeat_running).then(|| {
            let text = self
                .repeat_info
                .as_ref()
                .map(|i| kanso_gpui::app::sender::repeat_bar_text(i, &self.status))
                .unwrap_or_default();
            div()
                .flex_none()
                .h(px(26.0))
                .mx(px(10.0))
                .mb(px(4.0))
                .px(px(8.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .rounded(px(number("radius", "md")))
                .bg(th.ca("accent", 0x22))
                .text_size(px(12.0))
                .child(div().flex_1().min_w_0().truncate().child(text))
                .child(
                    button(
                        "sender-repeat-stop",
                        t(Message::UiSenderStop {}),
                        th,
                        true,
                        false,
                    )
                    .flex_none()
                    .w(px(64.0))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| this.stop(true, cx)),
                    ),
                )
        });

        // ---- 命令框 + 控制列 ----
        let run_label = if manual {
            t(Message::UiSenderStop {})
        } else {
            t(Message::UiSenderRun {})
        };
        let run = button("sender-run", run_label, th, manual || content, true).when(
            manual || content,
            |d| {
                d.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        if this.manual_running {
                            this.stop(false, cx);
                        } else {
                            this.run_clicked(cx);
                        }
                    }),
                )
            },
        );
        let clear_label = if editing {
            t(Message::UiSenderCancelEdit {})
        } else {
            t(Message::UiSenderClear {})
        };
        let clear = button("sender-clear", clear_label, th, true, false).on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, window, cx| this.clear_clicked(window, cx)),
        );
        let repeat = button(
            "sender-repeat",
            t(Message::UiSenderRepeat {}),
            th,
            content,
            false,
        )
        .when(content, |d| {
            d.on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| this.start(true, None, cx)),
            )
        });
        let save_enabled = self.save_allowed();
        let save_label = if editing {
            t(Message::UiSenderUpdate {})
        } else {
            t(Message::UiSenderSave {})
        };
        let save =
            button("sender-save", save_label, th, save_enabled, false).when(save_enabled, |d| {
                d.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                        this.open_save_menu(ev.position, cx)
                    }),
                )
            });
        let controls = div()
            .flex_none()
            .w(px(210.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(self.name.clone())
            .child(div().flex().gap(px(6.0)).child(run).child(clear))
            .child(div().flex().gap(px(6.0)).child(repeat).child(save));

        // ---- 已保存命令 ----
        let pages = page_count(self.visible.len());
        let items = page_items(&self.visible, self.page).to_vec();
        let profiles = self.profiles.clone();
        let groups = self.groups.clone();
        let chips: Vec<gpui::AnyElement> = items
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                let tip = snippet_tooltip(&s, &profiles, &groups);
                let (sid_click, sid_menu) = (s.clone(), s.clone());
                let choices = self.choices();
                div()
                    .id(("sender-snippet", i))
                    .max_w(px(180.0))
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(number("radius", "md")))
                    .border_1()
                    .border_color(th.c("border.field"))
                    .bg(th.c("bg.field"))
                    .text_size(px(12.0))
                    .truncate()
                    .when(!manual, |d| {
                        d.cursor_pointer().hover(|s| s.bg(th.ca("accent", 0x22)))
                    })
                    .when(manual, |d| d.opacity(0.5))
                    .tooltip(move |_window, cx: &mut App| {
                        let text = tip.clone();
                        cx.new(|_| TextTooltip { text, th }).into()
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            if !this.manual_running {
                                this.start(false, Some(&sid_click), cx);
                            }
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |_this, ev: &MouseDownEvent, _, cx| {
                            cx.emit(SenderEvent::OpenMenu {
                                items: snippet_menu(&sid_menu, &choices),
                                anchor: ev.position,
                            });
                        }),
                    )
                    .child(SharedString::from(s.name.clone()))
                    .into_any_element()
            })
            .collect();
        let empty_text = if self.library.is_empty() {
            Some(t(Message::UiSenderLibraryEmpty {}))
        } else if self.visible.is_empty() {
            Some(t(Message::UiSenderLibraryNone {}))
        } else {
            None
        };
        let pager = (pages > 1).then(|| {
            let page = self.page;
            let nav = |id: &'static str, label: String, enabled: bool| {
                button(id, label, th, enabled, false)
                    .flex_none()
                    .w(px(64.0))
                    .h(px(22.0))
            };
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.0))
                .pt(px(4.0))
                .child(
                    nav("sender-prev", t(Message::UiSenderPrevPage {}), page > 0).when(
                        page > 0,
                        |d| {
                            d.on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                    this.page = this.page.saturating_sub(1);
                                    cx.notify();
                                }),
                            )
                        },
                    ),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(th.c("text.secondary"))
                        .child(format!("{} / {}", page + 1, pages)),
                )
                .child(
                    nav(
                        "sender-next",
                        t(Message::UiSenderNextPage {}),
                        page + 1 < pages,
                    )
                    .when(page + 1 < pages, |d| {
                        d.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                this.page = clamp_page(this.page + 1, this.visible.len());
                                cx.notify();
                            }),
                        )
                    }),
                )
        });
        let library = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("sender-library")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_wrap()
                    .content_start()
                    .gap(px(6.0))
                    .children(chips)
                    .children(empty_text.map(|text| {
                        div()
                            .text_size(px(12.0))
                            .text_color(th.c("text.tertiary"))
                            .child(text)
                    })),
            )
            .children(pager);

        let body = div()
            .flex_1()
            .min_h_0()
            .px(px(10.0))
            .pb(px(8.0))
            .flex()
            .gap(px(8.0))
            .child(div().flex_1().min_w_0().h_full().child(self.area.clone()))
            .child(controls)
            .child(library);

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(th.c("bg.panel"))
            .border_t_1()
            .border_color(th.c("border.subtle"))
            .text_size(px(number("font", "ui.body")))
            .text_color(th.c("text.primary"))
            .child(header)
            .children(repeat_bar)
            .child(body)
    }
}
