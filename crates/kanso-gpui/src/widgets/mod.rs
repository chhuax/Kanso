//! 可复用控件的**纯逻辑**部分（不依赖窗口系统，三个平台都能编译与测试）。
//!
//! - [`edit`]：单行文本编辑状态（缓冲、选区、按词 / 按字素簇移动、预编辑、撤销、掩码、最大长度）。
//! - [`word`]：词边界与选词。
//! - [`undo`]：撤销 / 重做栈（带连续输入合并）。
//! - [`blink`]：光标闪烁相位。
//! - [`scroll`]：单行水平滚动（保持光标可见）。
//! - [`dialog`]：模态对话框的规格、焦点环与结果组装。
//! - [`style`]：输入框配色与尺寸，取自 `design/tokens.json`。
//!
//! GPUI 一侧的视图与自绘元素在 `ui/widgets/text_input.rs`（只在 Windows 编译）。

pub mod blink;
pub mod dialog;
pub mod edit;
pub mod scroll;
pub mod style;
pub mod undo;
pub mod word;
