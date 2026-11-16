//! 帧只读解析与校验。

use crate::layout::{
    Cell, CellExtra, FRAME_MAGIC, FRAME_VERSION, FrameHeader, HEADER_SIZE, LineMeta,
};
use bytemuck::try_cast_slice;
use core::fmt;
use core::mem::size_of;

/// 帧构造或解析失败的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// 输入短于帧头。
    TooShort,
    /// magic 不符。
    BadMagic,
    /// 版本不被支持。
    UnsupportedVersion(u16),
    /// `header_size` 小于本实现已知的帧头，或超过总长。
    BadHeaderSize,
    /// `total_len` 与切片长度不一致。
    TotalLenMismatch,
    /// 某个段的偏移或长度越界。
    SectionOutOfBounds(&'static str),
    /// 缓冲区或某个段的起点未按要求对齐。
    Misaligned,
    /// `Cell::extra` 指向不存在的 `CellExtra`。
    ExtraIndexOutOfRange,
    /// 字形簇区间超出字符串池。
    GraphemeOutOfRange,
    /// 字形簇不是合法 UTF-8。
    InvalidUtf8,
    /// `CellExtra` 数量已达上限。
    ExtrasFull,
    /// 字符串池已达上限。
    PoolFull,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => write!(f, "帧数据短于帧头"),
            Self::BadMagic => write!(f, "magic 不符"),
            Self::UnsupportedVersion(v) => write!(f, "不支持的帧版本 {v}"),
            Self::BadHeaderSize => write!(f, "header_size 非法"),
            Self::TotalLenMismatch => write!(f, "total_len 与数据长度不一致"),
            Self::SectionOutOfBounds(s) => write!(f, "{s} 段越界"),
            Self::Misaligned => write!(f, "缓冲区未对齐"),
            Self::ExtraIndexOutOfRange => write!(f, "extra 索引越界"),
            Self::GraphemeOutOfRange => write!(f, "字形簇区间超出字符串池"),
            Self::InvalidUtf8 => write!(f, "字形簇不是合法 UTF-8"),
            Self::ExtrasFull => write!(f, "CellExtra 数量已达上限"),
            Self::PoolFull => write!(f, "字符串池已达上限"),
        }
    }
}

impl std::error::Error for FrameError {}

/// 对一块帧字节的只读视图，解析时已校验，之后的访问器不会越界。
///
/// 输入切片起点必须 8 字节对齐（`Frame::as_bytes` 满足），否则返回 [`FrameError::Misaligned`]。
#[derive(Clone, Copy, Debug)]
pub struct FrameView<'a> {
    header: &'a FrameHeader,
    lines: &'a [LineMeta],
    cells: &'a [Cell],
    extras: &'a [CellExtra],
    pool: &'a [u8],
}

/// 取 `[offset, offset + len)`，越界（含算术溢出）返回错误。
fn section<'a>(
    bytes: &'a [u8],
    offset: u32,
    len: usize,
    name: &'static str,
) -> Result<&'a [u8], FrameError> {
    let start = offset as usize;
    let end = start
        .checked_add(len)
        .ok_or(FrameError::SectionOutOfBounds(name))?;
    bytes
        .get(start..end)
        .ok_or(FrameError::SectionOutOfBounds(name))
}

fn cast<T: bytemuck::AnyBitPattern>(bytes: &[u8]) -> Result<&[T], FrameError> {
    try_cast_slice(bytes).map_err(|_| FrameError::Misaligned)
}

impl<'a> FrameView<'a> {
    /// 解析并校验。
    ///
    /// 校验项：magic、version、header_size、`total_len`、各段不越界且对齐、
    /// 每个 `Cell::extra` 不越界、每个字形簇区间在池内且是合法 UTF-8。
    /// 帧来自本进程的 Rust 写入器时这些必然成立；校验面向测试与将来可能的外部输入。
    pub fn parse(bytes: &'a [u8]) -> Result<Self, FrameError> {
        if bytes.len() < 8 {
            return Err(FrameError::TooShort);
        }
        // 先只读 magic/version/header_size：这三个字段的位置永远不变，
        // 即使帧头在新版本里变长也能据此判断。
        let magic = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
        let version = u16::from_ne_bytes(bytes[4..6].try_into().unwrap());
        let header_size = u16::from_ne_bytes(bytes[6..8].try_into().unwrap());
        if magic != FRAME_MAGIC {
            return Err(FrameError::BadMagic);
        }
        if version != FRAME_VERSION {
            return Err(FrameError::UnsupportedVersion(version));
        }
        if usize::from(header_size) < HEADER_SIZE || usize::from(header_size) > bytes.len() {
            return Err(FrameError::BadHeaderSize);
        }
        let header: &FrameHeader =
            bytemuck::try_from_bytes(&bytes[..HEADER_SIZE]).map_err(|_| FrameError::Misaligned)?;
        if header.total_len as usize != bytes.len() {
            return Err(FrameError::TotalLenMismatch);
        }

        let rows = usize::from(header.rows);
        let cells = rows * usize::from(header.cols);
        let lines = cast::<LineMeta>(section(
            bytes,
            header.line_meta_offset,
            rows * size_of::<LineMeta>(),
            "line_meta",
        )?)?;
        let cell_slice = cast::<Cell>(section(
            bytes,
            header.cells_offset,
            cells * size_of::<Cell>(),
            "cells",
        )?)?;
        let extras = cast::<CellExtra>(section(
            bytes,
            header.extras_offset,
            (header.extras_count as usize) * size_of::<CellExtra>(),
            "extras",
        )?)?;
        let pool = section(bytes, header.pool_offset, header.pool_len as usize, "pool")?;

        if cell_slice
            .iter()
            .any(|c| usize::from(c.extra) > extras.len())
        {
            return Err(FrameError::ExtraIndexOutOfRange);
        }
        for e in extras.iter().filter(|e| e.grapheme_len != 0) {
            let start = e.grapheme_offset as usize;
            let end = start
                .checked_add(e.grapheme_len as usize)
                .ok_or(FrameError::GraphemeOutOfRange)?;
            let text = pool.get(start..end).ok_or(FrameError::GraphemeOutOfRange)?;
            core::str::from_utf8(text).map_err(|_| FrameError::InvalidUtf8)?;
        }

        Ok(Self {
            header,
            lines,
            cells: cell_slice,
            extras,
            pool,
        })
    }

    /// 帧头。
    pub fn header(&self) -> &'a FrameHeader {
        self.header
    }

    /// 某行的元数据；越界返回 `None`。
    pub fn line_meta(&self, row: u16) -> Option<&'a LineMeta> {
        self.lines.get(usize::from(row))
    }

    /// 第 `row` 行的全部单元格；越界返回空切片。
    pub fn row(&self, row: u16) -> &'a [Cell] {
        let cols = usize::from(self.header.cols);
        let start = usize::from(row) * cols;
        self.cells.get(start..start + cols).unwrap_or(&[])
    }

    /// 单元格的附加信息；`extra` 为 0 时返回 `None`。
    pub fn extra(&self, cell: &Cell) -> Option<&'a CellExtra> {
        usize::from(cell.extra)
            .checked_sub(1)
            .and_then(|i| self.extras.get(i))
    }

    /// 附加信息里的字形簇文本；`grapheme_len` 为 0 时返回 `None`。
    pub fn grapheme(&self, extra: &CellExtra) -> Option<&'a str> {
        if extra.grapheme_len == 0 {
            return None;
        }
        let start = extra.grapheme_offset as usize;
        let text = self.pool.get(start..start + extra.grapheme_len as usize)?;
        core::str::from_utf8(text).ok()
    }
}
