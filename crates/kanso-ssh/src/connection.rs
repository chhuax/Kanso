//! 单跳连接：TCP 连接、握手、主机密钥校验、认证，以及认证完成后的共享连接句柄（ssh.md §2、§3、§6）。
//!
//! 跳板链与 ProxyCommand 在 [`crate::chain`] / [`crate::proxy`]；[`connect_over`] 接受任意字节流，
//! 跳板链就是在上一跳的 `direct-tcpip` 通道上握手。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kanso_types::event::LegacyHop;
use kanso_types::message::{AlgorithmCategory, Endpoint, Reason};
use kanso_types::profile::KeepaliveConfig;
use kanso_types::prompt::CredentialKind;
use kanso_types::secret::SecretString;
use russh::client::{DisconnectReason, GexParams, Handle, Handler};
use russh::{Disconnect, Preferred, SshId};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::watch;

use crate::HostKeyStore;
use crate::algorithms::{
    self, GEX_MAX_BITS, GEX_MIN_BITS, GEX_PREFERRED_BITS, NegotiatedAlgorithms,
    is_pseudo_algorithm, legacy_algorithms_used,
};
use crate::auth::{self, AuthMethod};
use crate::channel::{ExecChannel, PtyRequest, ShellChannel};
use crate::error::SshError;
use crate::hostkey::{self, RevokedKeys};
use crate::interaction::Interaction;

/// keepalive 间隔（ssh.md §2.4）。
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
/// 连续多少次 keepalive 无回应后判定断线（ssh.md §2.4）。
///
/// russh 的语义是"已发出 N 次仍无任何收包，再过一个间隔仍无收包"才断开，
/// 所以实际在约 `(N + 1) * 间隔`（120 秒）后判定。
pub const KEEPALIVE_MAX: usize = 3;

/// 发给服务器的客户端标识。
const CLIENT_ID: &str = "SSH-2.0-Kanso";

/// 一次连接所需的全部输入。
///
/// 凭据都是可选的：缺少时通过 [`Interaction`] 向用户要。
pub struct ConnectParams {
    /// 连接对象；单跳时 `via` 为空，结构上为跳板链保留。
    pub endpoint: Endpoint,
    /// 登录用户（调用方保证非空，空用户在连接前就该报错，ssh.md §1）。
    pub user: String,
    /// 认证方式。
    pub auth: AuthMethod,
    /// 已保存的密码（[`AuthMethod::Password`] 用）。
    pub saved_password: Option<SecretString>,
    /// 已保存的私钥口令（[`AuthMethod::PublicKey`] 用）。
    pub saved_passphrase: Option<SecretString>,
    /// ssh-agent 的路径：Unix 为 unix socket 路径（core 读 `SSH_AUTH_SOCK` 后传入），
    /// Windows 为命名管道路径；`None` 表示没有指定。库内不读进程环境，便于测试与沙盒。
    pub agent_socket: Option<PathBuf>,
    /// 用户主目录，用来展开私钥路径开头的 `~/`；`None` 时回退到系统查询。
    pub home_dir: Option<PathBuf>,
    /// 主机密钥存储。
    pub host_keys: Arc<dyn HostKeyStore>,
    /// 主机密钥吊销查询；没有吊销概念的存储传 `None`。
    pub revoked_keys: Option<Arc<dyn RevokedKeys>>,
    /// 保活配置（每个 Profile 独立，跳板链上每一跳用自己 Profile 的配置）。
    pub keepalive: KeepaliveConfig,
}

/// 用户在提示里选了"记住"的凭据。认证成功后交给调用方，由 core 决定存不存。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RememberedCredential {
    /// 凭据种类。
    pub kind: CredentialKind,
    /// 凭据明文。
    pub secret: SecretString,
}

/// [`connect`] 成功的结果。
pub struct Connected {
    /// 已认证的连接，可克隆共享。
    pub connection: Connection,
    /// 用户在提示里选了"记住"的凭据（只在认证成功后才返回，避免记住错误的密码）。
    pub remember: Vec<RememberedCredential>,
}

