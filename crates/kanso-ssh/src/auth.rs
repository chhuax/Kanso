//! 认证（ssh.md §4.1、§4.2、§4.3）。
//!
//! 流程严格按规格：先发一次 `none`（既探测支持的方法，也处理 dropbear 那类放行 `none`
//! 却拒绝空密码的设备）；再按配置的方式认证；失败且服务器还提供 keyboard-interactive 时进入多轮问答；
//! 否则按"partial success / 密码 / 私钥 / agent"区分报错。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanso_types::message::{Endpoint, Reason};
use kanso_types::profile::AuthMethod as ProfileAuthMethod;
use kanso_types::prompt::{CredentialKind, KbdQuestion};
use kanso_types::secret::SecretString;
use russh::client::{AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::{Algorithm, HashAlg, PrivateKey, PrivateKeyWithHashAlg, decode_secret_key};
use russh::{MethodKind, MethodSet};

use crate::connection::{ClientHandler, RememberedCredential, SessionProbe};
use crate::error::SshError;
use crate::interaction::{Interaction, KbdRound, PROMPT_TIMEOUT};

/// keyboard-interactive 最多问几轮（ssh.md §4.2）：防止服务器一直追问，把连接和用户拖住。
pub const MAX_KBD_ROUNDS: u32 = 16;

/// 认证方式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthMethod {
    /// 密码（含 keyboard-interactive 的口令回退）。
    Password,
    /// 私钥文件；路径支持 `~/` 开头。
    PublicKey {
        /// 私钥文件路径。
        path: String,
    },
    /// 通过 ssh-agent 认证。
    Agent,
}

/// 认证所需的上下文。
pub(crate) struct AuthContext<'a, I> {
    pub endpoint: &'a Endpoint,
    pub user: &'a str,
    pub method: &'a AuthMethod,
    pub saved_password: Option<&'a SecretString>,
    pub saved_passphrase: Option<&'a SecretString>,
    pub agent_socket: Option<&'a Path>,
    pub home_dir: Option<&'a Path>,
    pub interaction: &'a I,
    pub probe: &'a SessionProbe,
}

type DynAgent = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

/// 服务器在一次失败的尝试之后给出的状态。
struct Pending {
    /// 服务器接着愿意接受的方法。
    remaining: MethodSet,
    /// 凭据已被接受，但还需要另一个因素（partial success）。
    partial: bool,
}

/// 一次方法尝试的结果。
enum Attempt {
    Success,
    Failed(Pending),
}

impl From<AuthResult> for Attempt {
    fn from(r: AuthResult) -> Self {
        match r {
            AuthResult::Success => Attempt::Success,
            AuthResult::Failure {
                remaining_methods,
                partial_success,
            } => Attempt::Failed(Pending {
                remaining: remaining_methods,
                partial: partial_success,
            }),
        }
    }
}

