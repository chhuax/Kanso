//! 终端帧快照的二进制格式（sans-IO）。
//!
//! 本 crate 只定义布局、写入器、读取器和 mock 生产者，不做任何 IO、不持有终端状态。
//! 数据流向：`kanso-term` 用 [`FrameBuilder`] 产出 [`Frame`]，`kanso-ffi` 把帧指针交给 Swift，
//! Swift 只读。完整格式说明见 `docs/frame-abi.md`，C 侧对应 `kanso_frame.h`，两者必须与本 crate 同步。
//!
//! 模块导览：
//! - [`layout`]：`#[repr(C)]` 的布局结构与全部 flag 常量。
//! - [`color`]：颜色的 `u32` 编码与解码。
//! - [`builder`]：[`FrameBuilder`] 与 [`Frame`]。
//! - [`view`]：[`FrameView`] 只读解析与校验。
//! - [`mock`]：确定性的 mock 帧，供渲染器在没有真实 core 时验证。

pub mod builder;
pub mod color;
pub mod layout;
pub mod mock;
pub mod view;

pub use builder::{CommandBlock, Frame, FrameBuilder};
pub use color::{Color, DecodedColor};
pub use layout::*;
pub use view::{FrameError, FrameView};
