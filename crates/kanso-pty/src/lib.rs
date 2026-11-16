//! 本地 PTY、调整大小、前台进程 cwd 查询、shell 集成 shim 与 locale 环境。
//!
//! 分层（详见 `docs/local-shell-impl.md`）：
//! - 纯逻辑，不碰系统状态，便于表格测试：[`resolve_command`]、[`build_env`]、[`decide_locale`]；
//! - 进程与系统：[`PtySession`]（启动、读写、调整大小、退出、前台进程 cwd）；
//! - zsh 提示符集成：[`install_zsh_shim`] 把 shim 脚本写进应用数据目录。

mod command;
mod cwd;
mod env;
mod error;
mod locale;
mod session;
mod shim;

pub use command::{CommandSpec, Platform, resolve_command};
pub use cwd::process_cwd;
pub use env::{
    ENV_SHELL_INTEGRATION, ENV_USER_ZDOTDIR, EnvChange, EnvOptions, EnvSnapshot, FORCED_ENV,
    REMOVED_ALWAYS, REMOVED_WHEN_FALSEY, ZshIntegration, build_env,
};
pub use error::PtyError;
pub use locale::{SystemLocaleInfo, decide_locale, system_locale_exists};
pub use session::{ExitStatus, PtySession, SpawnConfig};
pub use shim::install_zsh_shim;
