//! 辅助 Filer 条目的拖出（docs/spec/filer.md §7.2、§7.4、§7.5）。
//!
//! GPUI 0.2.2 没有"向系统发起拖放"的接口（Windows 上要 OLE `DoDragDrop`，需要 unsafe，本 crate 禁止），
//! 所以条目只能拖到**本应用内**的落点：目前是终端（向松手所在的终端插入路径，不自动执行）。
//! 拖到资源管理器 / 桌面这类系统落点做不到，已在交接文档里写明。
//!
//! - 本地条目：路径现成，落下即插入。
//! - 远端条目（Windows / Linux 旧版做法）：开始拖动就下载成本机暂存副本，状态行显示进度；
//!   准备期间松手 → 取消下载（不在手势结束后继续下完）；副本在手势结束后才完成 → 提示"请重新拖动"；
//!   暂存目录按每次拖动隔离，下次启动清理。
//!
//! 手势与下载之间的决定都在 `kanso_gpui::app::drag_out::DragPrep`（有单元测试），这里只接线。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{Context, IntoElement, Render, SharedString, Window, div, prelude::*, px};
use kanso_gpui::app::drag_out::{DragPrep, Effect, chip_text};
use kanso_gpui::app::filer_text::msg;
use kanso_gpui::app::remote_edit::EditFiles;
use kanso_gpui::widgets::style::number;
use kanso_types::TransferId;
use kanso_types::message::Message;
use kanso_types::transfer::ConflictPolicy;

use super::filer_edit::{DragGroup, DragRole, DragStatus, Role};
use super::filer_panel::{FilerEvent, FilerPanel, PanelMode, Source};
use super::shell::Theme;

/// 拖动载荷：落点（终端）通过 `prep` 决定怎么处理。
pub(crate) struct FilerDrag {
    /// 按下的栏与行（开始拖动时据此取选中项）。
    pub pane: usize,
    pub row: u32,
    /// 手势与准备（下载）的共享状态。
    pub prep: Arc<DragPrep>,
    /// 拖动提示 / 失败提示里用的名称，开始拖动时填。
    pub label: Arc<Mutex<String>>,
}

impl FilerDrag {
    pub(crate) fn label(&self) -> String {
        self.label.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

/// 拖动时跟着鼠标的小标签。
pub(crate) struct DragChip {
    text: SharedString,
    th: Theme,
}

impl DragChip {
    pub(crate) fn new(text: String, th: Theme) -> DragChip {
        DragChip {
            text: text.into(),
            th,
        }
    }
}

impl Render for DragChip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let th = self.th;
        div()
            .px(px(10.0))
            .py(px(4.0))
            .max_w(px(320.0))
            .overflow_hidden()
            .whitespace_nowrap()
            .rounded(px(number("radius", "md")))
            .bg(th.c("bg.popover"))
            .border_1()
            .border_color(th.c("accent"))
            .text_size(px(number("font", "ui.body")))
            .text_color(th.c("text.primary"))
            .child(self.text.clone())
    }
}

impl FilerPanel {
    /// 渲染时为一行创建拖动载荷（便宜：一个 `Arc`；每次按下都会重绘，所以每次手势拿到的是新的）。
    pub(crate) fn make_drag(&self, pane: usize, row: u32) -> FilerDrag {
        FilerDrag {
            pane,
            row,
            prep: Arc::new(DragPrep::remote()),
            label: Arc::new(Mutex::new(String::new())),
        }
    }

    /// 手势真正开始（鼠标移动超过 GPUI 的拖动阈值）。返回拖动提示文字。
    pub(crate) fn start_drag(&mut self, drag: &FilerDrag, cx: &mut Context<Self>) -> String {
        let idx = drag.pane;
        if self.mode != PanelMode::Aux || idx >= self.panes.len() {
            return String::new();
        }
        let Some(pressed) = self.panes[idx].model.row(drag.row).cloned() else {
            return String::new();
        };
        // 按下的行在选中集合里就拖整组选中项，否则只拖这一行。
        let selected = self.selected_rows(idx);
        let rows = if selected.iter().any(|r| r.path == pressed.path) {
            selected
        } else {
            vec![pressed]
        };
        let names: Vec<String> = rows.iter().map(|r| r.name.clone()).collect();
        let label = chip_text(&names);
        if let Ok(mut g) = drag.label.lock() {
            *g = label.clone();
        }
        let prep = drag.prep.clone();
        let group = match self.panes[idx].source {
            Source::Local => {
                prep.set_local(rows.iter().map(|r| r.path.clone()).collect());
                None
            }
            Source::Remote(session) => self.drag_prepare_remote(session, &rows, &label, &prep, cx),
        };
        self.drag_watch_end(prep, group, label.clone(), cx);
        label
    }

