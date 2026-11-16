//! 消息参数类型：参数值、参数类型描述，以及几个只服务于消息的辅助类型。

use std::io;

use serde::{Deserialize, Serialize};

use crate::profile::AuthMethod;
use crate::transfer::XferProtocol;

/// 字节数。
///
/// 单独的新类型是为了让参数类型能区分“字节数”与普通整数：
/// 前者由平台格式化器显示成 `1.2 MB`，后者交给 Fluent 做复数选择。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ByteCount(pub u64);

/// 连接对象（经过跳板机时附带跳板链）。
///
/// 每条与连接有关的错误都要指明 `host:port`，经跳板时还要写“经由”（ssh.md §11）。
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Endpoint {
    /// 目标主机名或地址。
    pub host: String,
    /// 目标端口。
    pub port: u16,
    /// 从外到内经过的跳板机显示名；空表示直连。
    pub via: Vec<String>,
}

impl Endpoint {
    /// 地址的 `host:port` 写法；IPv6 字面量（host 含 `:`）写成 `[addr]:port`。
    ///
    /// 这是地址格式化，不是面向用户的句子，所以允许在 core 里做；
    /// 句子本身（“无法连接到 …”）仍由平台按 key 本地化。
    pub fn host_port(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// 失败原因。平台用 `reason-*` 子 key 本地化后代入消息。
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Reason {
    /// 不存在。
    NotFound,
    /// 权限不足。
    PermissionDenied,
    /// 已存在。
    AlreadyExists,
    /// 超时。
    TimedOut,
    /// 连接被拒绝。
    ConnectionRefused,
    /// 连接被重置。
    ConnectionReset,
    /// 主机不可达。
    HostUnreachable,
    /// 域名解析失败。
    NameResolution,
    /// 其他原因。
    Other {
        /// 英文技术细节，不本地化。
        detail: String,
    },
}

impl Reason {
    /// 全部 `reason-*` 子 key，供多语言完整性校验。
    pub const ALL_KEYS: &'static [&'static str] = &[
        "reason-not-found",
        "reason-permission-denied",
        "reason-already-exists",
        "reason-timed-out",
        "reason-connection-refused",
        "reason-connection-reset",
        "reason-host-unreachable",
        "reason-name-resolution",
        "reason-other",
    ];

    /// 本地化子 key。`Other` 的 key 为 `reason-other`，参数 `detail` 见 [`Reason::detail`]。
    pub const fn key(&self) -> &'static str {
        match self {
            Reason::NotFound => "reason-not-found",
            Reason::PermissionDenied => "reason-permission-denied",
            Reason::AlreadyExists => "reason-already-exists",
            Reason::TimedOut => "reason-timed-out",
            Reason::ConnectionRefused => "reason-connection-refused",
            Reason::ConnectionReset => "reason-connection-reset",
            Reason::HostUnreachable => "reason-host-unreachable",
            Reason::NameResolution => "reason-name-resolution",
            Reason::Other { .. } => "reason-other",
        }
    }

    /// `Other` 的英文技术细节；其他原因为 `None`。
    pub fn detail(&self) -> Option<&str> {
        match self {
            Reason::Other { detail } => Some(detail),
            _ => None,
        }
    }
}

impl From<&io::Error> for Reason {
    fn from(err: &io::Error) -> Self {
        match err.kind() {
            io::ErrorKind::NotFound => Reason::NotFound,
            io::ErrorKind::PermissionDenied => Reason::PermissionDenied,
            io::ErrorKind::AlreadyExists => Reason::AlreadyExists,
            io::ErrorKind::TimedOut => Reason::TimedOut,
            io::ErrorKind::ConnectionRefused => Reason::ConnectionRefused,
            io::ErrorKind::ConnectionReset => Reason::ConnectionReset,
            io::ErrorKind::HostUnreachable => Reason::HostUnreachable,
            _ => Reason::Other {
                detail: err.to_string(),
            },
        }
    }
}

/// 全部辅助子 key（`reason-*` 加连接对象的两种渲染），供多语言完整性校验。
pub const AUX_KEYS: &[&str] = &[
    "reason-not-found",
    "reason-permission-denied",
    "reason-already-exists",
    "reason-timed-out",
    "reason-connection-refused",
    "reason-connection-reset",
    "reason-host-unreachable",
    "reason-name-resolution",
    "reason-other",
    "endpoint-direct",
    "endpoint-via",
];

/// Fluent 选择器的取值。
///
/// 实现者的每个取值都对应消息文本里的一个选择分支，`ALL_KEYS` 供校验各语言是否写全。
pub trait SelectKey {
    /// 当前值对应的选择器 key。
    fn select_key(&self) -> &'static str;
    /// 全部可能的选择器 key。
    const ALL_KEYS: &'static [&'static str];
}

/// 密码套件协商失败时涉及的算法类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AlgorithmCategory {
    /// 密钥交换。
    Kex,
    /// 主机密钥。
    HostKey,
    /// 加密。
    Cipher,
    /// 消息认证。
    Mac,
    /// 压缩。
    Compression,
}

/// SFTP 操作种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SftpOp {
    /// 列目录。
    List,
    /// 取属性。
    Stat,
    /// 建目录。
    Mkdir,
    /// 删除。
    Remove,
    /// 重命名。
    Rename,
    /// 建文件。
    CreateFile,
    /// 打开文件。
    Open,
}

