//! 正式 Lucide 图标素材（ISC / MIT），编译进程序，离线启动无需额外文件。
use gpui::{AssetSource, Hsla, SharedString, Styled, Svg, px, svg};
use std::borrow::Cow;

pub(crate) struct Icons;

impl AssetSource for Icons {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let bytes: &'static [u8] = match path {
            "icons/check.svg" => include_bytes!("../../../assets/icons/check.svg"),
            "icons/chevron-down.svg" => include_bytes!("../../../assets/icons/chevron-down.svg"),
            "icons/clock.svg" => include_bytes!("../../../assets/icons/clock.svg"),
            "icons/ellipsis.svg" => include_bytes!("../../../assets/icons/ellipsis.svg"),
            "icons/folder.svg" => include_bytes!("../../../assets/icons/folder.svg"),
            "icons/list.svg" => include_bytes!("../../../assets/icons/list.svg"),
            "icons/menu.svg" => include_bytes!("../../../assets/icons/menu.svg"),
            "icons/palette.svg" => include_bytes!("../../../assets/icons/palette.svg"),
            "icons/pencil.svg" => include_bytes!("../../../assets/icons/pencil.svg"),
            "icons/plug.svg" => include_bytes!("../../../assets/icons/plug.svg"),
            "icons/plus.svg" => include_bytes!("../../../assets/icons/plus.svg"),
            "icons/save.svg" => include_bytes!("../../../assets/icons/save.svg"),
            "icons/search.svg" => include_bytes!("../../../assets/icons/search.svg"),
            "icons/server.svg" => include_bytes!("../../../assets/icons/server.svg"),
            "icons/square.svg" => include_bytes!("../../../assets/icons/square.svg"),
            "icons/square-terminal.svg" => {
                include_bytes!("../../../assets/icons/square-terminal.svg")
            }
            "icons/sun-moon.svg" => include_bytes!("../../../assets/icons/sun-moon.svg"),
            "icons/tag.svg" => include_bytes!("../../../assets/icons/tag.svg"),
            "icons/text-cursor.svg" => include_bytes!("../../../assets/icons/text-cursor.svg"),
            "icons/type.svg" => include_bytes!("../../../assets/icons/type.svg"),
            "icons/underline.svg" => include_bytes!("../../../assets/icons/underline.svg"),
            "icons/x.svg" => include_bytes!("../../../assets/icons/x.svg"),
            "icons/settings.svg" => include_bytes!("../../../assets/icons/settings.svg"),
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(bytes)))
    }

    fn list(&self, _: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

pub(crate) fn icon(name: &'static str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(format!("icons/{name}.svg"))
        .size(px(size))
        .text_color(color)
        .flex_none()
}
