//! Windows / Linux 共用前端：GPUI 窗口、终端渲染与输入法接线。
//!
//! 结构：
//! - 本文件：命令行参数、日志与计时、核心装配、`run`；
//! - `shell`：窗口根视图（应用控制器 + 每个会话一个终端视图，分屏区、分隔条拖动、快捷键）；
//! - `sidebar`：左侧竖向标签栏（含右键标签菜单）；
//! - `status_bar`：底部状态栏；
//! - `view`：终端视图实体（持有会话，处理按键 / 鼠标 / 滚轮 / 输入法，`EntityInputHandler` 实现）；
//! - `element`：自绘元素（度量字体、算行列数、画背景 / 文字 / 光标 / 预编辑）；
//! - `settings` / `display`：显示设置窗口与模型、字体 / 外观的生效状态；
//! - `selftest` / `shell_selftest`：`--selftest` 自检（终端阶段 + 多标签 / 分屏阶段）；
//! - `memory`：进程内存读数（日志用）；
//! - `os_drag`：向资源管理器等其他应用发起系统文件拖放（OLE `DoDragDrop`，仅 Windows）。

mod clipboard;
mod decor;
mod demo_dialog;
mod demo_dropdown;
mod demo_input;
mod demo_menu;
mod display;
mod element;
mod filer;
mod filer_drag;
mod filer_edit;
mod filer_panel;
mod filer_panel_view;
mod filer_selftest;
mod find_bar;
mod git_panel;
mod git_selftest;
mod launcher;
mod mcp;
mod memory;
#[cfg(windows)]
mod os_drag;
mod remote_edit_selftest;
mod selftest;
mod sender;
mod session_editor;
mod settings;
mod sftp_bench;
mod shell;
mod shell_completion;
mod shell_data;
mod shell_filer;
mod shell_git;

mod shell_keys;
mod shell_launcher;
mod shell_selftest;
mod shell_sender;
mod sidebar;
mod ssh_import;
mod status_bar;
mod tab_strip;
mod term_selftest;
mod toolbar;
mod tz;
mod view;
mod widgets;
mod window_layout;

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

use shell::ShellView;

/// 进程启动时刻，所有计时日志以它为零点。
pub(crate) static START: OnceLock<Instant> = OnceLock::new();
/// 进程退出码（自检失败 / 会话异常时被改写）。
pub(crate) static EXIT_CODE: AtomicI32 = AtomicI32::new(0);
/// `--log-input`：把每个按键 / 文本 / 输入法回调写进日志（对应 mac 版 Debug 的 LoggingInputSink）。
pub(crate) static LOG_INPUT: AtomicBool = AtomicBool::new(false);
/// `--keep`：自检通过后不退出，方便截图。
pub(crate) static KEEP_OPEN: AtomicBool = AtomicBool::new(false);
/// `--settings-test`：外壳启动后自动走一遍显示设置（开窗口、改字号 / 行距 / 字体 / 外观），把结果写进日志。
pub(crate) static SETTINGS_TEST: AtomicBool = AtomicBool::new(false);
/// `--term-test`：外壳启动后自动走一遍终端显示功能（查找、时间戳栏、链接、块跳转），把 PASS / FAIL 写进日志。
pub(crate) static TERM_TEST: AtomicBool = AtomicBool::new(false);
/// `--sender-test`：外壳启动后自动展开 Sender、发一条命令，把终端屏幕里是否出现输出写进日志（PASS / FAIL）。
pub(crate) static SENDER_TEST: AtomicBool = AtomicBool::new(false);
/// `--filer-selftest`：外壳启动后自动走一遍 Filer 的本地浏览与文件操作，结果写进日志。
pub(crate) static FILER_TEST: AtomicBool = AtomicBool::new(false);
/// `--git-selftest`：外壳启动后在临时仓库里走一遍 Git 面板（摘要、差异、提交、丢弃），写 PASS / FAIL。
pub(crate) static GIT_TEST: AtomicBool = AtomicBool::new(false);
static LOG_FILE: OnceLock<Mutex<std::fs::File>> = OnceLock::new();
/// 日志文件路径（崩溃提示框里告诉用户去哪里找）。
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

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

/// GPUI 的呈现错误不会经过核心事件，必须接到宿主日志才能诊断空白窗口。
struct GraphicsLogger;

impl log::Log for GraphicsLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            log(&format!(
                "{} {}: {}",
                record.level(),
                record.target(),
                record.args()
            ));
        }
    }

    fn flush(&self) {}
}

