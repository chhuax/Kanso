//! 输入事件的镜像：键盘、鼠标、粘贴与时间戳模式。

use kanso_types::{
    Key, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind, PasteContent,
    TimestampMode,
};

use super::mirror_enum;
use super::profile::FfiViewportPoint;

/// 修饰键状态。Alt 区分左右（“Option 作为 Meta”可以只对一侧生效）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct FfiModifiers {
    /// Shift。
    pub shift: bool,
    /// Ctrl。
    pub ctrl: bool,
    /// 左 Option。
    pub alt_left: bool,
    /// 右 Option。
    pub alt_right: bool,
    /// macOS 的 ⌘。
    pub meta: bool,
}

impl From<Modifiers> for FfiModifiers {
    fn from(m: Modifiers) -> Self {
        FfiModifiers {
            shift: m.shift,
            ctrl: m.ctrl,
            alt_left: m.alt_left,
            alt_right: m.alt_right,
            meta: m.meta,
        }
    }
}

impl From<FfiModifiers> for Modifiers {
    fn from(m: FfiModifiers) -> Self {
        Modifiers {
            shift: m.shift,
            ctrl: m.ctrl,
            alt_left: m.alt_left,
            alt_right: m.alt_right,
            meta: m.meta,
        }
    }
}

/// 逻辑键。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiKey {
    /// 字符键：该键在当前布局下**不带修饰键**时产生的字符。
    Character {
        /// 字符。
        text: String,
    },
    /// 回车。
    Enter,
    /// Tab。
    Tab,
    /// 退格。
    Backspace,
    /// Esc。
    Escape,
    /// 空格。
    Space,
    /// 上。
    ArrowUp,
    /// 下。
    ArrowDown,
    /// 左。
    ArrowLeft,
    /// 右。
    ArrowRight,
    /// Home。
    Home,
    /// End。
    End,
    /// PageUp。
    PageUp,
    /// PageDown。
    PageDown,
    /// Insert。
    Insert,
    /// 向前删除。
    Delete,
    /// 功能键，`number` 为 1 到 24。
    Function {
        /// 功能键序号。
        number: u8,
    },
}

impl From<FfiKey> for Key {
    fn from(k: FfiKey) -> Self {
        match k {
            FfiKey::Character { text } => Key::Character(text),
            FfiKey::Enter => Key::Enter,
            FfiKey::Tab => Key::Tab,
            FfiKey::Backspace => Key::Backspace,
            FfiKey::Escape => Key::Escape,
            FfiKey::Space => Key::Space,
            FfiKey::ArrowUp => Key::ArrowUp,
            FfiKey::ArrowDown => Key::ArrowDown,
            FfiKey::ArrowLeft => Key::ArrowLeft,
            FfiKey::ArrowRight => Key::ArrowRight,
            FfiKey::Home => Key::Home,
            FfiKey::End => Key::End,
            FfiKey::PageUp => Key::PageUp,
            FfiKey::PageDown => Key::PageDown,
            FfiKey::Insert => Key::Insert,
            FfiKey::Delete => Key::Delete,
            FfiKey::Function { number } => Key::F(number),
        }
    }
}

impl From<Key> for FfiKey {
    fn from(k: Key) -> Self {
        match k {
            Key::Character(text) => FfiKey::Character { text },
            Key::Enter => FfiKey::Enter,
            Key::Tab => FfiKey::Tab,
            Key::Backspace => FfiKey::Backspace,
            Key::Escape => FfiKey::Escape,
            Key::Space => FfiKey::Space,
            Key::ArrowUp => FfiKey::ArrowUp,
            Key::ArrowDown => FfiKey::ArrowDown,
            Key::ArrowLeft => FfiKey::ArrowLeft,
            Key::ArrowRight => FfiKey::ArrowRight,
            Key::Home => FfiKey::Home,
            Key::End => FfiKey::End,
            Key::PageUp => FfiKey::PageUp,
            Key::PageDown => FfiKey::PageDown,
            Key::Insert => FfiKey::Insert,
            Key::Delete => FfiKey::Delete,
            Key::F(number) => FfiKey::Function { number },
        }
    }
}

mirror_enum! {
    /// 按键事件类型。
    FfiKeyEventKind <=> KeyEventKind {
        /// 按下。
        Press,
        /// 自动重复。
        Repeat,
        /// 释放。
        Release,
    }
}

/// 一次按键。IME 组字期间的按键不要发；IME 上屏文字走 `text_input`。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiKeyEvent {
    /// 逻辑键。
    pub key: FfiKey,
    /// 修饰键。
    pub mods: FfiModifiers,
    /// 系统按当前布局和修饰键产生的文本；非文本键为 `None`。
    pub text: Option<String>,
    /// 事件类型。
    pub kind: FfiKeyEventKind,
}

impl From<FfiKeyEvent> for KeyEvent {
    fn from(e: FfiKeyEvent) -> Self {
        KeyEvent {
            key: e.key.into(),
            mods: e.mods.into(),
            text: e.text,
            kind: e.kind.into(),
        }
    }
}

