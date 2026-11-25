//! 独立的 MCP 活动记录窗口（mcp.md §7.3）。

use std::collections::HashMap;

use gpui::{
    AnyElement, App, Bounds, Context, ElementId, FocusHandle, FontWeight, KeyDownEvent, Render,
    ScrollHandle, SharedString, Subscription, TitlebarOptions, Window, WindowBounds, WindowOptions,
    div, point, prelude::*, px, size,
};
use kanso_gpui::app::i18n::{Lang, label, lang};
use kanso_gpui::app::launcher::render_message;
use kanso_gpui::filer::format_mtime;
use kanso_gpui::widgets::dropdown::Item;
use kanso_gpui::widgets::style::{Appearance, number};
use kanso_types::ProfileId;
use kanso_types::mcp::{McpAccess, McpAuditDispatch, McpAuditFilter, McpAuditRecord};
use kanso_types::message::Message;
use kanso_types::profile::Profile;

use super::{McpActions, Th, caption, check_row, client_name, error_label, operator_error, row};
use crate::ui::mcp::{AuditChanged, McpModel};
use crate::ui::widgets::dropdown::{Dropdown, DropdownEvent};
use crate::ui::widgets::icons::icon;
use crate::ui::widgets::text_input::{TextInput, TextInputEvent};
use crate::ui::{display, log, tz};

const AUDIT_PAGE: u32 = 100;

