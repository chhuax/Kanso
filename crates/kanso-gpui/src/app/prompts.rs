//! 提示（Prompts）队列与状态机。对照 `Prompts/PromptViewModels.swift`、`PromptPresenter.swift`
//! （ssh.md §3、§4，store-import.md §2）。
//!
//! 同一时间只显示一个提示，其余按到达顺序排队。核心撤销提问（`PromptDismissed`：超时或会话关闭）时，
//! 正在显示的关闭且**不回答**，排队中的直接移除。UI 不自己计时：180 秒超时由核心处理。
//!
//! 安全约束：
//! - 主机密钥变更提示的默认按钮是"取消"，回车（[`PromptQueue::submit`]）不会接受一把变了的密钥，
//!   接受必须走显式的 [`PromptQueue::accept_host_key`]。
//! - 每个提示都带"是谁在问"（目标主机、跳板链、关联标签标题），避免把密码填给错误的主机。
//! - 输入过的明文在回答 / 撤销后立即清空；`Debug` 输出不含明文。

use std::collections::VecDeque;
use std::fmt;

use kanso_types::message::Endpoint;
use kanso_types::prompt::{CredentialKind, KbdQuestion, PromptRequest, PromptResponse};
use kanso_types::secret::SecretString;
use kanso_types::{PromptId, SessionId};

use super::backend::PromptResponder;

/// 对话框标题区所需的"是谁在问"信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptTarget {
    /// `host:port`（IPv6 写成 `[addr]:port`）。
    pub host_port: String,
    /// 从外到内的跳板机显示名；空表示直连。
    pub via: Vec<String>,
    /// 关联会话的标签标题；提问不关联会话时为 `None`。
    pub session_title: Option<String>,
}

impl PromptTarget {
    fn new(endpoint: &Endpoint, session_title: Option<String>) -> Self {
        PromptTarget {
            host_port: endpoint.host_port(),
            via: endpoint.via.clone(),
            session_title,
        }
    }

    /// 跳板链文字（多跳用 " → " 连接，顺序与连接顺序一致）；直连为 `None`。
    /// 视图把它代入"经由 {}"之类的文案。
    pub fn via_chain(&self) -> Option<String> {
        (!self.via.is_empty()).then(|| self.via.join(" → "))
    }
}

/// 按钮的语义：决定对话框里的默认按钮（回车触发）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptButtonRole {
    /// 主按钮（继续 / 连接）。
    Primary,
    /// 取消。
    Cancel,
}

/// 主机密钥已变更。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostKeyPrompt {
    /// 是谁在问。
    pub target: PromptTarget,
    /// 密钥类型，如 `ssh-ed25519`。
    pub key_type: String,
    /// 新密钥的 SHA256 指纹。
    pub fingerprint: String,
    /// `文件:行号`，行号与编辑器看到的一致。
    pub known_hosts_location: String,
}

impl HostKeyPrompt {
    /// 安全起见固定为取消。
    pub fn default_button(&self) -> PromptButtonRole {
        PromptButtonRole::Cancel
    }
}

/// keyboard-interactive 的一个输入框。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KbdField {
    /// 服务器原文问题（不本地化）。
    pub prompt: String,
    /// `echo == false` 时用密码框。
    pub is_secure: bool,
}

/// keyboard-interactive 一轮问答（二次验证等）。
///
/// 服务器给的 name / instruction / 问题文字是原文，不本地化。
#[derive(Clone, PartialEq, Eq)]
pub struct KeyboardInteractivePrompt {
    /// 是谁在问。
    pub target: PromptTarget,
    /// 登录用户。
    pub user: String,
    /// 服务器给出的名称；空串按"没有"处理。
    pub name: Option<String>,
    /// 服务器给出的说明；空串按"没有"处理。
    pub instruction: Option<String>,
    /// 前面无问题的轮次带过来的文字（如"推送已发送"），逐行显示。
    pub preamble: Vec<String>,
    /// 本轮输入框。
    pub fields: Vec<KbdField>,
    answers: Vec<String>,
}

