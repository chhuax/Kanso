//! 远程文件列表视图（SFTP 原型）：虚拟列表只渲染可见行，目录内容分批到达时边到边显示。
//!
//! 数据来自核心的 `Core::sftp_list`（流式 `DirStream`）；条目模型、排序、格式化在 `kanso_gpui::filer`。
//! 键盘：↑↓ PageUp/PageDown Home/End 移动选中，Enter 进入目录，Backspace / Alt+↑ 返回上一级，
//! F5 刷新，Ctrl+1/2/3 按名称 / 大小 / 时间排序（再按一次切换方向）。鼠标：单击选中，双击进入目录，点击列头排序。

use std::sync::{Arc, Mutex};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    ClickEvent, Context, FocusHandle, Focusable, KeyDownEvent, ScrollStrategy, Task,
    UniformListScrollHandle, Window, div, point, prelude::*, px, rgb, uniform_list,
};
use kanso_core::Core;
use kanso_gpui::filer::{
    BatchBuffer, DirModel, FrameStats, ListEnd, Sort, SortKey, format_mtime, format_size,
    frame_stats, parent_path,
};
use kanso_gpui::theme::Palette;
use kanso_types::event::SessionState;
use kanso_types::prompt::PromptResponse;
use kanso_types::{CoreEvent, SessionId};

use super::{EXIT_CODE, KEEP_OPEN, START, log, memory, tz};

/// 行高（逻辑像素）。
const ROW_H: f32 = 22.0;
/// 自检滚动测试每帧滚过的行数（约等于很快的一次滚轮甩动）。
const SCROLL_ROWS_PER_FRAME: f32 = 100.0;
/// 超过这个间隔的帧算卡顿（60 Hz 下一帧 16.7 ms，留一点余量）。
const SLOW_FRAME_MS: f64 = 20.0;
/// 界面两次并入读取缓冲之间至少隔这么久：软渲染一帧远长于它，所以实际是"每帧一次"；
/// 快机器上它把并入频率限制在约 60 Hz。首批不受此限制（先取后等）。
const APPLY_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Connecting,
    Listing,
    Done,
    Error(String),
}

/// 一次目录读取的计时（毫秒，相对 `list_start`）。
#[derive(Default)]
struct ListPerf {
    start: Option<Instant>,
    first_batch: Option<u128>,
    first_painted: Option<u128>,
    done: Option<u128>,
    done_painted: Option<u128>,
    batches: usize,
    /// 界面并入缓冲的次数、累计耗时（微秒）、单次最多并入的批数。
    applies: usize,
    apply_us: u128,
    max_per_apply: usize,
    first_painted_pending: bool,
    done_painted_pending: bool,
}

/// 自检的滚动阶段：每帧把偏移量往下推，直到到底。
struct ScrollTest {
    last_y: f32,
    stalled: u32,
    frames: u32,
    started: Instant,
}

struct FilerTest {
    stage: u8,
    deadline: Instant,
    results: Vec<String>,
    expect_count: Option<usize>,
    nav_from: Option<(String, usize)>,
    scroll_done: Option<(FrameStats, u32, f64)>,
}

pub(crate) struct FilerView {
    core: Arc<Core>,
    session: SessionId,
    focus: FocusHandle,
    palette: Palette,
    scroll: UniformListScrollHandle,
    tz_min: i32,
    initial_path: Option<String>,
    label: String,

    phase: Phase,
    path: String,
    model: DirModel,
    selected: Option<u32>,
    /// 目录请求代数：旧请求的批次 / 完成 / 错误一律丢弃（filer.md §4.3）。
    generation: u64,

    connect_start: Instant,
    connected_ms: Option<u128>,
    perf: ListPerf,

    last_render: Option<Instant>,
    recording: bool,
    frame_ms: Vec<f64>,
    /// 当前目录读取（后台读取任务 + 界面并入任务）；丢弃它即取消读取。
    list_task: Option<Task<()>>,
    scrolltest: Option<ScrollTest>,
    row_build_us: (u64, u64, u64),

