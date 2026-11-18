//! 文件面板与传输界面的文字：全部是 `Message::UiFiler*` / `Message::UiXfer*`（`kanso-types` 的消息清单）
//! 加 `locales/*.ftl`，与其他界面模块一样由 `kanso-l10n` 渲染。

use kanso_types::message::Message;

use crate::app::launcher::render_message;

/// 把消息渲染成界面文字（目前界面只有简体中文）。
pub fn msg(message: Message) -> String {
    render_message(&message)
}
