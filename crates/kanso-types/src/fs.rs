//! 远端文件系统条目（SFTP 目录列表，ssh.md §8）。

use serde::{Deserialize, Serialize};

/// 远端文件版本；未提供的属性保留为空，不伪造为零。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStat {
    /// 文件大小（字节）。
    pub size: Option<u64>,
    /// 修改时间（Unix 毫秒）。
    pub mtime_ms: Option<i64>,
}

/// 目录列表中的一项。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    /// 文件名（不含路径）。
    pub name: String,
    /// 完整路径。
    pub path: String,
    /// 是否目录（符号链接指向目录时也为 `true`）。
    pub is_dir: bool,
    /// 是否符号链接。
    pub is_symlink: bool,
    /// 大小（字节）；目录的值由服务器决定。
    pub size: u64,
    /// 修改时间（Unix 毫秒）；服务器未提供为 `None`。
    pub mtime_ms: Option<i64>,
    /// POSIX 权限位；服务器未提供为 `None`。
    pub permissions: Option<u32>,
    /// 所有者；服务器未提供为 `None`。
    pub owner: Option<String>,
    /// 所属组；服务器未提供为 `None`。
    pub group: Option<String>,
}
