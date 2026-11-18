//! `--selftest`：在真实窗口里端到端验证“会话 → 帧 → 渲染数据”这条链，
//! 并直接驱动输入法状态机与回滚。通过返回退出码 0，失败（含超时）返回 1，结果写进日志。

use std::time::{Duration, Instant};

use gpui::Context;
use kanso_types::input::Modifiers;
use kanso_types::{CellSide, ViewportPoint};

use super::view::{TerminalEvent, TerminalView};
use super::{START, log};

pub(crate) struct SelfTest {
    stage: u8,
    deadline: Instant,
    results: Vec<String>,
}

impl SelfTest {
    pub(crate) fn new(timeout: Duration) -> Self {
        SelfTest {
            stage: 0,
            deadline: Instant::now() + timeout,
            results: Vec::new(),
        }
    }
}

impl TerminalView {
    fn rows_with(&self, needle: &str) -> usize {
        self.screen.iter().filter(|r| r.contains(needle)).count()
    }

    /// 终端阶段结束：记录结果并通知外壳（`TerminalEvent::SelfTestFinished`），由外壳接着做多标签 / 分屏阶段。
    fn selftest_finish(&mut self, ok: bool, cx: &mut Context<Self>) {
        let results = self
            .selftest
            .as_ref()
            .map(|s| s.results.clone())
            .unwrap_or_default();
        for r in &results {
            log(&format!("selftest {r}"));
        }
        log("屏幕内容：");
        for (i, row) in self.screen.iter().enumerate() {
            log(&format!("  {i:02}| {row}"));
        }
        log(&format!(
            "selftest 终端阶段 {}（{} ms）",
            if ok { "PASS" } else { "FAIL" },
            START.get().map_or(0, |s| s.elapsed().as_millis()),
        ));
        self.selftest = None;
        cx.emit(TerminalEvent::SelfTestFinished(ok));
    }

    /// 每 150 ms 推进一步。返回 `false` 表示自检已结束，定时器可以停。
    pub(crate) fn selftest_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(st) = self.selftest.as_ref() else {
            return false;
        };
        let (stage, deadline) = (st.stage, st.deadline);
        if Instant::now() > deadline {
            let msg = format!("阶段 {stage} 超时");
            if let Some(st) = self.selftest.as_mut() {
                st.results.push(msg);
            }
            self.selftest_finish(false, cx);
            return false;
        }
        let pass = |this: &mut Self, msg: &str| {
            if let Some(st) = this.selftest.as_mut() {
                st.results.push(format!("PASS {msg}"));
                st.stage += 1;
            }
        };
        match stage {
            // 等第一帧里出现提示符。
            0 => {
                let ready = self.first_painted && self.screen.iter().any(|r| !r.is_empty());
                if ready {
                    pass(self, "首帧与提示符已出现");
                    self.send_text("echo KANSO_SELFTEST_OK");
                    self.send_enter();
                }
            }
            // 命令回显 + 输出 = 至少两行含标记。
            1 => {
                if self.rows_with("KANSO_SELFTEST_OK") >= 2 {
                    pass(self, "本地 shell 往返（输入 → PTY → 帧 → 渲染数据）");
                    // 模拟输入法：组字 ni，不应发给 shell；上屏“你好”后只发一次。
                    self.send_text("echo IME_");
                    self.ime_mark("ni", Some(2..2), cx);
                    let composing = self.ime.marked_range() == Some(0..2);
                    self.ime_commit("你好", cx);
                    let cleared = !self.ime.is_composing();
                    self.send_text("_OK");
                    self.send_enter();
                    if !(composing && cleared) {
                        if let Some(st) = self.selftest.as_mut() {
                            st.results.push("FAIL 输入法状态机".into());
                        }
                        self.selftest_finish(false, cx);
                        return false;
                    }
                }
            }
            // 命令回显一行，输出一行；组字串 ni 不得出现在输出里。
            2 => {
                if self.rows_with("IME_你好_OK") >= 2 {
                    let leaked = self.rows_with("IME_ni") > 0;
                    if leaked {
                        if let Some(st) = self.selftest.as_mut() {
                            st.results.push("FAIL 组字串泄漏到 shell".into());
                        }
                        self.selftest_finish(false, cx);
                        return false;
                    }
                    pass(self, "输入法上屏恰好一次、组字串未泄漏、宽字符进入网格");
                    #[cfg(windows)]
                    let command = "for /L %i in (1,1,150) do @echo LINE%i";
                    #[cfg(target_os = "linux")]
                    let command = "i=1; while [ $i -le 150 ]; do echo LINE$i; i=$((i+1)); done";
                    self.send_text(command);
                    self.send_enter();
                }
            }
            // 等输出跑完再往回滚。
            3 => {
                if self.rows_with("LINE150") >= 1 {
                    pass(self, "150 行输出完成");
                    let at = ViewportPoint {
                        col: 0,
                        row: 0,
                        side: CellSide::Left,
                    };
                    let _ = self.core.scroll(self.session, 40.0, at, Modifiers::NONE);
                }
            }
            4 => {
                let offset = self.grid.as_ref().map_or(0, |g| g.display_offset);
                if offset > 0 && self.rows_with("LINE150") == 0 {
                    pass(
                        self,
                        &format!("滚轮回滚：display_offset={offset}，最新行已滚出可见区"),
                    );
                    self.selftest_finish(true, cx);
                    return false;
                }
            }
            _ => return false,
        }
        true
    }
}
