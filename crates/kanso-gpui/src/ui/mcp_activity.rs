//! MCP 活动记录独立窗口（spec/mcp.md §7.3）：按时间倒序分页显示 AI 工具通过 MCP 执行的操作，
//! 支持按客户端、会话和「仅显示错误」筛选，订阅审计变化自动刷新或提示重载，支持确认后清空。

use std::collections::HashMap;

use gpui::{
    AnyElement, App, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight, Hsla,
    KeyDownEvent, MouseButton, MouseDownEvent, Render, ScrollHandle, SharedString, Subscription,
    TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, div, prelude::*, px, rgba,
    size,
};
use kanso_core::CoreError;
use kanso_gpui::app::i18n::{Lang, label, lang};
use kanso_gpui::app::launcher::render_message;
use kanso_gpui::filer::format_mtime;
use kanso_gpui::widgets::dropdown::Item;
use kanso_gpui::widgets::style::{Appearance, color, number};
use kanso_types::ProfileId;
use kanso_types::mcp::{McpAccess, McpAuditDispatch, McpAuditFilter, McpAuditRecord};
use kanso_types::message::Message;
use kanso_types::profile::Profile;

use crate::ui::mcp::{AuditChanged, McpModel};
use crate::ui::widgets::dropdown::{Dropdown, DropdownEvent};
use crate::ui::widgets::icons::icon;
use crate::ui::widgets::text_input::{TextInput, TextInputEvent};
use crate::ui::{display, log, tz};

const AUDIT_PAGE: u32 = 20;

#[derive(Clone, Copy)]
struct Palette(Appearance);

impl Palette {
    fn c(&self, name: &str) -> Hsla {
        rgba(color(name, self.0).to_u32()).into()
    }
}

