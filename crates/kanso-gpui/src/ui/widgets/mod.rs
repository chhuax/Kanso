//! Windows / Linux 共用的 GPUI 控件。纯逻辑部分在库里的 `kanso_gpui::widgets`。

pub(crate) mod dialog;
pub(crate) mod focus_chain;
pub(crate) mod menu;
pub(crate) mod text_area;
pub(crate) mod text_input;

pub(crate) mod dropdown;

pub(crate) mod card;
pub(crate) mod icons;

/// 持久控件可以保存字面文本或消息键；切换语言后在绘制时取当前文字。
#[derive(Clone)]
pub(crate) enum WidgetText {
    Text(gpui::SharedString),
    Message(kanso_types::Message),
}

impl WidgetText {
    pub(super) fn resolve(&self) -> gpui::SharedString {
        match self {
            Self::Text(text) => text.clone(),
            Self::Message(message) => kanso_gpui::app::i18n::t(message).into(),
        }
    }
}

impl From<kanso_types::Message> for WidgetText {
    fn from(message: kanso_types::Message) -> Self {
        Self::Message(message)
    }
}

impl From<String> for WidgetText {
    fn from(text: String) -> Self {
        Self::Text(text.into())
    }
}

impl From<&'static str> for WidgetText {
    fn from(text: &'static str) -> Self {
        Self::Text(text.into())
    }
}
