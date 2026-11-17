//! Kanso GPUI 前端原型的入口。
//!
//! 只有 Windows 构建真正的窗口；其他平台打印提示后退出，这样三平台 CI 的
//! `cargo build / clippy / test --workspace` 都不需要额外的图形系统库。

// Release 用 GUI 子系统：双击启动不弹控制台窗口。Debug 保留控制台，方便看 eprintln 输出。
// 此时日志与 panic 信息只写进 `%LOCALAPPDATA%\Kanso\kanso-gpui.log`（见 `ui::run_guarded`）。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod ui;

#[cfg(windows)]
fn main() {
    std::process::exit(ui::run_guarded());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("kanso-gpui: 暂不支持本平台（目前只有 Windows 原型）。");
    std::process::exit(2);
}