/// 连接为什么结束。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// keepalive 无响应。
    KeepaliveTimeout,
    /// 对端关闭、网络错误或其他意外中断。
    Lost,
    /// 本端主动断开（最后一个使用者释放）。
    Local,
}

impl CloseReason {
    /// 转成错误，供通道层告诉用户"为什么会话结束了"。
    pub fn to_error(self, endpoint: &Endpoint) -> SshError {
        match self {
            CloseReason::KeepaliveTimeout => SshError::KeepaliveTimeout {
                endpoint: endpoint.clone(),
            },
            CloseReason::Lost | CloseReason::Local => SshError::ConnectionLost {
                endpoint: endpoint.clone(),
            },
        }
    }
}

/// 握手期间 handler 内部使用的错误：除了 russh 自己的错误，还要把我们的结构化错误
/// （主机密钥变更等）原样带出 `connect_stream`，不被 russh 压扁成字符串。
#[derive(Debug)]
pub(crate) enum HandlerError {
    Russh(russh::Error),
    Ssh(SshError),
}

impl From<russh::Error> for HandlerError {
    fn from(e: russh::Error) -> Self {
        HandlerError::Russh(e)
    }
}

/// handler 与连接句柄之间共享的状态。
struct Shared {
    /// 首次密钥交换协商出的算法。
    negotiated: Mutex<Option<NegotiatedAlgorithms>>,
    /// 连接结束原因。发送端只放在 handler 里：handler 随会话任务结束而释放，
    /// 接收端就能据此得知"会话任务已经结束"，即使没来得及写入原因。
    closed: watch::Sender<Option<CloseReason>>,
    /// 会话是因为"服务器没有给出任何可继续的认证方法"而结束的（russh 此时主动结束会话）。
    auth_exhausted: Arc<AtomicBool>,
}

/// 认证阶段判断"会话是不是中途死了"的探针。
///
/// russh 在会话结束后调用 `authenticate_*` 会返回"空方法集、非 partial 的失败"，
/// 与"被服务器拒绝"长得一样；而服务器拒绝后若不再给任何可用方法，russh 也会主动结束会话。
/// 两种情况都表现为"失败 + 会话已关闭"，必须靠结束原因区分：前者是断线，后者是普通的认证失败。
pub(crate) struct SessionProbe {
    closed: watch::Receiver<Option<CloseReason>>,
    auth_exhausted: Arc<AtomicBool>,
}

impl SessionProbe {
    /// 会话是否因认证之外的原因（断线、对端关闭）已经结束。
    pub(crate) async fn died(&self, handle: &Handle<ClientHandler>) -> bool {
        if !handle.is_closed() {
            return false;
        }
        // 会话任务结束与 handler.disconnected 记录原因之间有一小段间隔，等它落定再判断；
        // 设上限是为了万一没有记录也不会卡住认证流程。
        let mut rx = self.closed.clone();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            rx.wait_for(|v| v.is_some()),
        )
        .await;
        !self.auth_exhausted.load(Ordering::SeqCst)
    }
}

pub(crate) struct ClientHandler {
    endpoint: Endpoint,
    store: Arc<dyn HostKeyStore>,
    revoked: Option<Arc<dyn RevokedKeys>>,
    shared: Arc<Shared>,
}

