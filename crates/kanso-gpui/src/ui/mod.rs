//! Windows 窗口前端：GPUI 窗口、终端渲染与输入法接线。
//!
//! 结构：
//! - 本文件：命令行参数、日志与计时、核心装配、`run`；
//! - `view`：终端视图实体（持有会话，处理按键 / 鼠标 / 滚轮 / 输入法，`EntityInputHandler` 实现）；
//! - `element`：自绘元素（度量字体、算行列数、画背景 / 文字 / 光标 / 预编辑）；
//! - `selftest`：`--selftest` 自检；
//! - `memory`：进程内存读数（日志用）。

mod element;
mod memory;
mod selftest;
mod view;

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::{
    App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size,
};
use kanso_core::{Core, CoreConfig, EventSink, Platform};
use kanso_types::CoreEvent;

use view::TerminalView;

/// 进程启动时刻，所有计时日志以它为零点。
pub(crate) static START: OnceLock<Instant> = OnceLock::new();
/// 进程退出码（自检失败 / 会话异常时被改写）。
pub(crate) static EXIT_CODE: AtomicI32 = AtomicI32::new(0);
/// `--log-input`：把每个按键 / 文本 / 输入法回调写进日志（对应 mac 版 Debug 的 LoggingInputSink）。
pub(crate) static LOG_INPUT: AtomicBool = AtomicBool::new(false);
/// `--keep`：自检通过后不退出，方便截图。
pub(crate) static KEEP_OPEN: AtomicBool = AtomicBool::new(false);
static LOG_FILE: OnceLock<Mutex<std::fs::File>> = OnceLock::new();

/// 应用标识：凭据管理器条目前缀与数据目录名，发布后不能改。
const BUNDLE_ID: &str = "com.chhuax.kanso";

/// 写一行日志：stderr + 日志文件，带启动以来的毫秒数。
pub(crate) fn log(msg: &str) {
    let ms = START.get().map_or(0, |s| s.elapsed().as_millis());
    let line = format!("[+{ms}ms] {msg}");
    eprintln!("{line}");
    if let Some(f) = LOG_FILE.get() {
        let mut f = f.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(f, "{line}");
    }
}

/// 命令行参数。
pub(crate) struct Args {
    pub selftest: bool,
    pub timeout_secs: u64,
    pub data_dir: PathBuf,
    pub log_path: PathBuf,
}

fn parse_args() -> Args {
    let default_dir = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Kanso");
    let mut args = Args {
        selftest: false,
        timeout_secs: 30,
        data_dir: default_dir.clone(),
        log_path: default_dir.join("kanso-gpui.log"),
    };
    let mut explicit_log = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--selftest" => args.selftest = true,
            "--log-input" => LOG_INPUT.store(true, Ordering::Relaxed),
            "--keep" => KEEP_OPEN.store(true, Ordering::Relaxed),
            "--timeout" => {
                if let Some(v) = it.next().and_then(|v| v.parse().ok()) {
                    args.timeout_secs = v;
                }
            }
            "--data-dir" => {
                if let Some(v) = it.next() {
                    args.data_dir = PathBuf::from(v);
                }
            }
            "--log" => {
                if let Some(v) = it.next() {
                    args.log_path = PathBuf::from(v);
                    explicit_log = true;
                }
            }
            other => eprintln!("忽略未知参数 {other}"),
        }
    }
    if !explicit_log {
        args.log_path = args.data_dir.join("kanso-gpui.log");
    }
    args
}

/// 把核心事件（任意工作线程）转进 UI 线程的通道。
struct ChannelSink(UnboundedSender<CoreEvent>);

impl EventSink for ChannelSink {
    fn on_event(&self, event: CoreEvent) {
        // 接收端关闭意味着应用在退出，丢弃即可。
        let _ = self.0.unbounded_send(event);
    }
}

fn build_core(args: &Args, tx: UnboundedSender<CoreEvent>) -> Result<Arc<Core>, String> {
    let config = CoreConfig::new(&args.data_dir, env!("CARGO_PKG_VERSION"), BUNDLE_ID);
    let mut platform = Platform::for_tests(Arc::new(ChannelSink(tx)));
    // Windows 凭据管理器。
    platform.secrets = Arc::new(kanso_secret::PlatformSecretStore::new(BUNDLE_ID));
    // 主机密钥用核心内置的 ~/.ssh/known_hosts（与 mac 前端一致）。
    platform.host_keys = None;
    Core::new(config, platform)
        .map(Arc::new)
        .map_err(|e| e.to_string())
}

/// 进程入口，返回退出码。
pub fn run() -> i32 {
    let t0 = Instant::now();
    let _ = START.set(t0);
    let args = parse_args();
    if let Err(e) = std::fs::create_dir_all(&args.data_dir) {
        eprintln!("无法创建数据目录 {}: {e}", args.data_dir.display());
        return 1;
    }
    if let Ok(f) = std::fs::File::create(&args.log_path) {
        let _ = LOG_FILE.set(Mutex::new(f));
    }
    log(&format!(
        "kanso-gpui {} 启动，数据目录 {}",
        env!("CARGO_PKG_VERSION"),
        args.data_dir.display()
    ));

    let (tx, rx) = unbounded::<CoreEvent>();
    let core = match build_core(&args, tx) {
        Ok(c) => c,
        Err(e) => {
            log(&format!("核心初始化失败：{e}"));
            return 1;
        }
    };
    log("核心已就绪");

    let session = match core.open_session(
        kanso_core::OpenTarget::DefaultLocalShell,
        kanso_types::GridSize {
            cols: 100,
            rows: 30,
        },
    ) {
        Ok(s) => s,
        Err(e) => {
            log(&format!("打开本地会话失败：{e}"));
            return 1;
        }
    };
    log(&format!("本地会话已打开（{session:?}）"));

    let core_for_ui = core.clone();
    let selftest = args.selftest;
    let timeout = args.timeout_secs;
    Application::new().run(move |cx: &mut App| {
        log("GPUI 应用循环已启动");
        let bounds = Bounds::centered(None, size(px(960.0), px(600.0)), cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Kanso".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                cx.new(|cx| {
                    TerminalView::new(core_for_ui, session, rx, selftest, timeout, window, cx)
                })
            },
        );
        match opened {
            Ok(_) => {
                log("窗口已创建");
                cx.activate(true);
            }
            Err(e) => {
                log(&format!("创建窗口失败：{e}"));
                EXIT_CODE.store(1, Ordering::SeqCst);
                cx.quit();
            }
        }
    });
    core.shutdown();
    log(&format!("退出，进程内存 {}", memory::describe()));
    EXIT_CODE.load(Ordering::SeqCst)
}
