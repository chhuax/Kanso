//! 导入 OpenSSH 配置对话框（store-import.md §7.2、§7.3；对照 mac 版 `SshConfigImportViewController`）：
//! 扫描配置文件，逐台列出别名与解析后的连接目标，用户勾选要导入哪些、选择目标分组；
//! 已保存的主机标出"将更新"；导入后在同一个对话框里切换成结果页（新增 / 更新 / 跳板机 / 多跳忽略 / 警告）。
//!
//! 勾选状态与候选行文字在库里的 `app::data_transfer::SshImportModel`（三平台可测），
//! 复选框的键盘光标 / 全选逻辑在 `widgets::checklist`；本视图只管控件与布局。
//! 解析与导入规则全在核心（`scan_ssh_config` / `import_ssh_config`）。
//!
//! 键盘：↑↓ 移动光标、空格切换勾选、Ctrl+A 全选、Tab / Shift+Tab 在对话框内循环、回车导入（结果页关闭）、Esc 关闭。

use kanso_types::message::Message;
use std::sync::Arc;

use gpui::{
    App, BoxShadow, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, KeyDownEvent,
    MouseButton, MouseDownEvent, PathPromptOptions, ScrollHandle, Subscription, Window, div, point,
    prelude::*, px, rgba,
};
use kanso_core::Core;
use kanso_gpui::app::data_transfer::{ResultContent, SshImportModel, ssh_import_result};
use kanso_gpui::app::i18n::{label, t};
use kanso_gpui::app::launcher::core_error_text;
use kanso_gpui::app::sessions::natural_cmp;
use kanso_gpui::widgets::checklist::Tri;
use kanso_gpui::widgets::dropdown::Item;
use kanso_gpui::widgets::focus_chain;
use kanso_gpui::widgets::style::{color, number};
use kanso_types::profile::Group;

use super::log;
use super::shell::Theme;
use super::widgets::dropdown::{Dropdown, DropdownEvent};

/// 对话框向宿主发出的事件。
#[derive(Clone, Debug)]
pub(crate) enum SshImportEvent {
    /// 关闭。
    Close,
}

/// 对话框所处阶段。
enum Phase {
    /// 正在读配置文件。
    Scanning,
    /// 列出候选，等用户勾选。
    Ready,
    /// 正在写入。
    Importing,
    /// 导入完成，显示结果。
    Done(ResultContent),
}

/// Tab 环上的站点。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    /// 候选列表（键盘光标在它里面）。
    List,
    Choose,
    Group,
    Cancel,
    Import,
}

const RING: [Stop; 5] = [
    Stop::List,
    Stop::Choose,
    Stop::Group,
    Stop::Cancel,
    Stop::Import,
];

/// 导入 OpenSSH 配置对话框。
pub(crate) struct SshImportView {
    core: Arc<Core>,
    model: SshImportModel,
    phase: Phase,
    /// 导入失败的原因（对话框保持打开让用户重试）。
    error: Option<String>,
    groups: Vec<Group>,
    group: Entity<Dropdown>,
    scroll: ScrollHandle,
    root: FocusHandle,
    choose: FocusHandle,
    cancel: FocusHandle,
    import: FocusHandle,
    /// 扫描代数：连续换文件时，较早发出的扫描可能后返回，不能覆盖新的结果。
    generation: u64,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SshImportEvent> for SshImportView {}

impl Focusable for SshImportView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.root.clone()
    }
}

