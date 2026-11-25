//! `--selftest` 的外壳阶段：终端阶段（`selftest.rs`）通过后接着在真实窗口里验证
//! 多标签、分屏、焦点切换、分隔条拖动、关闭与 shell 退出的自动收尾。
//! 每一步都走与键盘 / 鼠标相同的命令路径（`run_command` / `drag_divider_by`），再读终端屏幕确认。

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gpui::{Context, Keystroke, Window, point, px};
use kanso_gpui::app::confirm::CloseRequest;
use kanso_gpui::app::layout::Rect;
use kanso_gpui::app::shell::DIVIDER_THICKNESS;
use kanso_gpui::app::shortcuts::AppCommand;
use kanso_gpui::app::workspace::CloseConfirmation;
use kanso_types::SessionId;

use super::shell::ShellView;
use super::{EXIT_CODE, KEEP_OPEN, START, log, memory};

/// 外壳阶段的自检状态。
pub(crate) struct ShellSelfTest {
    stage: u8,
    deadline: Instant,
    results: Vec<String>,
    /// 按创建顺序记录的会话：t1 是原标签，之后是新标签与两次拆分出来的窗格。
    ids: Vec<SessionId>,
    ticks: u32,
    cols_before: u16,
    /// 弹出菜单 / 对话框后停留到这个时刻，方便外部脚本截图（`KANSO_SELFTEST_HOLD_MS`，缺省不停留）。
    hold_until: Option<Instant>,
}

impl ShellSelfTest {
    pub(crate) fn new(timeout: Duration) -> Self {
        ShellSelfTest {
            stage: 0,
            deadline: Instant::now() + timeout,
            results: Vec::new(),
            ids: Vec::new(),
            ticks: 0,
            cols_before: 0,
            hold_until: None,
        }
    }
}

