//! `--term-test`：终端显示功能的内置自检（查找、时间戳栏、命令块跳转、链接识别）。
//!
//! 程序自己驱动：往活动终端写一条带关键字和 URL 的 `echo`，然后依次检查，每项在日志里打
//! `[term-test] PASS ...` 或 `FAIL ...`，最后打汇总并（没有 `--keep` 时）退出；有失败则退出码为 1。
//! 不需要截图或键鼠自动化。命令：`kanso-gpui.exe --term-test --log <日志路径>`。

use std::sync::atomic::Ordering;
use std::time::Duration;

use gpui::{AsyncWindowContext, Context, Window};
use kanso_gpui::app::find::FindResult;
use kanso_gpui::app::settings::GutterMode;

use super::shell::ShellView;
use super::{EXIT_CODE, KEEP_OPEN, log};

struct Report {
    failed: u32,
    passed: u32,
}

impl Report {
    fn check(&mut self, name: &str, ok: bool, detail: &str) {
        if ok {
            self.passed += 1;
            log(&format!("[term-test] PASS {name} {detail}"));
        } else {
            self.failed += 1;
            log(&format!("[term-test] FAIL {name} {detail}"));
        }
    }
}

async fn wait(cx: &mut AsyncWindowContext, ms: u64) {
    let ex = cx.background_executor().clone();
    ex.timer(Duration::from_millis(ms)).await;
}

impl ShellView {
    /// 在外壳启动后驱动一遍终端显示功能，结果写进日志。
    pub(super) fn run_term_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let mut r = Report {
                failed: 0,
                passed: 0,
            };
            wait(cx, 2000).await;
            // 1. 产生带关键字和 URL 的输出。
            let _ = this.update_in(cx, |this, _w, cx| {
                if let Some(t) = this.active_terminal() {
                    t.update(cx, |t, _| {
                        t.send_text("echo kanso-needle kanso-needle https://example.com/a?b=1");
                        t.send_enter();
                    });
                }
            });
            wait(cx, 1500).await;

            // 2. 查找：两处匹配；非法正则报错；关闭后清除。
            let found = this.update_in(cx, |this, w, cx| {
                let t = this.active_terminal()?;
                Some(t.update(cx, |t, cx| {
                    t.open_find(w, cx);
                    t.selftest_query("kanso-needle", false, cx);
                    t.find.result
                }))
            });
            let found = found.ok().flatten();
            let total = match found {
                Some(FindResult::Found { total, .. }) => total,
                _ => 0,
            };
            r.check("find-count", total >= 2, &format!("{found:?}"));
            let invalid = this
                .update_in(cx, |this, _w, cx| {
                    let t = this.active_terminal()?;
                    Some(t.update(cx, |t, cx| {
                        t.selftest_query("(", true, cx);
                        t.find.result
                    }))
                })
                .ok()
                .flatten();
            r.check(
                "find-invalid-regex",
                invalid == Some(FindResult::InvalidRegex),
                &format!("{invalid:?}"),
            );
            let closed = this
                .update_in(cx, |this, w, cx| {
                    let t = this.active_terminal()?;
                    Some(t.update(cx, |t, cx| {
                        t.close_find(w, cx);
                        !t.find.open
                    }))
                })
                .ok()
                .flatten();
            r.check("find-close", closed == Some(true), "");

            // 3. 链接识别。
            let url = this
                .update_in(cx, |this, _w, cx| {
                    this.active_terminal()
                        .and_then(|t| t.read(cx).selftest_find_url())
                })
                .ok()
                .flatten();
            r.check(
                "link-detect",
                url.as_deref() == Some("https://example.com/a?b=1"),
                &format!("{url:?}"),
            );

            // 4. 时间戳栏：打开后栏宽 > 0、列数变少；再关闭恢复。
            let before = this
                .update_in(cx, |this, _w, cx| {
                    this.active_terminal().map(|t| {
                        let t = t.read(cx);
                        (t.size.cols, t.gutter_mode)
                    })
                })
                .ok()
                .flatten();
            let _ = this.update_in(cx, |this, _w, cx| {
                if before.map(|b| b.1) == Some(GutterMode::Off) {
                    this.toggle_timeline(cx);
                }
            });
            wait(cx, 800).await;
            let on = this
                .update_in(cx, |this, _w, cx| {
                    this.active_terminal().map(|t| {
                        let t = t.read(cx);
                        (t.size.cols, t.gutter_mode, t.gutter_width())
                    })
                })
                .ok()
                .flatten();
            let ok = match (before, on) {
                (Some((c0, _)), Some((c1, mode, w))) => {
                    mode != GutterMode::Off && w > 0.0 && c1 < c0
                }
                _ => false,
            };
            r.check("gutter-on", ok, &format!("before={before:?} on={on:?}"));
            let _ = this.update_in(cx, |this, _w, cx| this.toggle_timeline(cx));
            wait(cx, 800).await;
            let off = this
                .update_in(cx, |this, _w, cx| {
                    this.active_terminal().map(|t| {
                        let t = t.read(cx);
                        (t.size.cols, t.gutter_width())
                    })
                })
                .ok()
                .flatten();
            r.check(
                "gutter-off",
                matches!((before, off), (Some((c0, _)), Some((c1, w))) if c1 == c0 && w == 0.0),
                &format!("{off:?}"),
            );

            // 5. 命令块跳转：本地 shell 有 OSC 133，应能选中上一个块（信息项，没有块不算失败）。
            let sel = this
                .update_in(cx, |this, _w, cx| {
                    let t = this.active_terminal()?;
                    Some(t.update(cx, |t, cx| {
                        t.jump_block(false, cx);
                        t.block_sel.selected()
                    }))
                })
                .ok()
                .flatten();
            log(&format!("[term-test] INFO block-jump selected={sel:?}"));

            log(&format!(
                "[term-test] 汇总：{} 通过，{} 失败",
                r.passed, r.failed
            ));
            if r.failed > 0 {
                EXIT_CODE.store(1, Ordering::SeqCst);
            }
            let _ = this.update_in(cx, |_this, _w, cx| {
                if !KEEP_OPEN.load(Ordering::Relaxed) {
                    cx.quit();
                }
            });
        })
        .detach();
    }
}
