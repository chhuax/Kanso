//! GPUI 服务器监控面板视图（docs/spec/host-monitor.md）。

use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnyElement, Context, EventEmitter, FontWeight, Render, ScrollHandle, SharedString, Task,
    Window, div, prelude::*, px,
};
use kanso_core::Core;
use kanso_gpui::app::filer_text::msg;
use kanso_gpui::app::launcher::core_error_text;
use kanso_gpui::app::monitor_panel::{
    MonitorModel, MonitorSnapshot, POLL_INTERVAL, format_bytes, format_rate,
};
use kanso_gpui::widgets::style::number;
use kanso_types::SessionId;
use kanso_types::message::Message;

use super::display;
use super::shell::Theme;
use super::widgets::icons::icon;

/// 监控面板发给外壳的事件。
pub(super) enum MonitorEvent {
    /// 点关闭按钮，收起右侧栏。
    Close,
}

impl EventEmitter<MonitorEvent> for MonitorPanelView {}

/// 右侧服务器监控面板视图。
pub(super) struct MonitorPanelView {
    core: Arc<Core>,
    model: MonitorModel,
    visible: bool,
    started_at: Instant,
    poll_task: Option<Task<()>>,
    scroll: ScrollHandle,
}

impl MonitorPanelView {
    pub(super) fn new(core: Arc<Core>) -> Self {
        Self {
            core,
            model: MonitorModel::default(),
            visible: false,
            started_at: Instant::now(),
            poll_task: None,
            scroll: ScrollHandle::new(),
        }
    }

    /// 面板是否可见；不可见时停止轮询。
    pub(super) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            if self.model.target().is_some() {
                self.start_polling(cx);
            }
        } else {
            self.stop_polling();
        }
        cx.notify();
    }

    /// 跟随活动标签（仅已连接的 SSH / 纯 SFTP 会话非 `None`）。
    pub(super) fn set_target(&mut self, target: Option<SessionId>, cx: &mut Context<Self>) {
        if !self.model.set_target(target) {
            if self.visible && target.is_some() && self.poll_task.is_none() {
                self.start_polling(cx);
            }
            return;
        }
        self.poll_task = None;
        if self.visible && target.is_some() {
            self.start_polling(cx);
        }
        cx.notify();
    }

    fn stop_polling(&mut self) {
        self.model.pause();
        self.poll_task = None;
    }

    fn start_polling(&mut self, cx: &mut Context<Self>) {
        self.stop_polling();
        let Some((session, generation)) = self.model.begin_sample() else {
            return;
        };
        let core = self.core.clone();
        self.poll_task = Some(cx.spawn(async move |view, cx| {
            loop {
                let sample = core.host_sample(session).await;
                let keep_going = view
                    .update(cx, |this, cx| {
                        if !this.visible
                            || this.model.target() != Some(session)
                            || this.model.generation() != generation
                        {
                            return false;
                        }
                        let now_ms = this.started_at.elapsed().as_millis() as u64;
                        match sample {
                            Ok(raw) => {
                                this.model.apply_sample(session, generation, raw, now_ms);
                            }
                            Err(e) => {
                                this.model
                                    .apply_error(session, generation, core_error_text(&e));
                            }
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
                cx.background_executor().timer(POLL_INTERVAL).await;
                let still_active = view
                    .read_with(cx, |this, _| {
                        this.visible
                            && this.model.target() == Some(session)
                            && this.model.generation() == generation
                    })
                    .unwrap_or(false);
                if !still_active {
                    break;
                }
            }
        }));
    }

    fn render_metrics(&self, snap: &MonitorSnapshot, th: Theme) -> AnyElement {
        let mut sections: Vec<AnyElement> = Vec::new();
        let caption = px(number("font", "ui.caption"));

        // 负载
        sections.push(section(
            th,
            &msg(Message::UiMonitorLoad {}),
            vec![
                div()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!(
                        "{:.2}   {:.2}   {:.2}",
                        snap.load[0], snap.load[1], snap.load[2]
                    ))
                    .into_any_element(),
            ],
        ));

        // CPU
        let cpu_text = snap
            .cpu_percent
            .map_or_else(|| "—".to_owned(), |p| format!("{p}%"));
        let cpu_frac = snap.cpu_percent.map_or(0.0, |p| f32::from(p) / 100.0);
        sections.push(section(
            th,
            &msg(Message::UiMonitorCpu {}),
            vec![bar_row(th, &cpu_text, cpu_frac)],
        ));

        // 内存
        let mem_frac = if snap.mem_total > 0 {
            (snap.mem_used as f64 / snap.mem_total as f64) as f32
        } else {
            0.0
        };
        let mem_pct = (mem_frac * 100.0).round() as u32;
        let mem_text = format!(
            "{} / {} ({mem_pct}%)",
            format_bytes(snap.mem_used),
            format_bytes(snap.mem_total)
        );
        sections.push(section(
            th,
            &msg(Message::UiMonitorMemory {}),
            vec![bar_row(th, &mem_text, mem_frac)],
        ));

        // 交换区（总量大于 0 时显示）
        if snap.swap_total > 0 {
            let swap_frac = (snap.swap_used as f64 / snap.swap_total as f64) as f32;
            let swap_pct = (swap_frac * 100.0).round() as u32;
            let swap_text = format!(
                "{} / {} ({swap_pct}%)",
                format_bytes(snap.swap_used),
                format_bytes(snap.swap_total)
            );
            sections.push(section(
                th,
                &msg(Message::UiMonitorSwap {}),
                vec![bar_row(th, &swap_text, swap_frac)],
            ));
        }

        // 磁盘
        if !snap.disks.is_empty() {
            let rows = snap
                .disks
                .iter()
                .map(|d| {
                    let right = format!(
                        "{} / {} ({}%)",
                        format_bytes(d.used),
                        format_bytes(d.size),
                        d.percent
                    );
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .text_size(caption)
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(d.mount.clone()),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(caption)
                                        .text_color(th.c("text.secondary"))
                                        .child(right),
                                ),
                        )
                        .child(progress_bar(th, f32::from(d.percent) / 100.0))
                        .into_any_element()
                })
                .collect();
            sections.push(section(th, &msg(Message::UiMonitorDisk {}), rows));
        }

        // 网络
        if !snap.nets.is_empty() {
            let rows = snap
                .nets
                .iter()
                .map(|n| {
                    let rate = match (n.rx_per_sec, n.tx_per_sec) {
                        (Some(rx), Some(tx)) => {
                            Some(format!("↓ {}   ↑ {}", format_rate(rx), format_rate(tx)))
                        }
                        _ => None,
                    };
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .text_size(caption)
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(n.iface.clone()),
                                )
                                .children(rate.map(|r| {
                                    div()
                                        .flex_none()
                                        .text_size(caption)
                                        .text_color(th.c("text.secondary"))
                                        .child(r)
                                })),
                        )
                        .child(
                            div()
                                .text_size(caption)
                                .text_color(th.c("text.secondary"))
                                .child(format!(
                                    "↓ {}   ↑ {}",
                                    format_bytes(n.rx),
                                    format_bytes(n.tx)
                                )),
                        )
                        .into_any_element()
                })
                .collect();
            sections.push(section(th, &msg(Message::UiMonitorNet {}), rows));
        }

        div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .px(px(16.0))
            .py(px(10.0))
            .children(sections)
            .into_any_element()
    }
}