fn method_names(set: &MethodSet) -> Vec<String> {
    set.iter()
        .map(|m| <&'static str>::from(m).to_owned())
        .collect()
}

/// 连接中途掉线时 russh 的 `authenticate_*` 会返回"空方法集、非 partial 的失败"，
/// 看起来与"被拒绝"一模一样；所以失败后要再确认会话是否中途死了，避免把断线报成密码错误
/// （区分方式见 [`SessionProbe`]）。
async fn lost<I>(handle: &Handle<ClientHandler>, ctx: &AuthContext<'_, I>) -> Option<SshError> {
    ctx.probe
        .died(handle)
        .await
        .then(|| SshError::ConnectionLost {
            endpoint: ctx.endpoint.clone(),
        })
}

fn russh_err(endpoint: &Endpoint, e: russh::Error) -> SshError {
    match e {
        russh::Error::IO(io) => SshError::ConnectFailed {
            endpoint: endpoint.clone(),
            reason: Reason::from(&io),
        },
        // 发送失败 / 断开：会话任务已经结束。
        russh::Error::SendError | russh::Error::Disconnect | russh::Error::HUP => {
            SshError::ConnectionLost {
                endpoint: endpoint.clone(),
            }
        }
        other => SshError::ConnectFailed {
            endpoint: endpoint.clone(),
            reason: Reason::Other {
                detail: other.to_string(),
            },
        },
    }
}

/// 带 180 秒超时地向用户提问；超时、用户取消都返回 `None`（按取消处理）。
async fn ask<T>(fut: impl Future<Output = Option<T>>) -> Option<T> {
    tokio::time::timeout(PROMPT_TIMEOUT, fut)
        .await
        .ok()
        .flatten()
}

/// 向用户要凭据；取消或超时报 [`SshError::AuthCancelled`]。选了"记住"的凭据记入 `remember`。
async fn request_credential<I: Interaction>(
    ctx: &AuthContext<'_, I>,
    kind: CredentialKind,
    remember: &mut Vec<RememberedCredential>,
) -> Result<SecretString, SshError> {
    let Some((secret, remember_it)) =
        ask(ctx
            .interaction
            .credential(ctx.endpoint, ctx.user, kind.clone()))
        .await
    else {
        return Err(SshError::AuthCancelled {
            endpoint: ctx.endpoint.clone(),
        });
    };
    if remember_it {
        remember.push(RememberedCredential {
            kind,
            secret: secret.clone(),
        });
    }
    Ok(secret)
}

/// 执行 §4.1 的完整流程。成功时返回用户选了"记住"的凭据。
pub(crate) async fn authenticate<I: Interaction>(
    handle: &mut Handle<ClientHandler>,
    ctx: &AuthContext<'_, I>,
) -> Result<Vec<RememberedCredential>, SshError> {
    let mut remember = Vec::new();

    // 1. 先发 none：服务器直接放行就结束。
    let mut pending = match handle
        .authenticate_none(ctx.user)
        .await
        .map_err(|e| russh_err(ctx.endpoint, e))?
        .into()
    {
        Attempt::Success => return Ok(remember),
        Attempt::Failed(p) => Pending {
            remaining: p.remaining,
            // none 的失败里的 partial 标志没有意义，不继承。
            partial: false,
        },
    };

    // 2. 按配置的方式认证。
    // 已保存的密码一旦通过 password 方法被服务器拒绝，就不再拿去回答 keyboard-interactive，
    // 避免对同一个错误密码重复尝试、触发账户锁定。
    let mut password_for_kbd: Option<SecretString> = None;
    let attempt = match ctx.method {
        AuthMethod::Password => {
            let (a, pw) = try_password(handle, ctx, pending, &mut remember).await?;
            password_for_kbd = pw;
            a
        }
        AuthMethod::PublicKey { path } => {
            try_private_key(handle, ctx, path, pending, &mut remember).await?
        }
        AuthMethod::Agent => try_agent(handle, ctx, pending).await?,
    };
    pending = match attempt {
        Attempt::Success => return Ok(remember),
        Attempt::Failed(p) => p,
    };

    // 3. 失败但还提供 keyboard-interactive：多轮问答（各种 2FA 的通用做法）。
    if !handle.is_closed() && pending.remaining.contains(&MethodKind::KeyboardInteractive) {
        keyboard_interactive(handle, ctx, password_for_kbd).await?;
        return Ok(remember);
    }

    // 4. 私钥 / agent 没通过、服务器也没有 keyboard-interactive、但还接受 password：
    // 与 OpenSSH 一样回退到密码。不少堡垒机只开 password + publickey，
    // 本地默认私钥不在它的授权列表里时，用户实际是要输密码。
    let mut fell_back_to_password = false;
    if *ctx.method != AuthMethod::Password
        && !handle.is_closed()
        && pending.remaining.contains(&MethodKind::Password)
    {
        fell_back_to_password = true;
        let (attempt, _) = try_password(handle, ctx, pending, &mut remember).await?;
        pending = match attempt {
            Attempt::Success => return Ok(remember),
            Attempt::Failed(p) => p,
        };
        // 密码通过但还要第二因素（partial success）时，服务器可能接着给 keyboard-interactive。
        if !handle.is_closed() && pending.remaining.contains(&MethodKind::KeyboardInteractive) {
            keyboard_interactive(handle, ctx, None).await?;
            return Ok(remember);
        }
    }

    // 5. 报错。
    if let Some(e) = lost(handle, ctx).await {
        return Err(e);
    }
    // 回退到密码后仍失败，报的是密码错误，而不是"私钥被拒"。
    let method = if fell_back_to_password {
        &AuthMethod::Password
    } else {
        ctx.method
    };
    Err(final_error(ctx, method, &pending))
}

fn final_error<I>(ctx: &AuthContext<'_, I>, method: &AuthMethod, pending: &Pending) -> SshError {
    let endpoint = ctx.endpoint.clone();
    let user = ctx.user.to_owned();
    if pending.partial {
        // 凭据已被接受，但服务器还要另一个因素：不能说"密码错误"。
        return SshError::AuthPartialSuccess {
            endpoint,
            user,
            methods: method_names(&pending.remaining),
        };
    }
    match method {
        AuthMethod::Password => SshError::AuthFailed {
            endpoint,
            user,
            method: ProfileAuthMethod::Password,
        },
        AuthMethod::PublicKey { path } => SshError::AuthKeyRejected {
            endpoint,
            user,
            path: path.clone(),
        },
        AuthMethod::Agent => SshError::AuthAgentNoIdentity { endpoint, user },
    }
}

/// 服务器是否（可能）接受某方法：方法集为空说明服务器没告诉我们，那就照常尝试。
fn may_offer(pending: &Pending, kind: MethodKind) -> bool {
    pending.remaining.is_empty() || pending.remaining.contains(&kind)
}

/// 密码认证。返回值里的密码是"还没被服务器拒绝过、可以拿去回答 keyboard-interactive 的已保存密码"。
async fn try_password<I: Interaction>(
    handle: &mut Handle<ClientHandler>,
    ctx: &AuthContext<'_, I>,
    pending: Pending,
    remember: &mut Vec<RememberedCredential>,
) -> Result<(Attempt, Option<SecretString>), SshError> {
    let saved = ctx.saved_password.cloned();
    if !may_offer(&pending, MethodKind::Password) {
        // 服务器没有 password 方法（常见于只给 keyboard-interactive 的设备）：
        // 直接发 password 只会白白消耗一次尝试。
        return Ok((Attempt::Failed(pending), saved));
    }
    let password = match saved {
        Some(p) => p,
        None => request_credential(ctx, CredentialKind::Password, remember).await?,
    };
    let result = handle
        .authenticate_password(ctx.user, password.expose())
        .await
        .map_err(|e| russh_err(ctx.endpoint, e))?;
    Ok(match Attempt::from(result) {
        Attempt::Success => (Attempt::Success, None),
        failed => (failed, None),
    })
}

/// 展开私钥路径开头的 `~/`。
fn expand_path<I>(ctx: &AuthContext<'_, I>, path: &str) -> Option<PathBuf> {
    let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) else {
        return Some(PathBuf::from(path));
    };
    let home = ctx
        .home_dir
        .map(Path::to_path_buf)
        .or_else(std::env::home_dir)?;
    Some(home.join(rest))
}

