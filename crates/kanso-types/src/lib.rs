//! 基础类型：各类 id、几何与颜色、用户可见消息 key 与参数、core 推送给 UI 的事件枚举。
//!
//! 本 crate 不得依赖任何其他 kanso crate。
//!
//! 模块导览：
//! - [`ids`]：持久化 id（UUID）与运行时 id（`u64`）。
//! - [`geometry`]：网格尺寸、绝对行号、颜色。
//! - [`secret`]：不可序列化、不可打印的凭据字符串。
//! - [`profile`]：会话配置、分组、快捷片段（不含任何凭据）。
//! - [`display`]：下发给终端内核的显示选项（回滚行数、默认光标样式）。
//! - [`agent`]：AI 命令行 agent 种类。
//! - [`message`]：用户可见消息（key + 命名参数）与校验用的 `CATALOG`。
//! - [`prompt`]：需要用户回答的请求与应答。
//! - [`transfer`]：传输协议种类。
//! - [`input`]：键盘、鼠标、粘贴输入与时间戳显示模式。
//! - [`fs`]：远端目录条目。
//! - [`event`]：core 到 UI 的事件。

pub mod agent;
pub mod display;
pub mod event;
pub mod fs;
pub mod geometry;
pub mod ids;
pub mod input;
pub mod message;
pub mod profile;
pub mod prompt;
pub mod secret;
pub mod transfer;

pub use display::{CursorShape, TerminalOptions};
pub use event::CoreEvent;
pub use fs::DirEntry;
pub use geometry::{CellSide, GridSize, ViewportPoint};
pub use ids::{GroupId, ProfileId, PromptId, SessionId, SnippetId, TransferId};
pub use input::{
    Key, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind, PasteContent,
    TimestampMode,
};
pub use message::Message;
pub use profile::Profile;
pub use prompt::{PromptRequest, PromptResponse};
