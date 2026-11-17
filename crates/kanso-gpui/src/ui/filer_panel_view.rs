//! 文件面板的绘制（`FilerPanel` 的 `Render`）：头部身份、工具条、路径栏与面包屑、列表（虚拟滚动）、
//! 错误条、传输进度条、底部统计与拖放提示。样式取自设计 token，与 mac 版 `FilerPanelView` 对应。

use gpui::{
    AnyElement, Context, ExternalPaths, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, Render,
    SharedString, Stateful, Window, div, prelude::*, px, relative, uniform_list,
};
use kanso_gpui::app::filer_state::{
    Phase, date_text, datetime_text, row_tooltip, size_text, stats_text,
};
use kanso_gpui::app::filer_text::{t, tf};
use kanso_gpui::app::fspath::{self, is_hidden_name};
use kanso_gpui::app::transfer_queue::{Bar, BarFinish};
use kanso_gpui::widgets::style::number;

use super::display;
use super::filer_panel::{FilerEvent, FilerPanel, PanelMode, Source};
use super::shell::Theme;
use super::widgets::icons::icon;

/// 行高（逻辑像素）。
const ROW_H: f32 = 26.0;
/// 工具条图标按钮边长。
const BTN: f32 = 24.0;

/// 悬停提示。
struct Tip {
    text: SharedString,
    th: Theme,
}

impl Render for Tip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let th = self.th;
        div()
            .px(px(10.0))
            .py(px(6.0))
            .max_w(px(420.0))
            .rounded(px(number("radius", "md")))
            .bg(th.c("bg.popover"))
            .border_1()
            .border_color(th.c("border.strong"))
            .text_size(px(12.0))
            .text_color(th.c("text.primary"))
            .child(self.text.clone())
    }
}

fn with_tip<E: gpui::InteractiveElement + gpui::StatefulInteractiveElement>(
    el: E,
    text: String,
    th: Theme,
) -> E {
    el.tooltip(move |_window, cx| {
        let text: SharedString = text.clone().into();
        cx.new(|_| Tip { text, th }).into()
    })
}

/// 工具条上的图标按钮。`on_click` 由调用方挂；不可用时置灰且不响应。
fn icon_button(
    id: impl Into<gpui::ElementId>,
    name: &'static str,
    tip: String,
    enabled: bool,
    th: Theme,
) -> Stateful<gpui::Div> {
    let color = if enabled {
        th.c("text.secondary")
    } else {
        th.ca("text.tertiary", 0x80)
    };
    let b = div()
        .id(id)
        .flex_none()
        .size(px(BTN))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(number("radius", "sm")))
        .child(icon(name, 15.0, color));
    let b = if enabled {
        b.cursor_pointer().hover(move |s| s.bg(th.c("bg.rowHover")))
    } else {
        b
    };
    with_tip(b, tip, th)
}

