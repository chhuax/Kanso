//! 文件系统抽象（sans-IO）。

use std::path::{Path, PathBuf};

use crate::text::wildcard_match;

/// 文件读取抽象。解析器只通过它接触外部世界，测试用内存实现，生产用 [`StdFs`]。
pub trait ConfigFs {
    /// 读取文件内容；读不到（不存在、无权限、不是文件）返回 `None`，
    /// 调用方按 ssh 的行为直接跳过。
    fn read_to_string(&self, path: &Path) -> Option<String>;

    /// 展开 `Include` 的通配路径，只在模式含通配符时被调用。
    /// 只要求支持最后一级文件名里的 `*` / `?`，目录部分按字面。
    /// 实现不必保证有序，调用方会再排序。
    fn glob(&self, pattern: &Path) -> Vec<PathBuf>;

    /// 路径是否存在，用于默认私钥探测。
    fn exists(&self, path: &Path) -> bool;
}

/// 读真实文件系统的生产实现。
pub struct StdFs;

impl ConfigFs for StdFs {
    fn read_to_string(&self, path: &Path) -> Option<String> {
        // 用有损转换：配置里的注释可能含非 UTF-8 字节，不应该因此整个文件被丢掉。
        let bytes = std::fs::read(path).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn glob(&self, pattern: &Path) -> Vec<PathBuf> {
        let Some(name) = pattern
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
        else {
            return Vec::new();
        };
        let dir = match pattern.parent() {
            Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
            _ => PathBuf::from("."),
        };
        // 目录部分含通配符不支持，按“没有匹配”处理。
        if crate::text::has_wildcard(&dir.to_string_lossy()) {
            return Vec::new();
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out: Vec<PathBuf> = entries
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                // 与 glob(3) 一致：以 `.` 开头的文件只有模式本身也以 `.` 开头才匹配。
                if n.starts_with('.') && !name.starts_with('.') {
                    return None;
                }
                wildcard_match(&name, &n, false).then(|| dir.join(&n))
            })
            .collect();
        out.sort();
        out
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}