/// 打开或前置 MCP 活动记录独立窗口（单例，与原生 macOS `McpActivityWindowController` 一致）。
pub(crate) fn open_window(
    existing: Option<WindowHandle<McpActivityView>>,
    mcp: Entity<McpModel>,
    cx: &mut App,
) -> Option<WindowHandle<McpActivityView>> {
    if let Some(handle) = existing
        && handle
            .update(cx, |view, window, cx| {
                view.refresh_from_start(cx);
                window.activate_window();
            })
            .is_ok()
    {
        return Some(handle);
    }
    let title = render_message(&Message::WinMcpActivityTitle {});
    let bounds = Bounds::centered(None, size(px(640.0), px(520.0)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(title.clone().into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        move |window, cx| {
            #[cfg(target_os = "linux")]
            window.set_window_title(&title);
            cx.new(|cx| McpActivityView::new(mcp, window, cx))
        },
    );
    match opened {
        Ok(handle) => {
            log("MCP 活动记录窗口已打开");
            Some(handle)
        }
        Err(e) => {
            log(&format!("打开 MCP 活动记录窗口失败：{e:#}"));
            None
        }
    }
}

/// MCP 活动记录窗口根视图。
pub(crate) struct McpActivityView {
    model: Entity<McpModel>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    client_input: Entity<TextInput>,
    session_picker: Entity<Dropdown>,
    profiles: Vec<Profile>,
    levels: HashMap<ProfileId, McpAccess>,
    levels_error: Option<String>,
    filter: McpAuditFilter,
    records: Vec<McpAuditRecord>,
    audit_error: Option<String>,
    has_more: bool,
    has_updates: bool,
    confirm_clear: bool,
    language: Lang,
    _subs: Vec<Subscription>,
}

impl Focusable for McpActivityView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl McpActivityView {
    fn new(model: Entity<McpModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let client_input =
            cx.new(|cx| TextInput::new(window, cx).placeholder(Message::WinMcpClient {}));
        let session_picker = cx.new(|cx| {
            Dropdown::new(window, cx)
                .items(vec![Item::option(label(Message::WinMcpAllSessions {}))])
                .selected(0)
        });
        let subs = vec![
            cx.subscribe(&model, |this, _, _: &AuditChanged, cx| {
                if this.levels_error.is_some() {
                    this.refresh_accesses(cx);
                }
                if this.filter.before_id.is_none() {
                    this.load_page(true, cx);
                } else {
                    this.has_updates = true;
                    cx.notify();
                }
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.refresh_session_data(cx);
                    if this.filter.before_id.is_none() {
                        this.load_page(true, cx);
                    } else {
                        cx.notify();
                    }
                }
            }),
            cx.subscribe_in(
                &client_input,
                window,
                |this, input, event, window, cx| match event {
                    TextInputEvent::Submit(_) | TextInputEvent::Blur => {
                        let name = input.read(cx).text().trim();
                        let client = (!name.is_empty()).then(|| name.to_owned());
                        if client != this.filter.client {
                            this.filter.client = client;
                            this.load_page(true, cx);
                        }
                    }
                    TextInputEvent::Escape => {
                        if this.confirm_clear {
                            this.confirm_clear = false;
                            cx.notify();
                        } else {
                            window.remove_window();
                        }
                    }
                    _ => {}
                },
            ),
            cx.subscribe(&session_picker, |this, _, event, cx| {
                if let DropdownEvent::Change(i) = event {
                    this.filter.profile = i
                        .checked_sub(1)
                        .and_then(|idx| this.profiles.get(idx))
                        .map(|profile| profile.id);
                    this.load_page(true, cx);
                }
            }),
        ];
        let mut this = Self {
            model,
            focus,
            scroll: ScrollHandle::new(),
            client_input,
            session_picker,
            profiles: Vec::new(),
            levels: HashMap::new(),
            levels_error: None,
            filter: McpAuditFilter {
                limit: AUDIT_PAGE,
                ..Default::default()
            },
            records: Vec::new(),
            audit_error: None,
            has_more: false,
            has_updates: false,
            confirm_clear: false,
            language: lang(),
            _subs: subs,
        };
        this.refresh_from_start(cx);
        this
    }

    /// 回到第一页重载，并同步最新的会话列表与授权缓存。
    pub(crate) fn refresh_from_start(&mut self, cx: &mut Context<Self>) {
        self.refresh_session_data(cx);
        let name = self.client_input.read(cx).text().trim().to_owned();
        self.filter.client = (!name.is_empty()).then_some(name);
        self.load_page(true, cx);
    }

    fn refresh_session_data(&mut self, cx: &mut Context<Self>) {
        let mut profiles = self.model.read(cx).core().profiles();
        profiles.sort_by_cached_key(|p| p.name.to_lowercase());
        let language = lang();
        if profiles != self.profiles || language != self.language {
            self.language = language;
            self.profiles = profiles;
            let items = std::iter::once(Item::option(label(Message::WinMcpAllSessions {})))
                .chain(self.profiles.iter().map(|p| Item::option(p.name.clone())))
                .collect();
            let selected = self
                .filter
                .profile
                .and_then(|id| self.profiles.iter().position(|p| p.id == id).map(|i| i + 1));
            self.session_picker.update(cx, |d, cx| {
                d.set_items(items, cx);
                d.set_selected(Some(selected.unwrap_or(0)), cx);
            });
            if selected.is_none() {
                self.filter.profile = None;
            }
        }
        self.refresh_accesses(cx);
    }

    fn refresh_accesses(&mut self, cx: &mut Context<Self>) {
        match self.model.read(cx).core().mcp_accesses() {
            Ok(levels) => {
                self.levels = levels.into_iter().collect();
                self.levels_error = None;
            }
            Err(error) => {
                self.levels.clear();
                self.levels_error = Some(operator_error(error, Message::WinMcpGrantReadFailed {}));
            }
        }
    }

    fn load_page(&mut self, first_page: bool, cx: &mut Context<Self>) {
        self.filter.before_id = if first_page {
            None
        } else {
            self.records.last().map(|r| r.id)
        };
        match self.model.read(cx).core().mcp_audit(self.filter.clone()) {
            Ok(records) => {
                self.audit_error = None;
                self.has_more = records.len() == AUDIT_PAGE as usize;
                if first_page {
                    self.has_updates = false;
                    self.records = records;
                } else {
                    self.records.extend(records);
                }
            }
            Err(error) => {
                self.audit_error = Some(operator_error(error, Message::WinMcpAuditReadFailed {}));
                self.has_more = false;
            }
        }
        cx.notify();
    }

    fn clear_confirmed(&mut self, cx: &mut Context<Self>) {
        self.confirm_clear = false;
        match self.model.read(cx).core().mcp_audit_clear() {
            Ok(()) => self.load_page(true, cx),
            Err(error) => {
                self.audit_error = Some(operator_error(error, Message::WinMcpAuditClearFailed {}));
                cx.notify();
            }
        }
    }

    fn render_entry(
        &self,
        th: &Palette,
        record: &McpAuditRecord,
        names: &HashMap<ProfileId, &str>,
        offset: i32,
    ) -> AnyElement {
        let entry = &record.entry;
        let mut parts = vec![
            format_mtime(entry.at_ms, offset),
            client_name(&entry.client).to_owned(),
        ];
        if !entry.tool.is_empty() {
            parts.push(entry.tool.clone());
        }
        if let Some(id) = entry.profile {
            let name = match names.get(&id) {
                None => label(Message::WinMcpAuditDeleted {}),
                Some(_) if self.levels_error.is_none() && !self.levels.contains_key(&id) => {
                    label(Message::WinMcpAuditUnauthorized {})
                }
                Some(name) => name,
            };
            parts.push(name.to_owned());
        }
        if let Some(code) = &entry.error_code {
            parts.push(code.clone());
        }
        if matches!(
            entry.tool.as_str(),
            "kanso_terminal_run" | "kanso_terminal_send"
        ) && let Some(tab) = entry.session
        {
            parts.push(format!("tab={}", tab.raw()));
        }
        let input = entry.input();
        if let Some(dispatch) = input.as_ref().and_then(|i| i.dispatch) {
            parts.push(
                label(match dispatch {
                    McpAuditDispatch::Submitted => Message::WinMcpAuditSubmitted {},
                    McpAuditDispatch::Rejected => Message::WinMcpAuditNotSent {},
                    McpAuditDispatch::Unconfirmed => Message::WinMcpAuditUnconfirmed {},
                })
                .to_owned(),
            );
        }
        let args = input
            .as_ref()
            .map_or(entry.args.as_str(), |input| input.options);
        let head_color = if entry.error_code.is_some() {
            th.c("status.error")
        } else {
            th.c("text.secondary")
        };
        div()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .child(
                div()
                    .text_size(px(number("font", "ui.caption")))
                    .text_color(head_color)
                    .child(parts.join(" · ")),
            )
            .when_some(input, |d, input| {
                let full: SharedString = input.text.to_owned().into();
                let heading = if entry.tool == "kanso_terminal_send" {
                    label(Message::WinMcpAuditInput {})
                } else {
                    label(Message::WinMcpAuditCommand {})
                };
                let tip_th = *th;
                d.child(
                    div()
                        .id(("mcp-act-input", record.id as usize))
                        .py(px(4.0))
                        .px(px(8.0))
                        .rounded(px(number("radius", "sm")))
                        .bg(th.c("bg.rowHover"))
                        .text_size(px(number("font", "ui.caption")))
                        .text_color(th.c("text.primary"))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(heading))
                        .child(
                            div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(full.clone()),
                        )
                        .tooltip(move |_, cx| {
                            cx.new(|_| ArgsTooltip {
                                text: full.clone(),
                                th: tip_th,
                            })
                            .into()
                        }),
                )
            })
            .when(!args.is_empty(), |d| {
                let full: SharedString = args.to_owned().into();
                let tip_th = *th;
                d.child(
                    div()
                        .id(("mcp-act-args", record.id as usize))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(px(number("font", "ui.caption")))
                        .text_color(th.c("text.tertiary"))
                        .child(args.split_whitespace().collect::<Vec<_>>().join(" "))
                        .tooltip(move |_, cx| {
                            cx.new(|_| ArgsTooltip {
                                text: full.clone(),
                                th: tip_th,
                            })
                            .into()
                        }),
                )
            })
            .into_any_element()
    }
}

