//! 可复用控件的**纯逻辑**部分（不依赖窗口系统，三个平台都能编译与测试）。
//!
//! - [`dropdown`]：下拉选择框（选项模型、过滤、键盘导航、键入跳转、浮层定位）。
//! - [`edit`]：单行文本编辑状态（缓冲、选区、按词 / 按字素簇移动、预编辑、撤销、掩码、最大长度）。
//! - [`menu`]：右键菜单（菜单模型、键盘 / 悬停导航、尺寸估算与翻转定位）。
//! - [`word`]：词边界与选词。
//! - [`undo`]：撤销 / 重做栈（带连续输入合并）。
//! - [`blink`]：光标闪烁相位。
//! - [`scroll`]：单行水平滚动（保持光标可见）。
//! - [`checklist`]：勾选列表（复选框状态、键盘光标、全选 / 全不选）。
//! - [`dialog`]：模态对话框的规格、焦点环与结果组装。
//! - [`focus_chain`]：Tab / Shift+Tab 焦点链（顺序、跳过禁用项、首尾循环）。
//! - [`style`]：输入框配色与尺寸，取自 `design/tokens.json`。
//!
//! GPUI 一侧的视图与自绘元素在 `ui/widgets/text_input.rs`（只在 Windows 编译）。

pub mod blink;
pub mod checklist;
pub mod dialog;
pub mod dropdown;
pub mod edit;
pub mod focus_chain;
pub mod lines;
pub mod menu;
pub mod scroll;
pub mod style;
pub mod undo;
pub mod word;
