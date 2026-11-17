//! 核心提问（[`PromptDialog`]）到模态对话框规格的映射，以及对话框结果到应答的映射。
//! 对照 `Prompts/PromptSheetView.swift` 与 `Prompts/PromptViewModels.swift`（ssh.md §3、§4）。
//!
//! 安全约束沿用提示队列：
//! - 主机密钥变更的默认按钮是"取消"，回车不会接受一把变了的密钥；
//! - 每个对话框都写明"是谁在问"（目标主机、跳板链、标签标题），避免把密码填给错误的主机；
//! - 没有按钮 id 可识别（Esc、点遮罩且没有取消按钮）一律按取消处理。
//!
//! 对话框控件没有复选框，"保存到凭据存储"用两个按钮表达：**连接并保存**（默认）与**仅连接**。

use crate::app::i18n::{fmt, tr};
use kanso_types::prompt::PromptResponse;
use kanso_types::secret::SecretString;

use super::prompts::{CredentialPrompt, KeyboardInteractivePrompt, PromptDialog, PromptTarget};
use crate::widgets::dialog::{ButtonSpec, DialogResult, DialogSpec, InputSpec};

/// 取消。
pub const BTN_CANCEL: &str = "cancel";
/// 接受新的主机密钥。
pub const BTN_ACCEPT: &str = "accept";
/// 继续（keyboard-interactive）。
pub const BTN_CONTINUE: &str = "continue";
/// 连接并保存凭据。
pub const BTN_SAVE: &str = "save";
/// 仅连接，不保存凭据。
pub const BTN_ONCE: &str = "once";

/// "是谁在问"的文字：`host:port`，经由跳板链，所属标签。
pub fn target_text(target: &PromptTarget) -> String {
    let mut s = fmt(tr("服务器：{}"), &[&target.host_port]);
    if let Some(via) = target.via_chain() {
        s.push_str(&fmt(tr("（经由 {}）"), &[&via]));
    }
    if let Some(title) = &target.session_title {
        s.push_str(&fmt(tr("\n标签：{}"), &[&title]));
    }
    s
}

/// 提问对应的对话框规格；文件传输类提问（交给系统文件选择器）没有，返回 `None`。
pub fn spec_for(dialog: &PromptDialog) -> Option<DialogSpec> {
    match dialog {
        PromptDialog::HostKeyChanged(m) => Some(
            DialogSpec::new(tr("主机密钥已变更"))
                .text(&target_text(&m.target))
                .text(
                    "这台服务器的主机密钥与记录的不一致。可能遭到中间人攻击，\
                     也可能是服务器重装过。只有确认信任这次变更时才继续。",
                )
                .text(tr("密钥类型"))
                .code(&m.key_type)
                .text(tr("SHA256 指纹"))
                .code(&m.fingerprint)
                .text(tr("原记录位置"))
                .code(&m.known_hosts_location)
                .button(
                    ButtonSpec::secondary(BTN_CANCEL, tr("取消"))
                        .cancel()
                        .default_button(),
                )
                .button(ButtonSpec::danger(BTN_ACCEPT, tr("接受新密钥并连接"))),
        ),
        PromptDialog::KeyboardInteractive(m) => Some(keyboard_interactive_spec(m)),
        PromptDialog::Credential(m) => Some(credential_spec(m)),
        PromptDialog::Transfer(_) => None,
    }
}

fn keyboard_interactive_spec(m: &KeyboardInteractivePrompt) -> DialogSpec {
    // 服务器给的 name / instruction / 问题是原文，不本地化。
    let title = m.name.clone().unwrap_or_else(|| tr("需要验证").to_owned());
    let mut spec = DialogSpec::new(&title).text(&target_text(&m.target));
    if let Some(instruction) = &m.instruction {
        spec = spec.text(instruction);
    }
    for line in &m.preamble {
        spec = spec.text(line);
    }
    for (i, f) in m.fields.iter().enumerate() {
        let id = format!("f{i}");
        let label = f.prompt.trim_end();
        spec = spec.input(if f.is_secure {
            InputSpec::secure(&id, label)
        } else {
            InputSpec::new(&id, label)
        });
    }
    spec.button(ButtonSpec::secondary(BTN_CANCEL, tr("取消")).cancel())
        .button(ButtonSpec::primary(BTN_CONTINUE, tr("继续")))
}

fn credential_spec(m: &CredentialPrompt) -> DialogSpec {
    let (title, label, intro) = match &m.key_path {
        Some(path) => (
            tr("需要私钥口令"),
            tr("私钥口令"),
            fmt(
                tr("{}\n用户：{}\n私钥：{}"),
                &[&target_text(&m.target), &m.user, &path],
            ),
        ),
        None => (
            tr("需要密码"),
            tr("密码"),
            fmt(tr("{}\n用户：{}"), &[&target_text(&m.target), &m.user]),
        ),
    };
    let save = ButtonSpec::primary(BTN_SAVE, tr("连接并保存"));
    let once = ButtonSpec::secondary(BTN_ONCE, tr("仅连接"));
    let (first, second) = if m.remember {
        (once, save.default_button())
    } else {
        (save, once.default_button())
    };
    DialogSpec::new(title)
        .text(&intro)
        .input(InputSpec::secure("secret", label))
        .button(ButtonSpec::secondary(BTN_CANCEL, tr("取消")).cancel())
        .button(first)
        .button(second)
}