impl Render for McpActivityView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language != lang() {
            window.set_window_title(&render_message(&Message::WinMcpActivityTitle {}));
            self.refresh_session_data(cx);
        }
        let th = Palette(display::window_appearance(window));
        let names: HashMap<_, _> = self
            .profiles
            .iter()
            .map(|p| (p.id, p.name.as_str()))
            .collect();
        let offset = tz::local_offset_minutes();

        let checkbox = div()
            .id("mcp-act-errors-only")
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.filter.errors_only = !this.filter.errors_only;
                    this.load_page(true, cx);
                }),
            )
            .child(
                div()
                    .w(px(16.0))
                    .h(px(16.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(if self.filter.errors_only {
                        th.c("accent")
                    } else {
                        th.c("border.strong")
                    })
                    .bg(if self.filter.errors_only {
                        th.c("accent")
                    } else {
                        th.c("bg.field")
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(self.filter.errors_only, |d| {
                        d.child(icon("check", 11.0, th.c("text.onAccent")))
                    }),
            )
            .child(
                div()
                    .text_size(px(number("font", "ui.body")))
                    .text_color(th.c("text.primary"))
                    .child(render_message(&Message::WinMcpErrorsOnly {})),
            );

        let list_content: AnyElement = if let Some(error) = &self.audit_error {
            div()
                .text_size(px(number("font", "ui.caption")))
                .text_color(th.c("status.error"))
                .child(error.clone())
                .into_any_element()
        } else if self.records.is_empty() {
            div()
                .text_size(px(number("font", "ui.caption")))
                .text_color(th.c("text.tertiary"))
                .child(render_message(&Message::WinMcpNoAudit {}))
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .children(
                    self.records
                        .iter()
                        .map(|r| self.render_entry(&th, r, &names, offset)),
                )
                .into_any_element()
        };

        let make_btn = |id: &'static str, title: Message, danger: bool| {
            div()
                .id(id)
                .h(px(number("spacing", "field.height") - 4.0))
                .px(px(12.0))
                .rounded(px(number("radius", "md")))
                .border_1()
                .border_color(if danger {
                    th.c("status.error")
                } else {
                    th.c("border.strong")
                })
                .bg(if danger {
                    th.c("status.error")
                } else {
                    th.c("bg.buttonSecondary")
                })
                .text_size(px(number("font", "ui.body")))
                .text_color(if danger {
                    th.c("text.onAccent")
                } else {
                    th.c("text.primary")
                })
                .flex()
                .items_center()
                .cursor_pointer()
                .hover(|s| s.opacity(0.85))
                .child(render_message(&title))
        };

        let bottom_bar = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .when(self.has_updates, |d| {
                d.child(
                    make_btn("mcp-act-refresh", Message::WinMcpAuditRefresh {}, false)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                this.load_page(true, cx);
                            }),
                        ),
                )
            })
            .child(div().flex_1())
            .when(self.has_more, |d| {
                d.child(
                    make_btn("mcp-act-more", Message::WinMcpAuditMore {}, false).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.load_page(false, cx);
                        }),
                    ),
                )
            })
            .child(
                make_btn("mcp-act-clear", Message::WinMcpAuditClear {}, false).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.confirm_clear = true;
                        cx.notify();
                    }),
                ),
            );

        let confirm_overlay = self.confirm_clear.then(|| {
            div()
                .id("mcp-act-confirm-scrim")
                .absolute()
                .inset_0()
                .bg(th.c("bg.scrim"))
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.confirm_clear = false;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .id("mcp-act-confirm-box")
                        .w(px(380.0))
                        .p(px(18.0))
                        .rounded(px(number("radius", "lg")))
                        .bg(th.c("bg.dialog"))
                        .border_1()
                        .border_color(th.c("border.strong"))
                        .flex()
                        .flex_col()
                        .gap(px(12.0))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .text_size(px(number("font", "ui.body") + 1.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(th.c("text.primary"))
                                .child(render_message(&Message::WinMcpAuditClearConfirmTitle {})),
                        )
                        .child(
                            div()
                                .text_size(px(number("font", "ui.caption")))
                                .text_color(th.c("text.secondary"))
                                .child(render_message(&Message::WinMcpAuditClearConfirmBody {})),
                        )
                        .child(
                            div()
                                .mt(px(4.0))
                                .flex()
                                .justify_end()
                                .gap(px(8.0))
                                .child(
                                    make_btn("mcp-act-confirm-cancel", Message::UiCancel {}, false)
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                                cx.stop_propagation();
                                                this.confirm_clear = false;
                                                cx.notify();
                                            }),
                                        ),
                                )
                                .child(
                                    make_btn(
                                        "mcp-act-confirm-ok",
                                        Message::WinMcpAuditClear {},
                                        true,
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                            cx.stop_propagation();
                                            this.clear_confirmed(cx);
                                        }),
                                    ),
                                ),
                        ),
                )
        });

        div()
            .id("mcp-activity-root")
            .relative()
            .size_full()
            .bg(th.c("bg.window"))
            .font_family(display::ui_font_family())
            .text_size(px(number("font", "ui.body")))
            .text_color(th.c("text.primary"))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" && !ev.keystroke.modifiers.modified() {
                    if this.confirm_clear {
                        this.confirm_clear = false;
                        cx.notify();
                    } else {
                        window.remove_window();
                    }
                    cx.stop_propagation();
                }
            }))
            .p(px(20.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .text_size(px(number("font", "ui.caption")))
                    .text_color(th.c("text.tertiary"))
                    .child(render_message(&Message::WinMcpActivityIntro {})),
            )
            .when_some(self.levels_error.clone(), |d, err| {
                d.child(
                    div()
                        .text_size(px(number("font", "ui.caption")))
                        .text_color(th.c("status.error"))
                        .child(err),
                )
            })
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_size(px(number("font", "ui.caption")))
                                    .text_color(th.c("text.secondary"))
                                    .child(label(Message::WinMcpClient {})),
                            )
                            .child(self.client_input.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_size(px(number("font", "ui.caption")))
                                    .text_color(th.c("text.secondary"))
                                    .child(label(Message::UiSession {})),
                            )
                            .child(self.session_picker.clone()),
                    ),
            )
            .child(div().flex_none().child(checkbox))
            .child(
                div()
                    .id("mcp-act-list")
                    .flex_1()
                    .min_h(px(120.0))
                    .rounded(px(number("radius", "lg")))
                    .bg(th.c("bg.card"))
                    .border_1()
                    .border_color(th.c("border.card"))
                    .p(px(14.0))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(list_content),
            )
            .child(bottom_bar)
            .children(confirm_overlay)
    }
}

fn operator_error(error: CoreError, fallback: Message) -> String {
    log(&format!("MCP 活动记录操作失败：{error}"));
    render_message(error.user_message().unwrap_or(&fallback))
}

fn client_name(name: &str) -> &str {
    if name.is_empty() {
        label(Message::WinMcpUnknownClient {})
    } else {
        name
    }
}

struct ArgsTooltip {
    text: SharedString,
    th: Palette,
}

impl Render for ArgsTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(420.0))
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(number("radius", "md")))
            .bg(self.th.c("bg.popover"))
            .border_1()
            .border_color(self.th.c("border.strong"))
            .text_size(px(number("font", "ui.caption")))
            .text_color(self.th.c("text.primary"))
            .child(self.text.clone())
    }
}
