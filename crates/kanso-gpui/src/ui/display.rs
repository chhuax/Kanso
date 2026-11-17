//! 显示设置在 GPUI 一侧的生效状态：终端 / 界面字体、字号、行距，以及"强制浅色 / 深色"。
//!
//! 设置模型（`kanso_gpui::app::settings`）改变后由外壳调用 [`apply`] 写进这里，
//! 终端元素、度量与各控件在绘制时读取。放成全局状态是为了不改动既有的 `base_font()` / `measure()` 调用点，
//! 也让控件库里"跟随窗口外观"的那几行统一改成 [`window_appearance`]。

use std::sync::RwLock;
use std::sync::atomic::{AtomicU8, Ordering};

use gpui::{Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, Window, WindowAppearance};
use kanso_gpui::app::settings::{DisplaySettings, ThemeMode};
use kanso_gpui::widgets::style::Appearance;

/// 界面字体的缺省值（Windows 11 自带，中文显示正常）。
pub(crate) const DEFAULT_UI_FONT: &str = "Microsoft YaHei UI";
/// 终端字体的缺省值。
const DEFAULT_TERMINAL_FONT: &str = "Cascadia Mono";

struct State {
    term_family: String,
    nerd: Option<String>,
    term_size: f32,
    line_height: f32,
    ui_family: String,
    ui_size: f32,
    /// 最低对比度保护的目标比值；关闭为 `None`。
    min_contrast: Option<f64>,
}

static STATE: RwLock<State> = RwLock::new(State {
    term_family: String::new(),
    nerd: None,
    term_size: DisplaySettings::DEFAULT_FONT_SIZE as f32,
    line_height: 1.2,
    ui_family: String::new(),
    ui_size: DisplaySettings::DEFAULT_UI_FONT_SIZE as f32,
    min_contrast: None,
});

/// 0 跟随系统，1 强制浅色，2 强制深色。
static FORCED: AtomicU8 = AtomicU8::new(0);

/// 写入最新设置。`nerd` 是用于图标回退的已安装 Nerd Font（没有为 `None`）。
pub(crate) fn apply(display: &DisplaySettings, mode: ThemeMode, nerd: Option<String>) {
    FORCED.store(
        match mode {
            ThemeMode::System => 0,
            ThemeMode::Light => 1,
            ThemeMode::Dark => 2,
        },
        Ordering::Relaxed,
    );
    let mut s = STATE.write().unwrap_or_else(|e| e.into_inner());
    s.term_family = display.font_family.trim().to_owned();
    s.nerd = nerd;
    s.term_size = display.font_size as f32;
    s.line_height = display.line_height as f32;
    s.ui_family = display.ui_font_family.trim().to_owned();
    s.ui_size = display.ui_font_size as f32;
    s.min_contrast = (display.minimum_contrast_enabled && display.minimum_contrast_ratio > 1.0)
        .then_some(display.minimum_contrast_ratio);
}

fn read() -> std::sync::RwLockReadGuard<'static, State> {
    STATE.read().unwrap_or_else(|e| e.into_inner())
}

/// 终端字号（逻辑像素）。
pub(crate) fn terminal_font_size() -> f32 {
    read().term_size
}

/// 最低对比度保护的目标比值；关闭时为 `None`（terminal.md §15）。
pub(crate) fn min_contrast() -> Option<f64> {
    read().min_contrast
}

/// 行距倍数。
pub(crate) fn line_height() -> f32 {
    read().line_height
}

/// 界面字体族。
pub(crate) fn ui_font_family() -> String {
    let s = read();
    if s.ui_family.is_empty() {
        DEFAULT_UI_FONT.to_owned()
    } else {
        s.ui_family.clone()
    }
}

/// 界面字号。
pub(crate) fn ui_font_size() -> f32 {
    read().ui_size
}

/// 终端字体：用户指定的族优先，缺字形时依次回退到 Nerd Font（有的话）、Cascadia Mono、Consolas、中文字体与符号字体。
pub(crate) fn terminal_font() -> Font {
    let s = read();
    let mut fallbacks: Vec<String> = Vec::new();
    if let Some(n) = &s.nerd
        && *n != s.term_family
    {
        fallbacks.push(n.clone());
    }
    let family = if s.term_family.is_empty() {
        DEFAULT_TERMINAL_FONT.to_owned()
    } else {
        fallbacks.push(DEFAULT_TERMINAL_FONT.to_owned());
        s.term_family.clone()
    };
    fallbacks.extend(
        ["Consolas", "Microsoft YaHei UI", "Segoe UI Symbol"]
            .into_iter()
            .map(str::to_owned),
    );
    Font {
        family: family.into(),
        features: FontFeatures::default(),
        fallbacks: Some(FontFallbacks::from_fonts(fallbacks)),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    }
}

/// 系统当前是否深色（不看用户的强制选择）。
pub(crate) fn system_is_dark(window: &Window) -> bool {
    !matches!(
        window.appearance(),
        WindowAppearance::Light | WindowAppearance::VibrantLight
    )
}

/// 当前生效的外观：用户强制了浅色 / 深色就用它，否则跟随窗口（系统）外观。
pub(crate) fn window_appearance(window: &Window) -> Appearance {
    match FORCED.load(Ordering::Relaxed) {
        1 => Appearance::Light,
        2 => Appearance::Dark,
        _ if system_is_dark(window) => Appearance::Dark,
        _ => Appearance::Light,
    }
}

/// 当前生效的外观是否深色。
pub(crate) fn effective_dark(window: &Window) -> bool {
    window_appearance(window) == Appearance::Dark
}

/// 原生标题栏使用同一套外观色；只更新当前窗口，不改 Windows 全局主题。
#[allow(unsafe_code)] // DWM 只有原生 API；句柄与参数生命周期由本函数限定。
pub(crate) fn apply_titlebar(window: &Window) {
    use kanso_gpui::widgets::style::color;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
    };
    let handle = HasWindowHandle::window_handle(window).expect("GPUI 窗口持有原生句柄");
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        unreachable!("Windows 前端只使用 Win32 窗口");
    };
    let appearance = window_appearance(window);
    let colorref = |name| {
        let c = color(name, appearance);
        u32::from(c.r) | (u32::from(c.g) << 8) | (u32::from(c.b) << 16)
    };
    for (attribute, value) in [
        (
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            u32::from(appearance == Appearance::Dark),
        ),
        (DWMWA_CAPTION_COLOR, colorref("bg.sidebar")),
        (DWMWA_TEXT_COLOR, colorref("text.primary")),
    ] {
        // 句柄由当前 Window 借出，数值在调用期间有效；三个属性都接收 32 位值。
        let result = unsafe {
            DwmSetWindowAttribute(
                handle.hwnd.get() as _,
                attribute as _,
                (&value as *const u32).cast(),
                std::mem::size_of::<u32>() as u32,
            )
        };
        if result < 0 {
            super::log(&format!(
                "标题栏外观更新失败：{attribute} HRESULT={result:#x}"
            ));
        }
    }
}
