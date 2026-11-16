//! 需要用户回答的请求与应答。
//!
//! core 发 `CoreEvent::Prompt`，UI 通过 core 的统一应答接口回传 [`PromptResponse`]。

use serde::{Deserialize, Serialize};

use crate::message::Endpoint;
use crate::secret::SecretString;

/// keyboard-interactive 的一个问题。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KbdQuestion {
    /// 服务器给出的提示文字（原样，不本地化）。
    pub text: String,
    /// 输入时是否回显；`false` 表示按密码框处理。
    pub echo: bool,
}

/// 需要用户提供的凭据种类。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialKind {
    /// 登录密码。
    Password,
    /// 私钥口令。
    KeyPassphrase {
        /// 私钥文件路径，用于在提示里告诉用户是哪把钥匙。
        path: String,
    },
}

/// core 向 UI 发出的待回答请求。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptRequest {
    /// 主机密钥与已记录的不一致。连接已被拒绝；用户接受后 core 替换该主机的全部旧记录并重连。
    HostKeyChanged {
        /// 连接对象。
        endpoint: Endpoint,
        /// 密钥类型，如 `ssh-ed25519`。
        key_type: String,
        /// 新密钥的 SHA256 指纹。
        fingerprint_sha256: String,
        /// 已记录旧密钥的 known_hosts 文件路径。
        known_hosts_path: String,
        /// 冲突记录所在行号，与编辑器看到的一致（注释行也计数，ssh.md §3）。
        line: u32,
    },
    /// keyboard-interactive 一轮问答。
    KeyboardInteractive {
        /// 连接对象。
        endpoint: Endpoint,
        /// 登录用户。
        user: String,
        /// 服务器给出的名称；为空时不显示。
        name: String,
        /// 服务器给出的说明；为空时不显示。
        instruction: String,
        /// 前面没有问题的轮次带过来的文字（ssh.md §4.2），应一并展示。
        preamble: Vec<String>,
        /// 本轮问题。
        questions: Vec<KbdQuestion>,
    },
    /// 凭据缺失或读取失败时向用户询问（store-import.md §2）。
    Credential {
        /// 连接对象。
        endpoint: Endpoint,
        /// 登录用户。
        user: String,
        /// 需要的凭据种类。
        kind: CredentialKind,
    },
    /// ZMODEM 接收时询问保存位置。
    ZmodemSaveFile {
        /// 发送方建议的文件名（未经信任，UI 展示前 core 已做安全检查）。
        suggested_name: String,
        /// 文件大小；发送方未声明时为 `None`。
        size: Option<u64>,
    },
    /// ZMODEM 发送时让用户选择文件：多选，只能选文件。
    ZmodemPickFiles,
    /// XMODEM 发送时选择单个文件。
    XmodemPickFile,
    /// XMODEM 接收时选择保存位置。
    XmodemSaveFile,
}

/// UI 对 [`PromptRequest`] 的应答。
///
/// 类型与请求不匹配的应答由 core 按 `Cancel` 处理。core 侧超时
/// （如 keyboard-interactive 的 180 秒）会发 `CoreEvent::PromptDismissed`，UI 应关闭对应界面。
/// 因含 [`SecretString`]（不可序列化），本类型不实现 serde。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptResponse {
    /// 取消 / 拒绝。
    Cancel,
    /// 接受新的主机密钥。
    AcceptHostKey,
    /// keyboard-interactive 的答案，与问题一一对应。
    Answers(Vec<String>),
    /// 提供凭据。
    Credential {
        /// 凭据明文。
        secret: SecretString,
        /// 是否记住到凭据存储。
        remember: bool,
    },
    /// ZMODEM 接收的保存路径。
    SavePath(String),
    /// ZMODEM 发送选中的文件路径。
    Files(Vec<String>),
}
