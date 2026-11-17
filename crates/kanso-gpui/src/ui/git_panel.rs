//! Git 改动面板视图（filer.md §10）：终端右侧的面板，显示仓库摘要、改动文件列表、按需展开的差异、
//! 提交说明与提交、丢弃。外观照 mac 版 `apple/Kanso/App/Git/`。
//!
//! 数据全部来自 `kanso_core::git::GitPanel`（核心已有，mac 经 FFI 调用同一份）；阻塞的 git 进程只在后台线程跑，
//! 请求代次由核心持有：过时的结果丢掉。状态机与差异行展示数据在 `kanso_gpui::app::git_panel`。
//! 面板自己不处理菜单和对话框，发 [`GitEvent`] 由外壳（`shell_git.rs`）承载，结果交回。

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, Pixels, Point, ScrollHandle, SharedString, Stateful, Subscription, Window, div,
    prelude::*, px,
};
use kanso_core::CoreError;
use kanso_core::git::{GitPanel as CoreGit, GitSummary};
use kanso_gpui::app::filer_text::msg;
use kanso_gpui::app::git_panel::DiffKind;
use kanso_gpui::app::git_panel::{
    DiffData, DiffRow, DiscardKind, EmptyState, FileRow, GitModel, GitTarget, RefreshThrottle,
    Summary, Throttle, Toggle, count_text, diff_rows, discard_kind, kind_marker, path_label,
    status_is_conflict, totals_text,
};
use kanso_gpui::app::launcher::core_error_text;
use kanso_gpui::widgets::dialog::{ButtonSpec, DialogResult, DialogSpec};
use kanso_gpui::widgets::menu::MenuItem;
use kanso_gpui::widgets::style::number;
use kanso_types::message::Message;

use super::display;
use super::shell::Theme;
use super::widgets::icons::icon;
use super::widgets::text_input::{TextInput, TextInputEvent};

const BTN: f32 = 24.0;
const ROW_HEADER_H: f32 = 40.0;
const DIFF_LINE_H: f32 = 18.0;
const DIFF_FONT: f32 = 12.0;
const GUTTER_W: f32 = 44.0;
const MARKER_W: f32 = 14.0;

/// 面板向外壳发的事件。
#[derive(Clone, Debug)]
pub(crate) enum GitEvent {
    /// 关闭按钮。
    Close,
    /// 摘要或状态变化（入口按钮的统计要重画）。
    Changed,
    /// 在窗口坐标弹出菜单；选中后交给 [`GitPanelView::on_menu_action`]。
    Menu {
        items: Vec<MenuItem>,
        position: Point<Pixels>,
    },
    /// 丢弃确认框；结果交给 [`GitPanelView::on_dialog_result`]。
    Dialog(DialogSpec),
}

/// 等待确认框结果的丢弃操作。
struct PendingDiscard {
    path: Option<String>,
    /// 弹框时的请求代次：期间摘要变过就不再执行。
    request: u64,
}

