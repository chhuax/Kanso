//! 设置窗口的“键盘”页（ui-shell.md §7）：列出全部可改绑的命令（分组、本地化名称、当前快捷键），
//! 点击某行的按键框进入录制，按下新组合即绑定；Esc 取消，Backspace 清空；与别的命令冲突时在页内询问
//! “替换 / 取消”；每行可单独恢复默认，页顶可全部恢复默认。Alt+1–9 固定，显示但不可改。
//!
//! 纯逻辑（组合键、冲突、保留键、覆盖合并）在 `kanso_gpui::app::keybindings`；这里只管状态与绘制。
//! 本文件是 `settings.rs` 的子模块，可以直接使用它的私有绘制辅助。

use gpui::{
    AnyElement, Context, KeyDownEvent, MouseButton, MouseDownEvent, Window, div, prelude::*, px,
};
use kanso_gpui::app::keybindings::{
    self, AssignResult, Chord, Entry, Group, KeybindingSettings, Recorded, chords_text,
    command_label, group_label,
};
use kanso_gpui::app::launcher::render_message;
use kanso_gpui::keys::KeyInput;
use kanso_gpui::widgets::style::number;
use kanso_types::message::Message;

use super::{SettingsView, Th, caption, card, icon};

/// 等待用户确认的冲突：把 `chord` 绑给 `id`，需要从 `other` 手里拿走。
struct Conflict {
    id: String,
    chord: Chord,
    other: String,
}

/// 键盘页状态。
#[derive(Default)]
pub(super) struct KeysState {
    /// 正在录制的命令 id。
    recording: Option<String>,
    /// 等待确认的冲突。
    conflict: Option<Conflict>,
    /// 最近一次被拒绝的原因（保留键 / 缺修饰键）。
    warning: Option<String>,
}

impl KeysState {
    /// 放弃录制与提示；返回之前是否有东西被清掉（调用方据此决定是否重绘）。
    pub(super) fn cancel(&mut self) -> bool {
        let had = self.recording.is_some() || self.conflict.is_some() || self.warning.is_some();
        *self = KeysState::default();
        had
    }
}

fn entry_by_id(id: &str) -> Option<Entry> {
    keybindings::catalog().into_iter().find(|e| e.id == id)
}

