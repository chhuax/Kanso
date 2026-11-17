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
//! - [`settings`]：显示设置模型（字体、外观、时间轴）与持久化。
//! - [`shortcuts`]：应用级快捷键表。
//! - [`shell`]：窗口外壳的纯逻辑（分隔条拖动换算）。
//! - [`menus`]：终端 / 标签右键菜单与窗口级应用菜单的内容，菜单项 id 与 [`shortcuts::AppCommand`] 互转。
//! - [`confirm`]：关闭 / 退出确认对话框的内容与结果判断。
//! - [`find`] / [`gutter`] / [`blocks`] / [`links`]：终端内查找、时间戳 / 行号栏、命令块、链接的纯逻辑。
//! - [`sender`]：底部 Sender 面板的纯逻辑（草稿 / 编辑、保存作用域、分页、重复参数、菜单、文字）。
//! - [`completion`]：命令补全弹窗的纯逻辑（请求代次、键盘状态机、位置、设置、来源判断）。
//! - [`data_transfer`]：`.kanso` 导出 / 导入、OpenSSH 配置导入的文件名、结果摘要与候选主机勾选模型。
//! - [`remote_edit`]：用本地应用编辑远端文件（保存检测与防抖、冲突、重试、临时副本布局与清理）。
//! - [`drag_out`]：文件面板条目拖出（远端条目的准备状态机）。
//! - [`i18n`]：界面语言设置、解析与 `Message` 渲染小函数。
//! - [`status_bar`]：状态栏的内容模型、时钟格式与刷新对齐、目录截断。

pub mod backend;
pub mod blocks;
pub mod completion;
pub mod confirm;
pub mod controller;
pub mod data_transfer;
pub mod drag_out;
pub mod events;
pub mod filer_layout;
pub mod filer_ops;
pub mod filer_state;
pub mod filer_text;
pub mod find;
pub mod fspath;
pub mod gutter;
pub mod i18n;
pub mod launcher;
pub mod layout;
pub mod links;
pub mod local_fs;
pub mod menus;
pub mod prompt_dialogs;
pub mod prompts;
pub mod remote_edit;
pub mod sender;
pub mod sessions;
pub mod settings;
pub mod shell;
pub mod shortcuts;
pub mod status_bar;
pub mod tab;
pub mod term_transfer;
#[cfg(test)]
mod testutil;
pub mod transfer_queue;
pub mod workspace;