pub(super) fn open(model: gpui::Entity<McpModel>, cx: &mut App) {
    if let Some(handle) = model.read(cx).audit_window
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let bounds = Bounds::centered(None, size(px(640.0), px(520.0)), cx);
    let title = render_message(&Message::WinMcpAudit {});
    let view_model = model.clone();
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.0), px(340.0))),
            titlebar: Some(TitlebarOptions {
                title: Some(title.clone().into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        move |window, cx| {
            #[cfg(target_os = "linux")]
            window.set_window_title(&title);
            cx.new(|cx| AuditView::new(view_model, window, cx))
        },
    );
    match opened {
        Ok(handle) => model.update(cx, |model, _| model.audit_window = Some(handle)),
        Err(error) => log(&format!("打开 MCP 活动记录失败：{error:#}")),
    }
}

pub(in crate::ui) struct AuditView {
    model: gpui::Entity<McpModel>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    audit_client: gpui::Entity<TextInput>,
    audit_session: gpui::Entity<Dropdown>,
    profiles: Vec<Profile>,
    levels: Option<HashMap<ProfileId, McpAccess>>,
    audit_filter: McpAuditFilter,
    audit_records: Vec<McpAuditRecord>,
    audit_error: Option<String>,
    audit_has_more: bool,
    audit_has_updates: bool,
    expanded: Option<i64>,
    confirm_clear: bool,
    language: Lang,
    appearance: Option<Appearance>,
    _subs: Vec<Subscription>,
}

impl AuditView {
    fn new(model: gpui::Entity<McpModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let audit_client = cx.new(|cx| TextInput::new(window, cx));
        let audit_session = cx.new(|cx| Dropdown::new(window, cx));
        let subs = vec![
            cx.subscribe(&model, |this, _, _: &AuditChanged, cx| {
                if this.audit_filter.before_id.is_none() {
                    this.load_audit(true, cx);
                } else {
                    this.audit_has_updates = true;
                    cx.notify();
                }
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.sync_profiles(cx);
                }
            }),
            cx.subscribe(&audit_client, |this, input, event, cx| {
                if matches!(event, TextInputEvent::Submit(_) | TextInputEvent::Blur) {
                    let name = input.read(cx).text().trim();
                    let client = (!name.is_empty()).then(|| name.to_owned());
                    if client != this.audit_filter.client {
                        this.audit_filter.client = client;
                        this.reset_list(cx);
                    }
                }
            }),
            cx.subscribe(&audit_session, |this, _, event, cx| {
                if let DropdownEvent::Change(i) = event {
                    this.audit_filter.profile = i
                        .checked_sub(1)
                        .and_then(|i| this.profiles.get(i))
                        .map(|profile| profile.id);
                    this.reset_list(cx);
                }
            }),
        ];
        let mut view = Self {
            model,
            focus,
            scroll: ScrollHandle::new(),
            audit_client,
            audit_session,
            profiles: Vec::new(),
            levels: None,
            audit_filter: McpAuditFilter {
                limit: AUDIT_PAGE,
                ..Default::default()
            },
            audit_records: Vec::new(),
            audit_error: None,
            audit_has_more: false,
            audit_has_updates: false,
            expanded: None,
            confirm_clear: false,
            language: lang(),
            appearance: None,
            _subs: subs,
        };
        view.sync_profiles(cx);
        view.load_audit(true, cx);
        view
    }

    fn sync_profiles(&mut self, cx: &mut Context<Self>) {
        let core = self.model.read(cx).core();
        self.levels = core
            .mcp_accesses()
            .ok()
            .map(|levels| levels.into_iter().collect());
        let mut profiles = core.profiles();
        profiles.sort_by_cached_key(|p| p.name.to_lowercase());
        self.profiles = profiles;
        self.update_audit_sessions(cx);
        cx.notify();
    }

    fn reset_list(&mut self, cx: &mut Context<Self>) {
        self.expanded = None;
        self.scroll.set_offset(point(px(0.0), px(0.0)));
        self.load_audit(true, cx);
    }

    fn update_audit_sessions(&mut self, cx: &mut Context<Self>) {
        let items = std::iter::once(Item::option(label(Message::WinMcpAllSessions {})))
            .chain(self.profiles.iter().map(|p| Item::option(p.name.clone())))
            .collect();
        let selected = self.audit_filter.profile.and_then(|id| {
            self.profiles
                .iter()
                .position(|profile| profile.id == id)
                .map(|i| i + 1)
        });
        self.audit_session.update(cx, |d, cx| {
            d.set_items(items, cx);
            d.set_selected(Some(selected.unwrap_or(0)), cx);
        });
        if selected.is_none() && self.audit_filter.profile.take().is_some() {
            self.load_audit(true, cx);
        }
    }

    fn load_audit(&mut self, first_page: bool, cx: &mut Context<Self>) {
        self.audit_filter.before_id = if first_page {
            None
        } else {
            self.audit_records.last().map(|record| record.id)
        };
        match self
            .model
            .read(cx)
            .core()
            .mcp_audit(self.audit_filter.clone())
        {
            Ok(records) => {
                self.audit_error = None;
                self.audit_has_more = records.len() == AUDIT_PAGE as usize;
                if first_page {
                    self.audit_has_updates = false;
                    self.audit_records = records;
                    self.expanded = self
                        .expanded
                        .filter(|id| self.audit_records.iter().any(|record| record.id == *id));
                } else {
                    self.audit_records.extend(records);
                }
            }
            Err(error) => {
                self.audit_error = Some(operator_error(error, Message::WinMcpAuditReadFailed {}));
                self.audit_has_more = false;
            }
        }
        cx.notify();
    }

    fn clear_audit(&mut self, cx: &mut Context<Self>) {
        match self.model.read(cx).core().mcp_audit_clear() {
            Ok(()) => self.reset_list(cx),
            Err(error) => {
                self.audit_error = Some(operator_error(error, Message::WinMcpAuditClearFailed {}));
                cx.notify();
            }
        }
    }

    fn audit_entry(
        &self,
        th: &Th,
        record: &McpAuditRecord,
        names: &HashMap<ProfileId, &str>,
        offset: i32,
        actions: &mut McpActions<'_, '_, Self>,
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
                // 授权读取失败时无法判断是否未授权，只显示会话名。
                Some(_)
                    if self
                        .levels
                        .as_ref()
                        .is_some_and(|levels| !levels.contains_key(&id)) =>
                {
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
        if let Some(dispatch) = input.as_ref().and_then(|input| input.dispatch) {
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
        let has_input = input.is_some();
        let has_details = has_input || !args.is_empty();
        let id = record.id;
        let expanded = self.expanded == Some(id);
        let row_id: ElementId = ("mcp-audit-entry", id as usize).into();
        let content = div()
            .id(row_id.clone())
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .p(px(8.0))
            .rounded(px(number("radius", "sm")))
            .border_1()
            .border_color(gpui::transparent_black())
            .bg(th.c("bg.rowHover"))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .when(!expanded, |d| d.truncate())
                            .text_size(px(number("font", "ui.caption")))
                            .text_color(th.c("text.tertiary"))
                            .child(parts.join(" · ")),
                    )
                    .when(has_details, |d| {
                        d.child(icon(
                            if expanded {
                                "chevron-down"
                            } else {
                                "chevron-right"
                            },
                            14.0,
                            th.c("text.secondary"),
                        ))
                    }),
            )
            .when_some(input.as_ref(), |d, input| {
                let heading = if entry.tool == "kanso_terminal_send" {
                    label(Message::WinMcpAuditInput {})
                } else {
                    label(Message::WinMcpAuditCommand {})
                };
                d.child(
                    div()
                        .min_w_0()
                        .flex()
                        .gap(px(6.0))
                        .when(expanded, |d| d.flex_col())
                        .text_size(px(number("font", "ui.caption")))
                        .child(
                            div()
                                .flex_none()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(heading),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .when(!expanded, |d| d.flex_1().truncate())
                                .child(input.text.to_owned()),
                        ),
                )
            })
            .when(!args.is_empty() && (expanded || !has_input), |d| {
                d.child(
                    div()
                        .min_w_0()
                        .when(!expanded, |d| d.truncate())
                        .text_size(px(number("font", "ui.caption")))
                        .text_color(th.c("text.secondary"))
                        .child(if expanded {
                            args.to_owned()
                        } else {
                            args.split_whitespace().collect::<Vec<_>>().join(" ")
                        }),
                )
            });
        if has_details {
            actions.control(content, row_id, true, move |this, cx| {
                this.expanded = (!expanded).then_some(id);
                cx.notify();
            })
        } else {
            content.into_any_element()
        }
    }
}