pub(crate) struct GitPanelView {
    engine: Arc<CoreGit>,
    /// 当前请求代次（与核心同步）。
    request: u64,
    pub(crate) model: GitModel,
    throttle: RefreshThrottle,
    t0: Instant,
    input: Entity<TextInput>,
    message: String,
    scroll: ScrollHandle,
    focus: FocusHandle,
    pending_discard: Option<PendingDiscard>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<GitEvent> for GitPanelView {}

impl Focusable for GitPanelView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// 悬停提示。
struct Tip {
    text: SharedString,
    th: Theme,
}

impl Render for Tip {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
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

/// 给有 id 的元素挂悬停提示。
pub(crate) fn with_tip<E: gpui::InteractiveElement + gpui::StatefulInteractiveElement>(
    el: E,
    text: String,
    th: Theme,
) -> E {
    el.tooltip(move |_window, cx| {
        let text: SharedString = text.clone().into();
        cx.new(|_| Tip { text, th }).into()
    })
}

/// 图标按钮；不可用时置灰且不响应点击。
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
        .child(icon(name, 14.0, color));
    let b = if enabled {
        b.cursor_pointer().hover(move |s| s.bg(th.c("bg.rowHover")))
    } else {
        b
    };
    with_tip(b, tip, th)
}

impl GitPanelView {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextInput::new(window, cx).placeholder(msg(Message::UiGitCommitPlaceholder {}))
        });
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, _i, ev: &TextInputEvent, _window, cx| match ev {
                TextInputEvent::Change(t) => {
                    this.message = t.to_string();
                    cx.notify();
                }
                TextInputEvent::Submit(_) => this.commit(cx),
                _ => {}
            },
        );
        GitPanelView {
            engine: Arc::new(CoreGit::default()),
            request: 0,
            model: GitModel::default(),
            throttle: RefreshThrottle::default(),
            t0: Instant::now(),
            input,
            message: String::new(),
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            pending_discard: None,
            _subs: vec![sub],
        }
    }

    fn now_ms(&self) -> u64 {
        self.t0.elapsed().as_millis() as u64
    }

    // ---- 跟随与刷新 ----

    /// 外壳在活动标签、连接状态、目录变化时调用。目标变了就清掉旧仓库并重新读取。
    pub(crate) fn follow(&mut self, target: Option<GitTarget>, cx: &mut Context<Self>) {
        if self.model.follow(target) {
            self.reset_for_new_target(cx);
            cx.notify();
            cx.emit(GitEvent::Changed);
        }
    }

    fn reset_for_new_target(&mut self, cx: &mut Context<Self>) {
        self.invalidate();
        self.message.clear();
        self.pending_discard = None;
        self.input.update(cx, |i, cx| i.set_text("", cx));
        if self.model.target.is_some() {
            self.start_refresh(false, cx);
        }
    }

    /// 作废在途请求（核心同时丢掉旧摘要）。
    fn invalidate(&mut self) {
        self.request = self.engine.begin();
        self.model.loading = false;
    }

    /// 自动触发（命令结束、位置变化）或用户点击刷新。
    pub(crate) fn request_refresh(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.model.target.is_none() || self.model.writing {
            return;
        }
        match self.throttle.request(self.now_ms(), manual) {
            Throttle::Now => self.start_refresh(false, cx),
            Throttle::After(ms) => {
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(ms))
                        .await;
                    let _ = this.update(cx, |t, cx| {
                        if t.throttle.take_pending() && !t.model.writing && t.model.target.is_some()
                        {
                            t.start_refresh(false, cx);
                        }
                    });
                })
                .detach();
            }
            Throttle::Skip => {}
        }
    }

    fn start_refresh(&mut self, keep_error: bool, cx: &mut Context<Self>) {
        let Some(target) = self.model.target.clone() else {
            return;
        };
        self.throttle.started(self.now_ms());
        self.invalidate();
        self.model.begin_refresh(keep_error);
        let request = self.request;
        let engine = self.engine.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { engine.read(request, &target.cwd) })
                .await;
            let _ = this.update(cx, |t, cx| t.on_summary(request, result, cx));
        })
        .detach();
        cx.notify();
        cx.emit(GitEvent::Changed);
    }

    fn on_summary(
        &mut self,
        request: u64,
        result: Result<Option<GitSummary>, CoreError>,
        cx: &mut Context<Self>,
    ) {
        if request != self.request || !self.engine.is_current(request) {
            return;
        }
        match result {
            Ok(summary) => {
                let reload = self
                    .model
                    .apply_summary(summary.as_ref().map(Summary::from_core));
                for path in reload {
                    self.load_diff(path, cx);
                }
            }
            Err(e) => self.model.apply_error(core_error_text(&e)),
        }
        cx.notify();
        cx.emit(GitEvent::Changed);
    }

    fn load_diff(&mut self, path: String, cx: &mut Context<Self>) {
        let request = self.request;
        let engine = self.engine.clone();
        cx.spawn(async move |this, cx| {
            let p = path.clone();
            let e = engine.clone();
            let result = cx
                .background_executor()
                .spawn(async move { e.diff(request, &p) })
                .await;
            let _ = this.update(cx, |t, cx| {
                if request != t.request || !engine.is_current(request) {
                    return;
                }
                match result {
                    Ok(Some(d)) => t.model.apply_diff(&path, Ok(DiffData::from_core(&d))),
                    Ok(None) => return,
                    Err(e) => t.model.apply_diff(&path, Err(core_error_text(&e))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- 操作 ----

    pub(crate) fn toggle(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.model.toggle(path) == Toggle::Expand {
            self.load_diff(path.to_owned(), cx);
        }
        cx.notify();
    }

    /// 自检：不经输入框直接提交。
    pub(crate) fn selftest_commit(&mut self, message: &str, cx: &mut Context<Self>) {
        self.message = message.to_owned();
        self.commit(cx);
    }

    fn dismiss_error(&mut self, cx: &mut Context<Self>) {
        self.model.error = None;
        cx.notify();
    }

    fn copy_path(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(s) = &self.model.summary {
            let full = Path::new(&s.root).join(path);
            cx.write_to_clipboard(ClipboardItem::new_string(
                full.to_string_lossy().into_owned(),
            ));
        }
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        if !self.model.can_commit(&self.message) {
            return;
        }
        let message = self.message.clone();
        self.run_write(
            true,
            move |engine, request| engine.commit(request, &message),
            cx,
        );
    }

    pub(crate) fn discard(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        if !self.model.can_write() {
            return;
        }
        self.run_write(
            false,
            move |engine, request| engine.discard(request, path.as_deref()),
            cx,
        );
    }

    /// 写操作：进行中禁用刷新、另一个写操作与列表操作；失败显示具体错误，不重试。
    fn run_write(
        &mut self,
        is_commit: bool,
        op: impl FnOnce(&CoreGit, u64) -> Result<(), CoreError> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.model.begin_write();
        self.input.update(cx, |i, cx| i.set_disabled(true, cx));
        let engine = self.engine.clone();
        let request = self.request;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { op(&engine, request) })
                .await;
            let _ = this.update(cx, |t, cx| t.on_write_done(is_commit, result, cx));
        })
        .detach();
        cx.notify();
    }

    fn on_write_done(
        &mut self,
        is_commit: bool,
        result: Result<(), CoreError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Err(e) => {
                let mut text = core_error_text(&e);
                if is_commit {
                    // 暂存已经完成，之后的失败（身份、钩子、签名）不会回滚它。
                    text.push('\n');
                    text.push_str(&msg(Message::UiGitCommitMayStaged {}));
                }
                self.model.error = Some(text);
            }
            Ok(()) => {
                if is_commit {
                    self.message.clear();
                    self.input.update(cx, |i, cx| i.set_text("", cx));
                }
            }
        }
        self.input.update(cx, |i, cx| i.set_disabled(false, cx));
        if self.model.finish_write() == Some(true) {
            self.invalidate();
            self.message.clear();
            self.input.update(cx, |i, cx| i.set_text("", cx));
        }
        if self.model.target.is_some() {
            self.start_refresh(true, cx);
        }
        cx.notify();
        cx.emit(GitEvent::Changed);
    }

    // ---- 菜单与确认框 ----

    fn open_more(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.model.can_write() {
            return;
        }
        cx.emit(GitEvent::Menu {
            items: vec![MenuItem::action(
                "git:discard-all",
                msg(Message::UiGitDiscardAll {}),
            )],
            position,
        });
    }

    /// 外壳转交的菜单选择。
    pub(crate) fn on_menu_action(&mut self, id: &str, cx: &mut Context<Self>) {
        if id == "git:discard-all" {
            self.confirm_discard(None, cx);
        }
    }

    fn confirm_discard(&mut self, file: Option<FileRow>, cx: &mut Context<Self>) {
        let Some(summary) = self
            .model
            .summary
            .as_ref()
            .filter(|_| self.model.can_write())
        else {
            return;
        };
        let (title, body) = match &file {
            Some(f) => (
                msg(Message::UiGitDiscardTitleFile {
                    path: f.path.clone(),
                }),
                match discard_kind(f) {
                    DiscardKind::NewContent => msg(Message::UiGitDiscardBodyNew {}),
                    _ => msg(Message::UiGitDiscardBodyTracked {}),
                },
            ),
            None => (
                msg(Message::UiGitDiscardTitleAll {
                    repo: summary.repo_name(),
                }),
                msg(Message::UiGitDiscardBodyAll {}),
            ),
        };
        let spec = DialogSpec::new(&title)
            .text(&body)
            .text(&msg(Message::UiGitDiscardNoUndo {}))
            .button(ButtonSpec::secondary("cancel", &msg(Message::UiGitCancel {})).cancel())
            .button(ButtonSpec::danger(
                "discard",
                &msg(Message::UiGitDiscard {}),
            ));
        self.pending_discard = Some(PendingDiscard {
            path: file.map(|f| f.path),
            request: self.request,
        });
        cx.emit(GitEvent::Dialog(spec));
    }

    /// 外壳转交的确认框结果。
    pub(crate) fn on_dialog_result(&mut self, result: &DialogResult, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_discard.take() else {
            return;
        };
        if result.button.as_deref() == Some("discard") && pending.request == self.request {
            self.discard(pending.path, cx);
        }
    }

    // ---- 绘制 ----

    fn render_file(&self, f: &FileRow, th: Theme, cx: &mut Context<Self>) -> AnyElement {
        let expanded = self.model.expanded.contains(&f.path);
        let enabled = !self.model.writing && !self.model.loading;
        let (plus, minus) = count_text(f.added, f.deleted);
        let label = path_label(f);
        let path = f.path.clone();
        let file = f.clone();
        let header = div()
            .id(SharedString::from(format!("git-head-{}", f.path)))
            .h(px(ROW_HEADER_H))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(8.0))
            .bg(th.c("bg.card"))
            .when(enabled, |d| d.cursor_pointer())
            .on_click({
                let path = path.clone();
                cx.listener(move |this, _, _, cx| this.toggle(&path, cx))
            })
            .child(icon(
                if expanded {
                    "chevron-down"
                } else {
                    "chevron-right"
                },
                14.0,
                th.c("text.secondary"),
            ))
            .child(icon("file", 14.0, th.c("text.secondary")))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(12.0))
                    .child(label),
            )
            .child(
                icon_button(
                    SharedString::from(format!("git-copy-{}", f.path)),
                    "copy",
                    msg(Message::UiGitCopyPath {}),
                    enabled,
                    th,
                )
                .on_click({
                    let path = path.clone();
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.copy_path(&path, cx)
                    })
                }),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(8.0))
                    .h(px(18.0))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .rounded(px(9.0))
                    .bg(th.c("diff.badgeBackground"))
                    .text_size(px(10.0))
                    .child(div().text_color(th.c("diff.added")).child(plus))
                    .child(div().text_color(th.c("text.secondary")).child("·"))
                    .child(div().text_color(th.c("diff.removed")).child(minus)),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(14.0))
                    .text_size(px(11.0))
                    .text_color(if status_is_conflict(&f.status) {
                        th.c("status.error")
                    } else {
                        th.c("text.secondary")
                    })
                    .child(f.status.clone()),
            )
            .child(
                icon_button(
                    SharedString::from(format!("git-discard-{}", f.path)),
                    "undo-2",
                    msg(Message::UiGitDiscardFile {}),
                    enabled,
                    th,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if this.model.can_write() {
                        this.confirm_discard(Some(file.clone()), cx);
                    }
                })),
            );

        let mut card = div()
            .id(SharedString::from(format!("git-card-{}", f.path)))
            .flex_none()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(number("radius", "md")))
            .border_1()
            .border_color(th.c("border.subtle"))
            .bg(th.c("bg.panel"))
            .child(header);
        if expanded {
            card = card.child(self.render_diff_body(&f.path, th));
        }
        card.into_any_element()
    }

    /// 展开后的正文：差异行、说明、错误；读取中显示占位。
    fn render_diff_body(&self, path: &str, th: Theme) -> AnyElement {
        let mut body = div().flex().flex_col();
        let diff = self.model.diffs.get(path);
        let error = self.model.diff_errors.get(path);
        if let Some(d) = diff.filter(|d| !d.lines.is_empty()) {
            body = body.child(self.render_diff_lines(path, diff_rows(&d.lines), th));
        }
        let mut notes: Vec<(String, bool)> = Vec::new();
        if let Some(e) = error {
            notes.push((e.clone(), true));
        } else if let Some(d) = diff {
            if let Some(n) = &d.notice {
                notes.push((msg(n.clone()), false));
            }
            if d.truncated {
                notes.push((msg(Message::UiGitDiffTruncated {}), false));
            }
        } else {
            notes.push((msg(Message::UiGitReadingDiff {}), false));
        }
        for (text, is_error) in notes {
            body = body.child(
                div()
                    .px(px(16.0))
                    .py(px(8.0))
                    .text_size(px(11.0))
                    .text_color(if is_error {
                        th.c("status.error")
                    } else {
                        th.c("text.secondary")
                    })
                    .child(text),
            );
        }
        body.into_any_element()
    }

    fn render_diff_lines(&self, path: &str, rows: Vec<DiffRow>, th: Theme) -> AnyElement {
        let family = display::terminal_font().family.clone();
        let max_chars = rows
            .iter()
            .map(|r| r.text.chars().count())
            .max()
            .unwrap_or(0) as f32;
        // 等宽字体的近似宽度；行底色要铺满，所以按最长行定宽。
        let total_w = GUTTER_W + MARKER_W + 12.0 + max_chars * DIFF_FONT * 0.62;
        let mut col = div().flex().flex_col().py(px(4.0));
        for r in rows {
            if r.gap > 0 {
                col = col.child(
                    div()
                        .flex_none()
                        .w(px(total_w))
                        .min_w_full()
                        .h(px(16.0))
                        .pl(px(GUTTER_W))
                        .text_size(px(10.0))
                        .text_color(th.c("text.tertiary"))
                        .child(format!("⋯ {}", r.gap)),
                );
            }
            let (bg, hl, marker_color) = match r.kind {
                DiffKind::Added => (
                    Some(th.c("diff.addedBackground")),
                    th.c("diff.addedHighlight"),
                    th.c("diff.added"),
                ),
                DiffKind::Removed => (
                    Some(th.c("diff.removedBackground")),
                    th.c("diff.removedHighlight"),
                    th.c("diff.removed"),
                ),
                DiffKind::Context => (None, th.c("bg.rowHover"), th.c("text.tertiary")),
            };
            let mut text = div().flex().flex_row().whitespace_nowrap();
            match r.highlight.clone().filter(|h| !h.is_empty()) {
                Some(h) => {
                    text = text
                        .child(div().child(r.text[..h.start].to_owned()))
                        .child(div().bg(hl).child(r.text[h.clone()].to_owned()))
                        .child(div().child(r.text[h.end..].to_owned()));
                }
                None => text = text.child(div().child(r.text.clone())),
            }
            let mut line = div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .w(px(total_w))
                .min_w_full()
                .h(px(DIFF_LINE_H))
                .child(
                    div()
                        .flex_none()
                        .w(px(GUTTER_W))
                        .pr(px(8.0))
                        .text_right()
                        .text_color(th.c("text.tertiary"))
                        .child(r.number.to_string()),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(MARKER_W))
                        .text_color(marker_color)
                        .child(kind_marker(r.kind)),
                )
                .child(text);
            if let Some(bg) = bg {
                line = line.bg(bg);
            }
            col = col.child(line);
        }
        div()
            .id(SharedString::from(format!("git-diff-{path}")))
            .overflow_x_scroll()
            .font_family(family)
            .text_size(px(DIFF_FONT))
            .text_color(th.c("text.primary"))
            .child(col)
            .into_any_element()
    }

    fn render_header(&self, th: Theme, cx: &mut Context<Self>) -> AnyElement {
        let count = self.model.summary.as_ref().map_or(0, |s| s.files.len());
        let can_write = self.model.can_write();
        let more_enabled = can_write;
        div()
            .flex_none()
            .h(px(44.0))
            .flex()
            .items_center()
            .px(px(16.0))
            .gap(px(2.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(msg(Message::UiGitTitle {})),
                    )
                    .children((count > 0).then(|| {
                        div()
                            .text_size(px(11.0))
                            .text_color(th.c("text.secondary"))
                            .child(count.to_string())
                    })),
            )
            .children(
                self.model
                    .summary
                    .as_ref()
                    .is_some_and(|s| !s.files.is_empty())
                    .then(|| {
                        icon_button(
                            "git-more",
                            "ellipsis",
                            msg(Message::UiGitMore {}),
                            more_enabled,
                            th,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_more(window.mouse_position(), cx)
                        }))
                    }),
            )
            .child(
                icon_button(
                    "git-refresh",
                    "refresh-cw",
                    msg(Message::UiGitRefresh {}),
                    !self.model.writing,
                    th,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.dismiss_error(cx);
                    this.request_refresh(true, cx);
                })),
            )
            .child(
                icon_button("git-close", "x", msg(Message::UiGitClose {}), true, th)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(GitEvent::Close))),
            )
            .into_any_element()
    }

    fn render_summary_line(&self, th: Theme) -> Option<AnyElement> {
        let s = self.model.summary.as_ref()?;
        let (plus, minus) = totals_text(s.added, s.deleted);
        let tip = msg(Message::UiGitBranchTooltip {
            repo: s.root.clone(),
            branch: s.branch.clone(),
        });
        let el = div()
            .id("git-repo")
            .flex_none()
            .px(px(16.0))
            .pb(px(8.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(13.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(s.repo_name()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .gap(px(8.0))
                            .text_size(px(11.0))
                            .child(div().text_color(th.c("diff.added")).child(plus))
                            .child(div().text_color(th.c("diff.removed")).child(minus)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .text_size(px(11.0))
                    .text_color(th.c("text.secondary"))
                    .child(icon("git-branch", 12.0, th.c("text.secondary")))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(s.branch.clone()),
                    ),
            );
        Some(with_tip(el, tip, th).into_any_element())
    }

    fn render_error(&self, th: Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let e = self.model.error.clone()?;
        Some(
            div()
                .flex_none()
                .mx(px(16.0))
                .mb(px(8.0))
                .flex()
                .items_start()
                .gap(px(6.0))
                .text_size(px(11.0))
                .text_color(th.c("status.error"))
                .child(div().flex_1().min_w_0().child(e))
                .child(
                    icon_button("git-error-x", "x", msg(Message::UiGitClose {}), true, th)
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_error(cx))),
                )
                .into_any_element(),
        )
    }

    fn render_footer(&self, th: Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self
            .model
            .summary
            .as_ref()
            .is_none_or(|s| s.files.is_empty())
        {
            return None;
        }
        let can_commit = self.model.can_commit(&self.message);
        let button = div()
            .id("git-commit")
            .h(px(30.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .rounded(px(number("radius", "md")))
            .text_size(px(12.0))
            .bg(if can_commit {
                th.c("accent")
            } else {
                th.ca("accent", 0x55)
            })
            .text_color(th.c("text.onAccent"))
            .child(icon("check", 14.0, th.c("text.onAccent")))
            .child(msg(Message::UiGitCommit {}))
            .when(can_commit, |d| {
                d.cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.commit(cx)))
            });
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .p(px(12.0))
                .border_t_1()
                .border_color(th.c("border.subtle"))
                .child(self.input.clone())
                .child(button)
                .into_any_element(),
        )
    }
}

