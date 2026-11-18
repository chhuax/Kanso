//! 设置 → MCP：服务与接入、客户端、会话授权和审计记录（mcp.md §7）。

use std::collections::HashMap;

use gpui::{
    AnyElement, ClipboardItem, Context, Entity, FontWeight, Render, SharedString, Subscription,
    Window, div, prelude::*, px,
};
use kanso_core::CoreError;
use kanso_gpui::app::i18n::{Lang, label, lang};
use kanso_gpui::app::launcher::render_message;
use kanso_gpui::filer::format_mtime;
use kanso_gpui::widgets::dropdown::Item;
use kanso_gpui::widgets::style::number;
use kanso_mcp_server::{McpClientKind, McpServerStatus, McpUnavailable};
use kanso_types::ProfileId;
use kanso_types::mcp::{McpAccess, McpAuditFilter, McpAuditRecord};
use kanso_types::message::Message;
use kanso_types::profile::{Group, Profile};

use super::{Th, boxed, caption, card, check_row, checkbox, row};
use crate::ui::mcp::McpModel;
use crate::ui::widgets::dropdown::{Dropdown, DropdownEvent};
use crate::ui::widgets::text_input::{TextInput, TextInputEvent};
use crate::ui::{display, log, tz};

pub(in crate::ui) const MCP_ACCESS_LEVELS: [McpAccess; 3] =
    [McpAccess::Off, McpAccess::ReadOnly, McpAccess::Full];
const CLIENTS: [McpClientKind; 2] = [McpClientKind::ClaudeCode, McpClientKind::Codex];
const AUDIT_PAGE: u32 = 100;

pub(in crate::ui) fn mcp_access_title(level: McpAccess) -> String {
    render_message(&match level {
        McpAccess::Off => Message::WinMcpAccessOff {},
        McpAccess::ReadOnly => Message::WinMcpAccessReadOnly {},
        McpAccess::Full => Message::WinMcpAccessFull {},
    })
}