impl SettingsView {
    /// 录制中的按键处理（根视图的 `on_key_down`）。没在录制时什么都不做，按键照常传递。
    pub(super) fn keys_on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(id) = self.keys.recording.clone() else {
            return;
        };
        cx.stop_propagation();
        if ev.is_held {
            return;
        }
        let ks = &ev.keystroke;
        let input = KeyInput {
            key: &ks.key,
            key_char: ks.key_char.as_deref(),
            ctrl: ks.modifiers.control,
            alt: ks.modifiers.alt,
            shift: ks.modifiers.shift,
            platform: ks.modifiers.platform,
            is_held: ev.is_held,
        };
        match keybindings::record(&input) {
            Recorded::Ignore => return,
            Recorded::Cancel => {
                self.keys.cancel();
            }
            Recorded::Clear => {
                self.keys.cancel();
                self.model
                    .update(cx, |m, cx| m.update_keybindings(cx, |k| k.clear(&id)));
            }
            Recorded::Chord(chord) => self.keys_assign(&id, chord, cx),
        }
        cx.notify();
    }

    fn keys_assign(&mut self, id: &str, chord: Chord, cx: &mut Context<Self>) {
        let mut next = self.model.read(cx).keybindings().clone();
        match next.assign(id, &chord) {
            AssignResult::Assigned => {
                self.keys.cancel();
                self.model
                    .update(cx, |m, cx| m.update_keybindings(cx, |k| *k = next));
            }
            AssignResult::Conflict(other) => {
                self.keys.recording = None;
                self.keys.warning = None;
                self.keys.conflict = Some(Conflict {
                    id: id.to_owned(),
                    chord,
                    other,
                });
            }
            AssignResult::Reserved => {
                self.keys.recording = None;
                self.keys.warning = Some(render_message(&Message::UiKeysReserved {
                    chord: chord.display(),
                }));
            }
            AssignResult::Invalid => {
                self.keys.recording = None;
                self.keys.warning = Some(render_message(&Message::UiKeysNeedsModifier {}));
            }
        }
    }

    /// 用户确认替换。
    fn keys_confirm_replace(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.keys.conflict.take() else {
            return;
        };
        let mut next = self.model.read(cx).keybindings().clone();
        next.assign_replacing(&c.id, &c.chord);
        self.model
            .update(cx, |m, cx| m.update_keybindings(cx, |k| *k = next));
        cx.notify();
    }

    /// 页内全部卡片：顶部提示 / 冲突条，之后每个分组一张卡。
    pub(super) fn keys_cards(&mut self, th: &Th, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let settings = self.model.read(cx).keybindings().clone();
        let catalog = keybindings::catalog();
        let mut out = vec![self.keys_bar(th, &settings, cx)];
        let mut index = 0usize;
        for group in Group::ALL {
            let entries: Vec<&Entry> = catalog.iter().filter(|e| e.group == group).collect();
            let fixed = group == Group::Session;
            if entries.is_empty() && !fixed {
                continue;
            }
            let mut rows: Vec<AnyElement> = Vec::new();
            for entry in entries {
                rows.push(self.key_row(th, cx, index, entry, &settings));
                index += 1;
            }
            if fixed {
                rows.push(key_row_fixed(th));
            }
            let symbol = match group {
                Group::Session => "square-terminal",
                Group::Edit => "pencil",
                Group::View => "menu",
                Group::Other => "settings",
            };
            out.push(card(th, &group_label(group), symbol, rows));
        }
        out
    }

    /// 页顶：提示文字（或警告 / 冲突询问）与“全部恢复默认”。
    fn keys_bar(
        &mut self,
        th: &Th,
        settings: &KeybindingSettings,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut text_col = div().flex_1().min_w_0().flex().flex_col().gap(px(4.0));
        if let Some(c) = &self.keys.conflict {
            let other = entry_by_id(&c.other)
                .map(|e| command_label(&e))
                .unwrap_or_else(|| c.other.clone());
            let new = entry_by_id(&c.id)
                .map(|e| command_label(&e))
                .unwrap_or_else(|| c.id.clone());
            text_col = text_col
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(th.c("status.warning"))
                        .child(render_message(&Message::UiKeysConflictTitle {
                            chord: c.chord.display(),
                        })),
                )
                .child(
                    div()
                        .text_size(px(number("font", "ui.caption")))
                        .text_color(th.c("text.secondary"))
                        .child(render_message(&Message::UiKeysConflictBody {
                            chord: c.chord.display(),
                            holder: other,
                            new,
                        })),
                );
        } else if let Some(w) = &self.keys.warning {
            text_col = text_col.child(
                div()
                    .text_size(px(number("font", "ui.caption")))
                    .text_color(th.c("status.warning"))
                    .child(w.clone()),
            );
        } else {
            text_col = text_col.child(caption(th, &render_message(&Message::UiKeysHint {})));
        }

        let mut buttons = div().flex().items_center().gap(px(8.0)).flex_none();
        if self.keys.conflict.is_some() {
            buttons = buttons
                .child(
                    text_button(
                        th,
                        "keys-replace",
                        render_message(&Message::UiKeysReplace {}),
                        true,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation();
                            this.keys_confirm_replace(cx);
                        }),
                    ),
                )
                .child(
                    text_button(
                        th,
                        "keys-cancel",
                        render_message(&Message::UiCancel {}),
                        false,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation();
                            this.keys.cancel();
                            cx.notify();
                        }),
                    ),
                );
        } else {
            let any = !settings.overrides.is_empty();
            let mut b = text_button(
                th,
                "keys-restore-all",
                render_message(&Message::UiKeysRestoreAll {}),
                false,
            );
            if any {
                b = b.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        this.keys.cancel();
                        this.model
                            .update(cx, |m, cx| m.update_keybindings(cx, |k| k.restore_all()));
                        cx.notify();
                    }),
                );
            } else {
                b = b.opacity(0.45);
            }
            buttons = buttons.child(b);
        }
        div()
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(text_col)
            .child(buttons)
            .into_any_element()
    }

    /// 一行：命令名、按键框、（已自定义时）单项恢复默认。
    fn key_row(
        &mut self,
        th: &Th,
        cx: &mut Context<Self>,
        index: usize,
        entry: &Entry,
        settings: &KeybindingSettings,
    ) -> AnyElement {
        let id = entry.id.clone();
        let recording = self.keys.recording.as_deref() == Some(entry.id.as_str());
        let chords = settings.chords_of(entry);
        let (text, text_color) = if recording {
            (render_message(&Message::UiKeysRecording {}), th.c("accent"))
        } else if let Some(t) = chords_text(&chords) {
            (t, th.c("text.primary"))
        } else {
            (
                render_message(&Message::UiKeysUnbound {}),
                th.c("text.tertiary"),
            )
        };
        let chip_id = id.clone();
        let chip = div()
            .id(("key-chip", index))
            .w(px(190.0))
            .h(px(number("spacing", "field.height") - 4.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(number("radius", "md")))
            .cursor_pointer()
            .border_1()
            .border_color(if recording {
                th.c("accent")
            } else {
                th.c("border.field")
            })
            .bg(th.c("bg.field"))
            .text_color(text_color)
            .hover(|s| s.bg(th.c("bg.rowHover")))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window: &mut Window, cx| {
                    cx.stop_propagation();
                    this.keys.conflict = None;
                    this.keys.warning = None;
                    this.keys.recording = Some(chip_id.clone());
                    // 键盘事件要落在设置窗口的根焦点上，而不是某个输入框。
                    window.focus(&this.focus);
                    cx.notify();
                }),
            )
            .child(text);

        let restore_id = id.clone();
        let restore: AnyElement = if settings.is_customized(&id) {
            div()
                .id(("key-restore", index))
                .size(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(number("radius", "sm")))
                .cursor_pointer()
                .hover(|s| s.bg(th.c("bg.rowHover")))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        this.keys.cancel();
                        let id = restore_id.clone();
                        this.model
                            .update(cx, |m, cx| m.update_keybindings(cx, |k| k.restore(&id)));
                        cx.notify();
                    }),
                )
                .child(icon("rotate-ccw", 14.0, th.c("text.secondary")))
                .into_any_element()
        } else {
            div().size(px(24.0)).into_any_element()
        };

        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(th.c("text.primary"))
                    .child(command_label(entry)),
            )
            .child(chip)
            .child(restore)
            .into_any_element()
    }
}

/// 固定的 Alt+1–Alt+9 行（只读）。
fn key_row_fixed(th: &Th) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(th.c("text.primary"))
                .child(render_message(&Message::UiKeysFixedTabs {})),
        )
        .child(
            div()
                .w(px(190.0))
                .h(px(number("spacing", "field.height") - 4.0))
                .flex()
                .items_center()
                .justify_center()
                .text_color(th.c("text.tertiary"))
                .child(render_message(&Message::UiKeysFixedTabsKey {})),
        )
        .child(div().size(px(24.0)))
        .into_any_element()
}

/// 文字按钮；`primary` 用强调色底。
fn text_button(
    th: &Th,
    id: &'static str,
    text: String,
    primary: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(number("spacing", "field.height") - 4.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .rounded(px(number("radius", "md")))
        .cursor_pointer()
        .bg(if primary {
            th.c("accent")
        } else {
            th.c("bg.buttonSecondary")
        })
        .text_color(if primary {
            th.c("text.onAccent")
        } else {
            th.c("text.primary")
        })
        .hover(|s| s.opacity(0.85))
        .child(text)
}
