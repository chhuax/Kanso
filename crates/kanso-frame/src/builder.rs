//! 帧写入器与帧缓冲区。

use crate::layout::{
    Cell, CellExtra, FRAME_MAGIC, FRAME_VERSION, FrameHeader, HEADER_SIZE, LineMeta, MAX_CELLS,
    MAX_EXTRAS, MAX_POOL_LEN,
};
use crate::view::{FrameError, FrameView};
use bytemuck::{bytes_of, cast_slice, cast_slice_mut};
use core::mem::size_of;

const HEADER_WORDS: usize = HEADER_SIZE / 8;
const LINE_WORDS: usize = size_of::<LineMeta>() / 8;
const CELL_WORDS: usize = size_of::<Cell>() / 8;
// `Cell` 是 2 个 u64，`CellExtra` 同理；按字写入依赖这些尺寸整除，布局变动时这里先报错。
const _: () = assert!(HEADER_SIZE.is_multiple_of(8) && size_of::<LineMeta>().is_multiple_of(8));
const _: () =
    assert!(size_of::<Cell>().is_multiple_of(8) && size_of::<CellExtra>().is_multiple_of(8));

/// 一帧完整快照：一块 8 字节对齐的连续缓冲区。
///
/// 底层用 `Vec<u64>` 保证对齐，使 `Cell` / `LineMeta` 等结构可以直接按引用读取而不必拷贝。
/// 构造后不可变；需要复用分配时把它交给 [`FrameBuilder::reuse`]。
#[derive(Clone, Debug)]
pub struct Frame {
    buf: Vec<u64>,
    /// 与网格同一快照的可见命令块，经帧句柄传递。
    pub command_blocks: Vec<CommandBlock>,
}

/// 可见范围内的命令块装饰元数据。
#[derive(Clone, Debug)]
pub struct CommandBlock {
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
    pub duration: Option<kanso_types::Message>,
    /// 完整命令是否仍可复制。
    pub can_copy_command: bool,
    /// 是否存在输出范围。
    pub can_copy_output: bool,
}

impl Frame {
    /// 整帧字节，长度等于 `header().total_len`。
    pub fn as_bytes(&self) -> &[u8] {
        cast_slice(&self.buf)
    }

    /// 帧头。
    pub fn header(&self) -> &FrameHeader {
        // 缓冲区起点 8 字节对齐，且 finish 保证至少含完整帧头，因此不会 panic。
        bytemuck::from_bytes(&self.as_bytes()[..HEADER_SIZE])
    }

    /// 缓冲区起点指针，供 FFI 交给 Swift。指针在 `Frame` 被丢弃或复用前有效，期间内容不会变。
    pub fn as_ptr(&self) -> *const u8 {
        self.buf.as_ptr().cast()
    }

    /// 以只读视图解析（会做完整校验）。
    pub fn view(&self) -> Result<FrameView<'_>, FrameError> {
        FrameView::parse(self.as_bytes())
    }
}

/// 帧写入器。
///
/// 单元格与行元数据先按 `rows x cols` 零初始化（即空单元格、默认色），调用方只需写非空部分。
/// 帧头里的偏移、`total_len` 等由 [`FrameBuilder::finish`] 计算，调用方不能也不需要设置。
#[derive(Debug)]
pub struct FrameBuilder {
    cols: u16,
    rows: u16,
    header: FrameHeader,
    /// 帧头占位 + LineMeta 段 + Cell 段；extras 与字符串池在 finish 时才追加，因为它们的长度事先未知。
    buf: Vec<u64>,
    extras: Vec<CellExtra>,
    pool: Vec<u8>,
}

