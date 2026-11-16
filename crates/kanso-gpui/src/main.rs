//! Kanso GPUI 前端原型的入口。
//!
//! 只有 Windows 构建真正的窗口；其他平台打印提示后退出，这样三平台 CI 的
//! `cargo build / clippy / test --workspace` 都不需要额外的图形系统库。

#[cfg(windows)]
mod ui;

#[cfg(windows)]
fn main() {
    std::process::exit(ui::run());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("kanso-gpui: 暂不支持本平台（目前只有 Windows 原型）。");
    std::process::exit(2);
}