/// 对话框结果对应的应答。取消类结果（Esc、取消按钮、没有按钮 id）一律是 [`PromptResponse::Cancel`]。
pub fn response_for(dialog: &PromptDialog, result: &DialogResult) -> PromptResponse {
    let Some(button) = result.button.as_deref() else {
        return PromptResponse::Cancel;
    };
    match (dialog, button) {
        (PromptDialog::HostKeyChanged(_), BTN_ACCEPT) => PromptResponse::AcceptHostKey,
        (PromptDialog::KeyboardInteractive(m), BTN_CONTINUE) => PromptResponse::Answers(
            (0..m.fields.len())
                .map(|i| {
                    result
                        .value(&format!("f{i}"))
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect(),
        ),
        (PromptDialog::Credential(_), BTN_SAVE | BTN_ONCE) => PromptResponse::Credential {
            secret: SecretString::new(result.value("secret").unwrap_or_default().to_owned()),
            remember: button == BTN_SAVE,
        },
        _ => PromptResponse::Cancel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::dialog::{FieldValue, Trigger};
    use kanso_types::message::Endpoint;
    use kanso_types::prompt::{CredentialKind, KbdQuestion, PromptRequest};

    fn endpoint(via: &[&str]) -> Endpoint {
        Endpoint {
            host: "example.com".into(),
            port: 22,
            via: via.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn dialog(request: PromptRequest) -> PromptDialog {
        PromptDialog::from_request(request, Some("生产机".into()))
    }

    fn result(button: Option<&str>, values: &[(&str, &str, bool)]) -> DialogResult {
        DialogResult {
            button: button.map(str::to_owned),
            trigger: Trigger::Click,
            values: values
                .iter()
                .map(|(id, v, s)| FieldValue {
                    id: (*id).into(),
                    value: (*v).into(),
                    secure: *s,
                })
                .collect(),
        }
    }

    fn host_key() -> PromptDialog {
        dialog(PromptRequest::HostKeyChanged {
            endpoint: endpoint(&["jump1", "jump2"]),
            key_type: "ssh-ed25519".into(),
            fingerprint_sha256: "SHA256:abc".into(),
            known_hosts_path: "C:\\known_hosts".into(),
            line: 7,
        })
    }

    fn credential(kind: CredentialKind) -> PromptDialog {
        dialog(PromptRequest::Credential {
            endpoint: endpoint(&[]),
            user: "root".into(),
            kind,
        })
    }

    #[test]
    fn host_key_defaults_to_cancel_and_names_the_target() {
        let d = host_key();
        let spec = spec_for(&d).unwrap();
        let default = spec.default_button().unwrap();
        assert_eq!(spec.buttons[default].id, BTN_CANCEL);
        let text = format!("{:?}", spec.body);
        assert!(text.contains("example.com:22"));
        assert!(text.contains("jump1 → jump2"));
        assert!(text.contains("生产机"));
        assert!(text.contains("SHA256:abc"));
        assert!(text.contains("known_hosts:7"));
    }

    #[test]
    fn host_key_accept_only_by_explicit_button() {
        let d = host_key();
        assert_eq!(
            response_for(&d, &result(Some(BTN_ACCEPT), &[])),
            PromptResponse::AcceptHostKey
        );
        assert_eq!(
            response_for(&d, &result(Some(BTN_CANCEL), &[])),
            PromptResponse::Cancel
        );
        assert_eq!(response_for(&d, &result(None, &[])), PromptResponse::Cancel);
    }

    #[test]
    fn password_prompt_remembers_by_default_button() {
        let d = credential(CredentialKind::Password);
        let spec = spec_for(&d).unwrap();
        assert_eq!(spec.inputs.len(), 1);
        assert!(spec.inputs[0].secure);
        let default = spec.default_button().unwrap();
        assert_eq!(spec.buttons[default].id, BTN_SAVE);
        match response_for(&d, &result(Some(BTN_SAVE), &[("secret", "pw", true)])) {
            PromptResponse::Credential { remember, .. } => assert!(remember),
            other => panic!("{other:?}"),
        }
        match response_for(&d, &result(Some(BTN_ONCE), &[("secret", "pw", true)])) {
            PromptResponse::Credential { remember, .. } => assert!(!remember),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            response_for(&d, &result(Some(BTN_CANCEL), &[])),
            PromptResponse::Cancel
        );
    }

    #[test]
    fn passphrase_prompt_shows_the_key_path() {
        let d = credential(CredentialKind::KeyPassphrase {
            path: "C:\\id_ed25519".into(),
        });
        let spec = spec_for(&d).unwrap();
        assert_eq!(spec.title, "需要私钥口令");
        assert!(format!("{:?}", spec.body).contains("id_ed25519"));
    }

    #[test]
    fn keyboard_interactive_answers_follow_question_order() {
        let d = dialog(PromptRequest::KeyboardInteractive {
            endpoint: endpoint(&[]),
            user: "me".into(),
            name: "二次验证".into(),
            instruction: "请输入验证码".into(),
            preamble: vec!["推送已发送".into()],
            questions: vec![
                KbdQuestion {
                    text: "Code: ".into(),
                    echo: true,
                },
                KbdQuestion {
                    text: "Password: ".into(),
                    echo: false,
                },
            ],
        });
        let spec = spec_for(&d).unwrap();
        assert_eq!(spec.title, "二次验证");
        assert_eq!(spec.inputs.len(), 2);
        assert!(!spec.inputs[0].secure);
        assert!(spec.inputs[1].secure);
        assert_eq!(spec.inputs[0].label, "Code:");
        let r = result(
            Some(BTN_CONTINUE),
            &[("f0", "123456", false), ("f1", "pw", true)],
        );
        assert_eq!(
            response_for(&d, &r),
            PromptResponse::Answers(vec!["123456".into(), "pw".into()])
        );
    }

    #[test]
    fn transfer_prompts_have_no_dialog() {
        let d = dialog(PromptRequest::ZmodemPickFiles);
        assert!(spec_for(&d).is_none());
    }
}
