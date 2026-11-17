//! 与 Mac KCard 共用尺寸和设计 token 的分区卡片。
use super::icons::icon;
use crate::ui::shell::Theme;
use gpui::{AnyElement, Div, FontWeight, div, prelude::*, px};

pub(crate) fn card(
    th: Theme,
    title: impl Into<String>,
    symbol: &'static str,
    rows: Vec<AnyElement>,
) -> Div {
    div()
        .flex_none()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(12.0))
        .p(px(16.0))
        .rounded(px(10.0))
        .bg(th.c("bg.card"))
        .border_1()
        .border_color(th.c("border.card"))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .child(icon(symbol, 14.0, th.c("text.secondary")))
                .child(title.into()),
        )
        .children(rows)
}