impl Render for MonitorPanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = Theme {
            appearance: display::window_appearance(window),
        };
        let header = div()
            .flex_none()
            .h(px(40.0))
            .px(px(16.0))
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(th.c("border.subtle"))
            .child(
                div()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(msg(Message::UiMonitorTitle {})),
            )
            .child(
                div()
                    .id("monitor-close")
                    .w(px(24.0))
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(number("radius", "md")))
                    .cursor_pointer()
                    .hover(move |s| s.bg(th.c("bg.rowHover")))
                    .child(icon("x", 14.0, th.c("text.secondary")))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(MonitorEvent::Close))),
            );

        let body: AnyElement = if self.model.target().is_none() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px(px(24.0))
                .text_color(th.c("text.secondary"))
                .child(msg(Message::UiMonitorEmpty {}))
                .into_any_element()
        } else if let Some(snap) = self.model.snapshot() {
            div()
                .id("monitor-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .child(self.render_metrics(snap, th))
                .into_any_element()
        } else if self.model.is_sampling() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px(px(24.0))
                .text_color(th.c("text.secondary"))
                .child(msg(Message::UiMonitorLoading {}))
                .into_any_element()
        } else {
            div().flex_1().into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(th.c("bg.panel"))
            .font_family(SharedString::from(display::ui_font_family()))
            .text_size(px(display::ui_font_size()))
            .text_color(th.c("text.primary"))
            .child(header)
            .children(self.model.error().map(|err| {
                div()
                    .flex_none()
                    .px(px(16.0))
                    .pt(px(10.0))
                    .text_size(px(number("font", "ui.caption")))
                    .text_color(th.c("status.error"))
                    .child(err.to_owned())
            }))
            .child(body)
    }
}

fn section(th: Theme, title: &str, rows: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(th.c("text.secondary"))
                .child(title.to_owned()),
        )
        .children(rows)
        .into_any_element()
}

fn bar_row(th: Theme, text: &str, fraction: f32) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .child(
            div()
                .text_size(px(number("font", "ui.caption")))
                .font_weight(FontWeight::MEDIUM)
                .child(text.to_owned()),
        )
        .child(progress_bar(th, fraction))
        .into_any_element()
}

fn progress_bar(th: Theme, fraction: f32) -> AnyElement {
    let clamped = fraction.clamp(0.0, 1.0);
    div()
        .w_full()
        .h(px(4.0))
        .rounded(px(2.0))
        .overflow_hidden()
        .bg(th.c("border.subtle"))
        .child(
            div()
                .h_full()
                .w(gpui::relative(clamped))
                .rounded(px(2.0))
                .bg(th.c("accent")),
        )
        .into_any_element()
}
