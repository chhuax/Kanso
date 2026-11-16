//! 命令跟踪状态机的测试：terminal.md §5.1、§5.2 的每条边界与 §6.2 的轮次。
//!
//! 用真实的 `Terminal` 当屏幕（而不是模拟的屏幕接口），这样换行、滚动、清屏、全屏切换的行为
//! 与生产路径一致。时间是手动推进的毫秒数，没有任何真实等待。

use kanso_types::agent::AgentKind;
use kanso_types::event::Activity;
use kanso_types::geometry::GridSize;

use super::*;
use crate::TermConfig;

const PROMPT: &str = "u@h:~$ ";

struct H {
    t: Terminal,
    tr: CommandTracker,
    now: u64,
    fx: Vec<Effect>,
}

impl H {
    fn new() -> H {
        H::sized(80, 24, 1000)
    }

    fn sized(cols: u16, rows: u16, scrollback: usize) -> H {
        H {
            t: Terminal::new(TermConfig {
                size: GridSize { cols, rows },
                scrollback_lines: scrollback,
            }),
            tr: CommandTracker::new(),
            now: 10_000,
            fx: Vec::new(),
        }
    }

    /// 远端输出一批字节。
    fn out(&mut self, s: &str) {
        self.t.feed(s.as_bytes(), self.now as i64);
        let events = self.t.take_events();
        let fx = self.tr.on_output(&events, &self.t, self.now);
        self.fx.extend(fx);
    }

    fn input(&mut self, kind: InputKind) {
        let fx = self.tr.on_input(kind, &self.t, self.now);
        self.fx.extend(fx);
    }

    fn advance(&mut self, ms: u64) {
        self.now += ms;
        let fx = self.tr.on_tick(&self.t, self.now);
        self.fx.extend(fx);
    }

    /// 逐字符按键，每个字符的回显立即到达（本地回显的理想情形）。
    fn type_text(&mut self, s: &str) {
        for c in s.chars() {
            self.input(InputKind::Typing);
            self.out(&c.to_string());
        }
    }

    /// 回车：按键 + 回显换行。
    fn enter(&mut self) {
        self.input(InputKind::Enter);
        self.out("\r\n");
    }

    /// 提交一条命令并等到读取延迟过去。
    fn run(&mut self, cmd: &str) {
        self.type_text(cmd);
        self.enter();
        self.advance(READ_DELAY_MS);
    }

    fn take(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.fx)
    }
}

fn submitted(text: &str, history: bool) -> Effect {
    Effect::Submitted {
        text: text.to_owned(),
        history,
    }
}

const RUNNING: Effect = Effect::Activity(Activity::CommandRunning);
const IDLE: Effect = Effect::Activity(Activity::Idle);
const DONE: Effect = Effect::Completed {
    exit_code: None,
    agent: None,
};

fn agent_done(k: AgentKind) -> Effect {
    Effect::Completed {
        exit_code: None,
        agent: Some(k),
    }
}

fn agent_idle(k: AgentKind) -> Effect {
    Effect::Activity(Activity::AgentIdle(k))
}

fn agent_working(k: AgentKind) -> Effect {
    Effect::Activity(Activity::AgentWorking(k))
}

#[test]
fn inference_basic_command_roundtrip() {
    let mut h = H::new();
    h.out(PROMPT);
    h.type_text("ls -la");
    h.enter();
    // 回车之后、读取延迟之前：还没有读到命令文本，不报运行中。
    assert!(h.take().is_empty());
    h.advance(READ_DELAY_MS);
    assert_eq!(h.take(), vec![submitted("ls -la", true), RUNNING]);
    assert!(h.tr.is_command_running());

    h.out("a b\r\n");
    assert!(h.take().is_empty());
    h.out(PROMPT);
    assert_eq!(h.take(), vec![IDLE, DONE]);
    assert!(!h.tr.is_command_running());

    // 下一条命令照常被跟踪。
    h.run("pwd");
    assert_eq!(h.take(), vec![submitted("pwd", true), RUNNING]);
}