impl Handler for ClientHandler {
    type Error = HandlerError;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // 我们没有在算法表里声明证书主机密钥，服务器不应该出示证书；出示了也没有 CA 信任模型可用。
        let russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Err(HandlerError::Ssh(SshError::ConnectFailed {
                endpoint: self.endpoint.clone(),
                reason: Reason::Other {
                    detail: "host certificates are not supported".into(),
                },
            }));
        };
        let (store, revoked, endpoint, key) = (
            self.store.clone(),
            self.revoked.clone(),
            self.endpoint.clone(),
            key.clone(),
        );
        // HostKeyStore 的实现会做文件 IO，trait 要求在 spawn_blocking 里调用。
        let joined = tokio::task::spawn_blocking(move || {
            hostkey::evaluate(store.as_ref(), revoked.as_deref(), &endpoint, &key)
        })
        .await;
        match joined {
            Ok(Ok(_)) => Ok(true),
            Ok(Err(e)) => Err(HandlerError::Ssh(e)),
            Err(e) => Err(HandlerError::Ssh(SshError::ConnectFailed {
                endpoint: self.endpoint.clone(),
                reason: Reason::Other {
                    detail: format!("host key check task failed: {e}"),
                },
            })),
        }
    }

    async fn kex_done(
        &mut self,
        _shared_secret: Option<&[u8]>,
        names: &russh::Names,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        // 只记录首次密钥交换：重新协商用的是同一份偏好列表，结果不会往"更旧"的方向变。
        let mut slot = self
            .shared
            .negotiated
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(NegotiatedAlgorithms {
                kex: names.kex.as_ref().to_owned(),
                host_key: names.key.as_str().to_owned(),
                cipher: names.cipher.as_ref().to_owned(),
                client_mac: names.client_mac.as_ref().to_owned(),
                server_mac: names.server_mac.as_ref().to_owned(),
            });
        }
        Ok(())
    }

    async fn disconnected(
        &mut self,
        reason: DisconnectReason<Self::Error>,
    ) -> Result<(), Self::Error> {
        // 对 `Error` 必须原样再返回（russh 约定），这样 JoinHandle 也能看到同一个错误。
        match reason {
            DisconnectReason::ReceivedDisconnect(_) => {
                let _ = self.shared.closed.send(Some(CloseReason::Lost));
                Ok(())
            }
            DisconnectReason::Error(e) => {
                if matches!(&e, HandlerError::Russh(russh::Error::NoAuthMethod)) {
                    self.shared.auth_exhausted.store(true, Ordering::SeqCst);
                }
                let why = match &e {
                    HandlerError::Russh(russh::Error::KeepaliveTimeout) => {
                        CloseReason::KeepaliveTimeout
                    }
                    // 我们自己丢掉 Handle 触发的正常收尾。
                    HandlerError::Russh(russh::Error::Disconnect) => CloseReason::Local,
                    _ => CloseReason::Lost,
                };
                let _ = self.shared.closed.send(Some(why));
                Err(e)
            }
        }
    }
}

/// 把 russh 握手阶段的错误映射成 [`SshError`]。
fn map_handshake_error(endpoint: &Endpoint, e: HandlerError) -> SshError {
    match e {
        HandlerError::Ssh(e) => e,
        HandlerError::Russh(russh::Error::NoCommonAlgo { kind, theirs, .. }) => {
            let category = match kind {
                russh::AlgorithmKind::Kex => AlgorithmCategory::Kex,
                russh::AlgorithmKind::Key => AlgorithmCategory::HostKey,
                russh::AlgorithmKind::Cipher => AlgorithmCategory::Cipher,
                russh::AlgorithmKind::Mac => AlgorithmCategory::Mac,
                russh::AlgorithmKind::Compression => AlgorithmCategory::Compression,
            };
            // 只列服务器提供的那份，并去掉 ext-info-*、kex-strict-* 伪算法（ssh.md §2.3）。
            let server_offers = theirs
                .into_iter()
                .filter(|n| !is_pseudo_algorithm(n))
                .collect();
            SshError::NegotiationFailed {
                endpoint: endpoint.clone(),
                category,
                server_offers,
            }
        }
        HandlerError::Russh(russh::Error::IO(e)) => SshError::ConnectFailed {
            endpoint: endpoint.clone(),
            reason: Reason::from(&e),
        },
        HandlerError::Russh(russh::Error::HUP) => SshError::ConnectFailed {
            endpoint: endpoint.clone(),
            reason: Reason::ConnectionReset,
        },
        HandlerError::Russh(other) => SshError::ConnectFailed {
            endpoint: endpoint.clone(),
            reason: Reason::Other {
                detail: other.to_string(),
            },
        },
    }
}

