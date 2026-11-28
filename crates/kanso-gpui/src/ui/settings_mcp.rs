//! 设置 → MCP：开关与状态、接入配置和已连接的客户端（mcp.md §7.1）。
//! 会话授权在会话管理与会话编辑对话框中配置（§7.2），活动记录由独立窗口承载（§7.3）。

use std::{cell::Cell, rc::Rc};

use gpui::{
    AnyElement, ClipboardItem, Context, Div, ElementId, Entity, KeyDownEvent, Render, ScrollHandle,
    Stateful, Subscription, Window, canvas, div, prelude::*, px,
};
use kanso_core::CoreError;
use kanso_gpui::app::i18n::{Lang, label, lang};
use kanso_gpui::app::launcher::render_message;
use kanso_gpui::filer::format_mtime;
use kanso_gpui::widgets::dropdown::Item;
use kanso_gpui::widgets::style::number;
use kanso_mcp_server::{McpClientKind, McpServerStatus, McpUnavailable};
use kanso_types::mcp::McpAccess;
use kanso_types::message::Message;

use super::{Th, boxed, caption, card, check_row, checkbox_box, row};
use crate::ui::mcp::{McpModel, ShowActivity};
use crate::ui::widgets::dropdown::{Dropdown, DropdownEvent};
use crate::ui::widgets::text_input::TextInput;
use crate::ui::{display, log, tz};

pub(in crate::ui) const MCP_ACCESS_LEVELS: [McpAccess; 3] =
    [McpAccess::Off, McpAccess::ReadOnly, McpAccess::Full];
const CLIENTS: [McpClientKind; 2] = [McpClientKind::ClaudeCode, McpClientKind::Codex];

pub(in crate::ui) fn mcp_access_title(level: McpAccess) -> String {
    render_message(&match level {
        McpAccess::Off => Message::WinMcpAccessOff {},
        McpAccess::ReadOnly => Message::WinMcpAccessReadOnly {},
        McpAccess::Full => Message::WinMcpAccessFull {},
    })
}

fn client_items() -> Vec<Item> {
    CLIENTS
        .map(|client| {
            Item::option(label(match client {
                McpClientKind::ClaudeCode => Message::WinMcpClaudeCode {},
                McpClientKind::Codex => Message::WinMcpCodex {},
            }))
        })
        .into()
}

pub(super) struct McpPage {
    model: Entity<McpModel>,
    pub(super) scroll: ScrollHandle,
    client: McpClientKind,
    command: Entity<TextInput>,
    client_picker: Entity<Dropdown>,
    service_error: Option<String>,
    granted_summary: Result<usize, String>,
    shown: bool,
    summary_dirty: bool,
    language: Lang,
    _subs: Vec<Subscription>,
}

impl McpPage {
    pub(super) fn new(
        model: Entity<McpModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let client_picker =
            cx.new(|cx| Dropdown::new(window, cx).items(client_items()).selected(0));
        let command = cx.new(|cx| {
            TextInput::new(window, cx)
                .read_only()
                .placeholder(Message::WinMcpTokenNeeded {})
        });
        let subs = vec![
            cx.observe(&model, |this, _, cx| {
                if this.shown {
                    this.summary_dirty = true;
                    this.update_command(cx);
                }
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if this.shown && window.is_window_active() {
                    this.summary_dirty = true;
                    cx.notify();
                }
            }),
            cx.subscribe(&client_picker, |this, _, event, cx| {
                if let DropdownEvent::Change(i) = event {
                    this.client = CLIENTS[*i];
                    this.update_command(cx);
                }
            }),
        ];
        Self {
            model,
            scroll: ScrollHandle::new(),
            client: CLIENTS[0],
            command,
            client_picker,
            service_error: None,
            granted_summary: Ok(0),
            shown: false,
            summary_dirty: true,
            language: lang(),
            _subs: subs,
        }
    }

    pub(super) fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.shown = shown;
        if shown {
            self.summary_dirty = true;
            self.model.update(cx, |m, cx| m.refresh_token(cx));
            self.update_command(cx);
        }
    }

    fn update_command(&mut self, cx: &mut Context<Self>) {
        let text = self.model.read(cx).command(self.client).unwrap_or_default();
        // 模型的客户端通知可能很频繁；内容未变时保留用户正在选择的文字。
        if self.command.read(cx).text() != text {
            self.command
                .update(cx, |input, cx| input.set_text(&text, cx));
        }
        cx.notify();
    }

    fn toggle_service(&mut self, on: bool, cx: &mut Context<Self>) {
        self.service_error = self.model.update(cx, |m, cx| m.set_enabled(on, cx)).err();
        self.update_command(cx);
    }

    fn regenerate(&mut self, cx: &mut Context<Self>) {
        self.service_error = self.model.update(cx, |m, cx| m.regenerate_token(cx)).err();
        self.update_command(cx);
    }

    fn sync_summary(&mut self, cx: &mut Context<Self>) {
        self.language = lang();
        self.granted_summary = self
            .model
            .read(cx)
            .core()
            .mcp_accesses()
            .map(|levels| levels.len())
            .map_err(|error| operator_error(error, Message::WinMcpGrantReadFailed {}));
        self.summary_dirty = false;
    }

    fn status_text(status: &McpServerStatus) -> (Message, Option<Message>) {
        match status {
            McpServerStatus::Running { .. } => (Message::WinMcpRunning {}, None),
            McpServerStatus::Stopped => (Message::WinMcpStopped {}, None),
            McpServerStatus::Unavailable(reason) => (
                Message::WinMcpUnavailable {},
                Some(match reason {
                    McpUnavailable::NotSupported => Message::WinMcpNotSupported {},
                    McpUnavailable::AnotherInstance => Message::WinMcpAnotherInstance {},
                    McpUnavailable::Failed { .. } => Message::WinMcpStartFailed {},
                }),
            ),
        }
    }
}