#[test]
fn prompt_returning_within_read_delay_still_reports_command() {
    let mut h = H::new();
    h.out(PROMPT);
    h.type_text("true");
    h.enter();
    h.out(PROMPT);
    assert_eq!(
        h.take(),
        vec![submitted("true", true), RUNNING, IDLE, DONE],
        "读取延迟内提示符就回来了：命令文本要立即读，并完整报告运行与完成"
    );
}

#[test]
fn blank_enter_is_not_a_command() {
    let mut h = H::new();
    h.out(PROMPT);
    // 第一次按键就是回车：没有输入起点。
    h.enter();
    h.out(PROMPT);
    h.advance(500);
    assert!(h.take().is_empty());

    // 只敲了空格再回车：提交了，但文本为空，读取时放弃。
    h.type_text("  ");
    h.enter();
    h.advance(READ_DELAY_MS);
    h.out(PROMPT);
    assert!(h.take().is_empty());
    assert!(!h.tr.is_command_running());

    h.run("ls");
    assert_eq!(h.take(), vec![submitted("ls", true), RUNNING]);
}

#[test]
fn input_while_running_is_not_a_new_command_line() {
    let mut h = H::new();
    h.out(PROMPT);
    h.run("sudo ls");
    h.take();

    // sudo 的密码提示与用户输入：属于这条命令。
    h.out("Password: ");
    h.type_text("secret");
    h.enter();
    h.advance(1000);
    assert!(h.take().is_empty());
    assert!(h.tr.is_command_running());

    // 命令运行期间也不会替换等待的提示符特征：只有原提示符才算返回。
    h.out("sorry\r\nPassword: ");
    assert!(h.take().is_empty());
    h.out("\r\n");
    h.out(PROMPT);
    assert_eq!(h.take(), vec![IDLE, DONE]);
}

#[test]
fn quitting_top_with_q_does_not_record_a_command() {
    let mut h = H::new();
    h.out(PROMPT);
    h.run("top");
    h.take();
    // 全屏程序。
    h.out("\x1b[?1049h\x1b[2J\x1b[Htop - 12:00");
    h.input(InputKind::Typing); // q
    h.advance(READ_DELAY_MS);
    assert!(h.take().is_empty());
    // 退出全屏、提示符回来。
    h.out("\x1b[?1049l");
    h.out(PROMPT);
    assert_eq!(h.take(), vec![IDLE, DONE]);
}

#[test]
fn start_invalidated_when_screen_was_overwritten() {
    let mut h = H::new();
    h.out(PROMPT);
    h.input(InputKind::Typing);
    h.out("a");
    // 前台程序清屏重绘（ED 2 把屏幕推进回滚，光标回到左上角），输入起点所在行不再是光标所在行。
    h.out("\x1b[2J\x1b[Hfull screen app");
    h.enter();
    h.advance(READ_DELAY_MS);
    assert!(h.take().is_empty(), "作废的输入起点不能产生命令");
    assert!(!h.tr.is_command_running());

    // 之后的命令不被卡住。
    h.out(PROMPT);
    h.run("ls");
    assert_eq!(h.take(), vec![submitted("ls", true), RUNNING]);
}

#[test]
fn start_invalidated_when_line_scrolled_out_of_scrollback() {
    // 回滚只有 3 行：输出很多之后起点所在行被裁掉。
    let mut h = H::sized(80, 5, 3);
    h.out(PROMPT);
    h.input(InputKind::Typing);
    h.out("a");
    for i in 0..30 {
        h.out(&format!("line {i}\r\n"));
    }
    h.out(PROMPT);
    h.enter();
    h.advance(READ_DELAY_MS);
    assert!(h.take().is_empty());
    assert!(!h.tr.is_command_running());
}

#[test]
fn start_invalidated_after_clear_with_scrollback_erase() {
    let mut h = H::new();
    h.out(PROMPT);
    h.type_text("ab");
    h.out("\x1b[2J\x1b[3J\x1b[H");
    h.out(PROMPT);
    h.enter();
    h.advance(READ_DELAY_MS);
    assert!(h.take().is_empty());
}

