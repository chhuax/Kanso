//! `ShellView` 与底部 Sender 面板的接线：显隐与高度（可拖动的分隔条）、把窗口状态推给面板、
//! 定时推进核心调度（面板收起后任务照常运行，D7）、面板事件（提示、菜单、状态栏提示）。
//!
//! 面板本身在 `ui/sender.rs`，纯逻辑在 `kanso_gpui::app::sender`。

use std::time::Duration;

use gpui::{
    AnyElement, Context, CursorStyle, Entity, MouseButton, MouseDownEvent, MouseMoveEvent, Window,
    div, prelude::*, px,
};
use kanso_gpui::app::layout::Rect;
use kanso_gpui::app::sender::{
    HANDLE_THICKNESS, PANEL_DEFAULT_HEIGHT, SenderContext, SenderTab, clamp_panel_height,
    dragged_height,
};
use kanso_gpui::app::tab::TabConnection;

use super::sender::{SenderEvent, SenderPanel};
use super::shell::{ShellView, Theme};

/// 与 Sender 相关的外壳状态。
pub(super) struct SenderState {
    /// 面板实体（始终存在，隐藏只是不显示）。
    pub panel: Entity<SenderPanel>,
    /// 是否显示。
    pub visible: bool,
    /// 面板高度（不含顶部拖动条）。
    pub height: f32,
    /// 定时推进是否在跑。
    pub ticking: bool,
    /// 状态栏的“重复发送运行中”提示文字。
    pub chip: Option<String>,
    /// 正在拖动高度：（起始鼠标 y，起始高度）。
    pub drag: Option<(f32, f32)>,
}

impl SenderState {
    pub(super) fn new(panel: Entity<SenderPanel>) -> Self {
        SenderState {
            panel,
            visible: false,
            height: PANEL_DEFAULT_HEIGHT,
            ticking: false,
            chip: None,
            drag: None,
        }
    }
}

impl ShellView {
    /// 面板占用的总高度（含拖动条）；隐藏时为 0。
    pub(super) fn sender_total_height(&self) -> f32 {
        if self.sender.visible {
            self.sender.height + HANDLE_THICKNESS
        } else {
            0.0
        }
    }

    /// 当前窗口状态快照：活动标签与全部标签。
    pub(super) fn sender_context(&self) -> SenderContext {
        let ws = self.ctl.workspace();
        let tab = |id| {
            ws.item(id).map(|i| SenderTab {
                id: i.id,
                kind: i.kind,
                connected: i.connection == TabConnection::Connected,
                name: self.ctl.tab_title(id).unwrap_or_else(|| i.name.clone()),
                profile: i.profile,
            })
        };
        SenderContext {
            active: ws.active_session().and_then(tab),
            tabs: ws.sidebar_order().into_iter().filter_map(tab).collect(),
        }
    }

    /// 把窗口状态推给面板。
    pub(super) fn push_sender_context(&self, cx: &mut Context<Self>) {
        let ctx = self.sender_context();
        self.sender.panel.update(cx, |p, cx| p.set_context(ctx, cx));
    }

    /// 显示 / 隐藏面板（Ctrl+Alt+↓、视图菜单）。
    pub(super) fn toggle_sender(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sender.visible = !self.sender.visible;
        if self.sender.visible {
            self.push_sender_context(cx);
            self.sender.panel.update(cx, |p, cx| p.on_shown(window, cx));
        } else {
            self.sync_focus(window, cx);
        }
        cx.notify();
    }

