//! 模态对话框的**纯逻辑**：规格、按钮角色与默认按钮、焦点环、结果组装。
//!
//! 对照 mac 版 `Prompts/PromptSheetView.swift`：标题 + 正文 + 若干输入框 + 按钮行；
//! Enter 触发默认按钮，Esc 取消，点遮罩不关闭（除非规格允许）。
//! GPUI 视图在 `ui/widgets/dialog.rs`（只在 Windows 编译）。
//!
//! 安全约束：`Debug` 输出不含密码框内容；取消类结果不携带任何输入内容。

use std::fmt;

/// 按钮角色，决定配色与（缺省时）谁是默认按钮。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonRole {
    /// 主按钮（强调色底）。
    Primary,
    /// 次要按钮（取消、拒绝）。
    Secondary,
    /// 危险按钮（删除、覆盖；红色）。
    Danger,
}

/// 一个按钮。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ButtonSpec {
    /// 结果里回传的标识。
    pub id: String,
    /// 显示文字。
    pub label: String,
    /// 角色。
    pub role: ButtonRole,
    /// 显式指定为默认按钮（回车触发）。
    pub is_default: bool,
    /// Esc 触发的按钮（取消）。
    pub is_cancel: bool,
}

impl ButtonSpec {
    fn new(id: &str, label: &str, role: ButtonRole) -> Self {
        ButtonSpec {
            id: id.into(),
            label: label.into(),
            role,
            is_default: false,
            is_cancel: false,
        }
    }
    /// 主按钮。
    pub fn primary(id: &str, label: &str) -> Self {
        Self::new(id, label, ButtonRole::Primary)
    }
    /// 次要按钮。
    pub fn secondary(id: &str, label: &str) -> Self {
        Self::new(id, label, ButtonRole::Secondary)
    }
    /// 危险按钮。
    pub fn danger(id: &str, label: &str) -> Self {
        Self::new(id, label, ButtonRole::Danger)
    }
    /// 标为默认按钮。
    pub fn default_button(mut self) -> Self {
        self.is_default = true;
        self
    }
    /// 标为 Esc 取消按钮。
    pub fn cancel(mut self) -> Self {
        self.is_cancel = true;
        self
    }
}

/// 一个内嵌输入框。
#[derive(Clone, PartialEq, Eq)]
pub struct InputSpec {
    /// 结果里回传的标识。
    pub id: String,
    /// 输入框上方的说明文字。
    pub label: String,
    /// 占位文字。
    pub placeholder: String,
    /// 密码模式。
    pub secure: bool,
    /// 初始内容。
    pub initial: String,
}

impl InputSpec {
    /// 普通输入框。
    pub fn new(id: &str, label: &str) -> Self {
        InputSpec {
            id: id.into(),
            label: label.into(),
            placeholder: String::new(),
            secure: false,
            initial: String::new(),
        }
    }
    /// 密码输入框。
    pub fn secure(id: &str, label: &str) -> Self {
        InputSpec {
            secure: true,
            ..Self::new(id, label)
        }
    }
    /// 占位文字。
    pub fn placeholder(mut self, text: &str) -> Self {
        self.placeholder = text.into();
        self
    }
    /// 初始内容。
    pub fn initial(mut self, text: &str) -> Self {
        self.initial = text.into();
        self
    }
}

impl fmt::Debug for InputSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputSpec")
            .field("id", &self.id)
            .field("secure", &self.secure)
            .finish()
    }
}

/// 正文的一块。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyBlock {
    /// 普通段落（可含换行）。
    Text(String),
    /// 等宽代码块（指纹、路径）。
    Code(String),
}

/// 对话框规格。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogSpec {
    /// 标题。
    pub title: String,
    /// 正文。
    pub body: Vec<BodyBlock>,
    /// 输入框（自上而下）。
    pub inputs: Vec<InputSpec>,
    /// 按钮（自左而右，最后一个贴右边缘）。
    pub buttons: Vec<ButtonSpec>,
    /// 点遮罩是否按取消处理（默认否，与 mac sheet 一致）。
    pub dismiss_on_scrim: bool,
}