#[test]
fn ctrl_c_abandons_the_line() {
    let mut h = H::new();
    h.out(PROMPT);
    h.type_text("foo");
    h.input(InputKind::Interrupt);
    h.out("^C\r\n");
    h.out(PROMPT);
    h.run("ls");
    // 不是 "foo ls"：被放弃的行不会混进下一条命令。
    assert_eq!(h.take(), vec![submitted("ls", true), RUNNING]);
}

#[test]
fn ctrl_c_while_running_is_part_of_the_command() {
    let mut h = H::new();
    h.out(PROMPT);
    h.run("sleep 100");
    h.take();
    h.input(InputKind::Interrupt);
    h.out("^C\r\n");
    assert!(h.take().is_empty());
    h.out(PROMPT);
    assert_eq!(h.take(), vec![IDLE, DONE]);
}

#[test]
fn multiline_paste_invalidates_the_input_start() {
    let mut h = H::new();
    h.out(PROMPT);
    h.type_text("a");
    h.input(InputKind::PasteMultiline);
    h.out("b\r\nc\r\n");
    h.out(PROMPT);
    // 粘贴之后的第一条命令不跟踪（光标已经不在提示符末尾），回车之后重新就绪。
    h.type_text("ls");
    h.enter();
    h.advance(READ_DELAY_MS);
    assert!(h.take().is_empty());
    h.out(PROMPT);
    h.run("pwd");
    assert_eq!(h.take(), vec![submitted("pwd", true), RUNNING]);
}

#[test]
fn completion_reads_only_before_cursor_across_wrapped_lines() {
    let mut h = H::sized(20, 6, 100);
    h.out(PROMPT);
    h.type_text("cat long/path/to/file");
    h.out("\x1b[4D");
    assert_eq!(
        h.tr.completion_input(&h.t),
        Some(("cat long/path/to/".into(), PROMPT.len() as u16))
    );
    h.out("\x1b[4C");
    h.enter();
    h.advance(READ_DELAY_MS);
    assert!(h.tr.completion_input(&h.t).is_none());
}

#[test]
fn completion_is_invalid_after_interrupt_paste_clear_or_fullscreen() {
    for operation in 0..4 {
        let mut h = H::new();
        h.out(PROMPT);
        assert!(h.tr.completion_input(&h.t).is_none());
        h.type_text("cat f");
        assert!(h.tr.completion_input(&h.t).is_some());
        match operation {
            0 => h.input(InputKind::Interrupt),
            1 => h.input(InputKind::PasteMultiline),
            2 => h.out("\x1b[2J\x1b[3J\x1b[H"),
            _ => h.out("\x1b[?1049h"),
        }
        assert!(
            h.tr.completion_input(&h.t).is_none(),
            "operation {operation}"
        );
    }
}

#[test]
fn input_classification() {
    assert_eq!(InputKind::of_key_bytes(b"\r"), InputKind::Enter);
    assert_eq!(InputKind::of_key_bytes(&[0x03]), InputKind::Interrupt);
    assert_eq!(InputKind::of_key_bytes(b"a"), InputKind::Typing);
    assert_eq!(InputKind::of_key_bytes(b"\x1b[A"), InputKind::Typing);
    assert_eq!(InputKind::of_text("ls"), vec![InputKind::Typing]);
    assert_eq!(
        InputKind::of_text("ls\r"),
        vec![InputKind::Typing, InputKind::Enter]
    );
    assert_eq!(InputKind::of_text("\n"), vec![InputKind::Enter]);
    assert_eq!(
        InputKind::of_text("ls\r\n"),
        vec![InputKind::Typing, InputKind::Enter]
    );
    assert_eq!(
        InputKind::of_text("a\nb\n"),
        vec![InputKind::PasteMultiline]
    );
    assert_eq!(InputKind::of_text("a\nb"), vec![InputKind::PasteMultiline]);
    assert!(InputKind::of_text("\x1b[9001~2f746d70\n").is_empty());
    assert_eq!(InputKind::of_paste("ls -la"), InputKind::Typing);
    assert_eq!(InputKind::of_paste("a\nb"), InputKind::PasteMultiline);
    assert_eq!(InputKind::of_paste("ls\n"), InputKind::PasteMultiline);
}