impl FrameBuilder {
    /// 新建写入器。
    ///
    /// # Panics
    ///
    /// `cols * rows` 超过 [`MAX_CELLS`] 时 panic：这是调用方的尺寸契约，真实终端远小于此。
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_buffer(Vec::new(), cols, rows)
    }

    /// 复用旧帧的缓冲区分配，避免每帧重新分配。旧帧内容被丢弃。
    ///
    /// 调用方必须保证没有别处（例如 Swift）仍在读这块缓冲区。
    pub fn reuse(frame: Frame, cols: u16, rows: u16) -> Self {
        Self::with_buffer(frame.buf, cols, rows)
    }

    fn with_buffer(mut buf: Vec<u64>, cols: u16, rows: u16) -> Self {
        let cells = usize::from(cols) * usize::from(rows);
        assert!(cells <= MAX_CELLS, "帧尺寸超过 MAX_CELLS");
        buf.clear();
        // 必须先 clear 再 resize：复用时旧内容不能泄漏到新帧，且保证输出只取决于写入的内容。
        buf.resize(
            HEADER_WORDS + usize::from(rows) * LINE_WORDS + cells * CELL_WORDS,
            0,
        );
        let mut header = FrameHeader::zeroed_header();
        header.cols = cols;
        header.rows = rows;
        Self {
            cols,
            rows,
            header,
            buf,
            extras: Vec::new(),
            pool: Vec::new(),
        }
    }

    fn lines_mut(&mut self) -> &mut [LineMeta] {
        let end = HEADER_WORDS + usize::from(self.rows) * LINE_WORDS;
        cast_slice_mut(&mut self.buf[HEADER_WORDS..end])
    }

    fn cells_mut(&mut self) -> &mut [Cell] {
        let start = HEADER_WORDS + usize::from(self.rows) * LINE_WORDS;
        cast_slice_mut(&mut self.buf[start..])
    }

    /// 设置帧序号。
    pub fn set_seq(&mut self, seq: u64) {
        self.header.seq = seq;
    }

    /// 设置 `FRAME_*` 标志位。
    pub fn set_flags(&mut self, flags: u32) {
        self.header.flags = flags;
    }

    /// 设置第 0 行的绝对行号与显示行号基准。
    pub fn set_line_numbers(&mut self, first_abs_line: u64, line_number_base: u64) {
        self.header.first_abs_line = first_abs_line;
        self.header.line_number_base = line_number_base;
    }

    /// 设置滚动状态。
    pub fn set_scroll(&mut self, display_offset: u32, scrollback_lines: u32) {
        self.header.display_offset = display_offset;
        self.header.scrollback_lines = scrollback_lines;
    }

    /// 设置光标位置与形状（`CURSOR_*`）。可见性由 `FRAME_CURSOR_VISIBLE` 单独控制。
    pub fn set_cursor(&mut self, col: u16, row: u16, shape: u8) {
        self.header.cursor_col = col;
        self.header.cursor_row = row;
        self.header.cursor_shape = shape;
    }

    /// 取某行的元数据；行号越界返回 `None`。
    pub fn line_meta_mut(&mut self, row: u16) -> Option<&mut LineMeta> {
        self.lines_mut().get_mut(usize::from(row))
    }

    /// 写一个单元格。越界坐标被静默忽略：极小尺寸（如 1x1）下调用方（含 mock）
    /// 不必为每次写入单独判断边界。
    pub fn set_cell(&mut self, row: u16, col: u16, cell: Cell) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let idx = usize::from(row) * usize::from(self.cols) + usize::from(col);
        self.cells_mut()[idx] = cell;
    }

    /// 追加一个 `CellExtra`，返回应写入 `Cell::extra` 的值（从 1 起，0 保留为“无”）。
    pub fn push_extra(&mut self, extra: CellExtra) -> Result<u16, FrameError> {
        if self.extras.len() >= MAX_EXTRAS {
            return Err(FrameError::ExtrasFull);
        }
        self.extras.push(extra);
        Ok(self.extras.len() as u16)
    }

    /// 把字形簇文本放入字符串池，返回 `(offset, len)`，偏移相对池起点。
    /// 不做去重，去重由调用方决定。
    pub fn push_grapheme(&mut self, text: &str) -> Result<(u32, u32), FrameError> {
        if self.pool.len() + text.len() > MAX_POOL_LEN {
            return Err(FrameError::PoolFull);
        }
        let offset = self.pool.len() as u32;
        self.pool.extend_from_slice(text.as_bytes());
        Ok((offset, text.len() as u32))
    }

    /// 计算各段偏移、写入帧头，得到最终帧。
    pub fn finish(mut self) -> Frame {
        let line_meta_offset = HEADER_SIZE;
        let cells_offset = line_meta_offset + usize::from(self.rows) * size_of::<LineMeta>();
        let extras_offset = self.buf.len() * 8;
        debug_assert_eq!(
            extras_offset,
            cells_offset + usize::from(self.rows) * usize::from(self.cols) * size_of::<Cell>()
        );
        let pool_offset = extras_offset + self.extras.len() * size_of::<CellExtra>();
        // 池末尾补零到 8 字节，使整个缓冲区长度恰为 u64 的整数倍。
        let total_len = pool_offset + self.pool.len().next_multiple_of(8);

        self.buf.resize(total_len / 8, 0);
        // 必须按字节拷贝：`Vec<CellExtra>` 只保证 4 字节对齐（空 Vec 的悬垂指针也只有 4），
        // 直接 `cast_slice::<CellExtra, u64>` 会因源指针不满足 8 字节对齐而 panic。
        // 反过来把 u64 缓冲区看成字节永远合法。
        let buf_bytes: &mut [u8] = cast_slice_mut(&mut self.buf);
        let extra_bytes: &[u8] = cast_slice(&self.extras);
        buf_bytes[extras_offset..extras_offset + extra_bytes.len()].copy_from_slice(extra_bytes);
        buf_bytes[pool_offset..pool_offset + self.pool.len()].copy_from_slice(&self.pool);

        // 以上各偏移受 MAX_CELLS / MAX_EXTRAS / MAX_POOL_LEN 约束，必在 u32 内。
        let h = &mut self.header;
        h.magic = FRAME_MAGIC;
        h.version = FRAME_VERSION;
        h.header_size = HEADER_SIZE as u16;
        h.line_meta_offset = line_meta_offset as u32;
        h.cells_offset = cells_offset as u32;
        h.extras_offset = extras_offset as u32;
        h.extras_count = self.extras.len() as u32;
        h.pool_offset = pool_offset as u32;
        h.pool_len = self.pool.len() as u32;
        h.total_len = total_len as u32;
        let header_bytes = bytes_of(&self.header);
        cast_slice_mut::<u64, u8>(&mut self.buf[..HEADER_WORDS]).copy_from_slice(header_bytes);
        Frame {
            buf: self.buf,
            command_blocks: Vec::new(),
        }
    }
}

impl FrameHeader {
    fn zeroed_header() -> Self {
        bytemuck::Zeroable::zeroed()
    }
}