/// 构造 russh 客户端配置。`keepalive` 来自该跳 Profile 的配置（ssh.md §2.4）。
fn client_config(
    preferred: Preferred,
    keepalive: &KeepaliveConfig,
) -> Result<russh::client::Config, SshError> {
    let gex = GexParams::new(GEX_MIN_BITS, GEX_PREFERRED_BITS, GEX_MAX_BITS)
        .expect("gex 参数是编译期常量，满足 russh 的校验");
    Ok(russh::client::Config {
        client_id: SshId::Standard(CLIENT_ID.into()),
        preferred,
        gex,
        // 协议层 keepalive 可按会话关闭；不设空闲超时。
        keepalive_interval: keepalive
            .enabled
            .then(|| Duration::from_secs(keepalive.interval_secs as u64)),
        keepalive_max: keepalive.max_misses as usize,
        inactivity_timeout: None,
        // 交互输入延迟优先，关闭 Nagle。
        nodelay: true,
        ..Default::default()
    })
}

/// 解析并连接 TCP，随后握手、校验主机密钥、认证。
///
/// 不设连接超时：需要的话由调用方用 `tokio::time::timeout` 包住。
/// 认证中向用户提问通过 `interaction`；主机密钥变更不走提问，直接返回
/// [`SshError::HostKeyChanged`]（调用方问完用户后用 [`crate::accept_changed_key`] 再重连）。
pub async fn connect<I: Interaction>(
    params: ConnectParams,
    interaction: &I,
) -> Result<Connected, SshError> {
    let ep = &params.endpoint;
    let fail = |reason: Reason| SshError::ConnectFailed {
        endpoint: ep.clone(),
        reason,
    };
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((ep.host.as_str(), ep.port))
        .await
        .map_err(|_| fail(Reason::NameResolution))?
        .collect();
    if addrs.is_empty() {
        return Err(fail(Reason::NameResolution));
    }
    let stream = tokio::net::TcpStream::connect(&addrs[..])
        .await
        .map_err(|e| fail(Reason::from(&e)))?;
    // russh 的 `connect` 会替我们设置，但我们自己建流，所以这里显式关闭 Nagle。
    let _ = stream.set_nodelay(true);
    connect_over(stream, params, interaction).await
}

/// 在已有字节流上握手并认证。
///
/// 跳板链用它在上一跳的 `direct-tcpip` 通道里建立下一跳；`stream` 必须是到目标主机的端到端字节流
/// （握手是端到端的，中间跳只转发密文，ssh.md §5）。
pub async fn connect_over<S, I>(
    stream: S,
    params: ConnectParams,
    interaction: &I,
) -> Result<Connected, SshError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    I: Interaction,
{
    connect_with_preferred(stream, params, interaction, algorithms::preferred()).await
}

/// 与 [`connect_over`] 相同，但新连接挂在外层跳板 `outer` 上：内层持有外层的引用，
/// 保证释放时内层先断、外层后断（见 [`Connection`] 的 Drop 说明）。仅跳板链使用。
pub(crate) async fn connect_over_outer<S, I>(
    stream: S,
    params: ConnectParams,
    interaction: &I,
    outer: Connection,
) -> Result<Connected, SshError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    I: Interaction,
{
    connect_inner(
        stream,
        params,
        interaction,
        algorithms::preferred(),
        Some(outer),
    )
    .await
}

/// 与 [`connect_over`] 相同，但算法偏好可替换。仅供测试构造"协商失败"场景。
pub(crate) async fn connect_with_preferred<S, I>(
    stream: S,
    params: ConnectParams,
    interaction: &I,
    preferred: Preferred,
) -> Result<Connected, SshError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    I: Interaction,
{
    connect_inner(stream, params, interaction, preferred, None).await
}