fn access_items() -> Vec<Item> {
    MCP_ACCESS_LEVELS
        .map(|level| Item::option(mcp_access_title(level)))
        .into()
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

struct GrantGroup {
    name: String,
    batch: Entity<Dropdown>,
    rows: Vec<(ProfileId, String, Entity<Dropdown>)>,
}

pub(super) struct McpPage {
    model: Entity<McpModel>,
    client: McpClientKind,
    command: Entity<TextInput>,
    client_picker: Entity<Dropdown>,
    service_error: Option<String>,
    profiles: Vec<Profile>,
    groups: Vec<Group>,
    grants: Vec<GrantGroup>,
    levels: HashMap<ProfileId, McpAccess>,
    grants_error: Option<String>,
    sensitive_notice: bool,
    audit_client: Entity<TextInput>,
    audit_session: Entity<Dropdown>,
    audit_filter: McpAuditFilter,
    audit_records: Vec<McpAuditRecord>,
    audit_error: Option<String>,
    audit_has_more: bool,
    shown: bool,
    grants_dirty: bool,
    language: Lang,
    grant_subs: Vec<Subscription>,
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
        let audit_client = cx.new(|cx| TextInput::new(window, cx));
        let audit_session = cx.new(|cx| {
            Dropdown::new(window, cx)
                .items(vec![Item::option(label(Message::WinMcpAllSessions {}))])
                .selected(0)
        });
        let subs = vec![
            cx.observe(&model, |this, _, cx| {
                if this.shown {
                    this.grants_dirty = true;
                    this.update_command(cx);
                }
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if this.shown && window.is_window_active() {
                    this.grants_dirty = true;
                    this.load_audit(true, cx);
                }
            }),
            cx.subscribe(&client_picker, |this, _, event, cx| {
                if let DropdownEvent::Change(i) = event {
                    this.client = CLIENTS[*i];
                    this.update_command(cx);
                }
            }),
            cx.subscribe(&audit_client, |this, input, event, cx| {
                if matches!(event, TextInputEvent::Submit(_) | TextInputEvent::Blur) {
                    let name = input.read(cx).text().trim();
                    let client = (!name.is_empty()).then(|| name.to_owned());
                    if client != this.audit_filter.client {
                        this.audit_filter.client = client;
                        this.load_audit(true, cx);
                    }
                }
            }),
            cx.subscribe(&audit_session, |this, _, event, cx| {
                if let DropdownEvent::Change(i) = event {
                    this.audit_filter.profile = i
                        .checked_sub(1)
                        .and_then(|i| this.profiles.get(i))
                        .map(|profile| profile.id);
                    this.load_audit(true, cx);
                }
            }),
        ];
        Self {
            model,
            client: CLIENTS[0],
            command,
            client_picker,
            service_error: None,
            profiles: Vec::new(),
            groups: Vec::new(),
            grants: Vec::new(),
            levels: HashMap::new(),
            grants_error: None,
            sensitive_notice: false,
            audit_client,
            audit_session,
            audit_filter: McpAuditFilter {
                limit: AUDIT_PAGE,
                ..Default::default()
            },
            audit_records: Vec::new(),
            audit_error: None,
            audit_has_more: false,
            shown: false,
            grants_dirty: true,
            language: lang(),
            grant_subs: Vec::new(),
            _subs: subs,
        }
    }

    pub(super) fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.shown = shown;
        if shown {
            self.grants_dirty = true;
            self.model.update(cx, |m, cx| m.refresh_token(cx));
            self.update_command(cx);
            self.load_audit(true, cx);
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

    fn apply_grants(&mut self, ids: &[ProfileId], level: McpAccess, cx: &mut Context<Self>) {
        self.grants_error = None;
        let core = self.model.read(cx).core();
        for &id in ids {
            let before = core.mcp_access(id);
            match core.set_mcp_access(id, level) {
                Ok(()) => self.sensitive_notice |= before == McpAccess::Off && level != before,
                Err(error) => {
                    self.grants_error = Some(operator_error(error, Message::WinMcpGrantFailed {}));
                }
            }
        }
        // 重新读取已保存值，让失败的选择回到真实授权。
        self.grants_dirty = true;
        cx.notify();
    }

    fn sync_grants(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let core = self.model.read(cx).core();
        let mut profiles = core.profiles();
        let mut groups = core.groups();
        self.levels = core.mcp_accesses().into_iter().collect();
        profiles.sort_by_cached_key(|p| p.name.to_lowercase());
        groups.sort_by_cached_key(|g| g.name.to_lowercase());
        let language = lang();
        if profiles != self.profiles || groups != self.groups || language != self.language {
            self.language = language;
            self.profiles = profiles;
            self.groups = groups;
            self.rebuild_grants(window, cx);
            self.update_audit_sessions(cx);
        }
        for (id, _, dropdown) in self.grants.iter().flat_map(|group| &group.rows) {
            let level = self.levels.get(id).copied().unwrap_or_default();
            let selected = Some(level as usize);
            if dropdown.read(cx).selected_index() != selected {
                dropdown.update(cx, |d, cx| d.set_selected(selected, cx));
            }
        }
        self.grants_dirty = false;
    }

    fn rebuild_grants(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.grants.clear();
        self.grant_subs.clear();
        if self.profiles.is_empty() {
            return;
        }
        let mut buckets: HashMap<_, Vec<_>> = self
            .groups
            .iter()
            .map(|group| (Some(group.id), Vec::new()))
            .collect();
        for profile in &self.profiles {
            let key = profile.group.filter(|id| buckets.contains_key(&Some(*id)));
            buckets.entry(key).or_default().push(profile);
        }
        let sections = std::iter::once((None, label(Message::UiNoGroup {}))).chain(
            self.groups
                .iter()
                .map(|group| (Some(group.id), group.name.as_str())),
        );
        for (id, name) in sections {
            let Some(members) = buckets.remove(&id) else {
                continue;
            };
            let ids: Vec<_> = members.iter().map(|profile| profile.id).collect();
            let batch = cx.new(|cx| {
                let control = Dropdown::new(window, cx)
                    .items(access_items())
                    .button_label(Message::WinMcpSetGroup {});
                if ids.is_empty() {
                    control.disabled()
                } else {
                    control
                }
            });
            self.grant_subs
                .push(cx.subscribe(&batch, move |this, sender, event, cx| {
                    if let DropdownEvent::Change(i) = event {
                        this.apply_grants(&ids, MCP_ACCESS_LEVELS[*i], cx);
                        sender.update(cx, |d, cx| d.set_selected(None, cx));
                    }
                }));
            let rows = members
                .into_iter()
                .map(|profile| {
                    let id = profile.id;
                    let level = self.levels.get(&id).copied().unwrap_or_default();
                    let control = cx.new(|cx| {
                        Dropdown::new(window, cx)
                            .items(access_items())
                            .selected(level as usize)
                    });
                    self.grant_subs
                        .push(cx.subscribe(&control, move |this, _, event, cx| {
                            if let DropdownEvent::Change(i) = event {
                                this.apply_grants(&[id], MCP_ACCESS_LEVELS[*i], cx);
                            }
                        }));
                    (id, profile.name.clone(), control)
                })
                .collect();
            self.grants.push(GrantGroup {
                name: name.to_owned(),
                batch,
                rows,
            });
        }
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
                    self.audit_records = records;
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
            Ok(()) => self.load_audit(true, cx),
            Err(error) => {
                self.audit_error = Some(operator_error(error, Message::WinMcpAuditClearFailed {}));
                cx.notify();
            }
        }
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

    fn grant_rows(&self, th: &Th) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        if self.sensitive_notice {
            rows.push(caption(th, label(Message::WinMcpSensitiveNotice {})));
        }
        if let Some(error) = &self.grants_error {
            rows.push(error_label(th, error));
        }
        if self.grants.is_empty() {
            rows.push(caption(th, label(Message::WinMcpNoSessions {})));
        }
        for group in &self.grants {
            rows.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(grant_row(th, &group.name, &group.batch, true))
                    .children(
                        group
                            .rows
                            .iter()
                            .map(|(_, name, dropdown)| grant_row(th, name, dropdown, false)),
                    )
                    .into_any_element(),
            );
        }
        rows
    }

    fn audit_rows(&self, th: &Th, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = vec![
            div()
                .flex()
                .gap(px(10.0))
                .child(row(
                    th,
                    label(Message::WinMcpClient {}),
                    self.audit_client.clone(),
                ))
                .child(row(
                    th,
                    label(Message::UiSession {}),
                    self.audit_session.clone(),
                ))
                .into_any_element(),
            check_row(
                th,
                render_message(&Message::WinMcpErrorsOnly {}),
                checkbox(
                    th,
                    cx,
                    "mcp-audit-errors",
                    self.audit_filter.errors_only,
                    |this, on, cx| {
                        this.audit_filter.errors_only = on;
                        this.load_audit(true, cx);
                    },
                ),
            ),
        ];
        if let Some(error) = &self.audit_error {
            rows.push(error_label(th, error));
        } else if self.audit_records.is_empty() {
            rows.push(caption(th, label(Message::WinMcpNoAudit {})));
        } else {
            let names: HashMap<_, _> = self
                .profiles
                .iter()
                .map(|p| (p.id, p.name.as_str()))
                .collect();
            let offset = tz::local_offset_minutes();
            rows.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .children(
                        self.audit_records
                            .iter()
                            .map(|record| self.audit_entry(th, record, &names, offset)),
                    )
                    .into_any_element(),
            );
        }
        rows.push(
            div()
                .flex()
                .gap(px(8.0))
                .when(self.audit_has_more, |d| {
                    d.child(button(
                        th,
                        cx,
                        "mcp-audit-more",
                        Message::WinMcpAuditMore {},
                        false,
                        true,
                        |this, cx| this.load_audit(false, cx),
                    ))
                })
                .child(button(
                    th,
                    cx,
                    "mcp-audit-clear",
                    Message::WinMcpAuditClear {},
                    false,
                    true,
                    Self::clear_audit,
                ))
                .into_any_element(),
        );
        rows
    }

    fn audit_entry(
        &self,
        th: &Th,
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
                Some(_) if !self.levels.contains_key(&id) => {
                    label(Message::WinMcpAuditUnauthorized {})
                }
                Some(name) => name,
            };
            parts.push(name.to_owned());
        }
        if let Some(code) = &entry.error_code {
            parts.push(code.clone());
        }
        div()
            .flex()
            .flex_col()
            .child(caption(th, &parts.join(" · ")))
            .when(!entry.args.is_empty(), |d| {
                let full: SharedString = entry.args.clone().into();
                let th = Th(th.0);
                d.child(
                    div()
                        .id(("mcp-audit-args", record.id as usize))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(number("font", "ui.caption")))
                        .text_color(th.c("text.tertiary"))
                        .child(entry.args.split_whitespace().collect::<Vec<_>>().join(" "))
                        .tooltip(move |_, cx| {
                            cx.new(|_| ArgsTooltip {
                                text: full.clone(),
                                th: Th(th.0),
                            })
                            .into()
                        }),
                )
            })
            .into_any_element()
    }
}