/// 外部截图脚本要求的停留时间（毫秒）。
fn hold_ms() -> u64 {
    std::env::var("KANSO_SELFTEST_HOLD_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// 模拟按键。必须等当前对 `ShellView` 的更新结束后再派发（按键会回到 `ShellView` 的监听器里，
/// 在更新中直接派发会重入同一个实体），所以用 `Window::defer`；多次调用按先后顺序执行。
fn key(window: &mut Window, cx: &mut Context<ShellView>, name: &str) {
    if let Ok(k) = Keystroke::parse(name) {
        window.defer(cx, move |window, cx| {
            window.dispatch_keystroke(k, cx);
        });
    }
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.x < b.max_x() - 0.5 && b.x < a.max_x() - 0.5 && a.y < b.max_y() - 0.5 && b.y < a.max_y() - 0.5
}

impl ShellView {
    pub(super) fn on_terminal_selftest(
        &mut self,
        ok: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selftest_timeout.is_none() {
            return;
        }
        if !ok {
            self.shell_selftest_finish(false, "终端阶段失败", cx);
            return;
        }
        let timeout = self.selftest_timeout.unwrap_or(Duration::from_secs(30));
        self.shell_selftest = Some(ShellSelfTest::new(timeout));
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                match this.update_in(cx, |t, window, cx| t.shell_selftest_tick(window, cx)) {
                    Ok(true) => {}
                    _ => break,
                }
            }
        })
        .detach();
    }

    fn shell_selftest_finish(&mut self, ok: bool, why: &str, cx: &mut Context<Self>) {
        if let Some(st) = &self.shell_selftest {
            for r in &st.results {
                log(&format!("selftest {r}"));
            }
        }
        if !ok {
            log(&format!("selftest FAIL {why}"));
            EXIT_CODE.store(1, Ordering::SeqCst);
        }
        log(&format!(
            "SELFTEST {}（总耗时 {} ms，{}）",
            if ok { "PASS" } else { "FAIL" },
            START.get().map_or(0, |s| s.elapsed().as_millis()),
            memory::describe()
        ));
        self.shell_selftest = None;
        if ok && KEEP_OPEN.load(Ordering::Relaxed) {
            return;
        }
        cx.quit();
    }

    // Linux 提示符较长，窄分屏会把命令标记折到下一行；按连续文本统计回显与输出。
    fn st_occurrences(&self, id: SessionId, needle: &str, cx: &Context<Self>) -> usize {
        self.terminals
            .get(&id)
            .map_or(0, |t| t.read(cx).screen.concat().matches(needle).count())
    }

    fn st_type(&self, id: SessionId, text: &str, cx: &mut Context<Self>) {
        if let Some(t) = self.terminals.get(&id) {
            t.update(cx, |t, _| {
                t.send_text(text);
                t.send_enter();
            });
        }
    }

    fn st_note(&mut self, line: String) {
        if let Some(st) = self.shell_selftest.as_mut() {
            st.results.push(line);
        }
    }

    fn st_next(&mut self) {
        if let Some(st) = self.shell_selftest.as_mut() {
            st.stage += 1;
            st.ticks = 0;
        }
    }

    fn st_fail(&mut self, msg: String, cx: &mut Context<Self>) -> bool {
        self.st_note(format!("FAIL {msg}"));
        self.shell_selftest_finish(false, &msg, cx);
        false
    }

    /// 菜单 / 对话框已经弹出：记一行日志并按需停留，外部脚本据此截图。
    fn st_hold(&mut self, name: &str) {
        let ms = hold_ms();
        log(&format!("selftest HOLD {name}"));
        if ms > 0
            && let Some(st) = self.shell_selftest.as_mut()
        {
            st.hold_until = Some(Instant::now() + Duration::from_millis(ms));
            st.deadline += Duration::from_millis(ms);
        }
    }

    fn st_pass(&mut self, msg: &str) {
        self.st_note(format!("PASS {msg}"));
        self.st_next();
    }

    /// 不管会话是否真的在运行，强制弹出关闭确认框（自检用）。
    fn show_forced_close_dialog(
        &mut self,
        id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let req = CloseRequest {
            sessions: vec![id],
            quit: false,
        };
        let c = CloseConfirmation {
            sessions: vec![id],
            interrupts_transfers: false,
        };
        self.show_close_dialog(req, &c, window, cx);
    }

    /// 每 150 ms 推进一步；返回 `false` 表示结束。
    pub(super) fn shell_selftest_tick(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(st) = self.shell_selftest.as_mut() else {
            return false;
        };
        if st.hold_until.is_some_and(|t| Instant::now() < t) {
            return true;
        }
        st.hold_until = None;
        st.ticks += 1;
        let (stage, deadline) = (st.stage, st.deadline);
        let ids = st.ids.clone();
        if Instant::now() > deadline {
            return self.st_fail(format!("外壳阶段 {stage} 超时"), cx);
        }
        let active = self.ctl.workspace().active_session();
        match stage {
            // 新标签：独立的 shell。
            0 => {
                let Some(t1) = active else {
                    return self.st_fail("没有活动会话".into(), cx);
                };
                self.run_command(AppCommand::NewLocalTab, window, cx);
                let ws = self.ctl.workspace();
                let t2 = ws.active_session();
                if ws.tab_count() != 2
                    || ws.all_panes().len() != 1
                    || t2.is_none()
                    || t2 == Some(t1)
                {
                    return self.st_fail("新建标签后应有 2 个标签、1 个分屏".into(), cx);
                }
                let t2 = t2.unwrap_or(t1);
                if let Some(st) = self.shell_selftest.as_mut() {
                    st.ids = vec![t1, t2];
                }
                self.st_type(t2, "echo KT_TAB2", cx);
                self.st_pass("Ctrl+Shift+T：新开标签并成为活动标签");
            }
            1 => {
                let (t1, t2) = (ids[0], ids[1]);
                if self.st_occurrences(t2, "KT_TAB2", cx) >= 2 {
                    if self.st_occurrences(t1, "KT_TAB2", cx) != 0 {
                        return self.st_fail("标签 2 的输入串到了标签 1".into(), cx);
                    }
                    self.st_pass("新标签里的 shell 独立往返，输入没有串到别的标签");
                    self.run_command(AppCommand::SplitRight, window, cx);
                    let ws = self.ctl.workspace();
                    let t3 = ws.active_session();
                    if ws.tab_count() != 3 || ws.all_panes().len() != 2 || t3.is_none() {
                        return self.st_fail("向右拆分后应有 3 个标签、2 个分屏".into(), cx);
                    }
                    let t3 = t3.unwrap_or(t1);
                    if let Some(st) = self.shell_selftest.as_mut() {
                        st.ids.push(t3);
                    }
                    self.st_type(t3, "echo KT_SPLIT_R", cx);
                }
            }
            2 => {
                let (t2, t3) = (ids[1], ids[2]);
                if self.st_occurrences(t3, "KT_SPLIT_R", cx) >= 2 {
                    if self.st_occurrences(t2, "KT_SPLIT_R", cx) != 0 {
                        return self.st_fail("右侧窗格的输入串到了左侧窗格".into(), cx);
                    }
                    self.st_pass("向右拆分：新窗格里的 shell 独立往返");
                    self.run_command(AppCommand::SplitDown, window, cx);
                    let ws = self.ctl.workspace();
                    let t4 = ws.active_session();
                    if ws.tab_count() != 4 || ws.all_panes().len() != 3 || t4.is_none() {
                        return self.st_fail("向下拆分后应有 4 个标签、3 个分屏".into(), cx);
                    }
                    let t4 = t4.unwrap_or(t3);
                    if let Some(st) = self.shell_selftest.as_mut() {
                        st.ids.push(t4);
                    }
                    self.st_type(t4, "echo KT_SPLIT_D", cx);
                }
            }
            3 => {
                let (t3, t4) = (ids[2], ids[3]);
                if self.st_occurrences(t4, "KT_SPLIT_D", cx) >= 2 {
                    if self.st_occurrences(t3, "KT_SPLIT_D", cx) != 0 {
                        return self.st_fail("下方窗格的输入串到了上方窗格".into(), cx);
                    }
                    self.st_note("PASS 向下拆分：三个窗格同时显示，各自的 shell 独立往返".into());
                    // 几何：三个窗格互不重叠、都不小于最小边长。
                    let area = self.split_area(window);
                    let frames = self
                        .ctl
                        .workspace()
                        .layout()
                        .frames(area, DIVIDER_THICKNESS);
                    let rects: Vec<Rect> = frames.values().copied().collect();
                    for (i, a) in rects.iter().enumerate() {
                        if a.width < 100.0 || a.height < 100.0 {
                            return self.st_fail(format!("窗格过小 {a:?}"), cx);
                        }
                        for b in &rects[i + 1..] {
                            if overlaps(*a, *b) {
                                return self.st_fail(format!("窗格重叠 {a:?} {b:?}"), cx);
                            }
                        }
                    }
                    // 切换焦点：上一个分屏 = 右上窗格（t3）。
                    self.run_command(AppCommand::PrevPane, window, cx);
                    if self.ctl.workspace().active_session() != Some(t3) {
                        return self.st_fail("Ctrl+Alt+[ 后活动会话应是右上窗格".into(), cx);
                    }
                    let focused = self
                        .terminals
                        .get(&t3)
                        .is_some_and(|t| t.read(cx).focus.is_focused(window));
                    if !focused {
                        return self.st_fail("切换分屏后键盘焦点没有跟到该窗格的终端".into(), cx);
                    }
                    self.st_type(t3, "echo KT_FOCUS_BACK", cx);
                    self.st_pass("窗格不重叠；Ctrl+Alt+[ 切换分屏，键盘焦点跟随");
                }
            }
            4 => {
                let (t1, t2, t3, t4) = (ids[0], ids[1], ids[2], ids[3]);
                if self.st_occurrences(t3, "KT_FOCUS_BACK", cx) >= 2 {
                    for other in [t1, t2, t4] {
                        if self.st_occurrences(other, "KT_FOCUS_BACK", cx) != 0 {
                            return self
                                .st_fail(format!("焦点在 {t3:?} 时输入出现在了 {other:?}"), cx);
                        }
                    }
                    self.st_note("PASS 切换焦点后输入只进入目标窗格".into());
                    // 拖动分隔条：先记录左侧窗格的列数与分隔条位置。
                    let area = self.split_area(window);
                    let before = self
                        .ctl
                        .workspace()
                        .layout()
                        .dividers(area, DIVIDER_THICKNESS)
                        .first()
                        .map_or(0.0, |d| d.rect.x);
                    let cols = self.terminals.get(&t2).map_or(0, |t| t.read(cx).size.cols);
                    if let Some(st) = self.shell_selftest.as_mut() {
                        st.cols_before = cols;
                    }
                    if !self.drag_divider_by(window, -100.0) {
                        return self.st_fail("没有可拖动的分隔条".into(), cx);
                    }
                    let after = self
                        .ctl
                        .workspace()
                        .layout()
                        .dividers(area, DIVIDER_THICKNESS)
                        .first()
                        .map_or(0.0, |d| d.rect.x);
                    if (before - after - 100.0).abs() > 3.0 {
                        return self.st_fail(
                            format!("分隔条应左移约 100 像素，实际 {before} -> {after}"),
                            cx,
                        );
                    }
                    self.st_pass(&format!("拖动分隔条：{before:.0} -> {after:.0}"));
                    cx.notify();
                }
            }
            5 => {
                // 等重绘，让终端元素按新尺寸重算行列数。
                let t2 = ids[1];
                let (ticks, before) = self
                    .shell_selftest
                    .as_ref()
                    .map_or((0, 0), |s| (s.ticks, s.cols_before));
                let cols = self.terminals.get(&t2).map_or(0, |t| t.read(cx).size.cols);
                if cols != 0 && cols < before {
                    self.st_pass(&format!("分隔条左移后左侧终端列数 {before} -> {cols}"));
                    // Alt+1：切到侧栏第一个标签（原标签，被同窗格的标签 2 盖住）。
                    self.run_command(AppCommand::SelectTab(0), window, cx);
                    if self.ctl.workspace().active_session() != Some(ids[0]) {
                        return self.st_fail("Alt+1 应切到第一个标签".into(), cx);
                    }
                    self.st_type(ids[0], "echo KT_BACK_TO_TAB1", cx);
                } else if ticks > 30 {
                    return self
                        .st_fail(format!("终端列数没有随分隔条变化：{before} -> {cols}"), cx);
                }
            }
            6 => {
                let t1 = ids[0];
                if self.st_occurrences(t1, "KT_BACK_TO_TAB1", cx) >= 2 {
                    self.st_note("PASS Alt+1 切回被盖住的标签，终端仍可输入".into());
                    self.run_command(AppCommand::CloseSession, window, cx);
                    let ws = self.ctl.workspace();
                    if ws.tab_count() != 3 || self.terminals.contains_key(&t1) {
                        return self.st_fail("关闭标签后应剩 3 个标签且终端实体释放".into(), cx);
                    }
                    if self.ctl.workspace().active_session() != Some(ids[1]) {
                        return self.st_fail("关闭标签后活动会话应是同窗格的相邻标签".into(), cx);
                    }
                    self.st_pass("Ctrl+Shift+W 关闭当前标签，活动落到同窗格的相邻标签");
                }
            }
            7 => {
                // 关掉右下窗格里唯一的标签：窗格应收起。
                let t4 = ids[3];
                self.activate_session(t4, window, cx);
                self.run_command(AppCommand::CloseSession, window, cx);
                let ws = self.ctl.workspace();
                if ws.tab_count() != 2 || ws.all_panes().len() != 2 {
                    return self.st_fail(
                        "关闭窗格里唯一的标签后应收起该窗格（2 个标签、2 个分屏）".into(),
                        cx,
                    );
                }
                self.st_pass("关闭窗格里最后一个标签：窗格收起，空间并给相邻窗格");
                // shell 自己退出（退出码 0）：标签自动关闭。
                if let Some(id) = self.ctl.workspace().active_session() {
                    self.st_type(id, "exit", cx);
                }
            }
            8 => {
                if self.ctl.workspace().tab_count() == 1 {
                    self.st_pass("shell 输入 exit：标签自动关闭");
                }
            }
            // ---- 菜单、对话框、状态栏 ----
            9 => {
                let clock = self.clock.clone();
                if clock.len() != 16 || clock.as_bytes()[4] != b'/' || clock.as_bytes()[13] != b':'
                {
                    return self.st_fail(format!("状态栏时钟格式不对：{clock:?}"), cx);
                }
                self.st_note(format!("PASS 状态栏时钟 {clock}"));
                // 应用菜单（侧栏工具条的“≡”按钮会走同一个入口）。
                self.open_app_menu(point(px(8.0), px(38.0)), window, cx);
                self.st_next();
            }
            10 => {
                if self.menu.is_none() {
                    return self.st_fail("应用菜单没有弹出".into(), cx);
                }
                self.st_hold("app-menu");
                key(window, cx, "escape");
                self.st_next();
            }
            11 => {
                if self.menu.is_some() {
                    return self.st_fail("Esc 没有关闭应用菜单".into(), cx);
                }
                self.st_note("PASS 应用菜单弹出，Esc 关闭".to_owned());
                self.run_command(AppCommand::NewLocalTab, window, cx);
                let Some(b) = self.ctl.workspace().active_session() else {
                    return self.st_fail("没有活动会话".into(), cx);
                };
                if let Some(st) = self.shell_selftest.as_mut() {
                    st.ids = vec![b];
                }
                self.open_terminal_menu(b, point(px(500.0), px(300.0)), window, cx);
                self.st_next();
            }
            12 => {
                if self.menu.is_none() {
                    return self.st_fail("终端右键菜单没有弹出".into(), cx);
                }
                self.st_hold("terminal-menu");
                key(window, cx, "escape");
                self.st_next();
            }
            13 => {
                if self.menu.is_some() {
                    return self.st_fail("Esc 没有关闭终端右键菜单".into(), cx);
                }
                self.st_note("PASS 终端右键菜单弹出，Esc 关闭".to_owned());
                let b = ids[0];
                self.open_tab_menu(b, point(px(60.0), px(80.0)), window, cx);
                self.st_next();
            }
            14 => {
                if self.menu.is_none() {
                    return self.st_fail("标签右键菜单没有弹出".into(), cx);
                }
                self.st_hold("tab-menu");
                // 第一项是“重命名…”，分隔线之后是“关闭”：两次 Down 选中它，Enter 执行（空闲的本地 shell 不需要确认）。
                key(window, cx, "down");
                key(window, cx, "down");
                key(window, cx, "enter");
                self.st_next();
            }
            15 => {
                let b = ids[0];
                if self.menu.is_some() || self.terminals.contains_key(&b) {
                    return self.st_fail("标签菜单的“关闭”没有关掉标签".into(), cx);
                }
                self.st_note("PASS 标签右键菜单：关闭 经 AppCommand 关掉标签".to_owned());
                self.run_command(AppCommand::NewLocalTab, window, cx);
                let Some(c) = self.ctl.workspace().active_session() else {
                    return self.st_fail("没有活动会话".into(), cx);
                };
                if let Some(st) = self.shell_selftest.as_mut() {
                    st.ids = vec![c];
                }
                self.show_forced_close_dialog(c, window, cx);
                self.st_next();
            }
            16 => {
                if self.dialog.is_none() {
                    return self.st_fail("关闭确认对话框没有弹出".into(), cx);
                }
                self.st_hold("close-dialog");
                // 取消：标签保留。
                key(window, cx, "escape");
                self.st_next();
            }
            17 => {
                let c = ids[0];
                if self.dialog.is_some() || !self.terminals.contains_key(&c) {
                    return self.st_fail("取消后标签应保留且对话框关闭".into(), cx);
                }
                self.st_note("PASS 关闭确认：取消（Esc）保留标签".to_owned());
                self.show_forced_close_dialog(c, window, cx);
                self.st_next();
            }
            18 => {
                if self.dialog.is_none() {
                    return self.st_fail("关闭确认对话框没有再次弹出".into(), cx);
                }
                // 默认按钮是“取消”：Tab 到“关闭”，回车确认。
                key(window, cx, "tab");
                key(window, cx, "enter");
                self.st_next();
            }
            19 => {
                let c = ids[0];
                if self.dialog.is_some() || self.terminals.contains_key(&c) {
                    return self.st_fail("确认后标签应关闭且对话框消失".into(), cx);
                }
                self.st_pass("关闭确认：Tab + 回车确认后关闭标签");
                self.run_command(AppCommand::OpenPalette, window, cx);
            }
            20 => {
                if self.palette.as_ref().map(|palette| palette.selected) != Some(0) {
                    return self.st_fail("命令面板未打开或没有选中首项".into(), cx);
                }
                key(window, cx, "down");
                self.st_next();
            }
            21 => {
                if self.palette.as_ref().map(|palette| palette.selected) != Some(1) {
                    return self.st_fail("命令面板按下方向键后没有选中第二项".into(), cx);
                }
                key(window, cx, "up");
                self.st_next();
            }
            22 => {
                if self.palette.as_ref().map(|palette| palette.selected) != Some(0) {
                    return self.st_fail("命令面板按上方向键后没有选回首项".into(), cx);
                }
                key(window, cx, "escape");
                self.st_next();
            }
            23 => {
                if self.palette.is_some() {
                    return self.st_fail("Esc 没有关闭命令面板".into(), cx);
                }
                self.st_note("PASS 命令面板上下键切换选中项，Esc 关闭".to_owned());
                self.shell_selftest_finish(true, "", cx);
                return false;
            }
            _ => return false,
        }
        true
    }
}
