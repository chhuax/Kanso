//! 全宽「分组分片 + 自适应多列卡片」会话库渲染。
//!
//! 无内层左分组栏：左侧永远留给活跃标签栏（或顶部标签条），本视图直接在主工作区内
//! 以顶部轻筛选胶囊 + 可折叠分组分片 + 宽屏自适应多列卡片（单卡 ~260px，1–5 列）呈现。

use std::collections::HashSet;

use gpui::{
    ClickEvent, Context, FontWeight, Hsla, MouseButton, MouseDownEvent, Render, Window, div,
    prelude::*, px, rgb,
};
use kanso_gpui::app::launcher as logic;
use kanso_gpui::app::sessions::profile_matches;
use kanso_gpui::app::tab::TabPresentation;
use kanso_gpui::widgets::style::number;
use kanso_types::mcp::McpAccess;
use kanso_types::message::Message;
use kanso_types::profile::{Group, Profile, ProfileKind};
use kanso_types::{GroupId, ProfileId};

use super::launcher::{LauncherEvent, LauncherView, LibraryFilter, MenuTarget};
use super::shell::Theme;
use super::sidebar::SESSION_COLORS;
use super::widgets::icons::icon;

/// 拖拽会话卡片到分组时的载荷。
#[derive(Clone)]
struct ProfileDrag {
    id: ProfileId,
    name: String,
    color: Hsla,
}

/// 拖拽时跟随鼠标的轻量卡片预览。
struct DragPreview {
    name: String,
    color: Hsla,
    th: Theme,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let th = self.th;
        div()
            .h(px(34.0))
            .px(px(12.0))
            .rounded(px(7.0))
            .bg(th.c("bg.popover"))
            .border_1()
            .border_color(th.c("accent"))
            .flex()
            .items_center()
            .gap(px(8.0))
            .text_size(px(number("font", "ui.body")))
            .text_color(th.c("text.primary"))
            .child(div().w(px(3.0)).h(px(16.0)).rounded(px(2.0)).bg(self.color))
            .child(self.name.clone())
    }
}

/// 宽屏下按可用宽度计算 `(列数, 单张卡片精确宽)`：
/// 每张卡片保持约 260–320px，且同行/末行所有卡片像素级严格等宽。
fn card_grid_metrics(content_width: f32) -> (usize, f32) {
    let gap = 10.0;
    // 扣除滚动区左右 24px*2 与分片内边距 6px*2。
    let avail = (content_width - 60.0).max(240.0);
    let cols = ((avail + gap) / 270.0).floor().clamp(1.0, 5.0) as usize;
    let card_w = ((avail - gap * (cols.saturating_sub(1) as f32)) / (cols as f32)).floor();
    (cols, card_w.max(200.0))
}

/// 单个可折叠分组分片。
struct SectionSlice {
    group: Option<Group>,
    title: String,
    collapsed: bool,
    profiles: Vec<Profile>,
}