    test: Option<FilerTest>,
}

impl FilerView {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        core: Arc<Core>,
        session: SessionId,
        mut rx: UnboundedReceiver<CoreEvent>,
        label: String,
        initial_path: Option<String>,
        connect_start: Instant,
        selftest: Option<(Duration, Option<usize>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        cx.spawn(async move |this, cx| {
            while let Some(ev) = rx.next().await {
                if this.update(cx, |t, cx| t.on_core_event(ev, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let test = selftest.map(|(timeout, expect_count)| FilerTest {
            stage: 0,
            deadline: Instant::now() + timeout,
            results: Vec::new(),
            expect_count,
            nav_from: None,
            scroll_done: None,
        });
        if test.is_some() {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    match this.update(cx, |t, cx| t.test_tick(cx)) {
                        Ok(true) => {}
                        _ => break,
                    }
                }
            })
            .detach();
        }
        FilerView {
            core,
            session,
            focus,
            palette: Palette::kanso_dark(),
            scroll: UniformListScrollHandle::new(),
            tz_min: tz::local_offset_minutes(),
            initial_path,
            label,
            phase: Phase::Connecting,
            path: String::new(),
            model: DirModel::new(Sort::default()),
            selected: None,
            generation: 0,
            connect_start,
            connected_ms: None,
            perf: ListPerf::default(),
            last_render: None,
            recording: false,
            frame_ms: Vec::new(),
            list_task: None,
            scrolltest: None,
            row_build_us: (0, 0, 0),
            test,
        }
    }

    // ---- 核心事件 ----

    fn on_core_event(&mut self, ev: CoreEvent, cx: &mut Context<Self>) {
        match ev {
            CoreEvent::SessionStateChanged { session, state } if session == self.session => {
                match state {
                    SessionState::Connected => {
                        let ms = self.connect_start.elapsed().as_millis();
                        self.connected_ms = Some(ms);
                        log(&format!(
                            "PERF 连接耗时 {ms} ms（open_session → Connected）"
                        ));
                        match self.initial_path.take() {
                            Some(p) => self.navigate(p, cx),
                            None => self.go_home(cx),
                        }
                    }
                    SessionState::Closed(reason) => {
                        let msg = format!("连接已关闭：{reason:?}");
                        log(&msg);
                        self.phase = Phase::Error(msg);
                        cx.notify();
                    }
                    SessionState::Connecting => {}
                }
            }
            CoreEvent::Prompt {
                prompt, request, ..
            } => {
                // 原型不弹提问：密钥认证不应该有提问；主机密钥变更等一律拒绝。
                log(&format!("收到认证提问，原型自动取消：{request:?}"));
                self.core.respond_prompt(prompt, PromptResponse::Cancel);
            }
            _ => {}
        }
    }

    fn go_home(&mut self, cx: &mut Context<Self>) {
        let (core, session) = (self.core.clone(), self.session);
        cx.spawn(async move |this, cx| {
            let r = core.sftp_home(session).await;
            let _ = this.update(cx, |t, cx| match r {
                Ok(home) => t.navigate(home, cx),
                Err(e) => {
                    t.phase = Phase::Error(format!("读取家目录失败：{e}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // ---- 目录读取 ----

    pub(crate) fn navigate(&mut self, path: String, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        log(&format!("列目录 {path}（请求 #{generation}）"));
        self.path = path.clone();
        self.model.clear();
        self.selected = None;
        self.scroll
            .0
            .borrow()
            .base_handle
            .set_offset(point(px(0.0), px(0.0)));
        self.phase = Phase::Listing;
        self.perf = ListPerf {
            start: Some(Instant::now()),
            ..ListPerf::default()
        };
        cx.notify();

        let mut stream = match self.core.sftp_list(self.session, &path) {
            Ok(s) => s,
            Err(e) => {
                self.phase = Phase::Error(format!("无法列出 {path}：{e}"));
                return;
            }
        };
        if std::env::var_os("KANSO_SFTP_SERIAL").is_some() {
            self.list_task = Some(Self::spawn_serial_reader(stream, generation, cx));
            return;
        }

        // 后台线程持续读取，批次进共享缓冲；界面任务被唤醒后一次取走累计的全部批次。
        let buffer = Arc::new(Mutex::new(BatchBuffer::new(generation)));
        let (wake_tx, mut wake_rx) = futures::channel::mpsc::unbounded::<()>();
        let reader = {
            let buffer = buffer.clone();
            cx.background_executor().spawn(async move {
                let t0 = Instant::now();
                let (mut waited, mut max_wait, mut n) = (Duration::ZERO, Duration::ZERO, 0usize);
                loop {
                    if buffer.lock().unwrap_or_else(|e| e.into_inner()).is_cancelled() {
                        return;
                    }
                    let w = Instant::now();
                    let r = stream.next_batch().await;
                    let dw = w.elapsed();
                    waited += dw;
                    max_wait = max_wait.max(dw);
                    let (wake, last) = {
                        let mut b = buffer.lock().unwrap_or_else(|e| e.into_inner());
                        match r {
                            Ok(Some(batch)) => {
                                n += 1;
                                (b.push(batch), false)
                            }
                            Ok(None) => (b.finish(ListEnd::Done), true),
                            Err(e) => (b.finish(ListEnd::Failed(e.to_string())), true),
                        }
                    };
                    if wake {
                        let _ = wake_tx.unbounded_send(());
                    }
                    if last {
                        log(&format!(
                            "PERF 读取线程：{n} 批，共 {} ms，其中等待服务器 {} ms（单批最长 {} ms）",
                            t0.elapsed().as_millis(),
                            waited.as_millis(),
                            max_wait.as_millis()
                        ));
                        return;
                    }
                }
            })
        };
        self.list_task = Some(cx.spawn(async move |this, cx| {
            let _reader = reader; // 随本任务一起被丢弃（取消）
            while wake_rx.next().await.is_some() {
                let keep = this.update(cx, |t, cx| t.apply_buffer(generation, &buffer, cx));
                if !keep.unwrap_or(false) {
                    return; // 视图已销毁、请求已过期或读取已结束
                }
                cx.background_executor().timer(APPLY_INTERVAL).await;
            }
        }));
    }

    /// 旧的逐批方式（诊断开关 `KANSO_SFTP_SERIAL=1`，用来与缓冲方式对比）：
    /// 在界面任务里取一批、并入、重绘，再取下一批，并记录每批的等待、并入耗时与批间间隔。
    fn spawn_serial_reader(
        mut stream: kanso_core::DirStream,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let (mut wait_tot, mut apply_tot, mut gap_tot) =
                (Duration::ZERO, Duration::ZERO, Duration::ZERO);
            let mut n = 0usize;
            let mut last_end = Instant::now();
            loop {
                let w = Instant::now();
                let r = stream.next_batch().await;
                let got = Instant::now();
                let wait = got - w;
                let gap = got - last_end; // 上一批并入结束到这一批到达
                match r {
                    Ok(Some(batch)) => {
                        let t = Instant::now();
                        let alive = this.update(cx, |t, cx| t.on_batch(generation, batch, cx));
                        let apply = t.elapsed();
                        last_end = Instant::now();
                        n += 1;
                        wait_tot += wait;
                        apply_tot += apply;
                        gap_tot += gap;
                        if n <= 5 || n.is_multiple_of(50) {
                            log(&format!(
                                "PERF 逐批 #{n}：next_batch 等待 {:.1} ms，on_batch {:.1} ms，批间间隔 {:.1} ms",
                                wait.as_secs_f64() * 1000.0,
                                apply.as_secs_f64() * 1000.0,
                                gap.as_secs_f64() * 1000.0
                            ));
                        }
                        if !alive.unwrap_or(false) {
                            return;
                        }
                    }
                    Ok(None) => {
                        log(&format!(
                            "PERF 逐批合计 {n} 批：等待 {} ms，on_batch {} ms，批间间隔 {} ms",
                            wait_tot.as_millis(),
                            apply_tot.as_millis(),
                            gap_tot.as_millis()
                        ));
                        let _ = this.update(cx, |t, cx| t.on_done(generation, cx));
                        return;
                    }
                    Err(e) => {
                        let _ = this.update(cx, |t, cx| t.on_error(generation, e.to_string(), cx));
                        return;
                    }
                }
            }
        })
    }

    /// 把缓冲里累计的批次一次并入并重绘一次。返回 `false` 表示不必再等（过期或已结束）。
    fn apply_buffer(
        &mut self,
        generation: u64,
        buffer: &Mutex<BatchBuffer>,
        cx: &mut Context<Self>,
    ) -> bool {
        let t0 = Instant::now();
        let drained = buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take(self.generation);
        let Some(d) = drained else { return false };
        if generation != self.generation {
            return false;
        }
        if !d.entries.is_empty() {
            let ms = self.perf.start.map_or(0, |s| s.elapsed().as_millis());
            if self.perf.first_batch.is_none() {
                self.perf.first_batch = Some(ms);
                self.perf.first_painted_pending = true;
                log(&format!("PERF 首批 {} 项到达，{ms} ms", d.entries.len()));
            }
            self.perf.batches += d.batches;
            self.perf.max_per_apply = self.perf.max_per_apply.max(d.batches);
            self.model.push_batch(d.entries);
            cx.notify();
        }
        self.perf.applies += 1;
        self.perf.apply_us += t0.elapsed().as_micros();
        match d.end {
            None => true,
            Some(ListEnd::Done) => {
                log(&format!(
                    "PERF 并入 {} 次，累计 {} ms，单次最多 {} 批",
                    self.perf.applies,
                    self.perf.apply_us / 1000,
                    self.perf.max_per_apply
                ));
                self.on_done(generation, cx);
                false
            }
            Some(ListEnd::Failed(msg)) => {
                self.on_error(generation, msg, cx);
                false
            }
        }
    }

    /// 返回 `false` 表示这批属于过期请求。
    fn on_batch(
        &mut self,
        generation: u64,
        batch: Vec<kanso_types::fs::DirEntry>,
        cx: &mut Context<Self>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        let ms = self.perf.start.map_or(0, |s| s.elapsed().as_millis());
        if self.perf.first_batch.is_none() {
            self.perf.first_batch = Some(ms);
            self.perf.first_painted_pending = true;
            log(&format!("PERF 首批 {} 项到达，{ms} ms", batch.len()));
        }
        self.perf.batches += 1;
        self.model.push_batch(batch);
        cx.notify();
        true
    }

    fn on_done(&mut self, generation: u64, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        let ms = self.perf.start.map_or(0, |s| s.elapsed().as_millis());
        self.perf.done = Some(ms);
        self.perf.done_painted_pending = true;
        self.phase = Phase::Done;
        log(&format!(
            "PERF 列完 {} 项（{} 批），{ms} ms；{}",
            self.model.len(),
            self.perf.batches,
            memory::describe()
        ));
        cx.notify();
    }

    fn on_error(&mut self, generation: u64, msg: String, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        log(&format!("列目录失败：{msg}"));
        self.phase = Phase::Error(format!("无法列出 {}：{msg}", self.path));
        cx.notify();
    }

    // ---- 操作 ----

    fn enter_selected(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.selected.and_then(|id| self.model.row(id)) else {
            return;
        };
        if row.is_dir {
            let path = row.path.clone();
            self.navigate(path, cx);
        }
    }

    fn go_parent(&mut self, cx: &mut Context<Self>) {
        if self.phase == Phase::Connecting || self.path.is_empty() {
            return;
        }
        let parent = parent_path(&self.path);
        if parent != self.path {
            self.navigate(parent, cx);
        }
    }

    fn set_sort(&mut self, key: SortKey, cx: &mut Context<Self>) {
        let t0 = Instant::now();
        let next = self.model.sort().toggled(key);
        self.model.set_sort(next);
        log(&format!(
            "排序 {next:?}，{} 项重排 {:.1} ms",
            self.model.len(),
            t0.elapsed().as_secs_f64() * 1000.0
        ));
        if let Some(pos) = self.selected.and_then(|id| self.model.position_of(id)) {
            self.scroll.scroll_to_item(pos, ScrollStrategy::Center);
        }
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.model.len();
        if len == 0 {
            return;
        }
        let cur = self.selected.and_then(|id| self.model.position_of(id));
        let next = match cur {
            Some(p) => p.saturating_add_signed(delta).min(len - 1),
            None if delta < 0 => len - 1,
            None => 0,
        };
        self.select_pos(next, cx);
    }

    fn select_pos(&mut self, pos: usize, cx: &mut Context<Self>) {
        if let Some((id, _)) = self.model.at(pos) {
            self.selected = Some(id);
            self.scroll.scroll_to_item(pos, ScrollStrategy::Top);
            cx.notify();
        }
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let page = ((f32::from(self.scroll.0.borrow().base_handle.bounds().size.height) / ROW_H)
            as isize)
            .max(1);
        match (ks.key.as_str(), ks.modifiers.control, ks.modifiers.alt) {
            ("down", false, false) => self.move_selection(1, cx),
            ("up", false, false) => self.move_selection(-1, cx),
            ("pagedown", ..) => self.move_selection(page, cx),
            ("pageup", ..) => self.move_selection(-page, cx),
            ("home", ..) => self.select_pos(0, cx),
            ("end", ..) => self.select_pos(self.model.len().saturating_sub(1), cx),
            ("enter", ..) => self.enter_selected(cx),
            ("backspace", false, false) | ("up", false, true) => self.go_parent(cx),
            ("f5", ..) => {
                let p = self.path.clone();
                self.navigate(p, cx);
            }
            ("1", true, _) => self.set_sort(SortKey::Name, cx),
            ("2", true, _) => self.set_sort(SortKey::Size, cx),
            ("3", true, _) => self.set_sort(SortKey::Time, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    // ---- 绘制 ----

    fn status_text(&self) -> String {
        let n = self.model.len();
        let (d, l) = (self.model.dir_count(), self.model.link_count());
        match &self.phase {
            Phase::Connecting => format!("正在连接 {}…", self.label),
            Phase::Listing => format!("正在读取… 已收到 {n} 项"),
            Phase::Done => format!(
                "{n} 项（{d} 个目录，{} 个文件，{l} 个链接）",
                n.saturating_sub(d)
            ),
            Phase::Error(e) => e.clone(),
        }
    }

    fn header_cell(
        &self,
        id: &'static str,
        text: &str,
        key: SortKey,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let sort = self.model.sort();
        let mark = if sort.key == key {
            if sort.desc { " ▼" } else { " ▲" }
        } else {
            ""
        };
        div()
            .id(id)
            .cursor_pointer()
            .child(format!("{text}{mark}"))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_sort(key, cx)))
    }

    /// 每帧调用：计时、推进自检的滚动。
    fn tick_frame(&mut self, window: &mut Window) {
        let now = Instant::now();
        if let Some(prev) = self.last_render
            && self.recording
        {
            self.frame_ms.push((now - prev).as_secs_f64() * 1000.0);
        }
        self.last_render = Some(now);

        if let Some(st) = self.scrolltest.as_mut() {
            let handle = self.scroll.0.borrow().base_handle.clone();
            let max = f32::from(handle.max_offset().height);
            let y = f32::from(handle.offset().y);
            st.frames += 1;
            if (y - st.last_y).abs() < 0.5 {
                st.stalled += 1;
            } else {
                st.stalled = 0;
            }
            st.last_y = y;
            let at_end = max > 0.0 && -y >= max - 1.0;
            if st.frames % 30 == 1 {
                log(&format!(
                    "滚动测试：第 {} 帧，offset.y={y:.0}，max_offset={max:.0}，已用 {:.1} s",
                    st.frames,
                    st.started.elapsed().as_secs_f64()
                ));
            }
            if at_end
                || st.stalled >= 5
                || st.frames > 4000
                || st.started.elapsed() > Duration::from_secs(25)
            {
                let secs = st.started.elapsed().as_secs_f64();
                let frames = st.frames;
                self.scrolltest = None;
                self.recording = false;
                let stats = frame_stats(&self.frame_ms, SLOW_FRAME_MS);
                if let (Some(t), Some(s)) = (self.test.as_mut(), stats) {
                    t.scroll_done = Some((s, frames, secs));
                }
            } else {
                let target = (y - SCROLL_ROWS_PER_FRAME * ROW_H).max(-max.max(0.0));
                handle.set_offset(point(px(0.0), px(target)));
                window.request_animation_frame();
            }
        }
    }

    fn after_paint(&mut self) {
        let Some(start) = self.perf.start else { return };
        let ms = start.elapsed().as_millis();
        if self.perf.first_painted_pending && !self.model.is_empty() {
            self.perf.first_painted_pending = false;
            self.perf.first_painted = Some(ms);
            log(&format!(
                "PERF 首批已绘制（该帧的 render 调用时刻），{ms} ms"
            ));
        }
        if self.perf.done_painted_pending && self.phase == Phase::Done {
            self.perf.done_painted_pending = false;
            self.perf.done_painted = Some(ms);
            log(&format!("PERF 列完后的首帧已绘制，{ms} ms"));
        }
    }
}

impl Focusable for FilerView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FilerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.tick_frame(window);
        self.after_paint();

        let p = &self.palette;
        let bg = rgb(p.background.to_u32());
        let fg = rgb(p.foreground.to_u32());
        let dim = rgb(p.foreground.mix(p.background, 130).to_u32());
        let line = rgb(p.background.mix(p.foreground, 40).to_u32());
        let bar = rgb(p.background.mix(p.foreground, 14).to_u32());
        let sel_bg = rgb(p.selection_background.to_u32());
        let dir_color = rgb(p.ansi[3].to_u32());
        let link_color = rgb(p.ansi[5].to_u32());
        let file_color = rgb(p.foreground.mix(p.background, 150).to_u32());
        let err = matches!(self.phase, Phase::Error(_));
        let status_color = if err { rgb(p.ansi[1].to_u32()) } else { dim };

        let list = uniform_list(
            "entries",
            self.model.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let t0 = Instant::now();
                let mut rows = Vec::with_capacity(range.len());
                for pos in range {
                    let Some((id, r)) = this.model.at(pos) else {
                        continue;
                    };
                    let selected = this.selected == Some(id);
                    let (marker, mcolor) = if r.is_symlink {
                        ("↪", link_color)
                    } else if r.is_dir {
                        ("▸", dir_color)
                    } else {
                        ("·", file_color)
                    };
                    let size = if r.is_dir {
                        "—".to_owned()
                    } else {
                        format_size(r.size)
                    };
                    let mtime = r
                        .mtime_ms
                        .map_or_else(|| "—".to_owned(), |ms| format_mtime(ms, this.tz_min));
                    rows.push(
                        div()
                            .id(("row", pos))
                            .w_full()
                            .h(px(ROW_H))
                            .px_2()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .when(selected, |d| d.bg(sel_bg))
                            .child(
                                div()
                                    .w(px(14.0))
                                    .flex_none()
                                    .text_color(mcolor)
                                    .child(marker),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .when(r.is_dir, |d| d.text_color(dir_color))
                                    .child(r.name.clone()),
                            )
                            .child(
                                div()
                                    .w(px(90.0))
                                    .flex_none()
                                    .text_right()
                                    .text_color(dim)
                                    .child(size),
                            )
                            .child(div().w(px(140.0)).flex_none().text_color(dim).child(mtime))
                            .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                                this.selected = Some(id);
                                if ev.click_count() >= 2 {
                                    this.enter_selected(cx);
                                }
                                cx.notify();
                            })),
                    );
                }
                let us = t0.elapsed().as_micros() as u64;
                let b = &mut this.row_build_us;
                *b = (b.0 + 1, b.1 + us, b.2.max(us));
                rows
            }),
        )
        .track_scroll(self.scroll.clone())
        .flex_1()
        .min_h(px(0.0));

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(fg)
            .font_family("Microsoft YaHei UI")
            .text_size(px(13.0))
            .key_context("Filer")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                div()
                    .h(px(30.0))
                    .flex_none()
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .bg(bar)
                    .child(div().text_color(dim).child(self.label.clone()))
                    .child(div().flex_1().truncate().child(self.path.clone())),
            )
            .child(
                div()
                    .h(px(24.0))
                    .flex_none()
                    .px_2()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(line)
                    .text_color(dim)
                    .child(div().w(px(14.0)).flex_none())
                    .child(
                        self.header_cell("h-name", "名称", SortKey::Name, cx)
                            .flex_1(),
                    )
                    .child(
                        self.header_cell("h-size", "大小", SortKey::Size, cx)
                            .w(px(90.0))
                            .flex_none()
                            .text_right(),
                    )
                    .child(
                        self.header_cell("h-time", "修改时间", SortKey::Time, cx)
                            .w(px(140.0))
                            .flex_none(),
                    ),
            )
            .child(list)
            .child(
                div()
                    .h(px(24.0))
                    .flex_none()
                    .px_2()
                    .flex()
                    .items_center()
                    .bg(bar)
                    .text_color(status_color)
                    .child(self.status_text()),
            )
    }
}

// ---- 自检 ----

impl FilerView {
    fn test_finish(&mut self, ok: bool, cx: &mut Context<Self>) {
        let results = self
            .test
            .as_ref()
            .map(|t| t.results.clone())
            .unwrap_or_default();
        for r in &results {
            log(&format!("selftest {r}"));
        }
        let (n, us, max) = self.row_build_us;
        log(&format!(
            "PERF 行构建：{n} 次调用，平均 {:.0} µs，最大 {max} µs",
            us as f64 / n.max(1) as f64
        ));
        log(&format!(
            "SFTP SELFTEST {}（总耗时 {} ms，{}）",
            if ok { "PASS" } else { "FAIL" },
            START.get().map_or(0, |s| s.elapsed().as_millis()),
            memory::describe()
        ));
        if !ok {
            EXIT_CODE.store(1, Ordering::SeqCst);
        }
        if ok && KEEP_OPEN.load(Ordering::Relaxed) {
            self.test = None;
            return;
        }
        cx.quit();
    }