impl Render for AuditView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = display::window_appearance(window);
        if self.appearance != Some(appearance) {
            display::apply_titlebar(window);
            self.appearance = Some(appearance);
        }
        if self.language != lang() {
            self.language = lang();
            window.set_window_title(label(Message::WinMcpAudit {}));
            self.update_audit_sessions(cx);
        }
        let th = Th(appearance);
        let mut actions = McpActions {
            th: &th,
            scroll: None,
            window,
            cx,
        };
        let filters = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(caption(&th, label(Message::WinMcpAuditHint {})))
            .child(
                div()
                    .flex()
                    .gap(px(10.0))
                    .child(row(
                        &th,
                        label(Message::WinMcpClient {}),
                        self.audit_client.clone(),
                    ))
                    .child(row(
                        &th,
                        label(Message::UiSession {}),
                        self.audit_session.clone(),
                    )),
            )
            .child(check_row(
                &th,
                render_message(&Message::WinMcpErrorsOnly {}),
                actions.checkbox(
                    "mcp-audit-errors",
                    self.audit_filter.errors_only,
                    true,
                    |this, on, cx| {
                        this.audit_filter.errors_only = on;
                        this.reset_list(cx);
                    },
                ),
            ));
        let footer = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .when(self.audit_has_updates, |d| {
                d.child(actions.button(
                    "mcp-audit-refresh",
                    Message::WinMcpAuditRefresh {},
                    false,
                    true,
                    Self::reset_list,
                ))
            })
            .when(self.audit_has_more, |d| {
                d.child(actions.button(
                    "mcp-audit-more",
                    Message::WinMcpAuditMore {},
                    false,
                    true,
                    |this, cx| this.load_audit(false, cx),
                ))
            })
            .child(div().flex_1())
            .when(self.confirm_clear, |d| {
                d.child(caption(&th, label(Message::WinMcpAuditConfirmClear {})))
                    .child(actions.button(
                        "mcp-audit-cancel-clear",
                        Message::UiCancel {},
                        false,
                        true,
                        |this, cx| {
                            this.confirm_clear = false;
                            cx.notify();
                        },
                    ))
            })
            .child(actions.button(
                "mcp-audit-clear",
                Message::WinMcpAuditClear {},
                false,
                true,
                |this, cx| {
                    if this.confirm_clear {
                        this.confirm_clear = false;
                        this.clear_audit(cx);
                    } else {
                        this.confirm_clear = true;
                        cx.notify();
                    }
                },
            ));
        actions.scroll = Some(&self.scroll);
        let names: HashMap<_, _> = self
            .profiles
            .iter()
            .map(|p| (p.id, p.name.as_str()))
            .collect();
        let offset = tz::local_offset_minutes();
        let records = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .when_some(self.audit_error.as_deref(), |d, error| {
                d.child(error_label(&th, error))
            })
            .when(
                self.audit_error.is_none() && self.audit_records.is_empty(),
                |d| d.child(caption(&th, label(Message::WinMcpNoAudit {}))),
            )
            .children(
                self.audit_records
                    .iter()
                    .map(|record| self.audit_entry(&th, record, &names, offset, &mut actions)),
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .p(px(16.0))
            .bg(th.c("bg.window"))
            .font_family(SharedString::from(display::ui_font_family()))
            .text_size(px(display::ui_font_size()))
            .text_color(th.c("text.primary"))
            .track_focus(&self.focus)
            .tab_group()
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let key = &ev.keystroke;
                if key.key == "tab"
                    && !key.modifiers.control
                    && !key.modifiers.alt
                    && !key.modifiers.platform
                {
                    if key.modifiers.shift {
                        window.focus_prev();
                    } else {
                        window.focus_next();
                    }
                    cx.stop_propagation();
                } else if key.key == "escape" {
                    if this.confirm_clear {
                        this.confirm_clear = false;
                        cx.notify();
                    } else {
                        window.remove_window();
                    }
                    cx.stop_propagation();
                }
            }))
            .child(filters)
            .child(
                div()
                    .id("mcp-audit-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(records),
            )
            .child(footer)
    }
}