/// 命令行参数。
pub(crate) struct Args {
    pub selftest: bool,
    pub timeout_secs: u64,
    pub data_dir: PathBuf,
    pub log_path: PathBuf,
    /// `--sftp user@host[:port][:/path]`：打开远程文件列表而不是本地终端。
    pub sftp: Option<kanso_gpui::filer::SftpSpec>,
    /// `--key`：私钥文件路径（缺省用户主目录下的 `.ssh/id_ed25519`）。
    pub key: Option<String>,
    /// `--expect-count`：自检要求目录条目数恰好等于它。
    pub expect_count: Option<usize>,
    /// `--sftp-bench`：不开窗口，只在核心层测量连接与列目录。
    pub sftp_bench: bool,
    /// `--demo-input`：只打开输入框演示窗口（不启动核心）。
    pub demo_input: bool,
    /// `--demo-dialog`：只打开模态对话框演示窗口（不启动核心）。
    pub demo_dialog: bool,
    /// `--demo-menu`：只打开右键菜单演示窗口（不启动核心）。
    pub demo_menu: bool,
    /// `--demo-dropdown`：只打开下拉选择框演示窗口（不启动核心）。
    pub demo_dropdown: bool,
    /// `--remote-edit-test`：不开窗口，用真实临时文件自检远程编辑的本地部分。
    pub remote_edit_test: bool,
}

fn parse_args() -> Args {
    #[cfg(windows)]
    let default_dir = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Kanso");
    #[cfg(target_os = "linux")]
    let default_dir = linux_data_dir(
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        home_dir(),
    );
    let mut args = Args {
        selftest: false,
        timeout_secs: 30,
        data_dir: default_dir.clone(),
        log_path: default_dir.join("kanso-gpui.log"),
        sftp: None,
        key: None,
        expect_count: None,
        sftp_bench: false,
        demo_input: false,
        demo_dialog: false,
        demo_menu: false,
        demo_dropdown: false,
        remote_edit_test: false,
    };
    let mut explicit_log = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--selftest" => args.selftest = true,
            "--log-input" => LOG_INPUT.store(true, Ordering::Relaxed),
            "--keep" => KEEP_OPEN.store(true, Ordering::Relaxed),
            "--settings-test" => SETTINGS_TEST.store(true, Ordering::Relaxed),
            "--term-test" => TERM_TEST.store(true, Ordering::Relaxed),
            "--sender-test" => SENDER_TEST.store(true, Ordering::Relaxed),
            "--filer-selftest" => FILER_TEST.store(true, Ordering::Relaxed),
            "--git-selftest" => GIT_TEST.store(true, Ordering::Relaxed),
            "--sftp" => match it
                .next()
                .and_then(|v| kanso_gpui::filer::SftpSpec::parse(&v))
            {
                Some(spec) => args.sftp = Some(spec),
                None => eprintln!("--sftp 需要 user@host[:port][:/path]"),
            },
            "--key" => args.key = it.next(),
            "--expect-count" => args.expect_count = it.next().and_then(|v| v.parse().ok()),
            "--sftp-bench" => args.sftp_bench = true,
            "--demo-input" => args.demo_input = true,
            "--demo-dialog" => args.demo_dialog = true,
            "--demo-menu" => args.demo_menu = true,
            "--demo-dropdown" => args.demo_dropdown = true,
            "--remote-edit-test" => args.remote_edit_test = true,
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

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    #[cfg(target_os = "linux")]
    let home = std::env::var_os("HOME");
    home.map(PathBuf::from)
}

#[cfg(target_os = "linux")]
fn linux_data_dir(xdg: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    xdg.filter(|path| path.is_absolute())
        .or_else(|| home.map(|path| path.join(".local/share")))
        .unwrap_or_else(std::env::temp_dir)
        .join("Kanso")
}

