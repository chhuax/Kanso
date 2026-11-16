//! 文件传输协议种类，消息参数与传输事件共用。

use serde::{Deserialize, Serialize};

/// 传输协议。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum XferProtocol {
    /// SFTP。
    Sftp,
    /// ZMODEM。
    Zmodem,
    /// XMODEM。
    Xmodem,
}

/// 传输遇到目标已存在时的处理策略（filer.md §6.3 D4）。
///
/// 调用方（UI）通常先用 [`ConflictPolicy::Fail`] 发起传输，收到 `already_exists` 失败后询问用户，
/// 再用 `Overwrite` 或 `Skip` 重试；目录传输同样适用，目录本身总是合并，策略只作用于其中的同名文件。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConflictPolicy {
    /// 目标已存在即失败，不改动任何已有内容（默认：忘了先问用户时宁可失败也不悄悄覆盖）。
    #[default]
    Fail,
    /// 覆盖已存在的目标文件。
    Overwrite,
    /// 跳过已存在的目标文件，其余照常传输；跳过数通过传输结果的 `skipped` 带回。
    Skip,
}
