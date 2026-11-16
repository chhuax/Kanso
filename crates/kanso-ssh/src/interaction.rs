//! 认证过程中向用户提问的接口（ssh.md §4.2）。
//!
//! 由 core 实现并注入：core 把调用转成 `CoreEvent::Prompt`，等 UI 的 `PromptResponse` 再返回。
//! 本模块的形状刻意与 `kanso_types::prompt` 一一对应，并提供互转的辅助函数。
//!
//! 主机密钥变更**不**走这里：连接直接失败并返回结构化错误，UI 问完后调用方重连。

use std::future::Future;
use std::time::Duration;

use kanso_types::message::Endpoint;
use kanso_types::prompt::{CredentialKind, KbdQuestion, PromptRequest, PromptResponse};
use kanso_types::secret::SecretString;

/// 单次提问最长等待时间（ssh.md §4.2）：窗口可能已经关闭，不能让半开的连接一直挂着。
///
/// 由 kanso-ssh 用 `tokio::time::timeout` 包住对 [`Interaction`] 的调用实现，超时按取消处理；
/// 实现者不需要自己计时。
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(180);

/// keyboard-interactive 的一轮问答。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KbdRound {
    /// 正在发问的连接对象；跳板机会单独发问，让用户知道是哪一跳在要验证码。
    pub endpoint: Endpoint,
    /// 登录用户。
    pub user: String,
    /// 服务器给出的名称；为空时不显示。
    pub name: String,
    /// 服务器给出的说明；为空时不显示。
    pub instruction: String,
    /// 前面没有问题的轮次带过来的文字，应一并展示。
    pub preamble: Vec<String>,
    /// 本轮问题。
    pub questions: Vec<KbdQuestion>,
}

impl KbdRound {
    /// 转成 core 向 UI 发出的请求。
    pub fn into_prompt_request(self) -> PromptRequest {
        PromptRequest::KeyboardInteractive {
            endpoint: self.endpoint,
            user: self.user,
            name: self.name,
            instruction: self.instruction,
            preamble: self.preamble,
            questions: self.questions,
        }
    }
}

/// 缺失凭据请求转成 core 向 UI 发出的请求。
pub fn credential_prompt_request(
    endpoint: &Endpoint,
    user: &str,
    kind: CredentialKind,
) -> PromptRequest {
    PromptRequest::Credential {
        endpoint: endpoint.clone(),
        user: user.to_owned(),
        kind,
    }
}

/// 把 UI 的应答转成 [`Interaction::keyboard_interactive`] 的返回值；取消或类型不匹配为 `None`。
pub fn kbd_answers_from_response(resp: PromptResponse) -> Option<Vec<String>> {
    match resp {
        PromptResponse::Answers(a) => Some(a),
        _ => None,
    }
}

/// 把 UI 的应答转成 [`Interaction::credential`] 的返回值；取消或类型不匹配为 `None`。
pub fn credential_from_response(resp: PromptResponse) -> Option<(SecretString, bool)> {
    match resp {
        PromptResponse::Credential { secret, remember } => Some((secret, remember)),
        _ => None,
    }
}

/// 认证过程中的用户交互，由 core 实现。
///
/// 两个方法都可能等很久（等用户输入）；kanso-ssh 会用 [`PROMPT_TIMEOUT`] 包住调用。
/// 返回的 future 被超时丢弃时实现者应能干净地撤销提问（core 此时发 `PromptDismissed`）。
pub trait Interaction: Send + Sync {
    /// 问 keyboard-interactive 的一轮问题。
    ///
    /// 返回与 `round.questions` 一一对应的答案；`None` 表示用户取消。
    /// 答案数量与问题数不一致时，kanso-ssh 会截断或用空串补齐。
    fn keyboard_interactive(
        &self,
        round: KbdRound,
    ) -> impl Future<Output = Option<Vec<String>>> + Send;

    /// 向用户要缺失的凭据（密码或私钥口令）。
    ///
    /// 返回 `(凭据, 是否记住)`；`None` 表示用户取消。"记住"只是意愿，
    /// 存不存由 core 决定，kanso-ssh 会在认证成功后把它随 [`crate::Connected`] 交还。
    fn credential(
        &self,
        endpoint: &Endpoint,
        user: &str,
        kind: CredentialKind,
    ) -> impl Future<Output = Option<(SecretString, bool)>> + Send;
}