mirror_enum! {
    /// 鼠标按键。
    FfiMouseButton <=> MouseButton {
        /// 左键。
        Left,
        /// 中键。
        Middle,
        /// 右键。
        Right,
        /// 没有按键（单纯移动）。
        None,
    }
}

mirror_enum! {
    /// 鼠标事件类型。
    FfiMouseEventKind <=> MouseEventKind {
        /// 按下。
        Down,
        /// 抬起。
        Up,
        /// 按住移动。
        Drag,
        /// 未按键移动。
        Move,
    }
}

/// 一次鼠标事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiMouseEvent {
    /// 事件类型。
    pub kind: FfiMouseEventKind,
    /// 按键。
    pub button: FfiMouseButton,
    /// 视口内坐标。
    pub point: FfiViewportPoint,
    /// 修饰键。
    pub mods: FfiModifiers,
    /// 连击次数，从 1 起。
    pub click_count: u8,
}

impl From<FfiMouseEvent> for MouseEvent {
    fn from(e: FfiMouseEvent) -> Self {
        MouseEvent {
            kind: e.kind.into(),
            button: e.button.into(),
            point: e.point.into(),
            mods: e.mods.into(),
            click_count: e.click_count,
        }
    }
}

/// 粘贴内容（UI 读剪贴板后归类，转义与 bracketed paste 由 core 决定）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiPasteContent {
    /// 文本。
    Text {
        /// 文本内容。
        text: String,
    },
    /// 文件路径列表。
    Files {
        /// 文件路径。
        paths: Vec<String>,
    },
    /// 剪贴板里只有图片。
    ImageOnly,
}

impl From<FfiPasteContent> for PasteContent {
    fn from(c: FfiPasteContent) -> Self {
        match c {
            FfiPasteContent::Text { text } => PasteContent::Text(text),
            FfiPasteContent::Files { paths } => PasteContent::Files(paths),
            FfiPasteContent::ImageOnly => PasteContent::ImageOnly,
        }
    }
}

mirror_enum! {
    /// 时间戳栏的显示模式。
    FfiTimestampMode <=> TimestampMode {
        /// 关闭。
        Off,
        /// 仅行号。
        LineNumbers,
        /// 仅时间。
        Time,
        /// 行号与时间。
        Both,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanso_types::CellSide;
    use kanso_types::ViewportPoint;

    #[test]
    fn key_roundtrip_all_shapes() {
        let keys = [
            Key::Character("é".into()),
            Key::Enter,
            Key::Tab,
            Key::Backspace,
            Key::Escape,
            Key::Space,
            Key::ArrowUp,
            Key::ArrowDown,
            Key::ArrowLeft,
            Key::ArrowRight,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
            Key::Insert,
            Key::Delete,
            Key::F(12),
        ];
        for k in keys {
            assert_eq!(Key::from(FfiKey::from(k.clone())), k);
        }
    }

    #[test]
    fn key_event_maps_all_fields() {
        let e = FfiKeyEvent {
            key: FfiKey::Character { text: "a".into() },
            mods: FfiModifiers {
                ctrl: true,
                alt_right: true,
                ..Default::default()
            },
            text: Some("a".into()),
            kind: FfiKeyEventKind::Repeat,
        };
        let c = KeyEvent::from(e);
        assert_eq!(c.key, Key::Character("a".into()));
        assert!(c.mods.ctrl && c.mods.alt_right && !c.mods.alt_left && c.mods.alt());
        assert_eq!(c.text.as_deref(), Some("a"));
        assert_eq!(c.kind, KeyEventKind::Repeat);
    }

    #[test]
    fn mouse_and_paste_map() {
        let m = MouseEvent::from(FfiMouseEvent {
            kind: FfiMouseEventKind::Drag,
            button: FfiMouseButton::Middle,
            point: FfiViewportPoint {
                col: 1,
                row: 2,
                side: crate::ffi_types::FfiCellSide::Right,
            },
            mods: FfiModifiers {
                shift: true,
                ..Default::default()
            },
            click_count: 2,
        });
        assert_eq!(m.kind, MouseEventKind::Drag);
        assert_eq!(m.button, MouseButton::Middle);
        assert_eq!(
            m.point,
            ViewportPoint {
                col: 1,
                row: 2,
                side: CellSide::Right
            }
        );
        assert!(m.mods.shift);
        assert_eq!(m.click_count, 2);

        assert_eq!(
            PasteContent::from(FfiPasteContent::Files {
                paths: vec!["/a".into()]
            }),
            PasteContent::Files(vec!["/a".into()])
        );
        assert_eq!(
            PasteContent::from(FfiPasteContent::ImageOnly),
            PasteContent::ImageOnly
        );
        assert_eq!(
            TimestampMode::from(FfiTimestampMode::Time),
            TimestampMode::Time
        );
    }
}