#[test]
fn directory_widget_does_not_submit_partial_input() {
    let mut h = H::new();
    h.out(PROMPT);
    h.type_text("echo partial");
    h.take();
    for kind in InputKind::of_text("\x1b[9001~2f746d70\n") {
        h.input(kind);
    }
    h.advance(READ_DELAY_MS);
    assert!(h.take().is_empty());
    h.enter();
    h.advance(READ_DELAY_MS);
    assert!(h.take().contains(&submitted("echo partial", true)));
}

#[test]
fn zsh_right_prompt_is_cut_at_three_spaces() {
    let mut h = H::new();
    h.out(PROMPT);
    h.type_text("echo  hi");
    // 右提示符：保存光标、移到第 70 列、写出、恢复光标。
    h.out("\x1b7\x1b[70G~/proj\x1b8");
    h.enter();
    h.advance(READ_DELAY_MS);
    // 命令里两个空格保留，三个以上才截断。
    assert_eq!(h.take(), vec![submitted("echo  hi", true), RUNNING]);
}

#[test]
fn command_text_cleaning() {
    let cases: &[(&str, &str, bool)] = &[
        ("ls -la", "ls -la", true),
        ("  ls   ~/proj", "ls", false),
        (" secret", "secret", false),
        // 先去掉开头的空白再截断：开头的空格不是右提示符的分隔。
        ("   ~/proj", "~/proj", false),
        ("a  b", "a  b", true),
        ("echo hi    ", "echo hi", true),
    ];
    for (raw, text, history) in cases {
        match clean_command(raw) {
            Cleaned::Text {
                text: t,
                history: h,
            } => {
                assert_eq!((t.as_str(), h), (*text, *history), "{raw:?}");
            }
            _ => panic!("{raw:?} 应当是命令文本"),
        }
    }
    assert!(matches!(clean_command("   "), Cleaned::Blank));
    assert!(matches!(clean_command(""), Cleaned::Blank));
    let long = "x".repeat(MAX_COMMAND_CHARS + 1);
    assert!(matches!(clean_command(&long), Cleaned::TooLong));
    let max = "x".repeat(MAX_COMMAND_CHARS);
    assert!(matches!(clean_command(&max), Cleaned::Text { .. }));
}

#[test]
fn leading_space_command_is_not_recorded_in_history_but_still_runs() {
    let mut h = H::new();
    h.out(PROMPT);
    h.run(" export TOKEN=1");
    assert_eq!(h.take(), vec![submitted("export TOKEN=1", false), RUNNING]);
}

#[test]
fn overlong_command_is_discarded_but_tracking_continues() {
    let mut h = H::sized(200, 24, 1000);
    h.out(PROMPT);
    h.input(InputKind::Typing);
    h.out(&"x".repeat(MAX_COMMAND_CHARS + 10));
    h.enter();
    h.advance(READ_DELAY_MS);
    // 不产生命令文本（误捕获的输出），但命令确实提交了，仍要跟踪到返回。
    assert_eq!(h.take(), vec![RUNNING]);
    h.out(PROMPT);
    assert_eq!(h.take(), vec![IDLE, DONE]);
}