    fn fail(&mut self, msg: String, cx: &mut Context<Self>) -> bool {
        if let Some(t) = self.test.as_mut() {
            t.results.push(format!("FAIL {msg}"));
        }
        self.test_finish(false, cx);
        false
    }

    /// 每 100 ms 推进一步；返回 `false` 表示自检已结束。
    fn test_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(t) = self.test.as_ref() else {
            return false;
        };
        let (stage, deadline) = (t.stage, t.deadline);
        if Instant::now() > deadline {
            return self.fail(format!("阶段 {stage} 超时（{:?}）", self.phase), cx);
        }
        if let Phase::Error(e) = &self.phase {
            let e = e.clone();
            return self.fail(format!("阶段 {stage} 出错：{e}"), cx);
        }
        let pass = |this: &mut Self, msg: String| {
            if let Some(t) = this.test.as_mut() {
                t.results.push(format!("PASS {msg}"));
                t.stage += 1;
            }
        };
        match stage {
            // 等第一次列目录完成。
            0 => {
                if self.phase == Phase::Done && self.perf.done_painted.is_some() {
                    let p = &self.perf;
                    let expect = self.test.as_ref().and_then(|t| t.expect_count);
                    if let Some(e) = expect
                        && e != self.model.len()
                    {
                        return self
                            .fail(format!("条目数 {} 与期望 {e} 不符", self.model.len()), cx);
                    }
                    if !self.model.is_sorted() {
                        return self.fail("列表顺序不满足排序规则".into(), cx);
                    }
                    let msg = format!(
                        "列目录 {}：{} 项，连接 {} ms，首批到达 {} ms / 绘制 {} ms，列完 {} ms / 绘制 {} ms（{} 批），已按规则排序",
                        self.path,
                        self.model.len(),
                        self.connected_ms.unwrap_or(0),
                        p.first_batch.unwrap_or(0),
                        p.first_painted.unwrap_or(0),
                        p.done.unwrap_or(0),
                        p.done_painted.unwrap_or(0),
                        p.batches
                    );
                    pass(self, msg);
                    self.frame_ms.clear();
                    self.recording = true;
                    self.last_render = None;
                    self.scrolltest = Some(ScrollTest {
                        last_y: 1.0,
                        stalled: 0,
                        frames: 0,
                        started: Instant::now(),
                    });
                    cx.notify();
                }
            }
            // 滚动到底，统计帧间隔。
            1 => {
                let done = self.test.as_mut().and_then(|t| t.scroll_done.take());
                if let Some((s, frames, secs)) = done {
                    let msg = format!(
                        "滚动 {frames} 帧 / {secs:.2} s（每帧 {SCROLL_ROWS_PER_FRAME} 行）：帧间隔 p50 {:.1} ms，p95 {:.1} ms，p99 {:.1} ms，最大 {:.1} ms，超过 {SLOW_FRAME_MS} ms 的帧 {} 个（共 {} 个样本）；{}",
                        s.p50,
                        s.p95,
                        s.p99,
                        s.max,
                        s.slow,
                        s.count,
                        memory::describe()
                    );
                    pass(self, msg);
                } else if self.scrolltest.is_none() && !self.recording {
                    // 列表不足一屏，没有可滚动的内容。
                    pass(self, "列表不足一屏，无需滚动".into());
                }
            }
            // 进入第一个子目录。
            2 => {
                let first_dir = self
                    .model
                    .at(0)
                    .filter(|(_, r)| r.is_dir)
                    .map(|(id, r)| (id, r.name.clone(), r.path.clone()));
                match first_dir {
                    Some((id, name, path)) => {
                        self.selected = Some(id);
                        if let Some(t) = self.test.as_mut() {
                            t.nav_from = Some((self.path.clone(), self.model.len()));
                        }
                        self.enter_selected(cx);
                        pass(self, format!("进入子目录 {name}（{path}）"));
                    }
                    None => {
                        pass(self, "没有子目录，跳过导航测试".into());
                        if let Some(t) = self.test.as_mut() {
                            t.stage = 5;
                        }
                    }
                }
            }
            3 => {
                if self.phase == Phase::Done {
                    let n = self.model.len();
                    self.go_parent(cx);
                    pass(self, format!("子目录读完（{n} 项），返回上一级"));
                }
            }
            4 => {
                if self.phase == Phase::Done {
                    let from = self.test.as_ref().and_then(|t| t.nav_from.clone());
                    if let Some((_, n)) = from
                        && n != self.model.len()
                    {
                        return self.fail(
                            format!("返回后条目数 {} 与进入前 {n} 不同", self.model.len()),
                            cx,
                        );
                    }
                    pass(self, format!("返回上一级成功（{} 项）", self.model.len()));
                }
            }
            // 排序：每种排序后都满足排序规则，并记录重排耗时。
            5 => {
                for key in [SortKey::Size, SortKey::Size, SortKey::Time, SortKey::Name] {
                    self.set_sort(key, cx);
                    if !self.model.is_sorted() {
                        return self.fail(format!("按 {key:?} 排序后顺序不正确"), cx);
                    }
                }
                pass(self, "大小升降序、时间、名称排序均满足排序规则".into());
                self.test_finish(true, cx);
                return self.test.is_some();
            }
            _ => return false,
        }
        true
    }
}