/// 读取并解密私钥。
///
/// 口令的使用顺序：已保存的口令 → 仍失败（或没有口令）且密钥确实加密时向用户要一次 → 还失败则报解密失败。
async fn load_private_key<I: Interaction>(
    ctx: &AuthContext<'_, I>,
    path: &str,
    remember: &mut Vec<RememberedCredential>,
) -> Result<PrivateKey, SshError> {
    let unreadable = |reason: Reason| SshError::KeyFileUnreadable {
        endpoint: ctx.endpoint.clone(),
        path: path.to_owned(),
        reason,
    };
    let real = expand_path(ctx, path).ok_or_else(|| unreadable(Reason::NotFound))?;
    let text = tokio::fs::read_to_string(&real)
        .await
        .map_err(|e| unreadable(Reason::from(&e)))?;

    let first = decode_secret_key(&text, ctx.saved_passphrase.map(SecretString::expose));
    let err = match first {
        Ok(k) => return Ok(k),
        Err(e) => e,
    };
    // 区分"文件本身坏了 / 格式不支持"和"需要（或给错了）口令"：不给口令再试一次，
    // 只有明确报"已加密"才走口令流程。
    let encrypted = matches!(err, russh::keys::Error::KeyIsEncrypted)
        || matches!(
            decode_secret_key(&text, None),
            Err(russh::keys::Error::KeyIsEncrypted)
        );
    if !encrypted {
        return Err(unreadable(Reason::Other {
            detail: err.to_string(),
        }));
    }
    let passphrase = request_credential(
        ctx,
        CredentialKind::KeyPassphrase {
            path: path.to_owned(),
        },
        remember,
    )
    .await?;
    decode_secret_key(&text, Some(passphrase.expose())).map_err(|_| SshError::KeyDecryptFailed {
        endpoint: ctx.endpoint.clone(),
        path: path.to_owned(),
    })
}