impl DialogSpec {
    /// 只有标题的空规格。
    pub fn new(title: &str) -> Self {
        DialogSpec {
            title: title.into(),
            body: Vec::new(),
            inputs: Vec::new(),
            buttons: Vec::new(),
            dismiss_on_scrim: false,
        }
    }
    /// 追加正文段落。
    pub fn text(mut self, t: &str) -> Self {
        self.body.push(BodyBlock::Text(t.into()));
        self
    }
    /// 追加等宽代码块。
    pub fn code(mut self, t: &str) -> Self {
        self.body.push(BodyBlock::Code(t.into()));
        self
    }
    /// 追加输入框。
    pub fn input(mut self, i: InputSpec) -> Self {
        self.inputs.push(i);
        self
    }
    /// 追加按钮。
    pub fn button(mut self, b: ButtonSpec) -> Self {
        self.buttons.push(b);
        self
    }
    /// 允许点遮罩取消。
    pub fn dismiss_on_scrim(mut self) -> Self {
        self.dismiss_on_scrim = true;
        self
    }

    /// 默认按钮下标：显式标记的第一个；否则第一个主按钮；再没有则无默认
    /// （危险按钮不会被隐式当作默认，必须显式指定）。
    pub fn default_button(&self) -> Option<usize> {
        self.buttons.iter().position(|b| b.is_default).or_else(|| {
            self.buttons
                .iter()
                .position(|b| b.role == ButtonRole::Primary)
        })
    }

    /// Esc 对应的按钮下标（第一个标为取消的）。
    pub fn cancel_button(&self) -> Option<usize> {
        self.buttons.iter().position(|b| b.is_cancel)
    }
}

/// 焦点环上的一站。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// 第 n 个输入框。
    Input(usize),
    /// 第 n 个按钮。
    Button(usize),
}

/// Tab 循环顺序：所有输入框，然后所有按钮；首尾相接，焦点不会离开对话框。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusRing {
    slots: Vec<Slot>,
}

impl FocusRing {
    /// 按规格建环。
    pub fn new(spec: &DialogSpec) -> Self {
        let slots = (0..spec.inputs.len())
            .map(Slot::Input)
            .chain((0..spec.buttons.len()).map(Slot::Button))
            .collect();
        FocusRing { slots }
    }

    /// 所有站点。
    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    /// 打开时的初始焦点：第一个输入框；没有输入框则默认按钮；再没有则第一个站点。
    pub fn initial(&self, spec: &DialogSpec) -> Option<Slot> {
        if !spec.inputs.is_empty() {
            return Some(Slot::Input(0));
        }
        spec.default_button()
            .map(Slot::Button)
            .or_else(|| self.slots.first().copied())
    }

    /// Tab：下一站。当前不在环上（`None` 或未知）时落到第一站。
    pub fn next(&self, current: Option<Slot>) -> Option<Slot> {
        self.step(current, true)
    }

    /// Shift-Tab：上一站。当前不在环上时落到最后一站。
    pub fn prev(&self, current: Option<Slot>) -> Option<Slot> {
        self.step(current, false)
    }

    fn step(&self, current: Option<Slot>, forward: bool) -> Option<Slot> {
        let n = self.slots.len();
        if n == 0 {
            return None;
        }
        let pos = current.and_then(|c| self.slots.iter().position(|s| *s == c));
        let idx = match (pos, forward) {
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
            (None, true) => 0,
            (None, false) => n - 1,
        };
        Some(self.slots[idx])
    }
}

/// 谁触发了结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// 点击按钮。
    Click,
    /// 回车。
    Enter,
    /// Esc。
    Escape,
    /// 点遮罩（仅 `dismiss_on_scrim`）。
    Scrim,
}

/// 一个输入框的值。
#[derive(Clone, PartialEq, Eq)]
pub struct FieldValue {
    /// 输入框标识。
    pub id: String,
    /// 内容。
    pub value: String,
    /// 是否密码框（`Debug` 据此隐藏）。
    pub secure: bool,
}

impl fmt::Debug for FieldValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = f.debug_struct("FieldValue");
        s.field("id", &self.id);
        if self.secure {
            s.field("value", &"<redacted>");
        } else {
            s.field("value", &self.value);
        }
        s.finish()
    }
}

/// 对话框结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogResult {
    /// 按下的按钮标识；Esc / 点遮罩且规格没有取消按钮时为 `None`。
    pub button: Option<String>,
    /// 触发方式。
    pub trigger: Trigger,
    /// 各输入框内容（取消类结果为空，不泄露已输入的内容）。
    pub values: Vec<FieldValue>,
}