impl Render for McpPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.grants_dirty || self.language != lang() {
            self.sync_grants(window, cx);
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
        let (status, reason) = Self::status_text(&status);
        let mut service = vec![
            div().child(render_message(&status)).into_any_element(),
            row(
                &th,
                label(Message::WinMcpHelperPath {}),
                caption(&th, &helper),
            ),
        ];
        if let Some(reason) = reason {
            service.push(caption(&th, label(reason)));
        }
        if let Some(error) = &self.service_error {
            service.push(error_label(&th, error));
        }
        let has_command = !self.command.read(cx).text().is_empty();
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
                        .child(button(
                            &th,
                            cx,
                            "mcp-disconnect",
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
            .child(card(
                &th,
                label(Message::WinMcpPage {}),
                "plug",
                vec![
                    check_row(
                        &th,
                        render_message(&Message::WinMcpEnable {}),
                        checkbox(&th, cx, "mcp-enable", enabled, Self::toggle_service),
                    ),
                    caption(&th, label(Message::WinMcpEnableHint {})),
                ],
            ))
            .child(card(
                &th,
                label(Message::WinMcpService {}),
                "server",
                service,
            ))
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
                        label(Message::WinMcpPowerShellCommand {}),
                        self.command.clone(),
                    ),
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(button(
                            &th,
                            cx,
                            "mcp-copy-command",
                            Message::UiCopyCommand {},
                            true,
                            has_command,
                            |this, cx| {
                                let command = this.command.read(cx).text().to_owned();
                                cx.write_to_clipboard(ClipboardItem::new_string(command));
                            },
                        ))
                        .child(button(
                            &th,
                            cx,
                            "mcp-regenerate",
                            Message::WinMcpRegenerate {},
                            false,
                            has_command,
                            Self::regenerate,
                        ))
                        .into_any_element(),
                    caption(&th, label(Message::WinMcpTokenHistory {})),
                ],
            ))
            .child(card(
                &th,
                label(Message::WinMcpConnectedClients {}),
                "link",
                client_rows,
            ))
            .child(card(
                &th,
                label(Message::WinMcpSessionAccess {}),
                "tag",
                self.grant_rows(&th),
            ))
            .child(card(
                &th,
                label(Message::WinMcpAudit {}),
                "list",
                self.audit_rows(&th, cx),
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

fn grant_row(th: &Th, title: &str, selector: &Entity<Dropdown>, heading: bool) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .when(heading, |d| d.font_weight(FontWeight::SEMIBOLD))
                .when(!heading, |d| {
                    d.pl(px(12.0)).text_color(th.c("text.secondary"))
                })
                .child(title.to_owned()),
        )
        .child(boxed(selector.clone(), 160.0))
        .into_any_element()
}

fn button(
    th: &Th,
    cx: &mut Context<McpPage>,
    id: &'static str,
    title: Message,
    primary: bool,
    enabled: bool,
    run: impl Fn(&mut McpPage, &mut Context<McpPage>) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .flex_none()
        .h(px(number("spacing", "field.height") - 4.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .rounded(px(number("radius", "md")))
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
        .when(enabled, |el| {
            el.cursor_pointer()
                .hover(|s| s.opacity(0.85))
                .on_click(cx.listener(move |this, _, _, cx| run(this, cx)))
        })
        .when(!enabled, |el| el.opacity(0.45))
        .child(render_message(&title))
        .into_any_element()
}

struct ArgsTooltip {
    text: SharedString,
    th: Th,
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
