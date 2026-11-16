//! 帧数据面的 UniFFI 句柄：取帧走 UniFFI，帧内容零拷贝。
//!
//! Swift 拿不到 UniFFI 对象的裸指针，没法把 `KansoCore` 交给手写的 C 函数，所以取帧这一步
//! 走 UniFFI（只传一个句柄），而帧内容仍然由 Swift 直接按地址读 Rust 持有的缓冲区，
//! 不经过任何序列化（docs/frame-abi.md §8）。
//!
//! # 唯一的 unsafe 约定（调用方必须遵守）
//!
//! - Swift 持有 [`FrameHandle`] 期间，`header_address()` 返回的地址一直有效，缓冲区内容不会被修改；
//! - Swift 把句柄传回 `KansoCore::take_frame` 的 `reuse` 参数、且该调用**返回了新帧**之后，
//!   旧句柄的缓冲区会被 core 取走复用，**不得再读取旧地址**（旧句柄的 `header_address()` 此后返回 0）；
//! - Swift 只读，不得写入这块内存；
//! - 句柄释放（Swift 引用计数归零）时缓冲区随之释放，所以 Swift 读取帧的整个期间必须强引用句柄。

use std::sync::{Mutex, MutexGuard};

use kanso_frame::Frame;

/// 要复制的块文本范围。
#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum FfiBlockText {
    /// B 到 C 的命令。
    Command,
    /// C 到 D 或下一个 A 的输出。
    Output,
    /// 命令加输出。
    All,
}

/// 可见范围内的命令块装饰元数据。
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiCommandBlock {
    /// 会话内稳定的块标识。
    pub id: u64,
    /// 是否为整个会话的最新块，与当前回滚位置无关。
    pub latest: bool,
    /// 仍保留的块起始绝对行。
    pub start_line: u64,
    /// 块末尾绝对行（包含）。
    pub end_line: u64,
    /// 命令末行；尚未执行或无命令时为空。
    pub command_line: Option<u64>,
    /// 是否仍在执行命令。
    pub running: bool,
    /// OSC 133 D 携带的退出码。
    pub exit_code: Option<i32>,
    /// 按 C 到 D 计算的本地化耗时。
    pub duration: Option<crate::ffi_types::FfiMessage>,
    /// 完整命令是否仍可复制。
    pub can_copy_command: bool,
    /// 是否存在输出范围。
    pub can_copy_output: bool,
}

/// 一帧终端快照的所有权句柄。
///
/// 内部是 `Mutex<Option<Frame>>`：`Some` 表示缓冲区仍由本句柄持有，`None` 表示已被
/// `KansoCore::take_frame` 取走复用。锁只保护“取走”这一动作，读地址不需要等待渲染。
#[derive(uniffi::Object)]
pub struct FrameHandle {
    frame: Mutex<Option<Frame>>,
}

#[uniffi::export]
impl FrameHandle {
    /// 用真实识别器生成语义着色截图帧，不接触用户环境。
    #[uniffi::constructor]
    pub fn mock_semantic_colors(cols: u16, rows: u16) -> std::sync::Arc<Self> {
        let mut terminal = kanso_term::Terminal::new(kanso_term::TermConfig {
            size: kanso_types::geometry::GridSize { cols, rows },
            scrollback_lines: 0,
        });
        terminal.feed(
            concat!(
                "\x1b]133;A;kanso=first\x07",
                "\x1b]777;kanso-chip;start\x07 \x1b[92m\u{e718} v26.13.0\x1b[0m \x1b]777;kanso-chip;end\x07 ",
                "\x1b]777;kanso-chip;start\x07 \x1b[1m\u{f07b} ~/workspace/kanso\x1b[0m \x1b]777;kanso-chip;end\x07 ",
                "\x1b]777;kanso-chip;start\x07 \x1b[92m\u{e0a0} wp/prompt-chips\x1b[0m \x1b]777;kanso-chip;end\x07\r\n",
                "cargo test\x1b]133;B\x07\r\n\x1b]133;C\x07",
                "/srv/日志/app.log  ./main.rs  restart-yoncode.py\r\n",
                "mr657-阶段复核收缩说明.md  claude-import  iuap-zx-dsh-usage-host\r\n",
                "\"quoted text\"  '中文字符串'\r\n",
                "42  12.5  64MiB  75%\r\n",
                "2026-11-15T12:34:56Z  v1.2.3\r\n",
                "550e8400-e29b-41d4-a716-446655440000  a1b2c3d4\r\n",
                "iuap-zx-daily-filter-web-picker-01a1bfd4\r\n",
                "192.168.1.2  2001:db8::1  aa:bb:cc:dd:ee:ff\r\n",
                "https://example.com  dev@example.com  host-01\r\n",
                "--verbose  -x  $HOME  ${PATH}  PATH=/usr/bin\r\n",
                "worker=17  status: ready\r\nNAME SIZE OWNER\r\n",
                "-rwxr-xr--  drwxr-xr-x\r\n",
                "INFO ready  SUCCESS complete\r\nERROR failed  FATAL stopped\r\n",
                "WARN retry  WARNING changed\r\n",
                "+added line\r\n-removed line\r\n M main.rs\r\n",
                "\x1b[35mERROR program ANSI color\x1b[0m\r\n",
                "$ echo \"editing cursor line\""
            )
            .as_bytes(),
            1_700_000_000_000,
        );
        std::sync::Arc::new(Self::new(terminal.snapshot(None)))
    }
}

