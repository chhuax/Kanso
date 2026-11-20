//! 测试用进程内 russh server：监听 127.0.0.1 随机端口，行为由 [`ServerConfig`] 脚本化。
//!
//! 不访问外部网络，也不碰真实的 `~/.ssh` 或 agent。

use std::borrow::Cow;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kanso_types::message::Endpoint;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::{PrivateKey, PublicKey};
use russh::server::{
    Auth, Config, Handler, Msg, PublicKeyOffer, Response, Session, VerifiedPublicKey, run_stream,
};
use russh::{Channel, ChannelId, ChannelOpenFailure, MethodKind, MethodSet, Preferred, Pty};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// 测试用 ed25519 密钥（固定种子，可复现）。
pub fn ed25519_key(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

/// 测试用 RSA 密钥（`ssh-keygen` 预生成的一次性测试密钥，没有任何真实用途）。
/// 内嵌而不是运行时生成：debug 构建下生成 2048 位 RSA 要几秒到几十秒。
pub fn rsa_key() -> PrivateKey {
    PrivateKey::from_openssh(include_str!("data/test_rsa_key")).unwrap()
}

/// 加密的 ed25519 私钥（口令见 [`ENCRYPTED_KEY_PASSPHRASE`]）。
pub const ENCRYPTED_KEY_PEM: &str = include_str!("data/test_ed25519_encrypted_key");
/// [`ENCRYPTED_KEY_PEM`] 的口令。
pub const ENCRYPTED_KEY_PASSPHRASE: &str = "correct horse";

/// keyboard-interactive 的一轮（服务器侧脚本）。
#[derive(Clone, Debug)]
pub struct KbdRoundSpec {
    pub name: String,
    pub instruction: String,
    /// `(提示, 是否回显)`；为空表示只有文字的轮次。
    pub prompts: Vec<(String, bool)>,
}

impl KbdRoundSpec {
    pub fn question(prompt: &str, echo: bool) -> Self {
        Self {
            name: String::new(),
            instruction: String::new(),
            prompts: vec![(prompt.to_owned(), echo)],
        }
    }
    pub fn text(instruction: &str) -> Self {
        Self {
            name: String::new(),
            instruction: instruction.to_owned(),
            prompts: vec![],
        }
    }
}

/// 认证策略。
#[derive(Clone)]
pub struct Policy {
    /// 对外宣告的认证方法。
    pub methods: MethodSet,
    /// `none` 直接放行（dropbear 那类设备）。
    pub none_ok: bool,
    /// 接受的密码。
    pub password: Option<String>,
    /// 密码正确时只算 partial success。
    pub password_partial: bool,
    /// 接受的公钥。
    pub pubkeys: Vec<PublicKey>,
    /// 公钥正确时只算 partial success。
    pub pubkey_partial: bool,
    /// 拒绝用 SHA-1（`ssh-rsa`）签名的 RSA 公钥认证。
    pub reject_sha1_rsa: bool,
    /// partial success 之后服务器接着提供的方法。
    pub after_partial: MethodSet,
    /// keyboard-interactive 脚本；每轮后检查答案：`kbd_expect[i]` 为 `Some` 时必须逐项相等。
    pub kbd: Vec<KbdRoundSpec>,
    pub kbd_expect: Vec<Option<Vec<String>>>,
    /// 脚本跑完是否放行（`false` 则拒绝）。
    pub kbd_accept: bool,
    /// 无限追问（用于 16 轮上限测试）：脚本跑完后重复最后一轮。
    pub kbd_endless: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            methods: MethodSet::from(
                &[
                    MethodKind::Password,
                    MethodKind::PublicKey,
                    MethodKind::KeyboardInteractive,
                ][..],
            ),
            none_ok: false,
            password: None,
            password_partial: false,
            pubkeys: vec![],
            pubkey_partial: false,
            reject_sha1_rsa: false,
            after_partial: MethodSet::empty(),
            kbd: vec![],
            kbd_expect: vec![],
            kbd_accept: true,
            kbd_endless: false,
        }
    }
}

pub fn methods(list: &[MethodKind]) -> MethodSet {
    MethodSet::from(list)
}