#[test]
fn wrapped_long_command_is_read_across_rows() {
    let mut h = H::sized(20, 10, 100);
    h.out(PROMPT);
    h.input(InputKind::Typing);
    h.out("echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    h.enter();
    h.advance(READ_DELAY_MS);
    assert_eq!(
        h.take(),
        vec![
            submitted("echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", true),
            RUNNING
        ]
    );
}

#[test]
fn echo_arriving_within_the_delay_is_included() {
    let mut h = H::new();
    h.out(PROMPT);
    h.input(InputKind::Typing);
    h.out("l");
    // 最后一个字符的回显还在路上，用户已经按了回车。
    h.input(InputKind::Typing);
    h.input(InputKind::Enter);
    h.advance(30);
    h.out("s\r\n");
    h.advance(READ_DELAY_MS);
    assert_eq!(h.take(), vec![submitted("ls", true), RUNNING]);
}

#[test]
fn snapshot_is_used_when_the_line_is_gone() {
    let mut h = H::new();
    h.out(PROMPT);
    h.type_text("clear");
    h.enter();
    // 命令输出里有 clear：屏幕与回滚都被清掉，命令行不在了。
    h.out("\x1b[2J\x1b[3J\x1b[H");
    h.advance(READ_DELAY_MS);
    assert_eq!(h.take(), vec![submitted("clear", true), RUNNING]);
    h.out(PROMPT);
    assert_eq!(h.take(), vec![IDLE, DONE]);
}

#[test]
fn osc133_takes_precedence_over_inference() {
    let mut h = H::new();
    h.out("\x1b]133;A\x07");
    h.out(&format!("{PROMPT}\x1b]133;B\x07"));
    h.type_text("ls");
    h.enter();
    h.advance(5000);
    // 有 133 时，命令何时开始由 C 决定，推断不会自行转成运行中。
    assert!(h.take().is_empty());

    h.out("\x1b]133;C\x07");
    assert_eq!(h.take(), vec![submitted("ls", true), RUNNING]);

    // 输出里出现与提示符同特征的文本：不被推断当成返回。
    h.out(PROMPT);
    assert!(h.take().is_empty());

    h.out("\r\n\x1b]133;D;3\x07\x1b]133;A\x07");
    assert_eq!(
        h.take(),
        vec![
            IDLE,
            Effect::Completed {
                exit_code: Some(3),
                agent: None
            }
        ]
    );

    // 空回车只有 A，没有 C：不算命令。
    h.out(&format!("{PROMPT}\x1b]133;B\x07"));
    h.enter();
    h.out("\x1b]133;A\x07");
    h.out(PROMPT);
    assert!(h.take().is_empty());
}

#[test]
fn osc133_orphan_d_is_not_a_completion() {
    let mut h = H::new();
    h.out("\x1b]133;A\x07");
    h.out("\x1b]133;D\x07");
    assert!(h.take().is_empty());
}

#[test]
fn osc133_prompt_without_d_returns_to_idle_without_completion() {
    let mut h = H::new();
    h.out(&format!("\x1b]133;A\x07{PROMPT}\x1b]133;B\x07"));
    h.run("ls");
    h.out("\x1b]133;C\x07");
    h.take();
    h.out("\x1b]133;A\x07");
    assert_eq!(h.take(), vec![IDLE]);
}

#[test]
fn osc7_while_running_means_prompt_is_back() {
    let mut h = H::new();
    h.out("u@h:/home$ ");
    h.run("cd /tmp");
    h.take();
    // 提示符里带目录：`cd` 之后特征对不上，靠目录上报弥补。
    h.out("\x1b]7;file://h/tmp\x07u@h:/tmp$ ");
    assert_eq!(h.take(), vec![IDLE, DONE]);
    h.run("ls");
    assert_eq!(h.take(), vec![submitted("ls", true), RUNNING]);
}

#[test]
fn prompt_signature_mismatch_keeps_running() {
    let mut h = H::new();
    h.out("u@h:/home$ ");
    h.run("cd /tmp");
    h.take();
    h.out("u@h:/tmp$ ");
    assert!(h.take().is_empty());
    assert!(h.tr.is_command_running());
}

#[test]
fn reset_clears_state() {
    let mut h = H::new();
    h.out(PROMPT);
    h.run("sleep 10");
    assert!(h.tr.is_command_running());
    h.tr.reset();
    assert!(!h.tr.is_command_running());
    assert_eq!(h.tr.activity(), Activity::Idle);
    assert_eq!(h.tr.next_deadline(), None);
}

// ---- AI 轮次 ----

#[test]
fn agent_launch_is_agent_idle_not_command_running() {
    let mut h = H::new();
    h.out(PROMPT);
    h.run("claude");
    assert_eq!(
        h.take(),
        vec![submitted("claude", true), agent_idle(AgentKind::Claude)]
    );
}

#[test]
fn one_shot_agent_command_is_a_plain_command() {
    let mut h = H::new();
    h.out(PROMPT);
    h.run("claude -p hi");
    assert_eq!(h.take(), vec![submitted("claude -p hi", true), RUNNING]);
}

#[test]
fn agent_turn_lifecycle() {
    let k = AgentKind::Codex;
    let mut h = H::new();
    h.out(PROMPT);
    h.run("cd x && codex");
    h.take();

    // 只有输出、没有按键：不开启轮次（TUI 启动画面、空闲重绘）。
    h.out("\x1b[?1049h welcome");
    h.advance(5000);
    h.out("redraw");
    h.advance(5000);
    assert!(h.take().is_empty());
    assert_eq!(h.tr.activity(), Activity::AgentIdle(k));

    // 用户按键开启轮次。
    h.input(InputKind::Typing);
    assert_eq!(h.take(), vec![agent_working(k)]);

    // 工具的刷新输出让轮次保持：每隔 1.5 秒一次输出，远超 2 秒也不结束。
    for _ in 0..5 {
        h.advance(1500);
        h.out("spinner");
    }
    assert!(h.take().is_empty());

    // 连续 2 秒没有输出：结束，并产生完成通知。
    h.advance(AGENT_IDLE_MS - 1);
    assert!(h.take().is_empty());
    h.advance(1);
    assert_eq!(h.take(), vec![agent_idle(k), agent_done(k)]);

    // 之后的输出不会重新开启轮次。
    h.out("late redraw");
    h.advance(10_000);
    assert!(h.take().is_empty());

    // 第二个轮次。
    h.input(InputKind::Enter);
    assert_eq!(h.take(), vec![agent_working(k)]);
}

#[test]
fn agent_turn_without_any_output_ends_after_idle_timeout() {
    let k = AgentKind::Claude;
    let mut h = H::new();
    h.out(PROMPT);
    h.run("claude");
    h.take();
    h.input(InputKind::Typing);
    h.take();
    h.advance(AGENT_IDLE_MS);
    assert_eq!(h.take(), vec![agent_idle(k), agent_done(k)]);
}

#[test]
fn agent_typing_extends_the_turn() {
    let k = AgentKind::Claude;
    let mut h = H::new();
    h.out(PROMPT);
    h.run("claude");
    h.take();
    h.input(InputKind::Typing);
    h.take();
    h.advance(1500);
    h.input(InputKind::Typing);
    h.advance(1500);
    assert!(h.take().is_empty());
    h.advance(500);
    assert_eq!(h.take(), vec![agent_idle(k), agent_done(k)]);
}

#[test]
fn key_after_a_long_silence_starts_a_fresh_turn() {
    let k = AgentKind::Claude;
    let mut h = H::new();
    h.out(PROMPT);
    h.run("claude");
    h.take();
    h.input(InputKind::Typing);
    h.take();
    // 没有定时器唤醒（例如线程还没来得及跑）时，下一次输入也要先结束旧轮次再开新的。
    h.now += 10_000;
    h.input(InputKind::Typing);
    assert_eq!(
        h.take(),
        vec![agent_idle(k), agent_done(k), agent_working(k)]
    );
}

#[test]
fn agent_exit_returns_to_idle_and_drops_the_agent() {
    let k = AgentKind::Gemini;
    let mut h = H::new();
    h.out(PROMPT);
    h.run("gemini");
    h.take();

    // 空闲时退出：只回到 Idle，不再额外通知。
    h.out("bye\r\n");
    h.out(PROMPT);
    assert_eq!(h.take(), vec![IDLE]);
    assert_eq!(h.tr.next_deadline(), None);

    // 之后是普通命令，不再有轮次。
    h.run("ls");
    assert_eq!(h.take(), vec![submitted("ls", true), RUNNING]);
    h.input(InputKind::Typing);
    assert!(h.take().is_empty());

    // 工作中退出：算这一轮完成。
    h.out(PROMPT);
    h.take();
    h.run("gemini");
    h.take();
    h.input(InputKind::Typing);
    assert_eq!(h.take(), vec![agent_working(k)]);
    h.out("\r\n");
    h.out(PROMPT);
    assert_eq!(h.take(), vec![IDLE, agent_done(k)]);
}

#[test]
fn agent_with_osc133() {
    let k = AgentKind::Claude;
    let mut h = H::new();
    h.out(&format!("\x1b]133;A\x07{PROMPT}\x1b]133;B\x07"));
    h.type_text("claude");
    h.enter();
    h.out("\x1b]133;C\x07");
    assert_eq!(h.take(), vec![submitted("claude", true), agent_idle(k)]);
    h.input(InputKind::Typing);
    assert_eq!(h.take(), vec![agent_working(k)]);
    h.advance(AGENT_IDLE_MS);
    assert_eq!(h.take(), vec![agent_idle(k), agent_done(k)]);
    h.out("\x1b]133;D;0\x07\x1b]133;A\x07");
    assert_eq!(h.take(), vec![IDLE]);
}

#[test]
fn next_deadline_tracks_pending_work() {
    let mut h = H::new();
    h.out(PROMPT);
    assert_eq!(h.tr.next_deadline(), None);
    h.type_text("claude");
    h.enter();
    assert_eq!(h.tr.next_deadline(), Some(h.now + READ_DELAY_MS));
    h.advance(READ_DELAY_MS);
    assert_eq!(h.tr.next_deadline(), None);
    h.input(InputKind::Typing);
    assert_eq!(h.tr.next_deadline(), Some(h.now + AGENT_IDLE_MS));
}

// ---- Sender 能力（§5.3）----

#[test]
fn register_send_tracks_until_prompt_returns() {
    let mut h = H::new();
    h.out(PROMPT);
    let (r, fx) = h.tr.register_send("make test", &h.t, h.now);
    assert_eq!((r, fx), (Registered::Command, vec![]));
    assert!(h.tr.is_command_running());
    // 第二次登记：上一条还没返回。
    assert_eq!(h.tr.register_send("x", &h.t, h.now).0, Registered::Busy);

    // 发送出去的文字回显、回车。
    h.out("make test\r\n");
    h.advance(READ_DELAY_MS);
    // 以登记的文本为准。
    assert_eq!(h.take(), vec![submitted("make test", true), RUNNING]);
    h.out("ok\r\n");
    h.out(PROMPT);
    assert_eq!(h.take(), vec![IDLE, DONE]);
    assert!(!h.tr.is_command_running());
}

#[test]
fn register_send_cancel_restores_idle() {
    let mut h = H::new();
    h.out(PROMPT);
    h.tr.register_send("ls", &h.t, h.now);
    assert!(h.tr.is_command_running());
    assert!(h.tr.cancel_send().is_empty());
    assert!(!h.tr.is_command_running());
    h.advance(1000);
    assert!(h.take().is_empty());

    // 已经开始运行后撤销：回到空闲。
    h.tr.register_send("ls", &h.t, h.now);
    h.advance(READ_DELAY_MS);
    assert_eq!(h.take(), vec![submitted("ls", true), RUNNING]);
    assert_eq!(h.tr.cancel_send(), vec![IDLE]);

    // 不是登记出来的命令，撤销无效。
    h.out("\r\n");
    h.out(PROMPT);
    h.run("pwd");
    h.take();
    assert!(h.tr.cancel_send().is_empty());
    assert!(h.tr.is_command_running());
}

#[test]
fn register_send_needs_a_visible_prompt() {
    let mut h = H::new();
    // 屏幕还是空的：没有提示符可当特征。
    assert_eq!(
        h.tr.register_send("ls", &h.t, h.now).0,
        Registered::Untracked
    );
    h.out("\x1b[?1049h");
    assert_eq!(
        h.tr.register_send("ls", &h.t, h.now).0,
        Registered::Untracked
    );
}

#[test]
fn register_send_to_agent_counts_as_a_turn() {
    let k = AgentKind::Claude;
    let mut h = H::new();
    h.out(PROMPT);
    h.run("claude");
    h.take();
    let (r, fx) = h.tr.register_send("explain this", &h.t, h.now);
    assert_eq!((r, fx), (Registered::Turn, vec![agent_working(k)]));
    h.advance(AGENT_IDLE_MS);
    assert_eq!(h.take(), vec![agent_idle(k), agent_done(k)]);
}