    pub(super) fn on_sender_event(
        &mut self,
        ev: &SenderEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            SenderEvent::Notice(text, _error) => self.show_notice(text.clone(), cx),
            SenderEvent::OpenMenu { items, anchor } => {
                self.open_menu(items.clone(), *anchor, window, cx)
            }
            SenderEvent::Started => self.ensure_sender_timer(cx),
            SenderEvent::Close => {
                if self.sender.visible {
                    self.toggle_sender(window, cx);
                }
            }
            SenderEvent::Chip(text) => {
                if self.sender.chip != *text {
                    self.sender.chip = text.clone();
                    cx.notify();
                }
            }
        }
    }

    /// 有任务在运行时每 50 毫秒推进一次核心调度，跑完自动结束。
    fn ensure_sender_timer(&mut self, cx: &mut Context<Self>) {
        if self.sender.ticking {
            return;
        }
        self.sender.ticking = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let more = this.update(cx, |this, cx| {
                    this.push_sender_context(cx);
                    let running = this.sender.panel.update(cx, |p, cx| p.tick(cx));
                    if !running {
                        this.sender.ticking = false;
                    }
                    running
                });
                if !matches!(more, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// 窗口退出前停止全部 Sender 任务。
    pub(super) fn shutdown_sender(&self, cx: &mut Context<Self>) {
        self.sender.panel.update(cx, |p, _| p.shutdown());
    }

    /// `--sender-test`：展开面板、向当前会话发一条命令，几秒后检查终端屏幕里是否有输出，日志里打 PASS / FAIL。
    pub(super) fn run_sender_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let wait = |ms: u64| {
                let exec = cx.background_executor().clone();
                async move { exec.timer(Duration::from_millis(ms)).await }
            };
            wait(2500).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.toggle_sender(window, cx);
                this.push_sender_context(cx);
                this.sender
                    .panel
                    .update(cx, |p, cx| p.selftest_send("echo kanso-sender-ok", cx));
                super::log(&format!(
                    "[sender-test] 已发送，面板可见={} 运行中={}",
                    this.sender.visible,
                    this.sender.panel.read(cx).running()
                ));
            });
            wait(2500).await;
            let _ = this.update_in(cx, |this, _window, cx| {
                let screen = this
                    .ctl
                    .workspace()
                    .active_session()
                    .and_then(|s| this.terminals.get(&s))
                    .and_then(|t| {
                        t.read(cx).frame.as_ref().and_then(|f| {
                            f.view().ok().map(|v| kanso_gpui::render::screen_text(&v))
                        })
                    })
                    .unwrap_or_default();
                // 命令回显一次 + 输出一次。
                let hits = screen
                    .iter()
                    .filter(|l| l.contains("kanso-sender-ok"))
                    .count();
                let ok = hits >= 2;
                super::log(&format!(
                    "[sender-test] {} 屏幕出现 {hits} 行",
                    if ok { "PASS" } else { "FAIL" }
                ));
                if !ok {
                    super::EXIT_CODE.store(1, std::sync::atomic::Ordering::SeqCst);
                }
                if !super::KEEP_OPEN.load(std::sync::atomic::Ordering::Relaxed) {
                    cx.quit();
                }
            });
        })
        .detach();
    }

    // ---- 拖动高度 ----

    /// 鼠标移动：正在拖动高度时更新并返回 `true`（外壳不再处理分隔条）。
    pub(super) fn sender_mouse_move(
        &mut self,
        ev: &MouseMoveEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((start_y, start_h)) = self.sender.drag else {
            return false;
        };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.sender.drag = None;
            return false;
        }
        let dy = f32::from(ev.position.y) - start_y;
        self.sender.height = dragged_height(start_h, dy);
        cx.notify();
        true
    }

    /// 绘制面板（含顶部拖动条）；`area` 是分屏区（已扣掉面板高度）。
    pub(super) fn render_sender(
        &self,
        area: Rect,
        th: Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        if !self.sender.visible {
            return Vec::new();
        }
        let top = (area.y + area.height) as f32;
        let height = clamp_panel_height(self.sender.height);
        let handle = div()
            .id("sender-handle")
            .absolute()
            .left(px(area.x as f32))
            .top(px(top))
            .w(px(area.width as f32))
            .h(px(HANDLE_THICKNESS))
            .bg(th.c("border.strong"))
            .hover(|s| s.bg(th.c("accent")))
            .cursor(CursorStyle::ResizeUpDown)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                    this.sender.drag = Some((f32::from(ev.position.y), this.sender.height));
                    cx.stop_propagation();
                }),
            );
        let panel = div()
            .id("sender-panel")
            .absolute()
            .left(px(area.x as f32))
            .top(px(top + HANDLE_THICKNESS))
            .w(px(area.width as f32))
            .h(px(height))
            .overflow_hidden()
            .child(self.sender.panel.clone());
        vec![handle.into_any_element(), panel.into_any_element()]
    }
}