/// 服务器配置。
pub struct ServerConfig {
    pub preferred: Preferred,
    pub keys: Vec<PrivateKey>,
    pub policy: Policy,
    /// gex 只给 2048 位群（模拟只有固定小群的设备）。
    pub gex_group_2048: bool,
    /// 服务器的空闲超时；默认关闭：暂停时钟的测试里 spawn_blocking 会让虚拟时间自动跳进，服务器的 600 秒空闲计时会先到期而误断连接。
    pub inactivity: Option<Duration>,
    /// 提供 `sftp` 子系统（映射到临时目录的真实文件系统）；`None` 时拒绝子系统请求。
    pub sftp: Option<crate::test_sftp_server::SftpServerConfig>,
    /// 是否允许 `direct-tcpip`（跳板转发）；`false` 时按"管理上禁止"拒绝。
    /// 允许时转发到 **127.0.0.1** 上同端口（忽略请求里的主机名），连不上则以 connect failed 拒绝。
    pub forwarding: bool,
    /// exec 命令为 `sh`（远端 cwd 探测）时，收到 stdin EOF 后回复的 stdout 内容；
    /// `None` 表示一直不回复（用来测超时）。
    pub sh_reply: Option<Vec<u8>>,
    /// 模拟交互 shell 的提示符；`Some` 时 shell 通道按行缓冲输入并"执行"（见 `fake_shell_input`），
    /// 用来测试依赖提示符的功能（命令跟踪推断）。`None` 时 shell 通道是原样回显。
    pub fake_shell_prompt: Option<Vec<u8>>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            preferred: Preferred::default(),
            keys: vec![ed25519_key(1)],
            policy: Policy::default(),
            gex_group_2048: false,
            inactivity: None,
            sftp: None,
            forwarding: true,
            sh_reply: None,
            fake_shell_prompt: None,
        }
    }
}

/// 服务器观察到的事件，供断言使用。
#[derive(Default, Debug)]
pub struct Observed {
    pub accepted: usize,
    /// 服务器侧会话结束（handler 被释放）的次数。
    pub closed: usize,
    /// 每次会话结束时取的全局递增序号（跨服务器可比），用来断言"内层先于外层断开"。
    pub close_seqs: Vec<u64>,
    /// 收到的 `direct-tcpip` 请求 `(host, port)`。
    pub forwards: Vec<(String, u32)>,
    pub auth_log: Vec<String>,
    pub ptys: Vec<(String, u32, u32)>,
    pub envs: Vec<(String, String)>,
    pub window_changes: Vec<(u32, u32)>,
    pub kbd_answers: Vec<Vec<String>>,
    pub execs: Vec<String>,
    pub shells: usize,
    /// 正在运行的脚本化 exec（见 `run_exec_script`）数量及其历史峰值，用来断言并发上限。
    pub exec_running: usize,
    pub exec_peak: usize,
    pub stdin_seen: Vec<u8>,
}

/// 所有测试服务器共用的断开序号。
static CLOSE_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
pub struct Stats(Mutex<Observed>);

impl Stats {
    pub fn with<R>(&self, f: impl FnOnce(&mut Observed) -> R) -> R {
        f(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

pub struct TestServer {
    pub port: u16,
    pub stats: Arc<Stats>,
    /// SFTP 子系统的服务器侧统计（未启用 SFTP 时全为 0）。
    pub sftp_stats: Arc<crate::test_sftp_server::SftpServerStats>,
    task: JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TestServer {
    pub async fn start(cfg: ServerConfig) -> TestServer {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let stats = Arc::new(Stats::default());
        let sftp_stats = Arc::new(crate::test_sftp_server::SftpServerStats::default());
        let config = Arc::new(Config {
            keys: cfg.keys,
            preferred: cfg.preferred,
            methods: cfg.policy.methods.clone(),
            // 默认的 1 秒拒绝延迟只会拖慢测试。
            auth_rejection_time: Duration::from_millis(1),
            auth_rejection_time_initial: Some(Duration::ZERO),
            inactivity_timeout: cfg.inactivity,
            max_auth_attempts: 100,
            ..Config::default()
        });
        let policy = Arc::new(cfg.policy);
        let gex_2048 = cfg.gex_group_2048;
        let forwarding = cfg.forwarding;
        let sh_reply = cfg.sh_reply.map(Arc::new);
        let fake_prompt = cfg.fake_shell_prompt;
        let st = stats.clone();
        let sftp = cfg.sftp.map(|c| (c, sftp_stats.clone()));
        let task = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let handler = TestHandler {
                    policy: policy.clone(),
                    stats: st.clone(),
                    kbd_idx: 0,
                    gex_2048,
                    sftp: sftp.clone(),
                    channels: HashMap::new(),
                    sftp_channels: Default::default(),
                    forwarding,
                    sh_reply: sh_reply.clone(),
                    forward_channels: Default::default(),
                    sh_channels: Default::default(),
                    fake_prompt: fake_prompt.clone(),
                    fake_line: Vec::new(),
                    fake_running: false,
                };
                st.with(|o| o.accepted += 1);
                let config = config.clone();
                tokio::spawn(async move {
                    if let Ok(running) = run_stream(config, sock, handler).await {
                        let _ = running.await;
                    }
                });
            }
        });
        TestServer {
            port,
            stats,
            sftp_stats,
            task,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.port))
    }