impl LauncherView {
    pub(super) fn render_library(
        &self,
        th: Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let all = self.model.all_profiles();
        let groups = self.model.groups();
        let group_ids: HashSet<GroupId> = groups.iter().map(|g| g.id).collect();
        let needle = self.model.query().trim();
        let searching = !needle.is_empty();
        let selected = self.model.selected();
        let (cols, card_w) = card_grid_metrics(self.content_width);

        let ungrouped_total = all
            .iter()
            .filter(|p| p.group.is_none_or(|g| !group_ids.contains(&g)))
            .count();

        // 顶部单行工具条：左侧可横向滚动的分组过滤胶囊，右侧搜索框 + 「+ 新建会话」+「…」更多菜单。
        let mut filter_pills = div()
            .id("lib-filter-pills")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_x_scroll()
            .track_scroll(&self.pills_scroll)
            .flex()
            .items_center()
            .gap(px(4.0));

        let make_pill = |id: gpui::ElementId,
                         title: String,
                         count: usize,
                         active: bool,
                         target: LibraryFilter| {
            div()
                .id(id)
                .h(px(26.0))
                .px(px(10.0))
                .rounded(px(number("radius", "md")))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(5.0))
                .cursor_pointer()
                .text_size(px(number("font", "ui.caption")))
                .text_color(if active {
                    th.c("text.primary")
                } else {
                    th.c("text.secondary")
                })
                .when(active, |d| {
                    d.bg(th.c("bg.tabSelected")).font_weight(FontWeight::MEDIUM)
                })
                .hover(|s| s.bg(th.c("bg.rowHover")).text_color(th.c("text.primary")))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.set_library_filter(target, cx);
                    }),
                )
                .child(title)
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(th.c("text.tertiary"))
                        .child(count.to_string()),
                )
        };

        filter_pills = filter_pills.child(make_pill(
            "lib-pill-all".into(),
            logic::render_message(&Message::UiAllSessions {}),
            all.len(),
            self.library_filter == LibraryFilter::All,
            LibraryFilter::All,
        ));

        for (gi, g) in groups.iter().enumerate() {
            let gid = g.id;
            let g_menu = g.clone();
            let count = all.iter().filter(|p| p.group == Some(gid)).count();
            filter_pills = filter_pills.child(
                make_pill(
                    ("lib-pill-group", gi).into(),
                    g.name.clone(),
                    count,
                    self.library_filter == LibraryFilter::Group(gid),
                    LibraryFilter::Group(gid),
                )
                .drag_over::<ProfileDrag>(move |s, _, _, _| {
                    s.bg(th.c("bg.segmentSelected"))
                        .border_1()
                        .border_color(th.c("border.segmentSelected"))
                })
                .on_drop(cx.listener(move |this, d: &ProfileDrag, window, cx| {
                    this.drop_profile_to_group(d.id, Some(gid), window, cx);
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.open_group_menu(g_menu.clone(), ev.position, window, cx);
                    }),
                ),
            );
        }

        if (!groups.is_empty() && ungrouped_total > 0)
            || self.library_filter == LibraryFilter::Ungrouped
        {
            filter_pills = filter_pills.child(
                make_pill(
                    "lib-pill-ungrouped".into(),
                    logic::render_message(&Message::UiUngrouped {}),
                    ungrouped_total,
                    self.library_filter == LibraryFilter::Ungrouped,
                    LibraryFilter::Ungrouped,
                )
                .drag_over::<ProfileDrag>(move |s, _, _, _| {
                    s.bg(th.c("bg.segmentSelected"))
                        .border_1()
                        .border_color(th.c("border.segmentSelected"))
                })
                .on_drop(cx.listener(|this, d: &ProfileDrag, window, cx| {
                    this.drop_profile_to_group(d.id, None, window, cx);
                })),
            );
        }

        let header = div()
            .h(px(46.0))
            .flex_none()
            .px(px(24.0))
            .border_b_1()
            .border_color(th.divider())
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .child(filter_pills)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().w(px(192.0)).child(self.search.clone()))
                    .child(
                        div()
                            .id("lib-new-session")
                            .h(px(30.0))
                            .px(px(12.0))
                            .rounded(px(number("radius", "md")))
                            .bg(th.c("accent"))
                            .text_color(th.c("text.onAccent"))
                            .text_size(px(number("font", "ui.body")))
                            .font_weight(FontWeight::MEDIUM)
                            .flex()
                            .items_center()
                            .gap(px(5.0))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_, _: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(LauncherEvent::NewSession);
                                }),
                            )
                            .child(icon("plus", 14.0, th.c("text.onAccent")))
                            .child(logic::render_message(&Message::UiNewSession {})),
                    )
                    .child(
                        div()
                            .id("lib-more-actions")
                            .size(px(30.0))
                            .rounded(px(number("radius", "md")))
                            .bg(th.c("bg.card"))
                            .border_1()
                            .border_color(th.c("border.card"))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|s| {
                                s.bg(th.c("bg.rowHover"))
                                    .border_color(th.c("border.strong"))
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    let below =
                                        gpui::point(ev.position.x, ev.position.y + px(18.0));
                                    this.open_saved_menu(below, window, cx);
                                }),
                            )
                            .child(icon("ellipsis", 14.0, th.c("text.secondary"))),
                    ),
            );

        // 构造分组分片。
        let mut slices: Vec<SectionSlice> = Vec::new();
        let want_group = |gid: GroupId| match self.library_filter {
            LibraryFilter::All => true,
            LibraryFilter::Group(selected_gid) => selected_gid == gid,
            LibraryFilter::Ungrouped => false,
        };
        let want_ungrouped = matches!(
            self.library_filter,
            LibraryFilter::All | LibraryFilter::Ungrouped
        );

        for g in groups {
            if !want_group(g.id) {
                continue;
            }
            let members: Vec<Profile> = all
                .iter()
                .filter(|p| p.group == Some(g.id) && profile_matches(p, needle))
                .cloned()
                .collect();
            if members.is_empty() && searching {
                continue;
            }
            let collapsed = !searching
                && self.library_filter == LibraryFilter::All
                && self.model.collapsed_groups().contains(&g.id);
            slices.push(SectionSlice {
                group: Some(g.clone()),
                title: g.name.clone(),
                collapsed,
                profiles: members,
            });
        }

        if want_ungrouped {
            let members: Vec<Profile> = all
                .iter()
                .filter(|p| {
                    p.group.is_none_or(|g| !group_ids.contains(&g)) && profile_matches(p, needle)
                })
                .cloned()
                .collect();
            if !members.is_empty() || self.library_filter == LibraryFilter::Ungrouped {
                slices.push(SectionSlice {
                    group: None,
                    title: logic::render_message(&Message::UiUngrouped {}),
                    collapsed: self.ungrouped_collapsed
                        && !searching
                        && self.library_filter == LibraryFilter::All,
                    profiles: members,
                });
            }
        }

        let mut body = div()
            .id("library-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px(px(24.0))
            .py(px(14.0))
            .flex()
            .flex_col()
            .gap(px(4.0));

        let total_visible: usize = slices.iter().map(|s| s.profiles.len()).sum();
        if slices.is_empty() || (total_visible == 0 && (searching || !self.model.has_any_profile()))
        {
            let empty_text = if searching {
                logic::render_message(&Message::UiNoMatchingSessions {})
            } else {
                logic::render_message(&Message::UiNoSessions {})
            };
            body = body.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .py(px(48.0))
                    .child(
                        div()
                            .w(px(380.0))
                            .p(px(24.0))
                            .rounded(px(number("radius", "lg")))
                            .bg(th.c("bg.card"))
                            .border_1()
                            .border_color(th.c("border.card"))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(12.0))
                            .child(icon("server", 24.0, th.c("text.tertiary")))
                            .child(
                                div()
                                    .text_size(px(number("font", "ui.body")))
                                    .text_color(th.c("text.secondary"))
                                    .child(empty_text),
                            )
                            .when(!searching && !self.model.has_any_profile(), |box_div| {
                                box_div.child(
                                    div()
                                        .mt(px(4.0))
                                        .flex()
                                        .items_center()
                                        .gap(px(8.0))
                                        .child(
                                            div()
                                                .id("lib-empty-new-session")
                                                .h(px(30.0))
                                                .px(px(12.0))
                                                .rounded(px(number("radius", "md")))
                                                .bg(th.c("accent"))
                                                .text_size(px(number("font", "ui.caption")))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(th.c("text.onAccent"))
                                                .flex()
                                                .items_center()
                                                .gap(px(5.0))
                                                .cursor_pointer()
                                                .hover(|s| s.opacity(0.9))
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(|_, _: &MouseDownEvent, _, cx| {
                                                        cx.stop_propagation();
                                                        cx.emit(LauncherEvent::NewSession);
                                                    }),
                                                )
                                                .child(icon("plus", 13.0, th.c("text.onAccent")))
                                                .child(logic::render_message(
                                                    &Message::UiNewSession {},
                                                )),
                                        )
                                        .child(
                                            div()
                                                .id("lib-empty-import-ssh")
                                                .h(px(30.0))
                                                .px(px(12.0))
                                                .rounded(px(number("radius", "md")))
                                                .bg(th.c("bg.buttonSecondary"))
                                                .border_1()
                                                .border_color(th.c("border.strong"))
                                                .text_size(px(number("font", "ui.caption")))
                                                .text_color(th.c("text.primary"))
                                                .flex()
                                                .items_center()
                                                .gap(px(6.0))
                                                .cursor_pointer()
                                                .hover(|s| s.bg(th.c("bg.rowHover")))
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(|_, _: &MouseDownEvent, _, cx| {
                                                        cx.stop_propagation();
                                                        cx.emit(LauncherEvent::ImportSshConfig);
                                                    }),
                                                )
                                                .child(icon(
                                                    "download",
                                                    13.0,
                                                    th.c("text.secondary"),
                                                ))
                                                .child(logic::render_message(
                                                    &Message::UiImportOpensshConfig2 {},
                                                )),
                                        ),
                                )
                            }),
                    ),
            );
        } else {
            let show_section_headers =
                !(slices.len() == 1 && slices[0].group.is_none() && groups.is_empty());
            let mut card_seq = 0usize;
            for (si, slice) in slices.into_iter().enumerate() {
                let target_group = slice.group.as_ref().map(|g| g.id);
                let mut section = div()
                    .id(("lib-sec-drop", si))
                    .px(px(6.0))
                    .py(px(if slice.collapsed { 2.0 } else { 6.0 }))
                    .when(!slice.collapsed, |d| d.mb(px(10.0)))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .drag_over::<ProfileDrag>(move |s, _, _, _| {
                        s.bg(th.c("bg.segmentSelected"))
                            .border_color(th.c("border.segmentSelected"))
                    })
                    .on_drop(cx.listener(move |this, d: &ProfileDrag, window, cx| {
                        this.drop_profile_to_group(d.id, target_group, window, cx);
                    }));
                if show_section_headers {
                    let group_opt = slice.group.clone();
                    let group_id = slice.group.as_ref().map(|g| g.id);
                    let collapsed = slice.collapsed;
                    let can_toggle = !searching && self.library_filter == LibraryFilter::All;

                    let mut header_row = div()
                        .id(("lib-sec-hdr", si))
                        .h(px(30.0))
                        .px(px(6.0))
                        .rounded(px(6.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(px(number("font", "ui.caption")))
                        .text_color(th.c("text.secondary"))
                        .when_some(group_id, |d, gid| {
                            d.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                this.set_hovered_group_header(gid, *hovered, cx);
                            }))
                        })
                        .when(can_toggle, |d| {
                            d.cursor_pointer()
                                .hover(|s| s.bg(th.c("bg.rowHover")))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                        cx.stop_propagation();
                                        match group_id {
                                            Some(gid) => this.toggle_library_group(gid, cx),
                                            None => this.toggle_library_ungrouped(cx),
                                        }
                                    }),
                                )
                        });

                    if let Some(g_right) = group_opt.clone() {
                        header_row = header_row.on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.open_group_menu(g_right.clone(), ev.position, window, cx);
                            }),
                        );
                    }

                    let title_left = div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .when(can_toggle, |d| {
                            d.child(icon(
                                if collapsed {
                                    "chevron-right"
                                } else {
                                    "chevron-down"
                                },
                                12.0,
                                th.c("text.tertiary"),
                            ))
                        })
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if collapsed {
                                    th.c("text.secondary")
                                } else {
                                    th.c("text.primary")
                                })
                                .child(slice.title),
                        )
                        .child(
                            div()
                                .px(px(6.0))
                                .py(px(1.0))
                                .rounded_full()
                                .bg(th.c("bg.panel"))
                                .text_size(px(10.0))
                                .text_color(th.c("text.tertiary"))
                                .child(slice.profiles.len().to_string()),
                        );

                    header_row = header_row.child(title_left);
                    if let Some(g_more) = group_opt
                        && self.hovered_group_header == Some(g_more.id)
                    {
                        header_row = header_row.child(
                            self.small_button(("lib-sec-more", si), th).on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.open_group_menu(g_more.clone(), ev.position, window, cx);
                                }),
                            ),
                        );
                    }
                    header_row = header_row
                        .child(div().flex_1().h(px(1.0)).ml(px(6.0)).bg(th.divider_soft()));
                    section = section.child(header_row);
                }

                if !slice.collapsed {
                    if slice.profiles.is_empty() {
                        section = section.child(
                            div()
                                .py(px(10.0))
                                .text_size(px(number("font", "ui.caption")))
                                .text_color(th.c("text.tertiary"))
                                .child(logic::render_message(&Message::UiEmptyGroup {})),
                        );
                    } else {
                        for chunk in slice.profiles.chunks(cols) {
                            let mut row = div().flex().gap(px(10.0));
                            for p in chunk {
                                row = row.child(
                                    self.render_library_card(
                                        card_seq,
                                        p,
                                        self.mcp_levels
                                            .get(&p.id)
                                            .copied()
                                            .unwrap_or(McpAccess::Off),
                                        card_w,
                                        selected == Some(p.id),
                                        th,
                                        cx,
                                    ),
                                );
                                card_seq += 1;
                            }
                            section = section.child(row);
                        }
                    }
                }
                body = body.child(section);
            }
        }

        div()
            .id("sessions-library")
            .key_context("Launcher")
            .track_focus(&self.root)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .bg(th.c("bg.window"))
            .flex()
            .flex_col()
            .text_size(px(number("font", "ui.body")))
            .text_color(th.c("text.primary"))
            .child(header)
            .child(body)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_library_card(
        &self,
        index: usize,
        p: &Profile,
        mcp_level: McpAccess,
        card_w: f32,
        selected: bool,
        th: Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let pid = p.id;
        let show_actions = selected || self.hovered_profile == Some(pid);
        let accent_bar: Hsla = match p.color {
            Some(c) => rgb((u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)).into(),
            None => {
                rgb(SESSION_COLORS[TabPresentation::profile_color_index(pid, SESSION_COLORS.len())])
                    .into()
            }
        };
        let (kind_icon, kind_badge) = match p.kind {
            ProfileKind::Local(_) => (
                "square-terminal",
                logic::render_message(&Message::UiLocal {}),
            ),
            ProfileKind::Ssh(_) => ("server", "SSH".to_owned()),
            ProfileKind::Sftp(_) => ("folder", "SFTP".to_owned()),
        };

        let drag_payload = ProfileDrag {
            id: pid,
            name: p.name.clone(),
            color: accent_bar,
        };

        div()
            .id(("lib-card", index))
            .w(px(card_w))
            .flex_none()
            .h(px(66.0))
            .px(px(12.0))
            .rounded(px(8.0))
            .bg(if selected {
                th.c("bg.segmentSelected")
            } else {
                th.c("bg.card")
            })
            .border_1()
            .border_color(if selected {
                th.c("border.segmentSelected")
            } else {
                th.c("border.card")
            })
            .hover(|s| {
                s.bg(if selected {
                    th.c("bg.segmentSelected")
                } else {
                    th.c("bg.rowHover")
                })
                .border_color(if selected {
                    th.c("border.segmentSelected")
                } else {
                    th.c("border.strong")
                })
            })
            .flex()
            .items_center()
            .gap(px(10.0))
            .cursor_pointer()
            .on_drag(drag_payload, move |d, _off, _, cx| {
                cx.new(|_| DragPreview {
                    name: d.name.clone(),
                    color: d.color,
                    th,
                })
            })
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.set_hovered_profile(pid, *hovered, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.select_profile(pid, cx);
                }),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.select_and_connect(pid, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_profile_menu(pid, ev.position, window, cx);
                }),
            )
            // 左侧会话色条
            .child(
                div()
                    .w(px(3.0))
                    .h(px(30.0))
                    .flex_none()
                    .rounded(px(2.0))
                    .bg(accent_bar),
            )
            // 协议图标小块（无描边，仅靠微差底色）
            .child(
                div()
                    .size(px(30.0))
                    .flex_none()
                    .rounded(px(6.0))
                    .bg(th.c("bg.panel"))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(kind_icon, 14.0, th.c("text.secondary"))),
            )
            // 标题 + 副标题
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .min_w_0()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_shrink()
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(p.name.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .px(px(5.0))
                                    .py(px(1.0))
                                    .rounded(px(4.0))
                                    .bg(th.c("bg.panel"))
                                    .text_size(px(10.0))
                                    .text_color(th.c("text.tertiary"))
                                    .child(kind_badge),
                            )
                            .when(mcp_level != McpAccess::Off, |d| {
                                let level_text = match mcp_level {
                                    McpAccess::ReadOnly => {
                                        logic::render_message(&Message::WinMcpAccessReadOnly {})
                                    }
                                    McpAccess::Full => {
                                        logic::render_message(&Message::WinMcpAccessFull {})
                                    }
                                    McpAccess::Off => String::new(),
                                };
                                d.child(
                                    div()
                                        .flex_none()
                                        .px(px(5.0))
                                        .py(px(1.0))
                                        .rounded(px(4.0))
                                        .bg(th.c("bg.segmentSelected"))
                                        .text_size(px(10.0))
                                        .text_color(th.c("accent"))
                                        .child(format!("MCP · {level_text}")),
                                )
                            }),
                    )
                    .child(
                        div()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_size(px(number("font", "ui.caption")))
                            .text_color(th.c("text.secondary"))
                            .child(logic::subtitle_text(p)),
                    ),
            )
            // 悬停 / 选中时出现一键编辑按钮与「…」更多菜单（菜单含移到分组、MCP 访问、删除）
            .when(show_actions, |card| {
                card.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(2.0))
                        .child(
                            div()
                                .id(("lib-edit", index))
                                .size(px(24.0))
                                .rounded(px(number("radius", "sm")))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|s| s.bg(th.c("bg.tabHover")))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |_, _: &MouseDownEvent, _, cx| {
                                        cx.stop_propagation();
                                        cx.emit(LauncherEvent::Edit(pid));
                                    }),
                                )
                                .child(icon("pencil", 13.0, th.c("text.secondary"))),
                        )
                        .child(self.small_button(("lib-more", index), th).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.open_menu_target(
                                    MenuTarget::Profile(pid),
                                    ev.position,
                                    window,
                                    cx,
                                );
                            }),
                        )),
                )
            })
            .into_any_element()
    }
}
