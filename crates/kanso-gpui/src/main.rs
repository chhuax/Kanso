//! Kanso GPUI 前端原型的入口。
//!
//! Windows / Linux 共用窗口实现；macOS 使用独立的 AppKit 前端。

// Release 用 GUI 子系统：双击启动不弹控制台窗口。Debug 保留控制台，方便看 eprintln 输出。
// 此时日志与 panic 信息只写进 `%LOCALAPPDATA%\Kanso\kanso-gpui.log`（见 `ui::run_guarded`）。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(any(windows, target_os = "linux"))]
mod ui;

#[cfg(any(windows, target_os = "linux"))]
fn main() {
    std::process::exit(ui::run_guarded());
}

#[cfg(not(any(windows, target_os = "linux")))]
fn main() {
    eprintln!("kanso-gpui: 本入口支持 Windows / Linux；macOS 请使用 AppKit 版。");
    std::process::exit(2);
}