#[cfg(all(test, target_os = "linux"))]
#[test]
fn linux_data_directory_follows_xdg_without_reading_user_environment() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data = temp.path().join("data");
    assert_eq!(
        linux_data_dir(Some(data.clone()), Some(home.clone())),
        data.join("Kanso")
    );
    for invalid in [None, Some(PathBuf::new()), Some(PathBuf::from("relative"))] {
        assert_eq!(
            linux_data_dir(invalid, Some(home.clone())),
            home.join(".local/share/Kanso")
        );
    }
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
    // 自检使用内存替身；正常运行接入系统凭据库和核心的 known_hosts。
    if !args.selftest && !SETTINGS_TEST.load(Ordering::Relaxed) {
        platform.secrets = Arc::new(kanso_secret::PlatformSecretStore::new(BUNDLE_ID));
        platform.host_keys = None;
    }
    // 交互式桌面会话里通常没有 HOME，而核心按 `$HOME/.ssh/known_hosts` 找主机密钥：用 USERPROFILE 补上。
    let env: Vec<(String, String)> = std::env::vars().collect();
    #[cfg(windows)]
    let env = {
        let mut env = env;
        if !env.iter().any(|(k, _)| k == "HOME")
            && let Some(profile) = std::env::var_os("USERPROFILE")
        {
            env.push(("HOME".into(), profile.to_string_lossy().into_owned()));
        }
        env
    };
    Core::new_with_base_env(config, platform, env)
        .map(Arc::new)
        .map_err(|e| e.to_string())
}

/// 为 `--sftp` 目标保存一条临时的纯 SFTP Profile 并打开会话，返回会话 id。
///
/// 原型取舍：认证只做私钥文件（没有口令提问界面）；未知主机由核心自动记录到 `~/.ssh/known_hosts`
/// （ssh.md §3），主机密钥变更会收到提问，原型一律拒绝。
pub(crate) fn open_sftp_session(
    core: &Core,
    spec: &kanso_gpui::filer::SftpSpec,
    args: &Args,
) -> Result<kanso_types::SessionId, String> {
    use kanso_types::profile::{AuthMethod, Profile, ProfileKind, SshTarget};
    let key = args.key.clone().or_else(|| {
        home_dir().map(|h| {
            h.join(".ssh")
                .join("id_ed25519")
                .to_string_lossy()
                .into_owned()
        })
    });
    let id = kanso_types::ProfileId::new();
    let profile = Profile {
        id,
        name: format!("{}@{}", spec.user, spec.host),
        color: None,
        group: None,
        kind: ProfileKind::Sftp(SshTarget {
            host: spec.host.clone(),
            port: spec.port,
            user: spec.user.clone(),
            auth: AuthMethod::PrivateKey,
            private_key_path: key,
            jump: None,
            proxy_command: None,
            keepalive: kanso_types::profile::KeepaliveConfig::default(),
        }),
    };
    core.save_profile(profile).map_err(|e| e.to_string())?;
    core.open_session(
        kanso_core::OpenTarget::Profile(id),
        kanso_types::GridSize { cols: 80, rows: 24 },
    )
    .map_err(|e| e.to_string())
}

