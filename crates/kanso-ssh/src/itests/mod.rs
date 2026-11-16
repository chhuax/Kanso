//! kanso-ssh 的集成测试：全部使用进程内 russh server，不访问外部网络、
//! 不读写真实的 `~/.ssh`，也不使用真实的 ssh-agent。

#[cfg(unix)]
mod agent;
mod auth_flow;
mod chain;
mod channels;
mod cwd;
mod hostkey_flow;
mod negotiation;
#[cfg(unix)]
mod proxy;
mod sftp;
// 服务器实现在 `crate::test_server`（对下游测试也可见），这里保留旧路径别名。
pub(crate) use crate::test_server as server;
pub(crate) mod support;