async fn connect_inner<S, I>(
    stream: S,
    params: ConnectParams,
    interaction: &I,
    preferred: Preferred,
    outer: Option<Connection>,
) -> Result<Connected, SshError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    I: Interaction,
{
    let ConnectParams {
        endpoint,
        user,
        auth: method,
        saved_password,
        saved_passphrase,
        agent_socket,
        home_dir,
        host_keys,
        revoked_keys,
        keepalive,
    } = params;

    let (closed_tx, closed_rx) = watch::channel(None);
    let auth_exhausted = Arc::new(AtomicBool::new(false));
    let probe = SessionProbe {
        closed: closed_rx.clone(),
        auth_exhausted: auth_exhausted.clone(),
    };
    let shared = Arc::new(Shared {
        negotiated: Mutex::new(None),
        closed: closed_tx,
        auth_exhausted,
    });
    let handler = ClientHandler {
        endpoint: endpoint.clone(),
        store: host_keys,
        revoked: revoked_keys,
        shared: shared.clone(),
    };
    let config = Arc::new(client_config(preferred, &keepalive)?);
    let mut handle = russh::client::connect_stream(config, stream, handler)
        .await
        .map_err(|e| map_handshake_error(&endpoint, e))?;

    let remember = auth::authenticate(
        &mut handle,
        &auth::AuthContext {
            endpoint: &endpoint,
            user: &user,
            method: &method,
            saved_password: saved_password.as_ref(),
            saved_passphrase: saved_passphrase.as_ref(),
            agent_socket: agent_socket.as_deref(),
            home_dir: home_dir.as_deref(),
            interaction,
            probe: &probe,
        },
    )
    .await?;

    let negotiated = shared
        .negotiated
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        // kex_done 一定在 connect_stream 返回前被调用过；缺失说明 russh 行为变了，宁可显式失败。
        .ok_or_else(|| SshError::ConnectFailed {
            endpoint: endpoint.clone(),
            reason: Reason::Other {
                detail: "key exchange finished without negotiated algorithms".into(),
            },
        })?;
    let legacy = legacy_algorithms_used(&negotiated);
    let connection = Connection {
        inner: Arc::new(ConnectionInner {
            // 认证成功、连接对象成形时才分配：失败的握手不消耗 id，id 的大小顺序即"建立成功"的顺序。
            id: NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed),
            handle: Some(handle),
            endpoint,
            user,
            negotiated,
            legacy,
            closed: closed_rx,
            outer,
        }),
    };
    Ok(Connected {
        connection,
        remember,
    })
}

/// 认证完成后的连接句柄。克隆是共享同一条已认证的连接。
///
/// **共享与释放（ssh.md §6）**：内部是 `Arc`，每个 [`ShellChannel`] / [`ExecChannel`] 也持有一份。
/// 最后一个持有者释放时 `Drop` 才发 disconnect 并断开；`Arc` 保证 `Drop` 只执行一次，
/// 所以多个标签同时关闭既不会重复断开，也不会谁都不断开。
/// 连接已断时 [`Connection::open_shell`] 返回 [`SshError::SharedConnectionClosed`]，不会偷偷重连。
#[derive(Clone)]
pub struct Connection {
    inner: Arc<ConnectionInner>,
}

/// 连接 id 的分配器，进程内全局递增（从 1 开始）。
static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

struct ConnectionInner {
    /// 进程内唯一、按建立顺序递增的 id，见 [`Connection::id`]。
    id: u64,
    /// `Drop` 里需要把它搬进异步任务发 disconnect，所以包在 `Option` 里。
    handle: Option<Handle<ClientHandler>>,
    endpoint: Endpoint,
    user: String,
    negotiated: NegotiatedAlgorithms,
    legacy: Vec<String>,
    closed: watch::Receiver<Option<CloseReason>>,
    /// 跳板链里的外层连接（本连接的传输就跑在它的 `direct-tcpip` 通道上）；直连为 `None`。
    /// 不能省：没有这份引用，外层会在最后一个标签之外的地方被提前释放，内层随之断线。
    /// 释放顺序由 `Drop` 显式保证，不依赖字段声明顺序。
    outer: Option<Connection>,
}