/// `--sftp`：打开远程文件列表窗口。
fn run_sftp(
    core: Arc<Core>,
    rx: futures::channel::mpsc::UnboundedReceiver<CoreEvent>,
    spec: &kanso_gpui::filer::SftpSpec,
    args: &Args,
) -> i32 {
    let connect_start = Instant::now();
    let session = match open_sftp_session(&core, spec, args) {
        Ok(s) => s,
        Err(e) => {
            log(&format!("打开 SFTP 会话失败：{e}"));
            return 1;
        }
    };
    log(&format!(
        "SFTP 会话已发起（{session:?}）：{}@{}:{}",
        spec.user, spec.host, spec.port
    ));
    let label = format!("{}@{}:{}", spec.user, spec.host, spec.port);
    let initial = spec.path.clone();
    let selftest = args.selftest.then(|| {
        (
            std::time::Duration::from_secs(args.timeout_secs),
            args.expect_count,
        )
    });
    let core_for_ui = core.clone();
    let mut rx = Some(rx);
    Application::new()
        .with_assets(widgets::icons::Icons)
        .run(move |cx: &mut App| {
            log("GPUI 应用循环已启动");
            let bounds = Bounds::centered(None, size(px(900.0), px(640.0)), cx);
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
                        title: Some("Kanso 文件".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                move |window, cx| {
                    let rx = rx.take().expect("窗口只创建一次");
                    cx.new(|cx| {
                        filer::FilerView::new(
                            core_for_ui,
                            session,
                            rx,
                            label,
                            initial,
                            connect_start,
                            selftest,
                            window,
                            cx,
                        )
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
    core.close_session(session);
    core.shutdown();
    log(&format!("退出，进程内存 {}", memory::describe()));
    EXIT_CODE.load(Ordering::SeqCst)
}

/// panic 写进运行日志（含线程名、位置与回溯）：Release 是 GUI 子系统，没有控制台可看。
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        log(&format!(
            "PANIC 线程 {}：{info}\n{}",
            thread.name().unwrap_or("<未命名>"),
            std::backtrace::Backtrace::force_capture()
        ));
        previous(info);
    }));
}

/// 弹出一个原生错误框（GUI 子系统下没有控制台，崩溃时用户只能靠它知道发生了什么）。
#[cfg(windows)]
#[allow(unsafe_code)]
fn error_box(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let (text, title) = (wide(text), wide("Kanso"));
    // SAFETY: 两个缓冲区都以 NUL 结尾且在调用期间存活；父窗口为空。
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// Linux 没有内置的模态框；崩溃信息已写进日志，这里只补一行 stderr。
#[cfg(target_os = "linux")]
fn error_box(text: &str) {
    eprintln!("{text}");
}

/// 带兜底的进程入口：主线程上未被捕获的 panic 已由 hook 写进日志，这里再弹框并以 101 退出。
pub fn run_guarded() -> i32 {
    match std::panic::catch_unwind(run) {
        Ok(code) => code,
        Err(_) => {
            let log_path = LOG_PATH.get().map(|p| p.display().to_string());
            error_box(&format!(
                "Kanso 遇到未预期的错误，需要退出。\n\n详细信息已写入日志：\n{}",
                log_path.unwrap_or_else(|| "（日志不可用）".into())
            ));
            101
        }
    }
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
    // 默认日志位置：保留上一次运行的日志（kanso-gpui.log.1），崩溃后重启不会丢掉现场。
    if args.log_path == args.data_dir.join("kanso-gpui.log") {
        let _ = std::fs::rename(&args.log_path, args.data_dir.join("kanso-gpui.log.1"));
    }
    if let Ok(f) = std::fs::File::create(&args.log_path) {
        let _ = LOG_FILE.set(Mutex::new(f));
        let _ = LOG_PATH.set(args.log_path.clone());
    }
    log::set_logger(&GraphicsLogger).expect("进程只初始化一次日志接收器");
    log::set_max_level(log::LevelFilter::Info);
    install_panic_hook();
    log(&format!(
        "kanso-gpui {} 启动，数据目录 {}",
        env!("CARGO_PKG_VERSION"),
        args.data_dir.display()
    ));

    if args.demo_input {
        return demo_input::run();
    }
    if args.demo_dialog {
        return demo_dialog::run();
    }
    if args.demo_menu {
        return demo_menu::run();
    }
    if args.demo_dropdown {
        return demo_dropdown::run();
    }
    if args.remote_edit_test {
        return remote_edit_selftest::run();
    }

    let (tx, rx) = unbounded::<CoreEvent>();
    let core = match build_core(&args, tx) {
        Ok(c) => c,
        Err(e) => {
            log(&format!("核心初始化失败：{e}"));
            return 1;
        }
    };
    log("核心已就绪");

    if let Some(spec) = args.sftp.clone() {
        return if args.sftp_bench {
            sftp_bench::run(&core, rx, &spec, &args)
        } else {
            run_sftp(core, rx, &spec, &args)
        };
    }

    let core_for_ui = core.clone();
    let themes_dir = args.data_dir.join("themes");
    let selftest = args
        .selftest
        .then(|| std::time::Duration::from_secs(args.timeout_secs));
    let mut rx = Some(rx);
    Application::new()
        .with_assets(widgets::icons::Icons)
        .run(move |cx: &mut App| {
            log("GPUI 应用循环已启动");
            // 启动器 / 编辑对话框 / 提示框里的输入框需要这组按键绑定。
            widgets::text_input::bind_keys(cx);
            widgets::text_area::bind_keys(cx);
            let bounds = window_layout::initial_bounds(&core_for_ui, cx);
            // 主窗口关闭就退出（设置窗口是附属窗口，不能让它把进程留住）。
            cx.on_window_closed(|cx| {
                if !cx
                    .windows()
                    .iter()
                    .any(|w| w.downcast::<ShellView>().is_some())
                {
                    cx.quit();
                }
            })
            .detach();
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(bounds),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Kanso".into()),
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    app_id: Some(BUNDLE_ID.into()),
                    window_decorations: cfg!(target_os = "linux")
                        .then_some(gpui::WindowDecorations::Client),
                    ..Default::default()
                },
                move |window, cx| {
                    let rx = rx.take().expect("窗口只创建一次");
                    cx.new(|cx| ShellView::new(core_for_ui, rx, selftest, themes_dir, window, cx))
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