impl KeyboardInteractivePrompt {
    fn new(
        endpoint: &Endpoint,
        user: String,
        name: String,
        instruction: String,
        preamble: Vec<String>,
        questions: Vec<KbdQuestion>,
        session_title: Option<String>,
    ) -> Self {
        KeyboardInteractivePrompt {
            target: PromptTarget::new(endpoint, session_title),
            user,
            name: (!name.is_empty()).then_some(name),
            instruction: (!instruction.is_empty()).then_some(instruction),
            preamble,
            fields: questions
                .iter()
                .map(|q| KbdField {
                    prompt: q.text.clone(),
                    is_secure: !q.echo,
                })
                .collect(),
            answers: vec![String::new(); questions.len()],
        }
    }

    /// 默认按钮：继续。
    pub fn default_button(&self) -> PromptButtonRole {
        PromptButtonRole::Primary
    }

    /// 设置第 `index` 个输入框的内容；越界忽略。
    pub fn set_answer(&mut self, index: usize, text: impl Into<String>) {
        if let Some(a) = self.answers.get_mut(index) {
            *a = text.into();
        }
    }

    /// 第 `index` 个输入框当前内容（密码框的内容仅供视图回显圆点个数）。
    pub fn answer(&self, index: usize) -> &str {
        self.answers.get(index).map_or("", String::as_str)
    }

    fn continue_response(&self) -> PromptResponse {
        // 答案与问题一一对应、顺序一致。
        PromptResponse::Answers(self.answers.clone())
    }

    fn clear(&mut self) {
        self.answers.iter_mut().for_each(String::clear);
    }
}

impl fmt::Debug for KeyboardInteractivePrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyboardInteractivePrompt")
            .field("target", &self.target)
            .field("user", &self.user)
            .field("fields", &self.fields)
            .field("answers", &"<redacted>")
            .finish()
    }
}

/// 缺少凭据（密码 / 私钥口令）。
#[derive(Clone, PartialEq, Eq)]
pub struct CredentialPrompt {
    /// 是谁在问。
    pub target: PromptTarget,
    /// 登录用户。
    pub user: String,
    /// 口令时为私钥路径；密码为 `None`。
    pub key_path: Option<String>,
    /// 是否记住到凭据存储（默认勾选）。
    pub remember: bool,
    secret: String,
}

impl CredentialPrompt {
    fn new(
        endpoint: &Endpoint,
        user: String,
        kind: CredentialKind,
        session_title: Option<String>,
    ) -> Self {
        CredentialPrompt {
            target: PromptTarget::new(endpoint, session_title),
            user,
            key_path: match kind {
                CredentialKind::Password => None,
                CredentialKind::KeyPassphrase { path } => Some(path),
            },
            remember: true,
            secret: String::new(),
        }
    }

    /// 是私钥口令（否则是登录密码）。
    pub fn is_passphrase(&self) -> bool {
        self.key_path.is_some()
    }

    /// 默认按钮：连接。
    pub fn default_button(&self) -> PromptButtonRole {
        PromptButtonRole::Primary
    }

    /// 设置输入内容。
    pub fn set_secret(&mut self, text: impl Into<String>) {
        self.secret = text.into();
    }

    /// 当前输入的字符数（视图画圆点用；不暴露明文）。
    pub fn secret_len(&self) -> usize {
        self.secret.chars().count()
    }

    fn connect_response(&self) -> PromptResponse {
        PromptResponse::Credential {
            secret: SecretString::new(self.secret.clone()),
            remember: self.remember,
        }
    }

    fn clear(&mut self) {
        self.secret.clear();
    }
}

