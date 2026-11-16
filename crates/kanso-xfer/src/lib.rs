//! 按公开协议说明独立实现的 ZMODEM / XMODEM 纯状态机，依据见 docs/zmodem.md。
//! 字节、时钟和文件块均由调用方提供，不访问文件、网络或系统时钟。

mod wire;
mod xmodem;
mod zmodem;

pub use wire::{Handshake, Sentinel};
pub use xmodem::Xmodem;
pub use zmodem::{Action, BLOCK_SIZE, FileInfo, Machine};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod xmodem_tests;