impl DialogResult {
    /// 按标识取输入框内容。
    pub fn value(&self, id: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|v| v.id == id)
            .map(|v| v.value.as_str())
    }

    /// 日志用的单行摘要，密码框只写长度。
    pub fn log_line(&self) -> String {
        let vals: Vec<String> = self
            .values
            .iter()
            .map(|v| {
                if v.secure {
                    format!("{}=<{} 字符>", v.id, v.value.chars().count())
                } else {
                    format!("{}={:?}", v.id, v.value)
                }
            })
            .collect();
        format!(
            "button={:?} trigger={:?} values=[{}]",
            self.button,
            self.trigger,
            vals.join(", ")
        )
    }
}

/// 按钮被点击 / 回车选中时的结果；`texts` 与 `spec.inputs` 一一对应。
/// 按钮是取消按钮时按取消处理（不带输入内容）。
pub fn result_for_button(
    spec: &DialogSpec,
    button: usize,
    trigger: Trigger,
    texts: &[String],
) -> DialogResult {
    let Some(b) = spec.buttons.get(button) else {
        return result_for_cancel(spec, trigger);
    };
    let values = if b.is_cancel {
        Vec::new()
    } else {
        spec.inputs
            .iter()
            .enumerate()
            .map(|(i, inp)| FieldValue {
                id: inp.id.clone(),
                value: texts.get(i).cloned().unwrap_or_default(),
                secure: inp.secure,
            })
            .collect()
    };
    DialogResult {
        button: Some(b.id.clone()),
        trigger,
        values,
    }
}

/// Esc / 点遮罩：结果记为取消按钮（没有则 `None`），不带输入内容。
pub fn result_for_cancel(spec: &DialogSpec, trigger: Trigger) -> DialogResult {
    DialogResult {
        button: spec.cancel_button().map(|i| spec.buttons[i].id.clone()),
        trigger,
        values: Vec::new(),
    }
}