impl FrameHandle {
    pub(crate) fn new(frame: Frame) -> Self {
        FrameHandle {
            frame: Mutex::new(Some(frame)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<Frame>> {
        // 临界区只做 take / 读指针，不会留下半更新状态，中毒后数据仍可用。
        self.frame.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 在持有句柄锁的情况下把"槽"交给 `f`，由 core 的 `take_frame` 决定是否取走里面的缓冲区
    /// （取走之后本句柄的 `header_address` 返回 0）。
    pub(crate) fn with_slot<R>(&self, f: impl FnOnce(&mut Option<Frame>) -> R) -> R {
        f(&mut self.lock())
    }
}

#[uniffi::export]
impl FrameHandle {
    /// 确定性的命令块截图帧，不接触用户会话或环境。
    #[uniffi::constructor]
    pub fn mock_command_blocks(cols: u16, rows: u16) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::new(kanso_frame::mock::mock_command_blocks_frame(
            cols, rows,
        )))
    }

    /// 帧缓冲区起点（即帧头）的地址；帧已被回收复用时返回 0。
    ///
    /// 地址 8 字节对齐，缓冲区布局见 docs/frame-abi.md。有效期与读取约束见模块文档。
    pub fn header_address(&self) -> u64 {
        // `Frame::as_ptr` 返回堆上缓冲区的指针，句柄被移动时地址不变。
        self.lock().as_ref().map_or(0, |f| f.as_ptr().addr() as u64)
    }

    /// 帧缓冲区的字节数（等于帧头的 `total_len`）；帧已被回收复用时返回 0。
    pub fn byte_len(&self) -> u64 {
        self.lock()
            .as_ref()
            .map_or(0, |f| f.as_bytes().len() as u64)
    }

    /// 返回当前快照可见范围内的块元数据。
    pub fn command_blocks(&self) -> Vec<FfiCommandBlock> {
        self.lock()
            .as_ref()
            .map(|frame| {
                frame
                    .command_blocks
                    .iter()
                    .map(|block| FfiCommandBlock {
                        id: block.id,
                        latest: block.latest,
                        start_line: block.start_line,
                        end_line: block.end_line,
                        command_line: block.command_line,
                        running: block.running,
                        exit_code: block.exit_code,
                        duration: block.duration.as_ref().map(Into::into),
                        can_copy_command: block.can_copy_command,
                        can_copy_output: block.can_copy_output,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanso_frame::mock::mock_frame;

    #[test]
    fn address_and_len_while_held() {
        let f = mock_frame(80, 24, 1);
        let len = f.as_bytes().len() as u64;
        let h = FrameHandle::new(f);
        let a = h.header_address();
        assert_ne!(a, 0);
        assert_eq!(a % 8, 0);
        assert_eq!(h.byte_len(), len);
        // 重复读取地址稳定。
        assert_eq!(h.header_address(), a);
    }

    #[test]
    fn taken_handle_reports_zero() {
        let h = FrameHandle::new(mock_frame(10, 5, 0));
        assert!(h.with_slot(|slot| slot.take()).is_some());
        assert_eq!(h.header_address(), 0);
        assert_eq!(h.byte_len(), 0);
        assert!(h.with_slot(|slot| slot.take()).is_none());
    }
}