/// RSA 签名哈希：按服务器 `server-sig-algs` 选 rsa-sha2-512/256，因为新版服务器会拒绝 SHA-1 签名。
///
/// 服务器没有该扩展（RFC 8308 之前的老服务器）或没列出 sha2 时返回 `None`，即 SHA-1 的 `ssh-rsa`。
async fn rsa_hash(
    handle: &Handle<ClientHandler>,
    alg: &Algorithm,
    endpoint: &Endpoint,
) -> Result<Option<HashAlg>, SshError> {
    if !matches!(alg, Algorithm::Rsa { .. }) {
        return Ok(None);
    }
    Ok(handle
        .best_supported_rsa_hash()
        .await
        .map_err(|e| russh_err(endpoint, e))?
        .flatten())
}

/// 私钥认证。
async fn try_private_key<I: Interaction>(
    handle: &mut Handle<ClientHandler>,
    ctx: &AuthContext<'_, I>,
    path: &str,
    pending: Pending,
    remember: &mut Vec<RememberedCredential>,
) -> Result<Attempt, SshError> {
    if !may_offer(&pending, MethodKind::PublicKey) {
        return Ok(Attempt::Failed(pending));
    }
    let key = load_private_key(ctx, path, remember).await?;
    let hash = rsa_hash(handle, &key.algorithm(), ctx.endpoint).await?;
    let result = handle
        .authenticate_publickey(ctx.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
        .await
        .map_err(|e| russh_err(ctx.endpoint, e))?;
    Ok(result.into())
}

/// 连接 ssh-agent。失败时返回各种尝试方式各自的失败原因。
async fn connect_agent(socket: Option<&Path>) -> Result<DynAgent, Vec<String>> {
    #[cfg(unix)]
    {
        let Some(path) = socket else {
            return Err(vec!["no ssh-agent socket path was provided".to_owned()]);
        };
        match AgentClient::connect_uds(path).await {
            Ok(c) => Ok(c.dynamic()),
            Err(e) => Err(vec![format!("{}: {e}", path.display())]),
        }
    }
    #[cfg(windows)]
    {
        // Windows：先用指定的命名管道，没有就用 OpenSSH 默认管道；都失败再试 Pageant（ssh.md §4.3）。
        let pipe = socket
            .map(|p| p.as_os_str().to_owned())
            .unwrap_or_else(|| std::ffi::OsString::from(r"\\.\pipe\openssh-ssh-agent"));
        let mut details = Vec::new();
        match AgentClient::connect_named_pipe(&pipe).await {
            Ok(c) => return Ok(c.dynamic()),
            Err(e) => details.push(format!("named pipe {}: {e}", pipe.to_string_lossy())),
        }
        match AgentClient::connect_pageant().await {
            Ok(c) => Ok(c.dynamic()),
            Err(e) => {
                details.push(format!("pageant: {e}"));
                Err(details)
            }
        }
    }
}

/// agent 认证：依次尝试 agent 里的每个身份。
///
/// 服务器完全接受或 partial success 都停止尝试（后续认证沿用这个身份）；证书身份取其公钥。
async fn try_agent<I: Interaction>(
    handle: &mut Handle<ClientHandler>,
    ctx: &AuthContext<'_, I>,
    pending: Pending,
) -> Result<Attempt, SshError> {
    if !may_offer(&pending, MethodKind::PublicKey) {
        return Ok(Attempt::Failed(pending));
    }
    let mut agent = connect_agent(ctx.agent_socket).await.map_err(|details| {
        SshError::AuthAgentUnavailable {
            endpoint: ctx.endpoint.clone(),
            details,
        }
    })?;
    let identities =
        agent
            .request_identities()
            .await
            .map_err(|e| SshError::AuthAgentUnavailable {
                endpoint: ctx.endpoint.clone(),
                details: vec![format!("request identities: {e}")],
            })?;

    let mut last = pending;
    for identity in identities {
        let key = identity.public_key().into_owned();
        let hash = rsa_hash(handle, &key.algorithm(), ctx.endpoint).await?;
        match handle
            .authenticate_publickey_with(ctx.user, key, hash, &mut agent)
            .await
        {
            Ok(AuthResult::Success) => return Ok(Attempt::Success),
            Ok(AuthResult::Failure {
                remaining_methods,
                partial_success,
            }) => {
                last = Pending {
                    remaining: remaining_methods,
                    partial: partial_success,
                };
                if partial_success {
                    break;
                }
            }
            // agent 拒绝签名（例如要用户确认被拒）：跳过这个身份，试下一个。
            Err(russh::AgentAuthError::Key(_)) => {}
            Err(russh::AgentAuthError::Send(_)) => {
                return Err(SshError::ConnectionLost {
                    endpoint: ctx.endpoint.clone(),
                });
            }
        }
    }
    Ok(Attempt::Failed(last))
}

/// 是否可以用保存的密码自动回答这一轮：恰好一个问题、不回显、问题文本含 "password"（不区分大小写）。
///
/// 验证码、推送确认、菜单选择绝不能自动用账户密码作答（ssh.md §4.2），所以条件宁严勿宽。
fn is_password_question(prompts: &[russh::client::Prompt]) -> bool {
    matches!(prompts, [p] if !p.echo && p.prompt.to_lowercase().contains("password"))
}

/// keyboard-interactive 多轮问答（RFC 4256）。
///
/// `saved_password` 只在"认证方式本来是密码"时由调用方传入，并且最多自动填一次。
async fn keyboard_interactive<I: Interaction>(
    handle: &mut Handle<ClientHandler>,
    ctx: &AuthContext<'_, I>,
    mut saved_password: Option<SecretString>,
) -> Result<(), SshError> {
    let mut resp = handle
        .authenticate_keyboard_interactive_start(ctx.user, None::<String>)
        .await
        .map_err(|e| russh_err(ctx.endpoint, e))?;
    let mut rounds: u32 = 0;
    // 没有问题、只有文字的轮次（例如"推送已发送"）累积到下一轮有问题的对话框里一起显示。
    let mut preamble: Vec<String> = Vec::new();

    loop {
        let (name, instructions, prompts) = match resp {
            KeyboardInteractiveAuthResponse::Success => return Ok(()),
            KeyboardInteractiveAuthResponse::Failure {
                remaining_methods,
                partial_success,
            } => {
                if let Some(e) = lost(handle, ctx).await {
                    return Err(e);
                }
                return Err(if partial_success {
                    SshError::AuthPartialSuccess {
                        endpoint: ctx.endpoint.clone(),
                        user: ctx.user.to_owned(),
                        methods: method_names(&remaining_methods),
                    }
                } else {
                    SshError::AuthResponseRejected {
                        endpoint: ctx.endpoint.clone(),
                    }
                });
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => (name, instructions, prompts),
        };

        rounds += 1;
        if rounds > MAX_KBD_ROUNDS {
            return Err(SshError::AuthTooManyRounds {
                endpoint: ctx.endpoint.clone(),
                max_rounds: MAX_KBD_ROUNDS,
            });
        }

        let answers: Vec<String> = if prompts.is_empty() {
            for text in [&name, &instructions] {
                if !text.is_empty() {
                    preamble.push(text.clone());
                }
            }
            Vec::new()
        } else if is_password_question(&prompts)
            && let Some(pw) = saved_password.take()
        {
            // `take`：保存的密码最多自动填一次，服务器若再问就交给用户。
            vec![pw.expose().to_owned()]
        } else {
            let round = KbdRound {
                endpoint: ctx.endpoint.clone(),
                user: ctx.user.to_owned(),
                name,
                instruction: instructions,
                preamble: std::mem::take(&mut preamble),
                questions: prompts
                    .iter()
                    .map(|p| KbdQuestion {
                        text: p.prompt.clone(),
                        echo: p.echo,
                    })
                    .collect(),
            };
            let Some(mut a) = ask(ctx.interaction.keyboard_interactive(round)).await else {
                return Err(SshError::AuthCancelled {
                    endpoint: ctx.endpoint.clone(),
                });
            };
            // 协议要求答案数与问题数严格一致，调用方给多了截断、给少了补空串。
            a.resize(prompts.len(), String::new());
            a
        };

        resp = handle
            .authenticate_keyboard_interactive_respond(answers)
            .await
            .map_err(|e| russh_err(ctx.endpoint, e))?;
    }
}
