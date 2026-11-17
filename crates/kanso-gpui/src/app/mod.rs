//! 与 UI 框架无关的应用模型层：窗口里的标签 / 分屏、核心事件分发、提示队列、会话配置列表。
//!
//! 对应 mac 版 Swift 前端里的视图模型（`apple/Kanso/App/{Shell,Prompts,Sessions}` 与
//! `KansoUI/Workspace`）。GPUI 视图只负责"画"和"转发事件"：把按键 / 点击翻译成这里的方法调用，
//! 并在每帧读取这里的状态。本层不依赖 gpui，三个平台都能编译、都跑单元测试。
//!
//! 模块导览：
//! - [`layout`]：分屏布局树（比例、分隔条、几何）。
//! - [`tab`]：标签展示状态、主标题 / 副标题 / 悬停提示等纯函数。
//! - [`workspace`]：工作区（pane、标签、活动、关闭确认、事件应用）。
//! - [`backend`]：模型对核心的最小依赖（trait），`Core` 与测试替身都实现。
//! - [`events`]：核心事件到工作区更新的映射、传输计数、按帧取脏会话。
//! - [`controller`]：把工作区、事件、提示队列与核心调用串起来的应用控制器。
//! - [`prompts`]：提示队列与状态机（主机密钥变更、密码 / 口令、keyboard-interactive）。
//! - [`prompt_dialogs`]：提示 → 对话框规格 / 对话框结果 → 应答。
//! - [`launcher`]：启动器与编辑对话框的视图辅助（行展开、菜单项、确认文案）。
//! - [`sessions`]：会话启动器模型与会话编辑模型。
//! - [`shortcuts`]：应用级快捷键表。
//! - [`shell`]：窗口外壳的纯逻辑（分隔条拖动换算、关闭二次确认）。

pub mod backend;
pub mod controller;
pub mod events;
pub mod launcher;
pub mod layout;
pub mod prompt_dialogs;
pub mod prompts;
pub mod sessions;
pub mod shell;
pub mod shortcuts;
pub mod tab;
#[cfg(test)]
mod testutil;
pub mod workspace;