    pub fn endpoint(&self) -> Endpoint {
        Endpoint {
            host: "127.0.0.1".into(),
            port: self.port,
            via: vec![],
        }
    }
}

struct TestHandler {
    policy: Arc<Policy>,
    stats: Arc<Stats>,
    kbd_idx: usize,
    gex_2048: bool,
    sftp: Option<(
        crate::test_sftp_server::SftpServerConfig,
        Arc<crate::test_sftp_server::SftpServerStats>,
    )>,
    /// 已打开、尚未被子系统接管的通道（仅在启用 SFTP 时保存）。
    channels: HashMap<ChannelId, Channel<Msg>>,
    /// 已被 SFTP 子系统接管的通道：它们的数据由子系统自己的流读取，不能再走 shell 的回显逻辑。
    sftp_channels: std::collections::HashSet<ChannelId>,
    forwarding: bool,
    sh_reply: Option<Arc<Vec<u8>>>,
    /// `direct-tcpip` 通道：russh 也会把这些通道的数据交给 `data()`，必须忽略，
    /// 否则下面的回显逻辑会把转发的字节原样发回给对端。
    forward_channels: std::collections::HashSet<ChannelId>,
    /// 执行 `sh`（远端 cwd 探测）的 exec 通道；按通道判断，shell 通道上的回显不受影响。
    sh_channels: std::collections::HashSet<ChannelId>,
    /// 模拟 shell 的提示符，见 [`ServerConfig::fake_shell_prompt`]。
    fake_prompt: Option<Vec<u8>>,
    /// 模拟 shell 当前正在输入的行。
    fake_line: Vec<u8>,
    /// 模拟 shell 里有命令在"运行"（没有回到提示符）。
    fake_running: bool,
}

impl TestHandler {
    /// 模拟 shell：逐字节回显（回车回显成 `\r\n`）；回车时按整行"执行"：
    /// - 以 `hold`、`claude`、`codex` 开头的命令一直"运行"，直到输入 `done` / `exit` 或 Ctrl+C；
    /// - 其他命令立即输出 `ran:<行>` 并回到提示符；空行直接回到提示符。
    ///
    /// 提示符只在命令返回时输出（与真实 shell 一样，不会在没有回车时重复）。
    fn fake_shell_input(&mut self, data: &[u8], prompt: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for &b in data {
            match b {
                0x03 => {
                    self.fake_line.clear();
                    self.fake_running = false;
                    out.extend_from_slice(b"^C\r\n");
                    out.extend_from_slice(prompt);
                }
                b'\r' => {
                    out.extend_from_slice(b"\r\n");
                    let line =
                        String::from_utf8_lossy(&std::mem::take(&mut self.fake_line)).into_owned();
                    if self.fake_running {
                        if line == "done" || line == "exit" {
                            self.fake_running = false;
                            out.extend_from_slice(b"bye\r\n");
                            out.extend_from_slice(prompt);
                        }
                    } else if line.is_empty() {
                        out.extend_from_slice(prompt);
                    } else if ["hold", "claude", "codex"]
                        .iter()
                        .any(|c| line.starts_with(c))
                    {
                        self.fake_running = true;
                        out.extend_from_slice(b"TUI\r\n");
                    } else {
                        out.extend_from_slice(format!("ran:{line}\r\n").as_bytes());
                        out.extend_from_slice(prompt);
                    }
                }
                _ => {
                    self.fake_line.push(b);
                    out.push(b);
                }
            }
        }
        out
    }
}

