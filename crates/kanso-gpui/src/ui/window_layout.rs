//! 主窗口与侧栏的布局偏好，保存在当前数据目录的设置表。
use gpui::{App, Bounds, Pixels, Window, WindowBounds, point, px, size};
use kanso_core::Core;
use serde::{Deserialize, Serialize};

use super::shell::ShellView;
use kanso_gpui::app::sender::{PANEL_DEFAULT_HEIGHT, clamp_panel_height};
use kanso_gpui::widgets::style::number;

#[derive(Deserialize, Serialize)]
#[serde(default)]
pub(super) struct PanelLayout {
    pub sidebar_visible: bool,
    pub sidebar_width: f32,
    pub sender_visible: bool,
    pub sender_height: f32,
    pub right_click_menu: bool,
}

impl Default for PanelLayout {
    fn default() -> Self {
        Self {
            sidebar_visible: true,
            sidebar_width: number("spacing", "sidebar.defaultWidth"),
            sender_visible: false,
            sender_height: PANEL_DEFAULT_HEIGHT,
            right_click_menu: true,
        }
    }
}

impl PanelLayout {
    pub(super) fn load(core: &Core) -> Self {
        let mut value: Self = core
            .setting("windows.panels")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        value.sidebar_width = value
            .sidebar_width
            .max(number("spacing", "sidebar.minWidth"));
        value.sender_height = clamp_panel_height(value.sender_height);
        value
    }
}

#[derive(Deserialize, Serialize)]
struct SavedWindow {
    bounds: [f32; 4],
    maximized: bool,
}

pub(super) fn initial_bounds(core: &Core, cx: &App) -> WindowBounds {
    let saved = core
        .setting("windows.frame")
        .and_then(|s| serde_json::from_str::<SavedWindow>(&s).ok());
    if let Some(saved) = saved {
        let [x, y, w, h] = saved.bounds;
        let bounds = Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
        let title = Bounds::new(
            bounds.origin,
            size(bounds.size.width, px(super::toolbar::HEIGHT)),
        );
        if w > 0. && h > 0. && cx.displays().iter().any(|d| d.bounds().intersects(&title)) {
            return if saved.maximized {
                WindowBounds::Maximized(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            };
        }
    }
    WindowBounds::Windowed(Bounds::centered(None, size(px(1120.), px(680.)), cx))
}

impl ShellView {
    pub(super) fn save_panels(&self) {
        let layout = PanelLayout {
            sidebar_visible: self.sidebar_visible,
            sidebar_width: self.sidebar_preferred_width,
            sender_visible: self.sender.visible,
            sender_height: self.sender.height,
            right_click_menu: self.right_click_menu,
        };
        if let Err(e) = self.core.set_setting(
            "windows.panels",
            serde_json::to_string(&layout).expect("面板尺寸均为有限数值"),
        ) {
            super::log(&format!("保存面板布局失败：{e}"));
        }
    }

    pub(super) fn save_frame(&self, window: &Window) {
        let state = window.window_bounds();
        let Bounds::<Pixels> { origin, size } = state.get_bounds();
        let saved = SavedWindow {
            bounds: [
                origin.x.into(),
                origin.y.into(),
                size.width.into(),
                size.height.into(),
            ],
            maximized: matches!(state, WindowBounds::Maximized(_)),
        };
        if let Err(e) = self.core.set_setting(
            "windows.frame",
            serde_json::to_string(&saved).expect("窗口尺寸均为有限数值"),
        ) {
            super::log(&format!("保存窗口位置失败：{e}"));
        }
    }
}