impl SshImportView {
    /// 创建并开始扫描默认的 `~/.ssh/config`。必须在窗口上下文里创建。
    pub(crate) fn new(
        core: Arc<Core>,
        home: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut groups = core.groups();
        groups.sort_by(|a, b| natural_cmp(&a.name, &b.name));
        let group = cx.new(|cx| {
            Dropdown::new(window, cx)
                .items(
                    std::iter::once(Item::option(label(Message::UiNoGroup {})))
                        .chain(groups.iter().map(|g| Item::option(g.name.clone())))
                        .collect(),
                )
                .selected(0)
        });
        let sub = cx.subscribe_in(
            &group,
            window,
            |this, _d, ev: &DropdownEvent, _window, cx| {
                if let DropdownEvent::Change(i) = ev {
                    this.model.group = i
                        .checked_sub(1)
                        .and_then(|k| this.groups.get(k))
                        .map(|g| g.id);
                    cx.notify();
                }
            },
        );
        let root = cx.focus_handle();
        window.focus(&root);
        let mut this = SshImportView {
            core,
            model: SshImportModel::new(home),
            phase: Phase::Scanning,
            error: None,
            groups,
            group,
            scroll: ScrollHandle::new(),
            root,
            choose: cx.focus_handle().tab_stop(true),
            cancel: cx.focus_handle().tab_stop(true),
            import: cx.focus_handle().tab_stop(true),
            generation: 0,
            _subs: vec![sub],
        };
        this.scan(window, cx);
        this
    }

    // ---- 扫描与导入 ----

