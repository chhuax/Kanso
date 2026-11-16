//! Kanso 的 Windows / Linux 前端原型（GPUI）。
//!
//! 本库只放**与窗口系统无关的纯逻辑**，三个平台都能编译、都跑单元测试；
//! 真正的窗口、渲染与输入法接线在二进制（`src/ui`，只在 Windows 编译）里。
//!
//! 模块导览：
//! - [`theme`]：终端配色（取自 `design/terminal-themes`，与 macOS 前端同一份来源）。
//! - [`render`]：核心帧快照 → 渲染数据（背景矩形、文字片段、光标）。
//! - [`keys`]：GPUI 按键事件 → 核心 `KeyEvent` 的分流（哪些键编码、哪些交给文本路径）。
//! - [`ime`]：输入法预编辑状态机与预编辑的网格布局。
//! - [`filer`]：远程文件列表的条目模型（分批归并排序）、`--sftp` 参数解析、大小与时间格式。
//! - [`metrics`]：像素 ↔ 行列换算。
//! - [`app`]：与 UI 框架无关的应用模型（标签 / 分屏、事件分发、提示队列、会话列表）。

pub mod app;
pub mod filer;
pub mod ime;
pub mod keys;
pub mod metrics;
pub mod render;
pub mod theme;