    /// 远端条目：下载成本机暂存副本。
    fn drag_prepare_remote(
        &mut self,
        session: kanso_types::SessionId,
        rows: &[kanso_gpui::filer::Row],
        label: &str,
        prep: &Arc<DragPrep>,
        cx: &mut Context<Self>,
    ) -> Option<Arc<DragGroup>> {
        let fail = |this: &mut Self, why: String, cx: &mut Context<Self>| {
            let eff = prep.complete(Err(why));
            this.drag_effect(eff, label, cx);
        };
        let Some(dir) = self.edits.files.as_ref().map(EditFiles::drag_dir) else {
            fail(self, msg(Message::UiOperationFailed2 {}), cx);
            return None;
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            fail(self, e.to_string(), cx);
            return None;
        }
        let group = Arc::new(DragGroup {
            dir: dir.clone(),
            names: rows.iter().map(|r| r.name.clone()).collect(),
            remaining: AtomicUsize::new(rows.len()),
            failure: Mutex::new(None),
            transfers: Mutex::new(Vec::new()),
            label: label.to_owned(),
        });
        let mut started = 0usize;
        for r in rows {
            match self.core.download(
                session,
                &r.path,
                &dir.to_string_lossy(),
                ConflictPolicy::Overwrite,
            ) {
                Ok(id) => {
                    started += 1;
                    if let Ok(mut t) = group.transfers.lock() {
                        t.push(id);
                    }
                    self.edits.roles.insert(
                        id,
                        Role::Drag(Arc::new(DragRole {
                            prep: prep.clone(),
                            group: group.clone(),
                            session,
                        })),
                    );
                    if self.edits.drag.is_none() {
                        self.edits.drag = Some(DragStatus {
                            session,
                            title: msg(Message::FilerDragPreparing {
                                name: label.to_owned(),
                            }),
                            done: 0,
                            total: None,
                            transfer: Some(id),
                        });
                    }
                }
                Err(e) => {
                    let why = kanso_gpui::app::launcher::core_error_text(&e);
                    if let Ok(mut f) = group.failure.lock() {
                        f.get_or_insert(why);
                    }
                    // 没发起的条目不会再有结束事件：直接扣掉。
                    group.remaining.fetch_sub(1, Ordering::SeqCst);
                }
            }
        }
        if started == 0 {
            let why = group
                .failure
                .lock()
                .ok()
                .and_then(|mut f| f.take())
                .unwrap_or_default();
            self.edits.drag = None;
            let _ = std::fs::remove_dir_all(&dir);
            fail(self, why, cx);
            return None;
        }
        cx.notify();
        Some(group)
    }

    /// 轮询手势何时结束（GPUI 在松手后清掉活动拖动）：结束后做收尾（取消 / 提示）。
    fn drag_watch_end(
        &mut self,
        prep: Arc<DragPrep>,
        group: Option<Arc<DragGroup>>,
        label: String,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let over = this
                    .update(cx, |t, cx| {
                        if cx.has_active_drag() {
                            return false;
                        }
                        t.drag_ended(&prep, group.as_deref(), &label, cx);
                        true
                    })
                    .unwrap_or(true);
                if over {
                    break;
                }
            }
        })
        .detach();
    }

    fn drag_ended(
        &mut self,
        prep: &DragPrep,
        group: Option<&DragGroup>,
        label: &str,
        cx: &mut Context<Self>,
    ) {
        let eff = prep.on_ended();
        // 提前松手，或落下时还没准备完：停掉下载（不在手势结束后继续下完整个目录）。
        if (eff == Effect::CancelDownload || prep.is_cancelled())
            && let Some(g) = group
        {
            self.drag_cancel_group(g);
        }
        self.drag_effect(eff, label, cx);
    }

    fn drag_cancel_group(&mut self, g: &DragGroup) {
        let ids: Vec<TransferId> = g.transfers.lock().map(|t| t.clone()).unwrap_or_default();
        for id in ids {
            self.core.cancel_transfer(id);
        }
    }

    fn drag_effect(&mut self, eff: Effect, label: &str, cx: &mut Context<Self>) {
        match eff {
            Effect::Nothing | Effect::CancelDownload => {}
            Effect::TooLate => cx.emit(FilerEvent::Notice(msg(Message::FilerDragTooLate {}))),
            Effect::Failed(why) => cx.emit(FilerEvent::Notice(msg(Message::FilerDragFailed {
                name: label.to_owned(),
                why,
            }))),
        }
        cx.notify();
    }

    /// 一个拖出准备下载结束。`why`：`None` 成功，`Some(None)` 取消，`Some(Some(text))` 失败。
    pub(crate) fn drag_transfer_finished(
        &mut self,
        role: &DragRole,
        why: Option<Option<String>>,
        cx: &mut Context<Self>,
    ) {
        let g = &role.group;
        if let Some(w) = why
            && let Ok(mut f) = g.failure.lock()
        {
            f.get_or_insert_with(|| w.unwrap_or_else(|| msg(Message::FilerDragCancelled {})));
        }
        if g.remaining.fetch_sub(1, Ordering::SeqCst) != 1 {
            return;
        }
        // 全部结束。
        if self
            .edits
            .drag
            .as_ref()
            .is_some_and(|d| d.session == role.session)
        {
            self.edits.drag = None;
        }
        let failure = g.failure.lock().ok().and_then(|mut f| f.take());
        let result = match failure {
            None => Ok(g
                .names
                .iter()
                .map(|n| g.dir.join(n).to_string_lossy().into_owned())
                .collect()),
            Some(f) => {
                let _ = std::fs::remove_dir_all(&g.dir);
                Err(f)
            }
        };
        let eff = role.prep.complete(result);
        self.drag_effect(eff, &g.label, cx);
    }
}