    fn scan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.phase = Phase::Scanning;
        self.error = None;
        let core = self.core.clone();
        let path = self.model.config_path().to_owned();
        log(&format!("扫描 OpenSSH 配置：{path}"));
        cx.spawn_in(window, async move |this, cx| {
            let scan = core.scan_ssh_config(Some(&path)).await;
            this.update_in(cx, |this, _window, cx| {
                if this.generation != generation {
                    return;
                }
                match scan {
                    Ok(scan) => this.model.apply_scan(&scan),
                    Err(e) => this.model.apply_scan_error(core_error_text(&e)),
                }
                this.phase = Phase::Ready;
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn start_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.phase, Phase::Ready) || !self.model.can_import() {
            return;
        }
        self.phase = Phase::Importing;
        self.error = None;
        let core = self.core.clone();
        let path = self.model.config_path().to_owned();
        let aliases = self.model.selected_aliases();
        let group = self.model.group;
        log(&format!("导入 OpenSSH 配置：{} 台", aliases.len()));
        cx.spawn_in(window, async move |this, cx| {
            let result = core.import_ssh_config(Some(&path), &aliases, group).await;
            this.update_in(cx, |this, _window, cx| {
                match result {
                    Ok(report) => {
                        this.phase = Phase::Done(ssh_import_result(&report));
                    }
                    Err(e) => {
                        this.phase = Phase::Ready;
                        this.error = Some(core_error_text(&e));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn choose_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(label(Message::UiChooseOpensshConfigFile {}).into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths.first()
            {
                let path = p.to_string_lossy().into_owned();
                this.update_in(cx, |this, window, cx| {
                    this.model.set_config_path(path);
                    this.scan(window, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if matches!(self.phase, Phase::Importing) {
            return;
        }
        cx.emit(SshImportEvent::Close);
    }

    // ---- 键盘 ----

    fn handle(&self, s: Stop, cx: &App) -> FocusHandle {
        match s {
            Stop::List => self.root.clone(),
            Stop::Choose => self.choose.clone(),
            Stop::Group => self.group.read(cx).focus_handle(cx),
            Stop::Cancel => self.cancel.clone(),
            Stop::Import => self.import.clone(),
        }
    }

    /// 当前阶段 Tab 环上可停靠的站点（结果页只有"好"按钮，用 Cancel 的位置显示）。
    fn enabled(&self) -> [bool; 5] {
        let ready = matches!(self.phase, Phase::Ready);
        match self.phase {
            Phase::Done(_) => [false, false, false, true, false],
            _ => [
                ready && !self.model.rows().is_empty(),
                ready,
                ready,
                true,
                ready && self.model.can_import(),
            ],
        }
    }

    fn focused_stop(&self, window: &Window, cx: &App) -> Option<Stop> {
        RING.into_iter()
            .find(|s| self.handle(*s, cx).is_focused(window))
    }

    fn activate(&mut self, stop: Stop, window: &mut Window, cx: &mut Context<Self>) {
        match stop {
            Stop::List => {}
            Stop::Choose => self.choose_file(window, cx),
            Stop::Group => {}
            Stop::Cancel => self.close(cx),
            Stop::Import => self.start_import(window, cx),
        }
    }

    fn on_capture_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.phase, Phase::Importing) || self.group.read(cx).is_open() {
            return;
        }
        let m = &ev.keystroke.modifiers;
        let key = ev.keystroke.key.as_str();
        let focused = self.focused_stop(window, cx);
        let on_list = focused == Some(Stop::List) || focused.is_none();
        if m.control && !m.alt && !m.shift && key == "a" && on_list {
            self.model.checks_mut().select_all();
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if m.control || m.alt || m.platform {
            return;
        }
        match key {
            "tab" => {
                let enabled = self.enabled();
                let cur = focused.and_then(|s| RING.iter().position(|r| *r == s));
                if let Some(i) = focus_chain::step(&enabled, cur, !m.shift) {
                    window.focus(&self.handle(RING[i], cx));
                    cx.notify();
                }
                cx.stop_propagation();
            }
            "up" | "down" if on_list && matches!(self.phase, Phase::Ready) => {
                self.model
                    .checks_mut()
                    .move_cursor(if key == "down" { 1 } else { -1 });
                if let Some(i) = self.model.checks().cursor() {
                    // 第 0 个子元素是"全选"表头行。
                    self.scroll.scroll_to_item(i + 1);
                }
                cx.notify();
                cx.stop_propagation();
            }
            "space" if !m.shift => {
                if on_list && matches!(self.phase, Phase::Ready) {
                    if self.model.checks_mut().toggle_cursor() {
                        cx.notify();
                    }
                    cx.stop_propagation();
                } else if let Some(s) = focused.filter(|s| !matches!(s, Stop::Group | Stop::List)) {
                    self.activate(s, window, cx);
                    cx.stop_propagation();
                }
            }
            "enter" => {
                match focused {
                    Some(s) if !matches!(s, Stop::Group | Stop::List) => {
                        self.activate(s, window, cx)
                    }
                    _ => match self.phase {
                        Phase::Done(_) => self.close(cx),
                        _ => self.start_import(window, cx),
                    },
                }
                if !matches!(focused, Some(Stop::Group)) {
                    cx.stop_propagation();
                }
            }
            "escape" => {
                self.close(cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    // ---- 绘制 ----

    fn checkbox(&self, th: Theme, state: Tri) -> gpui::Div {
        let on = state != Tri::None;
        div()
            .flex_none()
            .size(px(18.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(number("radius", "sm")))
            .border_1()
            .border_color(if on {
                th.c("accent")
            } else {
                th.c("border.field")
            })
            .bg(if on { th.c("accent") } else { th.c("bg.field") })
            .text_color(th.c("text.onAccent"))
            .text_size(px(12.0))
            .child(match state {
                Tri::All => "✓",
                Tri::Some => "–",
                Tri::None => "",
            })
    }

    fn render_header_row(&self, th: Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = self.model.checks().state();
        div()
            .id("ssh-import-all")
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(10.0))
            .h(px(30.0))
            .border_b_1()
            .border_color(th.c("border.subtle"))
            .text_color(th.c("text.secondary"))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.model.checks_mut().toggle_all();
                    window.focus(&this.root);
                    cx.notify();
                }),
            )
            .child(self.checkbox(th, state))
            .child(t(&Message::UiSelectAllOfSelected {
                selected: (self.model.checks().count()) as i64,
                total: (self.model.rows().len()) as i64,
            }))
            .into_any_element()
    }

    fn render_row(&self, i: usize, th: Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let row = &self.model.rows()[i];
        let checked = self.model.checks().is_checked(i);
        let cursor = self.model.checks().cursor() == Some(i);
        let mut detail = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_x(px(10.0))
            .text_size(px(number("font", "ui.caption")))
            .text_color(th.c("text.secondary"))
            .child(row.destination.clone());
        if let Some(k) = &row.identity {
            detail = detail.child(div().text_color(th.c("text.tertiary")).child(t(
                &Message::UiKey {
                    path: k.to_string(),
                },
            )));
        }
        if let Some(j) = &row.jump {
            detail = detail.child(div().text_color(th.c("text.tertiary")).child(j.clone()));
        }
        let mut title = div().flex().items_center().gap(px(8.0)).child(
            div()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(row.alias.clone()),
        );
        if row.existing {
            title = title.child(
                div()
                    .px(px(6.0))
                    .rounded(px(number("radius", "sm")))
                    .bg(th.c("bg.segmentSelected"))
                    .text_size(px(number("font", "ui.caption")))
                    .text_color(th.c("text.secondary"))
                    .child(label(Message::UiSavedWillUpdate {})),
            );
        }
        let mut col = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(title)
            .child(detail);
        for w in &row.warnings {
            col = col.child(
                div()
                    .text_size(px(number("font", "ui.caption")))
                    .text_color(th.c("status.warning"))
                    .child(w.clone()),
            );
        }
        div()
            .id(("ssh-import-row", i))
            .flex()
            .items_start()
            .gap(px(10.0))
            .px(px(10.0))
            .py(px(6.0))
            .cursor_pointer()
            .when(cursor, |d| d.bg(th.c("bg.rowHover")))
            .hover(|s| s.bg(th.c("bg.rowHover")))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.model.checks_mut().toggle(i);
                    this.model.checks_mut().set_cursor(i);
                    window.focus(&this.root);
                    cx.notify();
                }),
            )
            .child(
                div()
                    .pt(px(1.0))
                    .child(self.checkbox(th, if checked { Tri::All } else { Tri::None })),
            )
            .child(col)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn button(
        &self,
        id: &'static str,
        handle: &FocusHandle,
        stop: Stop,
        label: &'static str,
        primary: bool,
        enabled: bool,
        window: &Window,
        th: Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let focused = handle.is_focused(window);
        let (bg, fg, border): (Hsla, Hsla, Hsla) = if primary {
            (th.c("accent"), th.c("text.onAccent"), th.c("accent"))
        } else {
            (
                th.c("bg.buttonSecondary"),
                th.c("text.primary"),
                th.c("border.strong"),
            )
        };
        let ring: Hsla = rgba(color("accent", th.appearance).with_alpha(0x80).to_u32()).into();
        div()
            .id(id)
            .track_focus(handle)
            .min_w(px(84.0))
            .h(px(30.0))
            .px(px(16.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_color(fg)
            .text_size(px(13.0))
            .when(!enabled, |d| d.opacity(0.45))
            .when(enabled, |d| d.cursor_pointer().hover(|s| s.opacity(0.88)))
            .when(focused, |d| {
                d.shadow(vec![BoxShadow {
                    color: ring,
                    offset: point(px(0.0), px(0.0)),
                    blur_radius: px(0.0),
                    spread_radius: px(2.0),
                }])
            })
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                if enabled && !ev.is_keyboard() {
                    this.activate(stop, window, cx);
                }
            }))
            .child(label)
    }

    fn render_body(&self, th: Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let note = |text: String, color: Hsla| {
            div()
                .py(px(24.0))
                .flex()
                .justify_center()
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        match &self.phase {
            Phase::Scanning => note(
                label(Message::UiReading {}).to_owned(),
                th.c("text.secondary"),
            ),
            Phase::Importing => note(
                label(Message::UiImporting {}).to_owned(),
                th.c("text.secondary"),
            ),
            Phase::Done(result) => {
                let mut col = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(result.summary.clone());
                if let Some(t) = &result.details_title {
                    col = col.child(
                        div()
                            .text_size(px(number("font", "ui.caption")))
                            .text_color(th.c("text.secondary"))
                            .child(t.clone()),
                    );
                }
                col = col.children(result.details.iter().map(|d| {
                    div()
                        .text_size(px(number("font", "ui.caption")))
                        .text_color(th.c("status.warning"))
                        .child(d.clone())
                }));
                div()
                    .id("ssh-import-result")
                    .max_h(px(320.0))
                    .overflow_y_scroll()
                    .child(col)
                    .into_any_element()
            }
            Phase::Ready => {
                if let Some(e) = self.model.scan_error() {
                    return note(e.to_owned(), th.c("status.error"));
                }
                if self.model.rows().is_empty() {
                    return note(
                        label(Message::UiNoHostsToImportIn {}).to_owned(),
                        th.c("text.secondary"),
                    );
                }
                let mut list = div()
                    .id("ssh-import-list")
                    .flex_1()
                    .min_h_0()
                    .max_h(px(320.0))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .child(self.render_header_row(th, cx));
                for i in 0..self.model.rows().len() {
                    list = list.child(self.render_row(i, th, cx));
                }
                let mut col = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_color(th.c("text.secondary"))
                            .text_size(px(number("font", "ui.caption")))
                            .child(self.model.summary_text()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_h_0()
                            .rounded(px(number("radius", "md")))
                            .border_1()
                            .border_color(th.c("border.field"))
                            .bg(th.c("bg.field"))
                            .child(list),
                    );
                for w in self.model.file_warnings() {
                    col = col.child(
                        div()
                            .text_size(px(number("font", "ui.caption")))
                            .text_color(th.c("status.warning"))
                            .child(w.clone()),
                    );
                }
                col.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            div()
                                .flex_none()
                                .child(label(Message::UiImportIntoGroup {})),
                        )
                        .child(div().w(px(220.0)).child(self.group.clone())),
                )
                .into_any_element()
            }
        }
    }
}

impl Render for SshImportView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = Theme {
            appearance: super::display::window_appearance(window),
        };
        let done = matches!(self.phase, Phase::Done(_));
        let ready = matches!(self.phase, Phase::Ready);
        let busy = matches!(self.phase, Phase::Importing);
        let max_h = (f32::from(window.viewport_size().height) - 48.0).max(240.0);

        let path_row = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(number("spacing", "field.height")))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .rounded(px(number("radius", "md")))
                    .border_1()
                    .border_color(th.c("border.field"))
                    .bg(th.c("bg.field"))
                    .text_color(th.c("text.secondary"))
                    .child(self.model.display_path()),
            )
            .child(self.button(
                "ssh-import-choose",
                &self.choose,
                Stop::Choose,
                label(Message::UiChoose {}),
                false,
                ready,
                window,
                th,
                cx,
            ));

        let mut buttons = div()
            .mt(px(4.0))
            .flex()
            .flex_row()
            .justify_end()
            .gap(px(8.0));
        if done {
            buttons = buttons.child(self.button(
                "ssh-import-cancel",
                &self.cancel,
                Stop::Cancel,
                label(Message::UiOk {}),
                true,
                true,
                window,
                th,
                cx,
            ));
        } else {
            buttons = buttons
                .child(self.button(
                    "ssh-import-cancel",
                    &self.cancel,
                    Stop::Cancel,
                    label(Message::UiCancel {}),
                    false,
                    !busy,
                    window,
                    th,
                    cx,
                ))
                .child(self.button(
                    "ssh-import-go",
                    &self.import,
                    Stop::Import,
                    label(Message::UiImport {}),
                    true,
                    ready && self.model.can_import(),
                    window,
                    th,
                    cx,
                ));
        }

        let card = div()
            .id("ssh-import-card")
            .w(px(620.0))
            .max_w_full()
            .max_h(px(max_h))
            .p(px(20.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(th.c("border.strong"))
            .bg(th.c("bg.dialog"))
            .text_color(th.c("text.primary"))
            .text_size(px(number("font", "ui.body")))
            .shadow(vec![BoxShadow {
                color: rgba(0x00000066).into(),
                offset: point(px(0.0), px(8.0)),
                blur_radius: px(24.0),
                spread_radius: px(0.0),
            }])
            // 卡片内的点击不能冒泡到遮罩。
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .text_size(px(number("font", "ui.title")))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(if done {
                        label(Message::UiImportComplete {})
                    } else {
                        label(Message::UiImportOpensshConfig {})
                    }),
            )
            .children((!done).then_some(path_row))
            .child(self.render_body(th, cx))
            .children(self.error.clone().map(|e| {
                div()
                    .text_size(px(12.0))
                    .text_color(th.c("status.error"))
                    .child(e)
            }))
            .child(buttons);

        div()
            .id("ssh-import")
            .key_context("SshImport")
            .track_focus(&self.root)
            .capture_key_down(cx.listener(Self::on_capture_key))
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(th.c("bg.scrim"))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(card)
    }
}
