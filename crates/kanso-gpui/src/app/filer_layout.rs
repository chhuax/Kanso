//! 辅助 Filer 面板的宽度与显隐（docs/spec/filer.md §1.1）：允许范围、窗口不足时优先保证终端、
//! 重启后恢复（存进核心设置表的一段 JSON），以及面板标题里的来源标识文字。
//!
//! 不依赖窗口系统，三个平台都能编译与测试。

use serde::{Deserialize, Serialize};

use crate::app::filer_text::{t, tf};

/// 设置表里的键。
pub const SETTING_KEY: &str = "gpui.filer";
/// 初始宽度。
pub const DEFAULT_WIDTH: f32 = 320.0;
/// 允许的最小宽度。
pub const MIN_WIDTH: f32 = 220.0;
/// 允许的最大宽度。
pub const MAX_WIDTH: f32 = 560.0;
/// 终端区域至少保留的宽度（窗口不足时面板让位）。
pub const MIN_TERMINAL_WIDTH: f32 = 300.0;
/// 面板被挤到比这更窄就干脆不显示。
const MIN_USABLE: f32 = 140.0;

/// 要保存的面板布局。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FilerLayout {
    /// 是否显示。
    pub visible: bool,
    /// 用户调整后的宽度。
    pub width: f32,
}

impl Default for FilerLayout {
    fn default() -> Self {
        FilerLayout {
            visible: false,
            width: DEFAULT_WIDTH,
        }
    }
}

impl FilerLayout {
    /// 从设置表里的 JSON 恢复；缺失或损坏退回默认，宽度夹进允许范围。
    pub fn from_json(json: Option<&str>) -> FilerLayout {
        let mut l: FilerLayout = json
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        if !l.width.is_finite() {
            l.width = DEFAULT_WIDTH;
        }
        l.width = l.width.clamp(MIN_WIDTH, MAX_WIDTH);
        l
    }

    /// 序列化。
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// 用户拖动时的新宽度：夹进允许范围。
    pub fn dragged(&self, width: f32) -> f32 {
        width.clamp(MIN_WIDTH, MAX_WIDTH)
    }

    /// 实际占用的宽度：不显示为 0；`available` 是标签栏右侧的剩余宽度，
    /// 面板在放不下时让位，保证终端至少有 [`MIN_TERMINAL_WIDTH`]；挤得太窄就不显示。
    pub fn effective_width(&self, available: f32) -> f32 {
        if !self.visible {
            return 0.0;
        }
        let room = (available - MIN_TERMINAL_WIDTH).max(0.0);
        let w = self.width.min(room);
        if w < MIN_USABLE { 0.0 } else { w }
    }
}

/// 面板标题里的来源标识：本地显示“本地”，远端显示“会话名 · 主机”（主机未知时只有会话名）。
pub fn identity_text(remote: bool, name: &str, host: Option<&str>) -> String {
    if !remote {
        return t("ui-filer-local");
    }
    match host.filter(|h| !h.is_empty() && *h != name) {
        Some(h) => tf("ui-filer-identity", &[("name", name), ("host", h)]),
        None => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_and_clamp() {
        let l = FilerLayout {
            visible: true,
            width: 400.0,
        };
        assert_eq!(FilerLayout::from_json(Some(&l.to_json())), l);
        assert_eq!(FilerLayout::from_json(None), FilerLayout::default());
        assert_eq!(FilerLayout::from_json(Some("not json")), FilerLayout::default());
        let wide = FilerLayout::from_json(Some(r#"{"visible":true,"width":9999}"#));
        assert_eq!(wide.width, MAX_WIDTH);
        let thin = FilerLayout::from_json(Some(r#"{"visible":true,"width":1}"#));
        assert_eq!(thin.width, MIN_WIDTH);
    }

    #[test]
    fn terminal_keeps_its_room() {
        let l = FilerLayout {
            visible: true,
            width: 400.0,
        };
        assert_eq!(l.effective_width(1000.0), 400.0);
        // 窗口变窄：面板让位，终端至少 300。
        assert_eq!(l.effective_width(600.0), 300.0);
        // 挤得放不下就不显示。
        assert_eq!(l.effective_width(400.0), 0.0);
        let hidden = FilerLayout {
            visible: false,
            ..l
        };
        assert_eq!(hidden.effective_width(2000.0), 0.0);
    }

    #[test]
    fn drag_stays_in_range() {
        let l = FilerLayout::default();
        assert_eq!(l.dragged(10.0), MIN_WIDTH);
        assert_eq!(l.dragged(10_000.0), MAX_WIDTH);
        assert_eq!(l.dragged(333.0), 333.0);
    }

    #[test]
    fn identity_names_local_or_remote() {
        assert_eq!(identity_text(false, "x", None), "本地");
        assert_eq!(identity_text(true, "prod", Some("10.0.0.5")), "prod · 10.0.0.5");
        assert_eq!(identity_text(true, "prod", None), "prod");
        assert_eq!(identity_text(true, "h", Some("h")), "h");
    }
}