impl fmt::Debug for CredentialPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialPrompt")
            .field("target", &self.target)
            .field("user", &self.user)
            .field("key_path", &self.key_path)
            .field("remember", &self.remember)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// 一个待显示的提示对话框。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptDialog {
    /// 主机密钥变更确认。
    HostKeyChanged(HostKeyPrompt),
    /// keyboard-interactive（二次验证）。
    KeyboardInteractive(KeyboardInteractivePrompt),
    /// 密码 / 私钥口令。
    Credential(CredentialPrompt),
    /// 文件传输相关（ZMODEM / XMODEM 选文件、选保存位置）：交给系统文件选择器，
    /// 选完后用 [`PromptQueue::respond`] 回答（`SavePath` / `Files` / `Cancel`）。
    Transfer(PromptRequest),
}

impl PromptDialog {
    /// 由核心的提问构造。
    pub fn from_request(request: PromptRequest, session_title: Option<String>) -> Self {
        match request {
            PromptRequest::HostKeyChanged {
                endpoint,
                key_type,
                fingerprint_sha256,
                known_hosts_path,
                line,
            } => PromptDialog::HostKeyChanged(HostKeyPrompt {
                target: PromptTarget::new(&endpoint, session_title),
                key_type,
                fingerprint: fingerprint_sha256,
                known_hosts_location: format!("{known_hosts_path}:{line}"),
            }),
            PromptRequest::KeyboardInteractive {
                endpoint,
                user,
                name,
                instruction,
                preamble,
                questions,
            } => PromptDialog::KeyboardInteractive(KeyboardInteractivePrompt::new(
                &endpoint,
                user,
                name,
                instruction,
                preamble,
                questions,
                session_title,
            )),
            PromptRequest::Credential {
                endpoint,
                user,
                kind,
            } => PromptDialog::Credential(CredentialPrompt::new(
                &endpoint,
                user,
                kind,
                session_title,
            )),
            other @ (PromptRequest::ZmodemSaveFile { .. }
            | PromptRequest::ZmodemPickFiles
            | PromptRequest::XmodemPickFile
            | PromptRequest::XmodemSaveFile) => PromptDialog::Transfer(other),
        }
    }

    /// 回车触发的应答：主机密钥与文件选择取消，其余提交输入。
    pub fn default_response(&self) -> PromptResponse {
        match self {
            PromptDialog::HostKeyChanged(_) | PromptDialog::Transfer(_) => PromptResponse::Cancel,
            PromptDialog::KeyboardInteractive(m) => m.continue_response(),
            PromptDialog::Credential(m) => m.connect_response(),
        }
    }

    /// 默认按钮的语义。
    pub fn default_button(&self) -> PromptButtonRole {
        match self {
            PromptDialog::HostKeyChanged(m) => m.default_button(),
            PromptDialog::KeyboardInteractive(m) => m.default_button(),
            PromptDialog::Credential(m) => m.default_button(),
            PromptDialog::Transfer(_) => PromptButtonRole::Cancel,
        }
    }

    fn clear(&mut self) {
        match self {
            PromptDialog::KeyboardInteractive(m) => m.clear(),
            PromptDialog::Credential(m) => m.clear(),
            PromptDialog::HostKeyChanged(_) | PromptDialog::Transfer(_) => {}
        }
    }
}

/// 队列里的一个提示。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptEntry {
    /// 核心给的请求 id，应答与撤销都靠它。
    pub id: PromptId,
    /// 关联会话。
    pub session: Option<SessionId>,
    /// 对话框数据。
    pub dialog: PromptDialog,
}

/// 提示队列。`R` 是回答核心的出口（生产环境 `Arc<Core>`）。
pub struct PromptQueue<R: PromptResponder> {
    responder: R,
    queue: VecDeque<PromptEntry>,
    current: Option<PromptEntry>,
    presentable: bool,
}

impl<R: PromptResponder> PromptQueue<R> {
    /// 创建空队列。
    pub fn new(responder: R) -> Self {
        PromptQueue {
            responder,
            queue: VecDeque::new(),
            current: None,
            presentable: true,
        }
    }

    /// 当前能否展示（主窗口是否存在）。不能展示时到达的提示直接按取消回答，
    /// 不让核心空等到超时。
    pub fn set_presentable(&mut self, presentable: bool) {
        self.presentable = presentable;
        if presentable {
            self.promote();
        }
    }