impl Drop for TestHandler {
    fn drop(&mut self) {
        let seq = CLOSE_SEQ.fetch_add(1, Ordering::SeqCst);
        self.stats.with(|o| {
            o.closed += 1;
            o.close_seqs.push(seq);
        });
    }
}

fn same_key(a: &PublicKey, b: &PublicKey) -> bool {
    a.key_data() == b.key_data()
}

impl TestHandler {
    fn log(&self, s: String) {
        self.stats.with(|o| o.auth_log.push(s));
    }

    fn kbd_prompt(&self, idx: usize) -> Auth {
        let spec = &self.policy.kbd[idx.min(self.policy.kbd.len() - 1)];
        Auth::Partial {
            name: Cow::Owned(spec.name.clone()),
            instructions: Cow::Owned(spec.instruction.clone()),
            prompts: Cow::Owned(
                spec.prompts
                    .iter()
                    .map(|(p, e)| (Cow::Owned(p.clone()), *e))
                    .collect(),
            ),
        }
    }

    fn partial_reject(&self) -> Auth {
        Auth::Reject {
            proceed_with_methods: Some(self.policy.after_partial.clone()),
            partial_success: true,
        }
    }
}

impl Handler for TestHandler {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        self.log("none".into());
        Ok(if self.policy.none_ok {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        self.log(format!("password:{password}"));
        if self.policy.password.as_deref() == Some(password) {
            return Ok(if self.policy.password_partial {
                self.partial_reject()
            } else {
                Auth::Accept
            });
        }
        Ok(Auth::reject())
    }

    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        offer: PublicKeyOffer<'_>,
    ) -> Result<Auth, Self::Error> {
        self.log(format!("pk-offer:{}", offer.algorithm));
        if self.policy.reject_sha1_rsa
            && matches!(offer.algorithm, russh::keys::Algorithm::Rsa { hash: None })
        {
            return Ok(Auth::reject());
        }
        if self
            .policy
            .pubkeys
            .iter()
            .any(|k| same_key(k, offer.public_key))
        {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        self.log(format!("publickey:{}", key.algorithm));
        if self
            .policy
            .pubkeys
            .iter()
            .any(|k| same_key(k, key.public_key))
        {
            return Ok(if self.policy.pubkey_partial {
                self.partial_reject()
            } else {
                Auth::Accept
            });
        }
        Ok(Auth::reject())
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        if self.policy.kbd.is_empty() {
            return Ok(Auth::reject());
        }
        let Some(response) = response else {
            self.log("kbd-start".into());
            self.kbd_idx = 0;
            return Ok(self.kbd_prompt(0));
        };
        let answers: Vec<String> = response
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .collect();
        self.stats.with(|o| o.kbd_answers.push(answers.clone()));
        if let Some(Some(expect)) = self.policy.kbd_expect.get(self.kbd_idx)
            && *expect != answers
        {
            return Ok(Auth::reject());
        }
        self.kbd_idx += 1;
        if self.kbd_idx < self.policy.kbd.len() || self.policy.kbd_endless {
            return Ok(self.kbd_prompt(self.kbd_idx));
        }
        Ok(if self.policy.kbd_accept {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.sftp.is_some() {
            self.channels.insert(channel.id(), channel);
        }
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let (Some((cfg, stats)), "sftp") = (self.sftp.as_ref(), name) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        let Some(ch) = self.channels.remove(&channel) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        stats
            .subsystem_opens
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.sftp_channels.insert(channel);
        session.channel_success(channel)?;
        tokio::spawn(crate::test_sftp_server::serve(
            ch.into_stream(),
            cfg.clone(),
            stats.clone(),
        ));
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.stats.with(|o| {
            o.forwards
                .push((host_to_connect.to_owned(), port_to_connect))
        });
        if !self.forwarding {
            reply
                .reject(ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        }
        let Ok(port) = u16::try_from(port_to_connect) else {
            reply.reject(ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(mut tcp) => {
                self.forward_channels.insert(channel.id());
                reply.accept().await;
                tokio::spawn(async move {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut tcp).await;
                });
            }
            Err(_) => reply.reject(ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.stats
            .with(|o| o.ptys.push((term.to_owned(), col_width, row_height)));
        session.channel_success(channel)?;
        Ok(())
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        variable_name: &str,
        variable_value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.stats.with(|o| {
            o.envs
                .push((variable_name.to_owned(), variable_value.to_owned()))
        });
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.stats.with(|o| o.shells += 1);
        session.channel_success(channel)?;
        session.data(channel, b"WELCOME\r\n".to_vec())?;
        if let Some(prompt) = self.fake_prompt.clone() {
            session.data(channel, prompt)?;
        }
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let cmd = String::from_utf8_lossy(data).into_owned();
        self.stats.with(|o| o.execs.push(cmd.clone()));
        if cmd == "sh" {
            self.sh_channels.insert(channel);
        }
        session.channel_success(channel)?;
        // 脚本化命令（MCP exec 测试用），见 `run_exec_script`。
        if is_exec_script(&cmd) {
            let handle = session.handle();
            let stats = self.stats.clone();
            tokio::spawn(run_exec_script(handle, channel, cmd, stats));
            return Ok(());
        }
        // "late-status"：先 eof 再发退出码再 close，模拟 eof/close 之后才发退出码的服务器。
        if cmd == "late-status" {
            session.data(channel, b"out\n".to_vec())?;
            session.extended_data(channel, 1, b"err\n".to_vec())?;
            session.eof(channel)?;
            session.exit_status_request(channel, 7)?;
            session.close(channel)?;
        }
        // 其他命令（如 "cat"）等 stdin 到 EOF 后再结束，见 channel_eof。
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.sftp_channels.contains(&channel) || self.forward_channels.contains(&channel) {
            return Ok(());
        }
        self.stats.with(|o| o.stdin_seen.extend_from_slice(data));
        // exec "cat"：只累积 stdin，等 EOF 后一次性回显（见 channel_eof）。
        // exec "sh"：stdin 是 cwd 探测脚本，只累积，不回显。
        if self
            .stats
            .with(|o| o.execs.last().map(String::as_str) == Some("cat"))
            || self.sh_channels.contains(&channel)
        {
            return Ok(());
        }
        if let Some(prompt) = self.fake_prompt.clone() {
            let out = self.fake_shell_input(data, &prompt);
            if !out.is_empty() {
                session.data(channel, out)?;
            }
            return Ok(());
        }
        if data == b"disconnect\n" {
            // 模拟服务器掉线：handler 返回错误会让整个会话结束。
            return Err(russh::Error::Disconnect);
        }
        if data == b"exit\n" {
            session.exit_status_request(channel, 3)?;
            session.eof(channel)?;
            session.close(channel)?;
        } else if data == b"err\n" {
            session.extended_data(channel, 1, b"to-stderr\n".to_vec())?;
        } else {
            session.data(channel, data.to_vec())?;
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.forward_channels.contains(&channel) {
            return Ok(());
        }
        // exec "cat"：stdin 结束后把收到的内容回显并带退出码结束。
        let seen = self.stats.with(|o| o.stdin_seen.clone());
        if self
            .stats
            .with(|o| o.execs.last().map(String::as_str) == Some("cat"))
        {
            session.data(channel, seen)?;
            session.eof(channel)?;
            session.exit_status_request(channel, 0)?;
            session.close(channel)?;
        } else if self.sh_channels.contains(&channel)
            && let Some(reply) = self.sh_reply.clone()
        {
            session.data(channel, reply.as_ref().clone())?;
            session.eof(channel)?;
            session.exit_status_request(channel, 0)?;
            session.close(channel)?;
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.stats
            .with(|o| o.window_changes.push((col_width, row_height)));
        Ok(())
    }

    async fn lookup_dh_gex_group(
        &mut self,
        gex_params: &russh::client::GexParams,
    ) -> Result<Option<russh::kex::dh::groups::DhGroup>, Self::Error> {
        if self.gex_2048 {
            // 设备只有一个 2048 位群：只要客户端允许的区间覆盖它就用它。
            let g = russh::kex::dh::groups::DH_GROUP14.clone();
            if gex_params.min_group_size() <= 2048 && 2048 <= gex_params.max_group_size() {
                return Ok(Some(g));
            }
            return Ok(None);
        }
        Ok(Some(russh::kex::dh::groups::DH_GROUP14.clone()))
    }
}

/// 命令是否是脚本化 exec：以 `echo ` / `sleep ` / `exit ` 开头，或就是 `hang`。
fn is_exec_script(cmd: &str) -> bool {
    cmd.starts_with("echo ")
        || cmd.starts_with("sleep ")
        || cmd.starts_with("exit ")
        || cmd == "hang"
}

/// 执行脚本化 exec。语法：用 `;` 分隔的语句——
/// - `echo <文本>`：写 stdout；末尾加 ` 1>&2` 写 stderr；文本里的 `\e` 换成 ESC（造 ANSI 序列）；
/// - `sleep <秒>`：等待（可带小数）；
/// - `exit <n>`：设置退出码（默认 0）；
/// - `hang`：永不结束（测超时与通道关闭）。
///
/// 全部语句执行完后发 eof、退出码并 close。运行期间计入 `exec_running` / `exec_peak`。
async fn run_exec_script(
    handle: russh::server::Handle,
    channel: ChannelId,
    cmd: String,
    stats: Arc<Stats>,
) {
    stats.with(|o| {
        o.exec_running += 1;
        o.exec_peak = o.exec_peak.max(o.exec_running);
    });
    let mut status = 0u32;
    for stmt in cmd.split(';').map(str::trim) {
        if let Some(rest) = stmt.strip_prefix("echo ") {
            let (text, to_stderr) = match rest.strip_suffix(" 1>&2") {
                Some(t) => (t, true),
                None => (rest, false),
            };
            let bytes = format!("{}\n", text.replace("\\e", "\x1b")).into_bytes();
            let _ = if to_stderr {
                handle
                    .extended_data(channel, 1, bytes)
                    .await
                    .map_err(|_| ())
            } else {
                handle.data(channel, bytes).await.map_err(|_| ())
            };
        } else if let Some(secs) = stmt.strip_prefix("sleep ") {
            tokio::time::sleep(Duration::from_secs_f64(secs.parse().unwrap_or(0.0))).await;
        } else if let Some(n) = stmt.strip_prefix("exit ") {
            status = n.parse().unwrap_or(0);
        } else if stmt == "hang" {
            std::future::pending::<()>().await;
        }
    }
    let _ = handle.eof(channel).await;
    let _ = handle.exit_status_request(channel, status).await;
    let _ = handle.close(channel).await;
    stats.with(|o| o.exec_running -= 1);
}

/// 只支持旧算法（DH group14-sha1 / ssh-rsa / aes128-cbc / hmac-sha1）、认证全放行的服务器配置，
/// 用来验证"使用了旧算法"的提示链路（ssh.md §2.2）。
pub fn legacy_only_config() -> ServerConfig {
    use russh::keys::Algorithm;
    use russh::{cipher, kex, mac};
    ServerConfig {
        preferred: Preferred {
            // 扩展标记要跟在 kex 列表后面，否则 russh 的严格 kex 协商对不上。
            kex: Cow::Owned(vec![
                kex::DH_G14_SHA1,
                kex::EXTENSION_SUPPORT_AS_SERVER,
                kex::EXTENSION_OPENSSH_STRICT_KEX_AS_SERVER,
            ]),
            key: Cow::Owned(vec![Algorithm::Rsa { hash: None }]),
            cipher: Cow::Owned(vec![cipher::AES_128_CBC]),
            mac: Cow::Owned(vec![mac::HMAC_SHA1]),
            ..Preferred::default()
        },
        keys: vec![rsa_key()],
        policy: Policy {
            none_ok: true,
            ..Policy::default()
        },
        ..ServerConfig::default()
    }
}

/// 公钥的 SSH wire 格式 blob，用来预置 known_hosts 记录。
pub fn public_key_blob(key: &PrivateKey) -> Vec<u8> {
    use russh::keys::PublicKeyBase64;
    key.public_key().public_key_bytes()
}