impl FilerPanel {
    fn theme(window: &Window) -> Theme {
        Theme {
            appearance: display::window_appearance(window),
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.path_input_focused(window, cx) {
            return;
        }
        let ks = &ev.keystroke;
        let page = self.panes.get(self.shown).map_or(10, |p| {
            ((f32::from(p.scroll.0.borrow().base_handle.bounds().size.height) / ROW_H) as isize)
                .max(1)
        });
        if self.on_list_key(
            &ks.key,
            ks.modifiers.control,
            ks.modifiers.alt,
            ks.modifiers.shift,
            page,
            cx,
        ) {
            cx.stop_propagation();
        }
    }

    /// 一栏的主体：头部、工具条、路径栏、面包屑、列表头、列表、错误条、统计。
    fn render_pane(
        &mut self,
        idx: usize,
        th: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let dual = self.mode == PanelMode::Dual;
        let tz = self.tz_min;
        let now = self.now_ms();
        let is_focused_pane = dual && self.shown == idx;
        let (identity, style, source, phase, hide_hidden) = {
            let p = &self.panes[idx];
            (
                p.identity.clone(),
                p.style,
                p.source,
                p.browse.phase.clone(),
                p.model.hide_hidden(),
            )
        };
        let remote = source != Source::Local;
        let can_write = self.panes[idx].browse.can_write()
            && !fspath::is_virtual_root(style, &self.panes[idx].browse.path);
        let has_sel = !self.panes[idx].sel.is_empty();
        let single_sel = self.panes[idx].sel.len() == 1;
        let busy = self.panes[idx].queue.is_busy();

        // ---- 头部：来源标识 ----
        let badge = match phase {
            Phase::Disconnected => Some((t("ui-filer-disconnected-badge"), th.c("status.error"))),
            Phase::Connecting => Some((t("ui-filer-connecting"), th.c("text.secondary"))),
            _ => None,
        };
        let header = div()
            .flex_none()
            .h(px(30.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(icon(
                if remote { "server" } else { "square-terminal" },
                14.0,
                th.c("text.secondary"),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_size(px(number("font", "ui.body")))
                    .text_color(th.c("text.primary"))
                    .child(identity),
            )
            .children(badge.map(|(text, color)| {
                div()
                    .flex_none()
                    .text_size(px(number("font", "ui.caption")))
                    .text_color(color)
                    .child(text)
            }));

        // ---- 工具条 ----
        let mut tools = div()
            .flex_none()
            .px(px(6.0))
            .pb(px(4.0))
            .flex()
            .items_center()
            .gap(px(2.0));
        let loc_id = |n: &'static str| (n, idx + 1);
        if dual {
            tools = tools
                .child(
                    icon_button(
                        loc_id("t-folder"),
                        "folder-plus",
                        t("ui-filer-new-folder"),
                        can_write,
                        th,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.shown = idx;
                        this.ask_name(idx, kanso_gpui::app::filer_ops::NameKind::NewFolder, cx)
                    })),
                )
                .child(
                    icon_button(
                        loc_id("t-rename"),
                        "pencil",
                        t("ui-filer-rename"),
                        can_write && single_sel,
                        th,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.shown = idx;
                        this.ask_rename(idx, cx)
                    })),
                )
                .child(
                    icon_button(
                        loc_id("t-delete"),
                        "trash-2",
                        t("ui-filer-delete"),
                        can_write && has_sel,
                        th,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.shown = idx;
                        this.ask_delete(idx, cx)
                    })),
                );
        } else {
            tools = tools
                .child(
                    icon_button(
                        loc_id("t-file"),
                        "file-plus",
                        t("ui-filer-new-file"),
                        can_write,
                        th,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ask_name(idx, kanso_gpui::app::filer_ops::NameKind::NewFile, cx)
                    })),
                )
                .child(
                    icon_button(
                        loc_id("t-folder"),
                        "folder-plus",
                        t("ui-filer-new-folder"),
                        can_write,
                        th,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ask_name(idx, kanso_gpui::app::filer_ops::NameKind::NewFolder, cx)
                    })),
                )
                .child(
                    icon_button(
                        loc_id("t-upload"),
                        "upload",
                        if remote {
                            t("ui-filer-tip-upload")
                        } else {
                            t("ui-filer-tip-upload-remote-only")
                        },
                        remote && can_write,
                        th,
                    )
                    .on_click(cx.listener(
                        move |this, ev: &gpui::ClickEvent, _, cx| {
                            let pos = ev.position();
                            this.open_upload_menu(idx, pos, cx)
                        },
                    )),
                )
                .child(
                    icon_button(
                        loc_id("t-download"),
                        "download",
                        if remote {
                            t("ui-filer-menu-download")
                        } else {
                            t("ui-filer-tip-upload-remote-only")
                        },
                        remote && has_sel && !busy,
                        th,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_and_download(idx, cx))),
                )
                .child(
                    icon_button(
                        loc_id("t-delete"),
                        "trash-2",
                        t("ui-filer-delete"),
                        can_write && has_sel,
                        th,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.ask_delete(idx, cx))),
                );
        }
        tools = tools
            .child(div().flex_1())
            .child(
                icon_button(
                    loc_id("t-hidden"),
                    "eye",
                    t("ui-filer-tip-hidden"),
                    true,
                    th,
                )
                .when(hide_hidden, |b| b.opacity(0.5))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_hidden(idx, cx))),
            )
            .child(
                icon_button(
                    loc_id("t-refresh"),
                    "refresh-cw",
                    t("ui-filer-refresh"),
                    true,
                    th,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.refresh(idx, cx))),
            );
        if !dual {
            tools = tools.child(
                icon_button(
                    loc_id("t-locate"),
                    "crosshair",
                    t("ui-filer-tip-locate"),
                    true,
                    th,
                )
                .on_click(cx.listener(|_, _, _, cx| cx.emit(FilerEvent::Locate))),
            );
        }

        // ---- 路径栏（可编辑）+ 上一级 / 家目录 ----
        let path_row = div()
            .flex_none()
            .px(px(6.0))
            .pb(px(4.0))
            .flex()
            .items_center()
            .gap(px(2.0))
            .child(
                icon_button(loc_id("p-up"), "arrow-up", t("ui-filer-tip-up"), true, th)
                    .on_click(cx.listener(move |this, _, _, cx| this.go_up(idx, cx))),
            )
            .child(
                icon_button(loc_id("p-home"), "house", t("ui-filer-tip-home"), true, th)
                    .on_click(cx.listener(move |this, _, _, cx| this.go_home(idx, cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(self.panes[idx].path_input.clone()),
            );

        // ---- 面包屑 ----
        let crumbs = fspath::breadcrumbs(style, &self.panes[idx].browse.path);
        let skip = crumbs.len().saturating_sub(5);
        let mut crumb_row = div()
            .flex_none()
            .h(px(20.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .overflow_hidden()
            .text_size(px(number("font", "ui.caption")))
            .text_color(th.c("text.secondary"));
        if skip > 0 {
            crumb_row = crumb_row.child("… ");
        }
        for (i, c) in crumbs.into_iter().enumerate().skip(skip) {
            let target = c.path.clone();
            if i > skip {
                crumb_row = crumb_row.child(icon("chevron-right", 11.0, th.c("text.tertiary")));
            }
            crumb_row = crumb_row.child(
                div()
                    .id(("crumb", idx * 100 + i))
                    .flex_none()
                    .px(px(3.0))
                    .rounded(px(3.0))
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .hover(move |s| s.bg(th.c("bg.rowHover")))
                    .child(c.label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.shown = idx;
                        this.navigate(idx, target.clone(), cx)
                    })),
            );
        }

        // ---- 列表头 ----
        let col_head = {
            let mut h = div()
                .flex_none()
                .h(px(22.0))
                .px(px(10.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .border_b_1()
                .border_color(th.c("border.subtle"))
                .text_size(px(number("font", "ui.caption")))
                .text_color(th.c("text.tertiary"))
                .child(div().w(px(16.0)).flex_none())
                .child(div().flex_1().child(t("ui-filer-col-name")));
            if dual {
                h = h
                    .child(
                        div()
                            .w(px(72.0))
                            .flex_none()
                            .text_right()
                            .child(t("ui-filer-col-size")),
                    )
                    .child(div().w(px(116.0)).flex_none().child(t("ui-filer-col-time")));
            } else {
                h = h.child(div().w(px(78.0)).flex_none().child(t("ui-filer-col-date")));
            }
            h
        };

        // ---- 列表 ----
        let visible = self.panes[idx].model.visible_len();
        let list = uniform_list(
            ("filer-list", idx),
            visible,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let th = this.cached_theme;
                let tz = this.tz_min;
                let dual = this.mode == PanelMode::Dual;
                let mut rows = Vec::with_capacity(range.len());
                for pos in range {
                    let Some((id, r)) = this.panes[idx].model.visible_at(pos) else {
                        continue;
                    };
                    let selected = this.panes[idx].sel.contains(id);
                    let r = r.clone();
                    let name_color = if r.is_symlink {
                        th.c("status.running")
                    } else if is_hidden_name(&r.name) {
                        th.c("text.secondary")
                    } else {
                        th.c("text.primary")
                    };
                    let (glyph, glyph_color) = if r.is_symlink {
                        ("link", th.c("status.running"))
                    } else if r.is_dir {
                        ("folder", th.c("accent"))
                    } else {
                        ("file", th.c("text.secondary"))
                    };
                    let size = size_text(&r);
                    let date = date_text(r.mtime_ms, tz);
                    let datetime = datetime_text(r.mtime_ms, tz);
                    let tip = row_tooltip(&r);
                    let mut row = div()
                        .id(("filer-row", idx * 10_000_000 + pos))
                        .w_full()
                        .h(px(ROW_H))
                        .px(px(10.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .text_size(px(number("font", "ui.body")))
                        .when(selected, |d| d.bg(th.ca("accent", 0x33)))
                        .when(!selected, |d| d.hover(move |s| s.bg(th.c("bg.rowHover"))))
                        .child(
                            div()
                                .w(px(16.0))
                                .flex_none()
                                .child(icon(glyph, 15.0, glyph_color)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_color(name_color)
                                .child(r.name.clone()),
                        );
                    if dual {
                        row = row
                            .child(
                                div()
                                    .w(px(72.0))
                                    .flex_none()
                                    .text_right()
                                    .text_size(px(number("font", "ui.caption")))
                                    .text_color(th.c("text.secondary"))
                                    .child(size),
                            )
                            .child(
                                div()
                                    .w(px(116.0))
                                    .flex_none()
                                    .text_size(px(number("font", "ui.caption")))
                                    .text_color(th.c("text.secondary"))
                                    .child(datetime),
                            );
                    } else {
                        row = row.child(
                            div()
                                .w(px(78.0))
                                .flex_none()
                                .text_size(px(number("font", "ui.caption")))
                                .text_color(th.c("text.secondary"))
                                .child(date),
                        );
                    }
                    let row = with_tip(row, tip, th)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                                if this.mode == PanelMode::Dual {
                                    this.shown = idx;
                                }
                                window.focus(&this.focus);
                                let m = ev.modifiers;
                                let pane = &mut this.panes[idx];
                                pane.sel.click(pos, &pane.model, m.control, m.shift);
                                if ev.click_count >= 2 && !m.control && !m.shift {
                                    this.open_row(idx, id, cx);
                                }
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                                window.focus(&this.focus);
                                this.open_row_menu(idx, id, ev.position, cx);
                                cx.stop_propagation();
                            }),
                        );
                    rows.push(row);
                }
                rows
            }),
        )
        .track_scroll(self.panes[idx].scroll.clone())
        .flex_1()
        .min_h(px(0.0));

        // 空目录 / 读取中 / 失败等占位说明（盖在列表上，不挡住右键）。
        let (placeholder, placeholder_color) = {
            let p = &self.panes[idx];
            if visible > 0 {
                (None, th.c("text.secondary"))
            } else {
                match (&p.browse.phase, &p.browse.error) {
                    (Phase::Failed, Some(e)) => (
                        Some(tf(
                            "ui-filer-open-failed",
                            &[("target", &e.target), ("reason", &e.text)],
                        )),
                        th.c("status.error"),
                    ),
                    (Phase::Ready, _) => (Some(t("ui-filer-folder-empty")), th.c("text.tertiary")),
                    (Phase::Disconnected, _) => {
                        (Some(t("ui-filer-disconnected-badge")), th.c("status.error"))
                    }
                    _ => (None, th.c("text.secondary")),
                }
            }
        };

        // 读取失败但旧列表仍在：错误条，并说明下面仍是原目录的内容。
        let stale_banner = {
            let p = &self.panes[idx];
            match (&p.browse.error, visible > 0) {
                (Some(e), true) => Some(format!(
                    "{} {}",
                    tf(
                        "ui-filer-open-failed",
                        &[("target", &e.target), ("reason", &e.text)]
                    ),
                    tf("ui-filer-still-showing", &[("path", &p.browse.path)])
                )),
                _ => None,
            }
        };
        let show_home_button = matches!(self.panes[idx].browse.phase, Phase::Failed)
            && self.panes[idx].browse.error.is_some();

        let list_area = div()
            .id(("filer-list-area", idx))
            .relative()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    // 点空白处：取消选中并把键盘焦点交给列表。
                    if this.mode == PanelMode::Dual {
                        this.shown = idx;
                    }
                    window.focus(&this.focus);
                    this.panes[idx].sel.clear();
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    this.open_blank_menu(idx, ev.position, cx);
                }),
            )
            .child(list)
            .children(placeholder.map(|text| {
                div()
                    .absolute()
                    .top(px(8.0))
                    .left_0()
                    .w_full()
                    .px(px(14.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .text_size(px(number("font", "ui.body")))
                    .text_color(placeholder_color)
                    .child(text)
                    .children(show_home_button.then(|| {
                        div()
                            .id(("go-home", idx))
                            .self_start()
                            .px(px(10.0))
                            .py(px(4.0))
                            .rounded(px(number("radius", "md")))
                            .border_1()
                            .border_color(th.c("border.strong"))
                            .bg(th.c("bg.buttonSecondary"))
                            .text_color(th.c("text.primary"))
                            .cursor_pointer()
                            .child(t("ui-filer-go-home"))
                            .on_click(cx.listener(move |this, _, _, cx| this.go_home(idx, cx)))
                    }))
            }));

        let stats = {
            let p = &self.panes[idx];
            stats_text(&p.model, &p.browse.phase)
        };
        let footer = div()
            .flex_none()
            .h(px(22.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .border_t_1()
            .border_color(th.c("border.subtle"))
            .text_size(px(number("font", "ui.caption")))
            .text_color(th.c("text.secondary"))
            .child(stats);
        let _ = (tz, now, window);

        div()
            .id(("filer-pane", idx))
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .when(is_focused_pane, |d| {
                d.border_t_2().border_color(th.c("accent"))
            })
            .child(header)
            .child(tools)
            .child(path_row)
            .child(crumb_row)
            .children(stale_banner.map(|text| {
                div()
                    .flex_none()
                    .px(px(10.0))
                    .py(px(3.0))
                    .text_size(px(number("font", "ui.caption")))
                    .text_color(th.c("status.error"))
                    .child(text)
            }))
            .child(col_head)
            .child(list_area)
            .child(footer)
            .into_any_element()
    }

    /// 传输进度条与错误条（队列所在栏的）。
    fn render_bars(&mut self, qidx: usize, th: Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let now = self.now_ms();
        let mut out = Vec::new();
        let Some(pane) = self.panes.get(qidx) else {
            return out;
        };
        if let Some(bar) = pane.queue.bar(now) {
            out.push(self.render_bar(qidx, &bar, th, cx));
        }
        if let Some(err) = pane.queue.error() {
            out.push(
                div()
                    .flex_none()
                    .px(px(10.0))
                    .py(px(4.0))
                    .flex()
                    .items_start()
                    .gap(px(6.0))
                    .border_t_1()
                    .border_color(th.c("border.subtle"))
                    .text_size(px(number("font", "ui.caption")))
                    .text_color(th.c("status.error"))
                    .child(div().flex_1().min_w_0().child(err.to_owned()))
                    .child(
                        icon_button(("err-x", qidx), "x", t("ui-filer-dismiss"), true, th)
                            .size(px(18.0))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.dismiss_error(qidx);
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        }
        out
    }

    fn render_bar(&self, qidx: usize, bar: &Bar, th: Theme, cx: &mut Context<Self>) -> AnyElement {
        let color = match bar.finish {
            Some(BarFinish::Failed) => th.c("status.error"),
            Some(BarFinish::Cancelled) => th.c("text.secondary"),
            _ => th.c("accent"),
        };
        let now = self.now_ms();
        let track = div()
            .relative()
            .h(px(4.0))
            .w_full()
            .rounded(px(2.0))
            .bg(th.c("bg.field"))
            .overflow_hidden();
        let track = match bar.fraction {
            Some(f) => track.child(
                div()
                    .h_full()
                    .w(relative(f as f32))
                    .rounded(px(2.0))
                    .bg(color),
            ),
            None if bar.finish.is_some() => track,
            // 总量未知：不确定进度，来回移动的色块，不能当作“传完了”。
            None => {
                let phase = ((now % 1600) as f32 / 800.0 - 1.0).abs();
                track.child(
                    div()
                        .absolute()
                        .top_0()
                        .h_full()
                        .left(relative(phase * 0.7))
                        .w(relative(0.3))
                        .rounded(px(2.0))
                        .bg(color),
                )
            }
        };
        div()
            .flex_none()
            .px(px(10.0))
            .py(px(6.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .border_t_1()
            .border_color(th.c("border.subtle"))
            .text_size(px(number("font", "ui.caption")))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_color(th.c("text.primary"))
                            .child(bar.title.clone()),
                    )
                    .children(bar.cancellable.then(|| {
                        icon_button(
                            ("xfer-cancel", qidx),
                            "x",
                            t("ui-filer-tip-cancel"),
                            true,
                            th,
                        )
                        .size(px(18.0))
                        .on_click(cx.listener(move |this, _, _, cx| this.cancel_transfer(qidx, cx)))
                    })),
            )
            .child(track)
            .children((!bar.detail.is_empty()).then(|| {
                div()
                    .text_color(th.c("text.secondary"))
                    .child(bar.detail.clone())
            }))
            .into_any_element()
    }

    /// 拖放高亮层：只在外部文件拖进面板时可见（`drag_over` 驱动），底部说明“放下以上传”或不能放的原因。
    fn render_drop_overlay(&self, th: Theme) -> AnyElement {
        let reason = self.drop_block_reason();
        let remote = self
            .panes
            .get(self.shown)
            .is_some_and(|p| p.source != Source::Local);
        let (text, accepted) = match reason {
            Some(r) => (r, false),
            None => (
                t(if remote {
                    "ui-filer-drop-upload"
                } else {
                    "ui-filer-drop-copy"
                }),
                true,
            ),
        };
        let tint: Hsla = if accepted {
            th.ca("accent", 0x22)
        } else {
            th.ca("status.error", 0x22)
        };
        let border = if accepted {
            th.c("accent")
        } else {
            th.c("status.error")
        };
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .opacity(0.0)
            .flex()
            .items_end()
            .justify_center()
            .pb(px(30.0))
            .child(
                div()
                    .px(px(12.0))
                    .py(px(6.0))
                    .rounded(px(number("radius", "md")))
                    .bg(th.c("bg.popover"))
                    .border_1()
                    .border_color(border)
                    .text_size(px(number("font", "ui.body")))
                    .text_color(th.c("text.primary"))
                    .child(text),
            )
            .drag_over::<ExternalPaths>(move |s, _, _, _| {
                s.opacity(1.0).bg(tint).border_2().border_color(border)
            })
            .into_any_element()
    }

    /// 双栏中间的上传 / 下载按钮。
    fn render_middle(&mut self, th: Theme, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.panes[0].queue.is_busy();
        div()
            .flex_none()
            .w(px(40.0))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .border_x_1()
            .border_color(th.c("border.subtle"))
            .child(
                icon_button(
                    ("mid", 0),
                    "upload",
                    t("ui-filer-send-to-remote"),
                    !busy,
                    th,
                )
                .on_click(cx.listener(|this, _, _, cx| this.dual_upload(cx))),
            )
            .child(
                icon_button(
                    ("mid", 1),
                    "download",
                    t("ui-filer-get-from-remote"),
                    !busy,
                    th,
                )
                .on_click(cx.listener(|this, _, _, cx| this.dual_download(cx))),
            )
            .into_any_element()
    }
}

impl Render for FilerPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = Self::theme(window);
        self.cached_theme = th;
        let mut root = div()
            .id("filer-panel")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(th.c("bg.panel"))
            .text_color(th.c("text.primary"))
            .key_context("FilerPanel")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                this.drop_paths(paths.paths().to_vec(), cx);
            }));
        if self.panes.is_empty() {
            return root
                .items_center()
                .justify_center()
                .text_color(th.c("text.tertiary"))
                .child(t("ui-filer-no-session"))
                .into_any_element();
        }
        match self.mode {
            PanelMode::Aux => {
                let idx = self.shown.min(self.panes.len() - 1);
                let pane = self.render_pane(idx, th, window, cx);
                let bars = self.render_bars(idx, th, cx);
                root = root
                    .child(pane)
                    .children(bars)
                    .child(self.render_drop_overlay(th));
            }
            PanelMode::Dual => {
                let left = self.render_pane(0, th, window, cx);
                let right = self.render_pane(1, th, window, cx);
                let mid = self.render_middle(th, cx);
                let bars = self.render_bars(0, th, cx);
                root = root
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.0))
                            .flex()
                            .flex_row()
                            .child(left)
                            .child(mid)
                            .child(right),
                    )
                    .children(bars);
            }
        }
        root.into_any_element()
    }
}