/// 测试用：按调用顺序记录每个连接发 disconnect 时的端口，断言"内层先于外层"。
/// 服务器侧的断开时刻受调度影响会有微小抖动，不能用来断言顺序。
#[cfg(test)]
pub(crate) static DISCONNECT_LOG: Mutex<Vec<u16>> = Mutex::new(Vec::new());

/// 内层会话任务结束的最长等待：对端无响应时不能让外层一直不释放。
const INNER_CLOSE_WAIT: Duration = Duration::from_secs(2);

impl Drop for ConnectionInner {
    fn drop(&mut self) {
        let outer = self.outer.take();
        let Some(handle) = self.handle.take() else {
            return;
        };
        // 会话任务已经结束就不用再发 disconnect。
        if handle.is_closed() {
            // 先内后外：显式按顺序释放，而不是依赖字段的隐式 drop 顺序。
            drop(handle);
            drop(outer);
            return;
        }
        // Drop 是同步的，发送 disconnect 消息要 await，所以交给运行时。
        // 没有运行时（进程退出阶段）时直接丢掉 Handle：russh 会因发送端关闭而关闭 TCP，对端同样会收到断开。
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let mut closed = self.closed.clone();
            #[cfg(test)]
            let port = self.endpoint.port;
            rt.spawn(async move {
                #[cfg(test)]
                DISCONNECT_LOG
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(port);
                let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
                // 跳板链：外层引用必须等内层会话任务真正结束（disconnect 已写进外层的转发通道）之后
                // 才能释放，否则外层可能先于内层的 disconnect 被断开（ssh.md §5：从最内层开始逐层关闭）。
                if outer.is_some() {
                    let _ =
                        tokio::time::timeout(INNER_CLOSE_WAIT, closed.wait_for(|v| v.is_some()))
                            .await;
                }
                drop(handle);
                drop(outer);
            });
        } else {
            drop(handle);
            drop(outer);
        }
    }
}

impl Connection {
    /// 进程内唯一的连接 id，按建立（认证成功）的先后递增；克隆共享同一条连接，返回同一个 id。
    ///
    /// 调用方据此识别"哪些会话用的是同一条连接"，以及在同一个 Profile 的多条连接里挑出最近建立的那条。
    pub fn id(&self) -> u64 {
        self.inner.id
    }

    /// 连接对象。
    pub fn endpoint(&self) -> &Endpoint {
        &self.inner.endpoint
    }

    /// 登录用户。
    pub fn user(&self) -> &str {
        &self.inner.user
    }

    /// 本次连接实际协商出的算法。
    pub fn negotiated(&self) -> &NegotiatedAlgorithms {
        &self.inner.negotiated
    }

    /// 用到的旧算法（ssh.md §2.2）；空表示没有。AEAD 加密时不会因 MAC 误报。
    pub fn legacy_algorithms(&self) -> &[String] {
        &self.inner.legacy
    }

    /// 旧算法提示用的跳信息，直接对应 `CoreEvent::LegacyAlgorithms` 的一项；没有旧算法时为 `None`。
    /// 只含本连接这一跳；整条链用 [`Connection::legacy_hops`]。
    pub fn legacy_hop(&self) -> Option<LegacyHop> {
        (!self.inner.legacy.is_empty()).then(|| LegacyHop {
            endpoint: self.inner.endpoint.clone(),
            algorithms: self.inner.legacy.clone(),
        })
    }