    /// 正在显示的提示。
    pub fn current(&self) -> Option<&PromptEntry> {
        self.current.as_ref()
    }

    /// 正在显示的提示（视图把用户输入写进去）。
    pub fn current_mut(&mut self) -> Option<&mut PromptEntry> {
        self.current.as_mut()
    }

    /// 排队中的提示 id（按到达顺序）。
    pub fn queued_ids(&self) -> Vec<PromptId> {
        self.queue.iter().map(|e| e.id).collect()
    }

    /// 没有任何提示（正在显示的和排队的都没有）。
    pub fn is_idle(&self) -> bool {
        self.current.is_none() && self.queue.is_empty()
    }

    /// 收到核心的提问。`session_title` 是关联标签的标题，用于说明是哪个标签在问。
    ///
    /// 同一个 id 重复到达时忽略。
    pub fn enqueue(
        &mut self,
        prompt: PromptId,
        session: Option<SessionId>,
        request: PromptRequest,
        session_title: Option<String>,
    ) {
        if self.current.as_ref().is_some_and(|c| c.id == prompt)
            || self.queue.iter().any(|e| e.id == prompt)
        {
            return;
        }
        self.queue.push_back(PromptEntry {
            id: prompt,
            session,
            dialog: PromptDialog::from_request(request, session_title),
        });
        self.promote();
    }

    /// 核心撤销提问（超时或会话关闭）：正在显示的关闭且不回答，排队中的直接移除。
    pub fn dismiss(&mut self, prompt: PromptId) {
        if self.current.as_ref().is_some_and(|c| c.id == prompt) {
            if let Some(mut done) = self.current.take() {
                done.dialog.clear();
            }
            self.promote();
        } else {
            self.queue.retain(|e| e.id != prompt);
        }
    }

    /// 用显式的应答回答当前提示（按钮点击、文件选择器的结果）。没有当前提示时返回 false。
    pub fn respond(&mut self, response: PromptResponse) -> bool {
        let Some(mut done) = self.current.take() else {
            return false;
        };
        self.responder.respond_prompt(done.id, response);
        done.dialog.clear();
        self.promote();
        true
    }

    /// 回车 / 主按钮：提交当前提示的默认应答（见 [`PromptDialog::default_response`]）。
    pub fn submit(&mut self) -> bool {
        match self.current.as_ref().map(|c| c.dialog.default_response()) {
            Some(response) => self.respond(response),
            None => false,
        }
    }

    /// 取消当前提示。
    pub fn cancel(&mut self) -> bool {
        self.respond(PromptResponse::Cancel)
    }

    /// 接受新的主机密钥。只在当前提示是主机密钥变更时有效。
    pub fn accept_host_key(&mut self) -> bool {
        if !matches!(
            self.current.as_ref().map(|c| &c.dialog),
            Some(PromptDialog::HostKeyChanged(_))
        ) {
            return false;
        }
        self.respond(PromptResponse::AcceptHostKey)
    }

    /// 全部按取消回答（应用退出时），队列清空。
    pub fn cancel_all(&mut self) {
        if let Some(mut done) = self.current.take() {
            self.responder
                .respond_prompt(done.id, PromptResponse::Cancel);
            done.dialog.clear();
        }
        for mut e in self.queue.drain(..) {
            self.responder.respond_prompt(e.id, PromptResponse::Cancel);
            e.dialog.clear();
        }
    }

