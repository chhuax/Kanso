//! OpenSSH 客户端配置的解析与求值（sans-IO，文件读取通过 trait 注入）。
//!
//! 只做“列出别名并求出连接目标”所需的子集：`Host` / `Match` 块结构、`HostName`、`Port`、
//! `User`、`IdentityFile`、`ProxyJump`、`Include`，其余关键字忽略。规则见
//! `docs/spec/store-import.md` §7.1；细节以 `ssh -G` 的实际行为为准。
//!
//! 已知取舍：
//! - `Match` 块不求值，视为“不选中任何主机”的块并给出 [`Warning::MatchUnsupported`]；
//! - `Include` 路径里与主机相关的 token（`%h` 等）不支持，环境变量 `${VAR}` 不展开；
//! - `IdentityFile` 只取第一个。

mod eval;
mod fs;
mod parse;
mod text;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

pub use fs::{ConfigFs, StdFs};

/// 求值需要的本机环境。由调用方填写，本 crate 不读环境变量、不查系统。
#[derive(Clone, Debug)]
pub struct Env {
    /// 用户主目录：`~`、`%d` 的值，相对 `Include` 与默认私钥探测的基准（`<home>/.ssh`）。
    pub home: PathBuf,
    /// 本地用户名：没写 `User` 时的默认值，也是 `%u`。
    pub local_user: String,
    /// 本机主机名（含域名）：`%l`；去掉域名的短名是 `%L`。
    pub local_host: String,
    /// 本地用户 ID：`%i`。
    pub local_uid: u32,
}

/// 解析/求值过程中产生的结构化警告。core 层负责把它映射成面向用户的消息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Warning {
    /// `Include` 嵌套超过 16 层，没有读取 `include` 指向的文件。
    IncludeTooDeep {
        /// 写着这行 `Include` 的文件。
        file: PathBuf,
        /// 被放弃的文件。
        include: PathBuf,
    },
    /// 文件里有 `Match` 行；本实现不求值，该块被当作不选中任何主机。每个文件只报一次。
    MatchUnsupported {
        /// 含 `Match` 的文件。
        file: PathBuf,
    },
    /// `Port` 不是 1–65535 的整数，已改用 22。
    InvalidPort {
        /// 配置里的原始值。
        value: String,
    },
    /// 值里出现了该选项不支持的 `%` token，保留原样。
    UnknownToken {
        /// 选项名（`HostName` / `User` / `IdentityFile` / `Include`）。
        option: &'static str,
        /// 展开前的整个值。
        value: String,
        /// `%` 后面的字符；结尾孤立的 `%` 记为 `'%'`。
        token: char,
    },
}

/// 一个别名求值后的结果（相当于 `ssh -G <别名>` 对支持选项的输出）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// 别名（Host 行里的写法，大小写取首次出现的）。
    pub alias: String,
    /// 实际主机；没写 `HostName` 时为别名本身。
    pub host_name: String,
    /// 端口；没写或非法时为 22。
    pub port: u16,
    /// 用户；没写时为本地用户名。
    pub user: String,
    /// 显式写了的私钥路径（已展开 `~` 与 `%`）；没写为 `None`。相对路径原样保留。
    pub identity_file: Option<PathBuf>,
    /// 没写 `IdentityFile` 时，`<home>/.ssh` 下按 id_ed25519、id_ecdsa、id_rsa 顺序实际存在的第一个。
    pub default_identity: Option<PathBuf>,
    /// `ProxyJump` 原文（不展开 token、不拆分）；`none` 视为 `None`。
    pub proxy_jump: Option<String>,
    /// 仅与这个别名相关的警告。[`scan`] 的文件级警告在 [`ScanResult::warnings`]，
    /// [`resolve`] 则把两类合并放在这里。
    pub warnings: Vec<Warning>,
}

/// [`scan`] 的结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanResult {
    /// 所有可导入别名，按首次出现的顺序。
    pub hosts: Vec<Resolved>,
    /// 文件级警告（Include 超限、Match 不支持、Include 路径里的未知 token），不属于任何单个别名。
    pub warnings: Vec<Warning>,
}

fn load(path: &Path, fs: &dyn ConfigFs, env: &Env) -> (Vec<parse::Item>, Vec<Warning>) {
    let mut parser = parse::Parser {
        fs,
        env,
        warnings: Vec::new(),
    };
    let items = parser.parse_file(path, 0).unwrap_or_default();
    (items, parser.warnings)
}

/// 读取 `path`（通常是 `~/.ssh/config`）及其 `Include`，列出所有可导入别名并逐个求值。
///
/// 主文件读不到时返回空结果（与 ssh 一样视为没有配置）。
pub fn scan(path: &Path, fs: &dyn ConfigFs, env: &Env) -> ScanResult {
    let (items, warnings) = load(path, fs, env);
    let mut aliases = Vec::new();
    eval::collect_aliases(&items, &mut aliases);
    let hosts = aliases
        .iter()
        .map(|a| eval::resolve_alias(&items, a, fs, env))
        .collect();
    ScanResult { hosts, warnings }
}

/// 对单个主机名求值，等价于 `ssh -G <host>` 对支持选项的结果。
///
/// `host` 不必是 [`scan`] 列出的别名，任何名字都可以（只会命中 `Host *` 之类的通用块）。
/// 文件级警告会合并进返回值的 `warnings`。
pub fn resolve(host: &str, path: &Path, fs: &dyn ConfigFs, env: &Env) -> Resolved {
    let (items, mut warnings) = load(path, fs, env);
    let mut r = eval::resolve_alias(&items, host, fs, env);
    warnings.append(&mut r.warnings);
    r.warnings = warnings;
    r
}