    /// 整条链上用到旧算法的跳，从外到内；没有旧算法时为空。
    ///
    /// 直连时最多一项（与 [`Connection::legacy_hop`] 相同）。每一跳都单独协商算法，
    /// 所以经由旧设备中转的新服务器也要提示（ssh.md §2.2、§5）。
    pub fn legacy_hops(&self) -> Vec<LegacyHop> {
        let mut hops = self
            .inner
            .outer
            .as_ref()
            .map(Connection::legacy_hops)
            .unwrap_or_default();
        hops.extend(self.legacy_hop());
        hops
    }

    /// 连接是否已经断开（keepalive 超时、对端关闭、网络错误）。
    pub fn is_closed(&self) -> bool {
        self.handle().is_closed()
    }

    /// 等到连接断开，返回原因。所有通道在连接断开时也会各自收到结束通知（读返回 `None`）。
    pub async fn wait_closed(&self) -> CloseReason {
        let mut rx = self.inner.closed.clone();
        let waited = rx.wait_for(|v| v.is_some()).await.map(|v| *v);
        match waited {
            Ok(v) => v.unwrap_or(CloseReason::Lost),
            // 发送端（handler）已随会话任务释放：连接结束了，只是没来得及记录原因。
            Err(_) => (*rx.borrow()).unwrap_or(CloseReason::Lost),
        }
    }

    /// 已记录的断开原因；还没断开或原因未记录时为 `None`。
    pub fn close_reason(&self) -> Option<CloseReason> {
        *self.inner.closed.borrow()
    }

    /// 连接断开后给通道操作用的错误：区分 keepalive 超时与其他中断。
    pub(crate) fn lost_error(&self) -> SshError {
        self.close_reason()
            .unwrap_or(CloseReason::Lost)
            .to_error(&self.inner.endpoint)
    }

    pub(crate) fn closed_error(&self) -> SshError {
        SshError::SharedConnectionClosed {
            endpoint: self.inner.endpoint.clone(),
        }
    }

    pub(crate) fn handle(&self) -> &Handle<ClientHandler> {
        self.inner
            .handle
            .as_ref()
            .expect("handle 只在 Drop 里被取走")
    }

    /// 在这条已认证的连接上再开一个 shell 通道（复制标签 / 分屏，不重新握手、不重新认证）。
    ///
    /// 连接已断时返回 [`SshError::SharedConnectionClosed`]，不会偷偷重连。
    pub async fn open_shell(&self, pty: PtyRequest) -> Result<ShellChannel, SshError> {
        ShellChannel::open(self, pty).await
    }

    /// 开一个 exec 通道执行命令（远端 cwd 查询等，ssh.md §10）。
    ///
    /// 与 shell 通道互不影响；调用方把脚本写进 stdin，读 stdout，直到通道关闭。
    pub async fn open_exec(&self, command: &str) -> Result<ExecChannel, SshError> {
        ExecChannel::open(self, command).await
    }

    /// 与 [`Connection::open_exec`] 相同，但在 exec 请求之前发送环境变量（尽力而为，服务器可能忽略）。
    /// 不请求 PTY。
    pub async fn open_exec_with_env(
        &self,
        command: &str,
        env: &[(&str, &str)],
    ) -> Result<ExecChannel, SshError> {
        ExecChannel::open_with_env(self, command, env).await
    }
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("endpoint", &self.inner.endpoint)
            .field("user", &self.inner.user)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_config_uses_per_session_keepalive() {
        let mut k = KeepaliveConfig {
            interval_secs: 42,
            max_misses: 7,
            ..Default::default()
        };
        let config = client_config(algorithms::preferred(), &k).unwrap();
        assert_eq!(config.keepalive_interval, Some(Duration::from_secs(42)));
        assert_eq!(config.keepalive_max, 7);
        assert_eq!(config.inactivity_timeout, None);

        k.enabled = false;
        let config = client_config(algorithms::preferred(), &k).unwrap();
        assert_eq!(config.keepalive_interval, None);
        assert_eq!(config.keepalive_max, 7);
    }
}