/// 为枚举实现 [`SelectKey`]，保证 `select_key` 与 `ALL_KEYS` 来自同一份列表。
macro_rules! select_key_enum {
    ($ty:ty { $($variant:path => $key:literal),+ $(,)? }) => {
        impl SelectKey for $ty {
            fn select_key(&self) -> &'static str {
                match self {
                    $($variant => $key,)+
                }
            }
            const ALL_KEYS: &'static [&'static str] = &[$($key),+];
        }
    };
}

select_key_enum!(AlgorithmCategory {
    AlgorithmCategory::Kex => "kex",
    AlgorithmCategory::HostKey => "host-key",
    AlgorithmCategory::Cipher => "cipher",
    AlgorithmCategory::Mac => "mac",
    AlgorithmCategory::Compression => "compression",
});
select_key_enum!(AuthMethod {
    AuthMethod::Password => "password",
    AuthMethod::PrivateKey => "private-key",
    AuthMethod::Agent => "agent",
});
select_key_enum!(SftpOp {
    SftpOp::List => "list",
    SftpOp::Stat => "stat",
    SftpOp::Mkdir => "mkdir",
    SftpOp::Remove => "remove",
    SftpOp::Rename => "rename",
    SftpOp::CreateFile => "create-file",
    SftpOp::Open => "open",
});
select_key_enum!(XferProtocol {
    XferProtocol::Sftp => "sftp",
    XferProtocol::Zmodem => "zmodem",
    XferProtocol::Xmodem => "xmodem",
});

/// 消息参数的运行时取值，由 [`Message::args`](super::Message::args) 产生。
///
/// 仅用于把参数交给平台的本地化层；消息本身的 serde 直接走 `Message` 的字段，不经过它。
#[derive(Clone, Debug, PartialEq)]
pub enum MessageArg {
    /// 普通文本（文件名、路径、主机名等数据）。
    Str(String),
    /// 计数、退出码等整数，Fluent 可据此做复数选择。
    Int(i64),
    /// 字节数，由平台格式化器显示。
    ByteSize(u64),
    /// 列表，由平台的列表格式化器连接。
    StrList(Vec<String>),
    /// Fluent 选择器的取值（见 [`SelectKey`]）。
    Select(&'static str),
    /// 失败原因，平台用 `reason-*` 子 key 本地化后再代入。
    Reason(Reason),
    /// 连接对象，平台用 `endpoint-direct` / `endpoint-via` 子 key 渲染。
    Endpoint(Endpoint),
}

/// 参数的类型描述，供 [`CATALOG`](super::CATALOG) 校验用。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgKind {
    /// 对应 [`MessageArg::Str`]。
    Str,
    /// 对应 [`MessageArg::Int`]。
    Int,
    /// 对应 [`MessageArg::ByteSize`]。
    ByteSize,
    /// 对应 [`MessageArg::StrList`]。
    StrList,
    /// 对应 [`MessageArg::Select`]，附全部合法取值。
    Select(&'static [&'static str]),
    /// 对应 [`MessageArg::Reason`]。
    Reason,
    /// 对应 [`MessageArg::Endpoint`]。
    Endpoint,
}

/// 一个参数的名称与类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParamSpec {
    /// 参数名（snake_case），即 Fluent 里的变量名。
    pub name: &'static str,
    /// 参数类型。
    pub kind: ArgKind,
}

/// 一条消息的规格：key 与参数表（按声明顺序）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageSpec {
    /// 消息 key。
    pub key: &'static str,
    /// 参数表。
    pub params: &'static [ParamSpec],
}

/// 消息字段的 Rust 类型到参数类型 / 参数值的映射。
///
/// 由 `messages!` 宏使用；新增可作为消息字段的类型时在此实现，类型与 `ArgKind` 的对应
/// 由 `KIND` 与 `to_arg` 在同一处给出，避免目录与实际取值不一致。
pub trait IntoArg {
    /// 对应的参数类型描述。
    const KIND: ArgKind;
    /// 转成运行时参数值。
    fn to_arg(&self) -> MessageArg;
}

impl IntoArg for String {
    const KIND: ArgKind = ArgKind::Str;
    fn to_arg(&self) -> MessageArg {
        MessageArg::Str(self.clone())
    }
}

impl IntoArg for i64 {
    const KIND: ArgKind = ArgKind::Int;
    fn to_arg(&self) -> MessageArg {
        MessageArg::Int(*self)
    }
}

impl IntoArg for ByteCount {
    const KIND: ArgKind = ArgKind::ByteSize;
    fn to_arg(&self) -> MessageArg {
        MessageArg::ByteSize(self.0)
    }
}

impl IntoArg for Vec<String> {
    const KIND: ArgKind = ArgKind::StrList;
    fn to_arg(&self) -> MessageArg {
        MessageArg::StrList(self.clone())
    }
}

impl IntoArg for Reason {
    const KIND: ArgKind = ArgKind::Reason;
    fn to_arg(&self) -> MessageArg {
        MessageArg::Reason(self.clone())
    }
}

impl IntoArg for Endpoint {
    const KIND: ArgKind = ArgKind::Endpoint;
    fn to_arg(&self) -> MessageArg {
        MessageArg::Endpoint(self.clone())
    }
}

// 所有 `SelectKey` 实现者统一映射为 Select。`String` 等类型不实现本地 trait `SelectKey`，
// 所以这个 blanket impl 与上面的具体 impl 不冲突；以后若给这些类型实现 `SelectKey` 会编译失败，
// 这正是想要的保护。
impl<T: SelectKey> IntoArg for T {
    const KIND: ArgKind = ArgKind::Select(T::ALL_KEYS);
    fn to_arg(&self) -> MessageArg {
        MessageArg::Select(self.select_key())
    }
}