impl Render for McpPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.summary_dirty || self.language != lang() {
            self.sync_summary(cx);
        }
        let th = Th(display::window_appearance(window));
        let (enabled, status, helper, clients) = {
            let model = self.model.read(cx);
            (
                model.enabled(),
                model.status().clone(),
                model.helper_path().to_string_lossy().into_owned(),
                model.clients().to_vec(),
            )
        };
        let owns_service = !matches!(
            status,
            McpServerStatus::Unavailable(
                McpUnavailable::AnotherInstance | McpUnavailable::NotSupported
            )
        );
        let status_color = match status {
            McpServerStatus::Running { .. } => th.c("status.unreadDone"),
            McpServerStatus::Stopped => th.c("status.disconnected"),
            McpServerStatus::Unavailable(_) => th.c("status.warning"),
        };
        let (status_msg, reason) = Self::status_text(&status);
        let has_command = !self.command.read(cx).text().is_empty();
        let mut actions = McpActions {
            th: &th,
            scroll: &self.scroll,
            window,
            cx,
        };
        let mut mcp_rows = vec![
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .child(check_row(
                    &th,
                    render_message(&Message::WinMcpEnable {}),
                    actions.checkbox("mcp-enable", enabled, owns_service, Self::toggle_service),
                ))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(px(number("font", "ui.caption")))
                        .text_color(th.c("text.secondary"))
                        .child(div().w(px(7.0)).h(px(7.0)).rounded_full().bg(status_color))
                        .child(render_message(&status_msg)),
                )
                .into_any_element(),
        ];
        // 只展示当前最需要看的一条说明：出错看错误，不可用看原因，正常态才显示功能说明。
        if let Some(error) = &self.service_error {
            mcp_rows.push(error_label(&th, error));
        } else if let Some(reason) = reason {
            mcp_rows.push(caption(&th, label(reason)));
        } else {
            mcp_rows.push(caption(&th, label(Message::WinMcpEnableHint {})));
        }
        // 已授权会话数的只读摘要，具体授权在会话管理与会话编辑对话框中修改。
        match &self.granted_summary {
            Ok(0) => mcp_rows.push(caption(&th, label(Message::WinMcpGrantedNone {}))),
            Ok(n) => mcp_rows.push(caption(
                &th,
                &render_message(&Message::WinMcpGrantedCount {
                    count: i64::try_from(*n).unwrap_or(i64::MAX),
                }),
            )),
            Err(error) => mcp_rows.push(error_label(&th, error)),
        }
        mcp_rows.push(
            div()
                .flex()
                .gap(px(8.0))
                .child(actions.button(
                    "mcp-open-activity",
                    Message::WinMcpOpenActivity {},
                    false,
                    true,
                    |this, cx| {
                        this.model.update(cx, |_, cx| cx.emit(ShowActivity));
                    },
                ))
                .into_any_element(),
        );
        let offset = tz::local_offset_minutes();
        let client_rows: Vec<_> = if clients.is_empty() {
            vec![caption(&th, label(Message::WinMcpNoClients {}))]
        } else {
            clients
                .into_iter()
                .map(|client| {
                    div()
                        .id(("mcp-live-client", client.id as usize))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(client_name(&client.name).to_owned()),
                        )
                        .child(caption(&th, &format_mtime(client.connected_at_ms, offset)))
                        .child(caption(
                            &th,
                            &render_message(&Message::WinMcpCalls {
                                count: i64::try_from(client.calls).unwrap_or(i64::MAX),
                            }),
                        ))
                        .child(actions.button(
                            ("mcp-disconnect", client.id as usize),
                            Message::WinMcpDisconnect {},
                            false,
                            true,
                            move |this, cx| this.model.read(cx).disconnect(client.id),
                        ))
                        .into_any_element()
                })
                .collect()
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(card(&th, label(Message::WinMcpPage {}), "plug", mcp_rows))
            .child(card(
                &th,
                label(Message::WinMcpConfiguration {}),
                "copy",
                vec![
                    row(
                        &th,
                        label(Message::WinMcpClient {}),
                        boxed(self.client_picker.clone(), 180.0),
                    ),
                    row(
                        &th,
                        label(if cfg!(windows) {
                            Message::WinMcpPowerShellCommand {}
                        } else {
                            Message::WinMcpShellCommand {}
                        }),
                        self.command.clone(),
                    ),
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(actions.button(
                            "mcp-copy-command",
                            Message::UiCopyCommand {},
                            true,
                            has_command,
                            |this, cx| {
                                let command = this.command.read(cx).text().to_owned();
                                cx.write_to_clipboard(ClipboardItem::new_string(command));
                            },
                        ))
                        .child(actions.button(
                            "mcp-regenerate",
                            Message::WinMcpRegenerate {},
                            false,
                            has_command && owns_service,
                            Self::regenerate,
                        ))
                        .into_any_element(),
                    caption(
                        &th,
                        label(if cfg!(windows) {
                            Message::WinMcpTokenHistory {}
                        } else {
                            Message::WinMcpTokenHistoryShell {}
                        }),
                    ),
                    row(
                        &th,
                        label(Message::WinMcpHelperPath {}),
                        caption(&th, &helper),
                    ),
                ],
            ))
            .child(card(
                &th,
                label(Message::WinMcpConnectedClients {}),
                "link",
                client_rows,
            ))
    }
}