    fn promote(&mut self) {
        while self.current.is_none() {
            let Some(next) = self.queue.pop_front() else {
                return;
            };
            if !self.presentable {
                // 没有窗口可展示：不能让核心一直等到超时，按取消回答。
                self.responder
                    .respond_prompt(next.id, PromptResponse::Cancel);
                continue;
            }
            self.current = Some(next);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    /// 记录应答的替身。
    #[derive(Clone, Default)]
    pub(crate) struct Recorder(pub Rc<RefCell<Vec<(PromptId, PromptResponse)>>>);

    impl PromptResponder for Recorder {
        fn respond_prompt(&self, prompt: PromptId, response: PromptResponse) {
            self.0.borrow_mut().push((prompt, response));
        }
    }

    pub(crate) fn pid(n: u64) -> PromptId {
        PromptId::from_raw(n)
    }

    pub(crate) fn endpoint() -> Endpoint {
        Endpoint {
            host: "db.example.com".into(),
            port: 2222,
            via: vec!["bastion".into(), "gw".into()],
        }
    }

    pub(crate) fn host_key_request() -> PromptRequest {
        PromptRequest::HostKeyChanged {
            endpoint: endpoint(),
            key_type: "ssh-ed25519".into(),
            fingerprint_sha256: "SHA256:abc".into(),
            known_hosts_path: "/home/u/.ssh/known_hosts".into(),
            line: 7,
        }
    }

    pub(crate) fn kbd_request() -> PromptRequest {
        PromptRequest::KeyboardInteractive {
            endpoint: endpoint(),
            user: "alice".into(),
            name: String::new(),
            instruction: "Use your token".into(),
            preamble: vec!["Push sent".into()],
            questions: vec![
                KbdQuestion {
                    text: "Verification code: ".into(),
                    echo: false,
                },
                KbdQuestion {
                    text: "Nickname: ".into(),
                    echo: true,
                },
            ],
        }
    }

    pub(crate) fn credential_request(kind: CredentialKind) -> PromptRequest {
        PromptRequest::Credential {
            endpoint: Endpoint {
                host: "::1".into(),
                port: 22,
                via: vec![],
            },
            user: "bob".into(),
            kind,
        }
    }

    fn queue() -> (PromptQueue<Recorder>, Recorder) {
        let r = Recorder::default();
        (PromptQueue::new(r.clone()), r)
    }

    #[test]
    fn host_key_dialog_data_and_safe_default() {
        let (mut q, rec) = queue();
        q.enqueue(
            pid(1),
            Some(SessionId::from_raw(5)),
            host_key_request(),
            Some("prod".into()),
        );
        let cur = q.current().unwrap();
        assert_eq!(cur.id, pid(1));
        assert_eq!(cur.session, Some(SessionId::from_raw(5)));
        let PromptDialog::HostKeyChanged(m) = &cur.dialog else {
            panic!()
        };
        assert_eq!(m.target.host_port, "db.example.com:2222");
        assert_eq!(m.target.via_chain().as_deref(), Some("bastion → gw"));
        assert_eq!(m.target.session_title.as_deref(), Some("prod"));
        assert_eq!(m.known_hosts_location, "/home/u/.ssh/known_hosts:7");
        assert_eq!(m.default_button(), PromptButtonRole::Cancel);
        assert_eq!(cur.dialog.default_button(), PromptButtonRole::Cancel);

        // 回车（submit）不会接受变了的密钥。
        assert!(q.submit());
        assert_eq!(
            rec.0.borrow().as_slice(),
            &[(pid(1), PromptResponse::Cancel)]
        );
    }

    #[test]
    fn accept_host_key_is_explicit() {
        let (mut q, rec) = queue();
        q.enqueue(pid(1), None, host_key_request(), None);
        assert!(q.accept_host_key());
        assert_eq!(
            rec.0.borrow().as_slice(),
            &[(pid(1), PromptResponse::AcceptHostKey)]
        );
        assert!(q.is_idle());
        assert!(!q.accept_host_key(), "没有当前提示");

        q.enqueue(pid(2), None, kbd_request(), None);
        assert!(!q.accept_host_key(), "不是主机密钥提示时无效");
        assert_eq!(rec.0.borrow().len(), 1);
        assert_eq!(q.current().unwrap().id, pid(2), "提示仍在");
    }

    #[test]
    fn keyboard_interactive_collects_answers_in_order() {
        let (mut q, rec) = queue();
        q.enqueue(pid(3), None, kbd_request(), None);
        {
            let PromptDialog::KeyboardInteractive(m) = &mut q.current_mut().unwrap().dialog else {
                panic!()
            };
            assert_eq!(m.name, None, "空串视为没有");
            assert_eq!(m.instruction.as_deref(), Some("Use your token"));
            assert_eq!(m.preamble, vec!["Push sent".to_owned()]);
            assert_eq!(
                m.fields,
                vec![
                    KbdField {
                        prompt: "Verification code: ".into(),
                        is_secure: true
                    },
                    KbdField {
                        prompt: "Nickname: ".into(),
                        is_secure: false
                    },
                ]
            );
            m.set_answer(0, "123456");
            m.set_answer(1, "al");
            m.set_answer(9, "ignored");
            assert_eq!(m.answer(0), "123456");
            assert_eq!(m.answer(9), "");
            assert!(!format!("{m:?}").contains("123456"), "Debug 不含明文");
        }
        assert!(q.submit());
        assert_eq!(
            rec.0.borrow().as_slice(),
            &[(
                pid(3),
                PromptResponse::Answers(vec!["123456".into(), "al".into()])
            )]
        );
    }

    #[test]
    fn credential_password_and_passphrase() {
        let (mut q, rec) = queue();
        q.enqueue(
            pid(4),
            None,
            credential_request(CredentialKind::Password),
            None,
        );
        {
            let PromptDialog::Credential(m) = &mut q.current_mut().unwrap().dialog else {
                panic!()
            };
            assert!(!m.is_passphrase() && m.key_path.is_none());
            assert!(m.remember, "默认记住");
            assert_eq!(m.target.host_port, "[::1]:22");
            assert_eq!(m.target.via_chain(), None);
            m.set_secret("p@ss");
            assert_eq!(m.secret_len(), 4);
            m.remember = false;
            assert!(!format!("{m:?}").contains("p@ss"));
        }
        q.submit();
        let log = rec.0.borrow();
        let PromptResponse::Credential { secret, remember } = &log[0].1 else {
            panic!()
        };
        assert_eq!(secret.expose(), "p@ss");
        assert!(!remember);
        drop(log);

        q.enqueue(
            pid(5),
            None,
            credential_request(CredentialKind::KeyPassphrase {
                path: "/k/id".into(),
            }),
            None,
        );
        let PromptDialog::Credential(m) = &q.current().unwrap().dialog else {
            panic!()
        };
        assert!(m.is_passphrase());
        assert_eq!(m.key_path.as_deref(), Some("/k/id"));
    }

    #[test]
    fn clear_wipes_typed_secrets() {
        let mut d = PromptDialog::from_request(credential_request(CredentialKind::Password), None);
        if let PromptDialog::Credential(m) = &mut d {
            m.set_secret("hunter2");
        }
        d.clear();
        let PromptDialog::Credential(m) = &d else {
            panic!()
        };
        assert_eq!(m.secret_len(), 0);

        let mut d = PromptDialog::from_request(kbd_request(), None);
        if let PromptDialog::KeyboardInteractive(m) = &mut d {
            m.set_answer(0, "123");
        }
        d.clear();
        assert_eq!(
            d.default_response(),
            PromptResponse::Answers(vec![String::new(), String::new()]),
            "清空后答案个数仍与问题一致"
        );
    }

    #[test]
    fn queue_shows_one_at_a_time_in_arrival_order() {
        let (mut q, rec) = queue();
        q.enqueue(pid(1), None, kbd_request(), None);
        q.enqueue(pid(2), None, host_key_request(), None);
        q.enqueue(
            pid(3),
            None,
            credential_request(CredentialKind::Password),
            None,
        );
        assert_eq!(q.current().unwrap().id, pid(1));
        assert_eq!(q.queued_ids(), vec![pid(2), pid(3)]);
        q.cancel();
        assert_eq!(q.current().unwrap().id, pid(2));
        q.cancel();
        assert_eq!(q.current().unwrap().id, pid(3));
        q.cancel();
        assert!(q.is_idle());
        let ids: Vec<_> = rec.0.borrow().iter().map(|(i, _)| *i).collect();
        assert_eq!(ids, vec![pid(1), pid(2), pid(3)]);
    }

    #[test]
    fn duplicate_prompt_id_is_ignored() {
        let (mut q, _) = queue();
        q.enqueue(pid(1), None, kbd_request(), None);
        q.enqueue(pid(1), None, kbd_request(), None);
        q.enqueue(pid(2), None, kbd_request(), None);
        q.enqueue(pid(2), None, kbd_request(), None);
        assert_eq!(q.queued_ids(), vec![pid(2)]);
    }

    #[test]
    fn dismiss_current_does_not_respond_and_shows_next() {
        let (mut q, rec) = queue();
        q.enqueue(pid(1), None, kbd_request(), None);
        q.enqueue(pid(2), None, host_key_request(), None);
        q.dismiss(pid(1));
        assert_eq!(q.current().unwrap().id, pid(2));
        assert!(rec.0.borrow().is_empty(), "核心已撤销，不需要回答");
        // 撤销后迟到的应答针对的是下一个提示。
        q.cancel();
        assert_eq!(rec.0.borrow()[0].0, pid(2));
    }

    #[test]
    fn dismiss_queued_removes_without_response() {
        let (mut q, rec) = queue();
        q.enqueue(pid(1), None, kbd_request(), None);
        q.enqueue(pid(2), None, kbd_request(), None);
        q.enqueue(pid(3), None, kbd_request(), None);
        q.dismiss(pid(2));
        assert_eq!(q.queued_ids(), vec![pid(3)]);
        q.dismiss(pid(99));
        assert_eq!(q.queued_ids(), vec![pid(3)]);
        assert!(rec.0.borrow().is_empty());
    }

    #[test]
    fn respond_without_current_is_noop() {
        let (mut q, rec) = queue();
        assert!(!q.respond(PromptResponse::Cancel));
        assert!(!q.submit());
        assert!(rec.0.borrow().is_empty());
    }

    #[test]
    fn transfer_prompts_use_explicit_responses() {
        let (mut q, rec) = queue();
        q.enqueue(
            pid(1),
            None,
            PromptRequest::ZmodemSaveFile {
                suggested_name: "a.bin".into(),
                size: Some(10),
            },
            None,
        );
        q.enqueue(pid(2), None, PromptRequest::ZmodemPickFiles, None);
        assert!(matches!(
            q.current().unwrap().dialog,
            PromptDialog::Transfer(_)
        ));
        assert_eq!(
            q.current().unwrap().dialog.default_button(),
            PromptButtonRole::Cancel
        );
        q.respond(PromptResponse::SavePath("/tmp/a.bin".into()));
        q.respond(PromptResponse::Files(vec!["/x".into()]));
        let log = rec.0.borrow();
        assert_eq!(
            log[0],
            (pid(1), PromptResponse::SavePath("/tmp/a.bin".into()))
        );
        assert_eq!(log[1], (pid(2), PromptResponse::Files(vec!["/x".into()])));
    }

    #[test]
    fn not_presentable_cancels_instead_of_hanging() {
        let (mut q, rec) = queue();
        q.set_presentable(false);
        q.enqueue(pid(1), None, kbd_request(), None);
        assert!(q.is_idle());
        assert_eq!(
            rec.0.borrow().as_slice(),
            &[(pid(1), PromptResponse::Cancel)]
        );
    }

    #[test]
    fn cancel_all_answers_everything_with_cancel() {
        let (mut q, rec) = queue();
        q.enqueue(pid(1), None, kbd_request(), None);
        q.enqueue(pid(2), None, kbd_request(), None);
        q.cancel_all();
        assert!(q.is_idle());
        let log = rec.0.borrow();
        assert_eq!(log.len(), 2);
        assert!(log.iter().all(|(_, r)| *r == PromptResponse::Cancel));
    }
}
