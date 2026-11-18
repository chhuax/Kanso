//! `kanso-mcp` 转发程序与 App 内 MCP 服务共用的约定（architecture §16.1）。
//!
//! - Unix 的 `paths`、Windows 的 `windows`：单实例锁与本机通道规则；
//! - [`hello`]：转发程序连上 socket 后发送的第一行握手；
//! - [`jsonrpc`]：连不上 App 时回复客户端用的最小 JSON-RPC 处理。
//!
//! 这些规则只在这里写一份：转发程序与 `kanso-mcp-server` 都用它，两端不会各自演变到对不上。
//! 平台部分只依赖 std 与系统 API 绑定，转发程序要小、启动要快。

pub mod hello;
pub mod jsonrpc;
#[cfg(unix)]
pub mod paths;
#[cfg(windows)]
pub mod windows;

/// Direct 渠道的 bundle id。转发程序只随 Direct 版打包，编译进常量，不靠配置文件与 App 互相告知；
/// Debug 与 Release 的 Direct 构建也是这个 id（apple/project.yml）。
pub const DIRECT_BUNDLE_ID: &str = "com.chhuax.kanso";

/// 令牌的环境变量名，由设置页生成的配置命令写入（spec/mcp.md §1.1）。
pub const TOKEN_ENV: &str = "KANSO_MCP_TOKEN";