fn operator_error(error: CoreError, fallback: Message) -> String {
    log(&format!("MCP 设置操作失败：{error}"));
    render_message(error.user_message().unwrap_or(&fallback))
}

fn client_name(name: &str) -> &str {
    if name.is_empty() {
        label(Message::WinMcpUnknownClient {})
    } else {
        name
    }
}

fn error_label(th: &Th, error: &str) -> AnyElement {
    div()
        .text_size(px(number("font", "ui.caption")))
        .text_color(th.c("status.error"))
        .child(error.to_owned())
        .into_any_element()
}

struct McpActions<'a, 'cx> {
    th: &'a Th,
    scroll: &'a ScrollHandle,
    window: &'a mut Window,
    cx: &'a mut Context<'cx, McpPage>,
}

impl McpActions<'_, '_> {
    fn button(
        &mut self,
        id: impl Into<ElementId>,
        title: Message,
        primary: bool,
        enabled: bool,
        run: impl Fn(&mut McpPage, &mut Context<McpPage>) + 'static,
    ) -> AnyElement {
        let id = id.into();
        let th = self.th;
        let button = div()
            .id(id.clone())
            .flex_none()
            .h(px(number("spacing", "field.height") - 4.0))
            .px(px(12.0))
            .flex()
            .items_center()
            .rounded(px(number("radius", "md")))
            .border_1()
            .border_color(gpui::transparent_black())
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
            .child(render_message(&title));
        self.control(button, id, enabled, run)
    }

    fn checkbox(
        &mut self,
        id: &'static str,
        checked: bool,
        enabled: bool,
        toggle: impl Fn(&mut McpPage, bool, &mut Context<McpPage>) + 'static,
    ) -> AnyElement {
        self.control(
            checkbox_box(self.th, id, checked),
            id.into(),
            enabled,
            move |this, cx| {
                toggle(this, !checked, cx);
            },
        )
    }

    fn control(
        &mut self,
        element: Stateful<Div>,
        id: ElementId,
        enabled: bool,
        run: impl Fn(&mut McpPage, &mut Context<McpPage>) + 'static,
    ) -> AnyElement {
        if !enabled {
            return element.opacity(0.45).into_any_element();
        }
        let state = self.window.use_keyed_state(id, self.cx, |_, cx| {
            (cx.focus_handle().tab_stop(true), Rc::new(Cell::new(false)))
        });
        let (focus, was_focused) = state.read(self.cx).clone();
        let border = self.th.c("text.primary");
        let scroll = self.scroll.clone();
        let run = Rc::new(run);
        let key_run = run.clone();
        element
            .relative()
            .track_focus(&focus)
            .focus(move |style| style.border_color(border))
            .cursor_pointer()
            .hover(|style| style.opacity(0.85))
            .on_key_down(self.cx.listener(move |this, ev: &KeyDownEvent, _, cx| {
                let key = &ev.keystroke;
                if matches!(key.key.as_str(), "enter" | "space") && !key.modifiers.modified() {
                    if !ev.is_held {
                        key_run(this, cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .on_click(self.cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                // 键盘在按下时触发，避免 GPUI 抬键时合成的点击重复执行。
                if !ev.is_keyboard() {
                    run(this, cx);
                }
            }))
            .child(
                canvas(
                    move |bounds, window, _| {
                        let focused = focus.is_focused(window);
                        if !was_focused.replace(focused) && focused {
                            let viewport = scroll.bounds();
                            let dy = if bounds.top() < viewport.top() {
                                viewport.top() - bounds.top()
                            } else if bounds.bottom() > viewport.bottom() {
                                viewport.bottom() - bounds.bottom()
                            } else {
                                px(0.0)
                            };
                            if dy != px(0.0) {
                                let scroll = scroll.clone();
                                window.on_next_frame(move |window, _| {
                                    let mut offset = scroll.offset();
                                    offset.y += dy;
                                    scroll.set_offset(offset);
                                    window.refresh();
                                });
                            }
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }
}
