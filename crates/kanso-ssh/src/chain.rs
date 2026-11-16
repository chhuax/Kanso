//! 跳板链（ProxyJump，ssh.md §5）。
//!
//! 第一跳直接 TCP（或 ProxyCommand）；之后每一跳在上一跳上开 `direct-tcpip` 通道（等价于 `ssh -W host:port`），
//! 再用 [`crate::connect_over`] 的同一套握手与认证在通道里建立。握手是端到端的：
//! 跳板机只转发密文，不会看到下一跳的密钥、密码或明文。
//!
//! # 所有权与断开顺序
//!
//! 内层 [`Connection`] 持有外层的引用（`Arc`），链上只有最内层（目标）交给调用方：
//! 所有使用者（标签、通道）释放后，内层先发 disconnect 并等自己的会话任务结束，再释放外层，
//! 外层随后同样处理它自己的外层——因此整条链**从最内层逐层向外**断开（ssh.md §5）。
//! 复制标签共享的是目标这一条 `Connection`，也就共享了整条链（ssh.md §6）。

use kanso_types::message::{Endpoint, Reason};
use russh::ChannelOpenFailure;

use crate::connection::{
    ConnectParams, Connected, Connection, RememberedCredential, connect, connect_over_outer,
};
use crate::error::SshError;
use crate::interaction::Interaction;
use crate::proxy;

/// 链上一跳的输入。
pub struct HopParams {
    /// 这一跳的显示名（通常是 Profile 名）：内层各跳的 [`Endpoint::via`] 与错误信息里的"经由 X"用它。
    pub name: String,
    /// 连接参数。其中 `endpoint.via` 会被 [`connect_chain`] 按链的位置覆盖，调用方不必填。
    pub connect: ConnectParams,
    /// 用 ProxyCommand 作为传输（只有第一跳允许；与跳板机互斥，ssh.md §5）。
    /// 非第一跳设置它会被拒绝，而不是悄悄忽略。
    pub proxy_command: Option<String>,
}

/// [`connect_chain`] 的结果。
pub struct ChainConnected {
    /// 目标主机（链上最后一跳）的连接；它持有整条链。
    pub connection: Connection,
    /// 各跳用户选了"记住"的凭据，下标与传入的 `hops` 一一对应。
    pub remember: Vec<Vec<RememberedCredential>>,
}

/// 把 russh 开通道失败的原因映射成消息里的 [`Reason`]。
fn open_failure_reason(e: &russh::Error) -> Reason {
    match e {
        russh::Error::ChannelOpenFailure(ChannelOpenFailure::AdministrativelyProhibited) => {
            Reason::PermissionDenied
        }
        russh::Error::ChannelOpenFailure(ChannelOpenFailure::ConnectFailed) => {
            // 协议里只有一个"connect failed"，分不出拒绝还是不可达，不编造更具体的原因。
            Reason::Other {
                detail: "connect failed".into(),
            }
        }
        other => Reason::Other {
            detail: other.to_string(),
        },
    }
}

/// 建立整条链并返回目标连接。`hops` 从外到内，最后一个是目标；不能为空。
///
/// - 每一跳用它自己的认证配置与主机密钥策略；提问（keyboard-interactive 等）的 endpoint 带 `via`，
///   用户能看出是哪一跳在要验证码。
/// - 某一跳开不了到下一跳的通道：[`SshError::JumpUnreachable`]。
/// - 任何一跳的主机密钥变更：在那一跳停下，返回的 [`SshError::HostKeyChanged`] 的 endpoint 就是那一跳
///   （含 `via`）。调用方让用户确认并 [`crate::accept_changed_key`] 之后**重新调用本函数**（整条链重连）：
///   失败时已建立的各跳会立刻按"内先外后"释放，不会残留半条链。
/// - 不设整体超时，需要的话由调用方用 `tokio::time::timeout` 包住。
pub async fn connect_chain<I: Interaction>(
    hops: Vec<HopParams>,
    interaction: &I,
) -> Result<ChainConnected, SshError> {
    let names: Vec<String> = hops.iter().map(|h| h.name.clone()).collect();
    let mut remember = Vec::with_capacity(hops.len());
    let mut prev: Option<Connection> = None;
    for (i, hop) in hops.into_iter().enumerate() {
        let mut params = hop.connect;
        params.endpoint.via = names[..i].to_vec();
        let connected = match prev.take() {
            None => match hop.proxy_command {
                Some(command) => {
                    proxy::connect_via_proxy_command(&command, params, interaction).await?
                }
                None => connect(params, interaction).await?,
            },
            Some(outer) => {
                if hop.proxy_command.is_some() {
                    return Err(SshError::ConnectFailed {
                        endpoint: params.endpoint,
                        reason: Reason::Other {
                            detail: "proxy command is only allowed on the first hop".into(),
                        },
                    });
                }
                open_next_hop(&outer, &names[i - 1], params, interaction).await?
            }
        };
        let Connected {
            connection,
            remember: r,
        } = connected;
        remember.push(r);
        prev = Some(connection);
    }
    let connection = prev.ok_or_else(|| SshError::ConnectFailed {
        endpoint: Endpoint {
            host: String::new(),
            port: 0,
            via: Vec::new(),
        },
        reason: Reason::Other {
            detail: "empty jump chain".into(),
        },
    })?;
    Ok(ChainConnected {
        connection,
        remember,
    })
}

/// 在 `outer` 上开到 `params.endpoint` 的 `direct-tcpip` 通道，并在其中握手认证。
///
/// 握手失败时 `outer`（已被移进新连接的参数里）随错误一起释放；成功时新连接持有 `outer`。
async fn open_next_hop<I: Interaction>(
    outer: &Connection,
    jump_name: &str,
    params: ConnectParams,
    interaction: &I,
) -> Result<Connected, SshError> {
    let target = params.endpoint.clone();
    let unreachable = |reason: Reason| SshError::JumpUnreachable {
        jump: jump_name.to_owned(),
        target: target.clone(),
        reason,
    };
    if outer.is_closed() {
        return Err(outer.closed_error());
    }
    // originator 按 `ssh -W` 的习惯填本机回环地址与端口 0：它只用于服务器的日志与策略。
    let channel = match outer
        .handle()
        .channel_open_direct_tcpip(target.host.clone(), u32::from(target.port), "127.0.0.1", 0)
        .await
    {
        Ok(c) => c,
        // 发送失败 / 会话已结束：外层连接本身断了，不是"下一跳不可达"。
        Err(russh::Error::SendError | russh::Error::Disconnect) => return Err(outer.lost_error()),
        Err(e) => return Err(unreachable(open_failure_reason(&e))),
    };
    connect_over_outer(channel.into_stream(), params, interaction, outer.clone()).await
}