/// 回车该触发哪个按钮：焦点在按钮上时是该按钮（与 Windows 习惯一致），否则是默认按钮。
pub fn enter_target(spec: &DialogSpec, focused: Option<Slot>) -> Option<usize> {
    match focused {
        Some(Slot::Button(i)) if i < spec.buttons.len() => Some(i),
        _ => spec.default_button(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_key() -> DialogSpec {
        DialogSpec::new("未知主机")
            .text("是否信任？")
            .code("SHA256:abc")
            .button(ButtonSpec::secondary("reject", "拒绝").cancel())
            .button(ButtonSpec::primary("accept", "信任并继续"))
    }

    fn passphrase() -> DialogSpec {
        DialogSpec::new("口令")
            .input(InputSpec::secure("pass", "口令"))
            .input(InputSpec::new("note", "备注"))
            .button(ButtonSpec::secondary("cancel", "取消").cancel())
            .button(ButtonSpec::primary("ok", "确定"))
    }

    #[test]
    fn default_button_rules() {
        assert_eq!(host_key().default_button(), Some(1));
        // 显式默认优先于主按钮。
        let s = DialogSpec::new("t")
            .button(ButtonSpec::secondary("c", "取消").default_button())
            .button(ButtonSpec::primary("p", "好"));
        assert_eq!(s.default_button(), Some(0));
        // 危险按钮不会被隐式当默认。
        let d = DialogSpec::new("t")
            .button(ButtonSpec::secondary("c", "取消"))
            .button(ButtonSpec::danger("d", "删除"));
        assert_eq!(d.default_button(), None);
        let d = DialogSpec::new("t")
            .button(ButtonSpec::secondary("c", "取消").cancel().default_button())
            .button(ButtonSpec::danger("d", "删除"));
        assert_eq!(d.default_button(), Some(0));
        assert_eq!(d.cancel_button(), Some(0));
    }

    #[test]
    fn ring_order_and_wrap() {
        let s = passphrase();
        let r = FocusRing::new(&s);
        assert_eq!(
            r.slots(),
            &[
                Slot::Input(0),
                Slot::Input(1),
                Slot::Button(0),
                Slot::Button(1)
            ]
        );
        assert_eq!(r.next(Some(Slot::Input(0))), Some(Slot::Input(1)));
        assert_eq!(r.next(Some(Slot::Button(1))), Some(Slot::Input(0)));
        assert_eq!(r.prev(Some(Slot::Input(0))), Some(Slot::Button(1)));
        assert_eq!(r.prev(Some(Slot::Button(0))), Some(Slot::Input(1)));
        // 焦点在对话框外（None）：Tab 进第一站，Shift-Tab 进最后一站。
        assert_eq!(r.next(None), Some(Slot::Input(0)));
        assert_eq!(r.prev(None), Some(Slot::Button(1)));
    }

    #[test]
    fn full_forward_cycle_returns_to_start() {
        let s = passphrase();
        let r = FocusRing::new(&s);
        let start = r.initial(&s);
        let mut cur = start;
        for _ in 0..r.slots().len() {
            cur = r.next(cur);
        }
        assert_eq!(cur, start);
    }

    #[test]
    fn initial_focus() {
        let p = passphrase();
        assert_eq!(FocusRing::new(&p).initial(&p), Some(Slot::Input(0)));
        let h = host_key();
        assert_eq!(FocusRing::new(&h).initial(&h), Some(Slot::Button(1)));
        let d = DialogSpec::new("t")
            .button(ButtonSpec::secondary("c", "取消"))
            .button(ButtonSpec::danger("d", "删除"));
        assert_eq!(FocusRing::new(&d).initial(&d), Some(Slot::Button(0)));
        let empty = DialogSpec::new("t");
        assert_eq!(FocusRing::new(&empty).initial(&empty), None);
        assert_eq!(FocusRing::new(&empty).next(None), None);
    }

    #[test]
    fn enter_targets_focused_button_else_default() {
        let p = passphrase();
        assert_eq!(enter_target(&p, Some(Slot::Input(0))), Some(1));
        assert_eq!(enter_target(&p, None), Some(1));
        assert_eq!(enter_target(&p, Some(Slot::Button(0))), Some(0));
        let d = DialogSpec::new("t").button(ButtonSpec::danger("d", "删除"));
        assert_eq!(enter_target(&d, Some(Slot::Input(0))), None);
    }

    #[test]
    fn button_result_carries_values() {
        let p = passphrase();
        let r = result_for_button(&p, 1, Trigger::Enter, &["s3cret".into(), "n".into()]);
        assert_eq!(r.button.as_deref(), Some("ok"));
        assert_eq!(r.trigger, Trigger::Enter);
        assert_eq!(r.value("pass"), Some("s3cret"));
        assert_eq!(r.value("note"), Some("n"));
        assert_eq!(r.value("zzz"), None);
    }

    #[test]
    fn missing_texts_become_empty() {
        let p = passphrase();
        let r = result_for_button(&p, 1, Trigger::Click, &[]);
        assert_eq!(r.value("pass"), Some(""));
    }

    #[test]
    fn cancel_results_drop_values() {
        let p = passphrase();
        let r = result_for_button(&p, 0, Trigger::Click, &["s3cret".into(), "n".into()]);
        assert_eq!(r.button.as_deref(), Some("cancel"));
        assert!(r.values.is_empty());
        let e = result_for_cancel(&p, Trigger::Escape);
        assert_eq!(e.button.as_deref(), Some("cancel"));
        assert_eq!(e.trigger, Trigger::Escape);
        assert!(e.values.is_empty());
        let none = DialogSpec::new("t").button(ButtonSpec::primary("ok", "好"));
        assert_eq!(result_for_cancel(&none, Trigger::Scrim).button, None);
    }

    #[test]
    fn secrets_are_redacted() {
        let p = passphrase();
        let r = result_for_button(&p, 1, Trigger::Click, &["s3cret".into(), "n".into()]);
        let dbg = format!("{r:?}");
        assert!(!dbg.contains("s3cret"));
        assert!(dbg.contains("\"n\""));
        let line = r.log_line();
        assert!(!line.contains("s3cret"));
        assert!(line.contains("pass=<6 字符>"));
        assert!(line.contains("button=Some(\"ok\")"));
        let spec_dbg = format!("{:?}", InputSpec::secure("p", "x").initial("hunter2"));
        assert!(!spec_dbg.contains("hunter2"));
    }

    #[test]
    fn out_of_range_button_is_cancel() {
        let p = passphrase();
        let r = result_for_button(&p, 9, Trigger::Click, &["x".into()]);
        assert_eq!(r.button.as_deref(), Some("cancel"));
        assert!(r.values.is_empty());
    }
}
