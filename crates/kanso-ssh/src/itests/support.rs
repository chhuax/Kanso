//! 测试共用的客户端侧工具：脚本化的 [`Interaction`]、参数构造、可"黑洞化"的字节流。

use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use kanso_types::message::Endpoint;
use kanso_types::prompt::CredentialKind;
use kanso_types::secret::SecretString;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::server::TestServer;
use crate::{
    AuthMethod, ConnectParams, Connected, HostKeyStore, Interaction, KbdRound, MemoryHostKeyStore,
    SshError, connect,
};

/// 一次 keyboard-interactive 提问的脚本化回答。
#[derive(Clone, Debug)]
pub enum KbdReply {
    Answers(Vec<String>),
    Cancel,
    /// 永远不回答（用于 180 秒超时测试）。
    Hang,
}

/// 脚本化的 [`Interaction`]，同时记录收到的所有提问。
#[derive(Default)]
pub struct Scripted {
    kbd: Mutex<VecDeque<KbdReply>>,
    /// 脚本用完后的默认回答；`None` 表示用完就取消。
    pub default_kbd: Mutex<Option<Vec<String>>>,
    creds: Mutex<VecDeque<Option<(String, bool)>>>,
    pub kbd_rounds: Mutex<Vec<KbdRound>>,
    pub cred_requests: Mutex<Vec<CredentialKind>>,
    /// 最近一次 keyboard-interactive 提问开始的（tokio）时刻，用来精确测量超时时长。
    pub kbd_started: Mutex<Option<tokio::time::Instant>>,
}

impl Scripted {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn kbd(self, replies: impl IntoIterator<Item = KbdReply>) -> Self {
        self.kbd.lock().unwrap().extend(replies);
        self
    }
    pub fn answers(self, a: &[&str]) -> Self {
        self.kbd(std::iter::once(KbdReply::Answers(
            a.iter().map(|s| (*s).to_owned()).collect(),
        )))
    }
    pub fn default_answers(self, a: &[&str]) -> Self {
        *self.default_kbd.lock().unwrap() = Some(a.iter().map(|s| (*s).to_owned()).collect());
        self
    }
    pub fn cred(self, c: Option<(&str, bool)>) -> Self {
        self.creds
            .lock()
            .unwrap()
            .push_back(c.map(|(s, r)| (s.to_owned(), r)));
        self
    }
    pub fn kbd_count(&self) -> usize {
        self.kbd_rounds.lock().unwrap().len()
    }
}

impl Interaction for Scripted {
    fn keyboard_interactive(
        &self,
        round: KbdRound,
    ) -> impl Future<Output = Option<Vec<String>>> + Send {
        self.kbd_rounds.lock().unwrap().push(round);
        *self.kbd_started.lock().unwrap() = Some(tokio::time::Instant::now());
        let next = self.kbd.lock().unwrap().pop_front();
        let default = self.default_kbd.lock().unwrap().clone();
        async move {
            match next {
                Some(KbdReply::Answers(a)) => Some(a),
                Some(KbdReply::Cancel) => None,
                Some(KbdReply::Hang) => std::future::pending().await,
                None => default,
            }
        }
    }

    fn credential(
        &self,
        _endpoint: &Endpoint,
        _user: &str,
        kind: CredentialKind,
    ) -> impl Future<Output = Option<(SecretString, bool)>> + Send {
        self.cred_requests.lock().unwrap().push(kind);
        let next = self.creds.lock().unwrap().pop_front().flatten();
        async move { next.map(|(s, r)| (SecretString::new(s), r)) }
    }
}

pub const USER: &str = "tester";

/// 构造连接参数；主机密钥存储用内存实现，需要别的存储时再覆盖 `host_keys`。
pub fn params(server: &TestServer, auth: AuthMethod) -> ConnectParams {
    params_ep(server.endpoint(), auth)
}

pub fn params_ep(endpoint: Endpoint, auth: AuthMethod) -> ConnectParams {
    ConnectParams {
        endpoint,
        user: USER.into(),
        auth,
        saved_password: None,
        saved_passphrase: None,
        agent_socket: None,
        home_dir: None,
        host_keys: Arc::new(MemoryHostKeyStore::new()),
        revoked_keys: None,
        keepalive: kanso_types::profile::KeepaliveConfig::default(),
    }
}

pub fn params_with_store(
    server: &TestServer,
    auth: AuthMethod,
    store: Arc<dyn HostKeyStore>,
) -> ConnectParams {
    ConnectParams {
        host_keys: store,
        ..params(server, auth)
    }
}

/// 连接并认证：失败时 panic，便于只关心成功路径的测试。
pub async fn connect_ok(server: &TestServer, auth: AuthMethod, i: &Scripted) -> Connected {
    connect(params(server, auth), i)
        .await
        .unwrap_or_else(|e| panic!("connect failed: {e}"))
}

pub async fn connect_err(p: ConnectParams, i: &Scripted) -> SshError {
    match connect(p, i).await {
        Ok(_) => panic!("expected connect to fail"),
        Err(e) => e,
    }
}

/// 包住一个字节流，`block` 置位后读不到任何数据、写入被静默丢弃：模拟网络黑洞
/// （既收不到 keepalive 的回复，也发不出去），用来测 keepalive 断线检测。
pub struct Blackhole<S> {
    pub inner: S,
    pub block: Arc<AtomicBool>,
}

impl<S: AsyncRead + Unpin> AsyncRead for Blackhole<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.block.load(Ordering::SeqCst) {
            // 不注册 waker 也不返回数据：对端"再也没有声音"。
            return Poll::Pending;
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Blackhole<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.block.load(Ordering::SeqCst) {
            return Poll::Ready(Ok(data.len()));
        }
        Pin::new(&mut self.inner).poll_write(cx, data)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// 等一个条件成立（真实时间，最多 3 秒）。
pub async fn eventually(mut cond: impl FnMut() -> bool) {
    for _ in 0..300 {
        if cond() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("condition not met in time");
}
