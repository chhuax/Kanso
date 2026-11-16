//! 本 crate 的错误类型。

use std::fmt;

/// 启动或操作本地 PTY 时可能出现的错误。
///
/// 这是面向日志与诊断的技术性错误；面向用户的本地化文案由上层会话按场景选择，
/// 不要直接把 `Display` 的结果展示给用户。
#[derive(Debug)]
#[non_exhaustive]
pub enum PtyError {
    /// 命令行无法按 shell 规则拆分（例如引号没有闭合）。
    InvalidCommandLine(String),
    /// 拆分后没有可执行的程序名（例如命令行是 `""`）。
    EmptyProgram,
    /// 打开 PTY 或启动子进程失败；内容来自 portable-pty / 系统调用。
    Spawn(String),
    /// 读写、调整大小、等待子进程等 IO 操作失败。
    Io(std::io::Error),
    /// 写 shim 文件失败。调用方据此不设置 `ZDOTDIR`，让 shell 按原样启动。
    Shim {
        /// 出错的路径。
        path: std::path::PathBuf,
        /// 底层错误。
        source: std::io::Error,
    },
}

impl fmt::Display for PtyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PtyError::InvalidCommandLine(m) => write!(f, "invalid command line: {m}"),
            PtyError::EmptyProgram => write!(f, "empty program name"),
            PtyError::Spawn(m) => write!(f, "failed to spawn pty: {m}"),
            PtyError::Io(e) => write!(f, "pty io error: {e}"),
            PtyError::Shim { path, source } => {
                write!(f, "failed to install shim at {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for PtyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PtyError::Io(e) | PtyError::Shim { source: e, .. } => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for PtyError {
    fn from(e: std::io::Error) -> Self {
        PtyError::Io(e)
    }
}