impl Render for GitPanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = Theme {
            appearance: display::window_appearance(window),
        };
        let mut root = div()
            .id("git-panel")
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(th.c("bg.panel"))
            .text_color(th.c("text.primary"))
            .key_context("GitPanel")
            .track_focus(&self.focus)
            .child(self.render_header(th, cx))
            .children(self.render_summary_line(th))
            .children(self.render_error(th, cx));

        let empty = match self.model.empty_state() {
            EmptyState::None => None,
            EmptyState::Loading => Some(msg(Message::UiGitLoading {})),
            EmptyState::Unsupported => Some(msg(Message::UiGitUnsupported {})),
            EmptyState::Clean => Some(msg(Message::UiGitClean {})),
        };
        if let Some(text) = empty {
            root = root.child(
                div()
                    .flex_1()
                    .px(px(24.0))
                    .pt(px(24.0))
                    .flex()
                    .justify_center()
                    .text_size(px(12.0))
                    .text_color(th.c("text.secondary"))
                    .child(div().w_full().min_w_0().text_center().child(text)),
            );
        } else {
            let files: Vec<FileRow> = self
                .model
                .summary
                .as_ref()
                .map(|s| s.files.clone())
                .unwrap_or_default();
            let rows: Vec<AnyElement> = files.iter().map(|f| self.render_file(f, th, cx)).collect();
            root = root.child(
                div()
                    .id("git-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .p(px(10.0))
                    .children(rows),
            );
        }
        root.children(self.render_footer(th, cx))
    }
}
