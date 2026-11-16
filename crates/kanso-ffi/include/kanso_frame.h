/*
 * kanso_frame.h：终端帧快照的 C ABI（手写，与 crates/kanso-frame 的布局完全一致）。
 *
 * 约束：
 *  - 帧是 Rust 持有的一块连续只读缓冲区，调用方只读，绝不能写。
 *  - 帧在 kanso_frame_destroy 之前一直有效，期间 Rust 不会修改它；因此可以在任意线程读取，
 *    但销毁必须发生在所有读取结束之后，由调用方保证。
 *  - 所有整数是本机字节序（只支持 aarch64，小端）。
 *  - 改动任何布局都必须同步升 KANSO_FRAME_VERSION、kanso-frame 与 docs/frame-abi.md。
 *  - 本 WP 只导出 mock 帧。按会话取帧的 acquire / release 接口等 core API 定下后再加。
 */
#ifndef KANSO_FRAME_H
#define KANSO_FRAME_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define KANSO_FRAME_MAGIC 0x4D52464Bu /* 小端读出为 ASCII "KFRM" */
#define KANSO_FRAME_VERSION 1u

/* FrameHeader.flags */
#define KANSO_FRAME_ALT_SCREEN (1u << 0)
#define KANSO_FRAME_CURSOR_VISIBLE (1u << 1)
#define KANSO_FRAME_CURSOR_BLINK (1u << 2)
#define KANSO_FRAME_MOUSE_REPORTING (1u << 3)
#define KANSO_FRAME_FOCUSED (1u << 4)

/* FrameHeader.cursor_shape */
#define KANSO_CURSOR_BLOCK 0u
#define KANSO_CURSOR_UNDERLINE 1u
#define KANSO_CURSOR_BAR 2u
#define KANSO_CURSOR_HOLLOW_BLOCK 3u

/* KansoLineMeta.flags */
#define KANSO_LINE_WRAPPED (1u << 0)
#define KANSO_LINE_PROMPT (1u << 1)
#define KANSO_LINE_CURSOR (1u << 2)
#define KANSO_LINE_BLANK (1u << 3)
#define KANSO_LINE_DIRTY (1u << 4)
#define KANSO_LINE_FIRST_PROMPT (1u << 5)

/* KansoCell.flags */
#define KANSO_CELL_BOLD (1u << 0)
#define KANSO_CELL_DIM (1u << 1)
#define KANSO_CELL_ITALIC (1u << 2)
#define KANSO_CELL_UNDERLINE_SHIFT 3u
#define KANSO_CELL_UNDERLINE_MASK (7u << 3)
#define KANSO_CELL_STRIKE (1u << 6)
#define KANSO_CELL_INVERSE (1u << 7)
#define KANSO_CELL_HIDDEN (1u << 8)
#define KANSO_CELL_WIDE (1u << 9)
#define KANSO_CELL_WIDE_SPACER (1u << 10)
#define KANSO_CELL_SELECTED (1u << 11)
#define KANSO_CELL_MATCH (1u << 12)
#define KANSO_CELL_MATCH_CURRENT (1u << 13)
#define KANSO_CELL_BLINK (1u << 14)

/* 下划线样式（取 flags & KANSO_CELL_UNDERLINE_MASK 右移 SHIFT 位） */
#define KANSO_UNDERLINE_NONE 0u
#define KANSO_UNDERLINE_SINGLE 1u
#define KANSO_UNDERLINE_DOUBLE 2u
#define KANSO_UNDERLINE_CURLY 3u
#define KANSO_UNDERLINE_DOTTED 4u
#define KANSO_UNDERLINE_DASHED 5u

/* 颜色编码：高 8 位标签，低 24 位负载。语义类别只会出现在 fg 上。 */
#define KANSO_COLOR_TAG_DEFAULT 0u
#define KANSO_COLOR_TAG_INDEXED 1u
#define KANSO_COLOR_TAG_RGB 2u
#define KANSO_COLOR_TAG_SEMANTIC 3u
#define KANSO_COLOR_TAG_SHIFT 24u

/* 帧头，位于缓冲区起点。以后只在末尾追加字段，读取方应按 header_size 前进。 */
typedef struct KansoFrameHeader {
    uint32_t magic;
    uint16_t version;
    uint16_t header_size;
    uint64_t seq;
    uint16_t cols;
    uint16_t rows;
    uint32_t flags;
    uint64_t first_abs_line;
    uint64_t line_number_base; /* 显示行号 = abs_line - line_number_base + 1 */
    uint32_t display_offset;
    uint32_t scrollback_lines;
    uint16_t cursor_col;
    uint16_t cursor_row;
    uint8_t cursor_shape;
    uint8_t reserved0[3];
    uint32_t line_meta_offset; /* 以下偏移均为字节，相对缓冲区起点 */
    uint32_t cells_offset;
    uint32_t extras_offset;
    uint32_t extras_count;
    uint32_t pool_offset;
    uint32_t pool_len;
    uint32_t total_len;
    uint32_t reserved1;
} KansoFrameHeader;

typedef struct KansoLineMeta {
    uint64_t abs_line;
    int64_t timestamp_ms; /* 0 表示无 */
    uint32_t flags;
    uint32_t reserved;
} KansoLineMeta;

typedef struct KansoCell {
    uint32_t ch; /* 0 表示空单元格 */
    uint32_t fg;
    uint32_t bg;
    uint16_t flags;
    uint16_t extra; /* 0 表示无；n 表示 extras[n-1] */
} KansoCell;

typedef struct KansoCellExtra {
    uint32_t grapheme_offset; /* 相对字符串池起点 */
    uint32_t grapheme_len;    /* 0 表示无 */
    uint32_t hyperlink_id;    /* 0 表示无 */
    uint32_t underline_color; /* 0 表示跟随前景 */
} KansoCellExtra;

_Static_assert(sizeof(KansoFrameHeader) == 88, "KansoFrameHeader 尺寸");
_Static_assert(sizeof(KansoLineMeta) == 24, "KansoLineMeta 尺寸");
_Static_assert(sizeof(KansoCell) == 16, "KansoCell 尺寸");
_Static_assert(sizeof(KansoCellExtra) == 16, "KansoCellExtra 尺寸");

_Static_assert(offsetof(KansoFrameHeader, version) == 4, "version 偏移");
_Static_assert(offsetof(KansoFrameHeader, header_size) == 6, "header_size 偏移");
_Static_assert(offsetof(KansoFrameHeader, seq) == 8, "seq 偏移");
_Static_assert(offsetof(KansoFrameHeader, cols) == 16, "cols 偏移");
_Static_assert(offsetof(KansoFrameHeader, flags) == 20, "flags 偏移");
_Static_assert(offsetof(KansoFrameHeader, first_abs_line) == 24, "first_abs_line 偏移");
_Static_assert(offsetof(KansoFrameHeader, line_number_base) == 32, "line_number_base 偏移");
_Static_assert(offsetof(KansoFrameHeader, display_offset) == 40, "display_offset 偏移");
_Static_assert(offsetof(KansoFrameHeader, cursor_col) == 48, "cursor_col 偏移");
_Static_assert(offsetof(KansoFrameHeader, cursor_shape) == 52, "cursor_shape 偏移");
_Static_assert(offsetof(KansoFrameHeader, line_meta_offset) == 56, "line_meta_offset 偏移");
_Static_assert(offsetof(KansoFrameHeader, cells_offset) == 60, "cells_offset 偏移");
_Static_assert(offsetof(KansoFrameHeader, extras_offset) == 64, "extras_offset 偏移");
_Static_assert(offsetof(KansoFrameHeader, pool_offset) == 72, "pool_offset 偏移");
_Static_assert(offsetof(KansoFrameHeader, total_len) == 80, "total_len 偏移");
_Static_assert(offsetof(KansoLineMeta, timestamp_ms) == 8, "timestamp_ms 偏移");
_Static_assert(offsetof(KansoLineMeta, flags) == 16, "LineMeta.flags 偏移");
_Static_assert(offsetof(KansoCell, fg) == 4, "fg 偏移");
_Static_assert(offsetof(KansoCell, bg) == 8, "bg 偏移");
_Static_assert(offsetof(KansoCell, flags) == 12, "Cell.flags 偏移");
_Static_assert(offsetof(KansoCell, extra) == 14, "extra 偏移");
_Static_assert(offsetof(KansoCellExtra, grapheme_len) == 4, "grapheme_len 偏移");
_Static_assert(offsetof(KansoCellExtra, hyperlink_id) == 8, "hyperlink_id 偏移");
_Static_assert(offsetof(KansoCellExtra, underline_color) == 12, "underline_color 偏移");

/* 不透明帧句柄。 */
typedef struct KansoFrame KansoFrame;

/* 创建确定性的 mock 帧；失败返回 NULL。用 kanso_frame_destroy 释放。 */
KansoFrame *kanso_mock_frame_create(uint16_t cols, uint16_t rows, uint64_t seq);
/* 创建"满屏彩色文字"的 mock 帧（渲染器性能测试用）；失败返回 NULL。用 kanso_frame_destroy 释放。 */
KansoFrame *kanso_mock_frame_create_dense(uint16_t cols, uint16_t rows, uint64_t seq);
/* 返回帧缓冲区起点（帧头）；frame 为 NULL 返回 NULL。 */
const KansoFrameHeader *kanso_frame_header(const KansoFrame *frame);
/* 销毁帧；传 NULL 无操作，不可重复销毁。 */
void kanso_frame_destroy(KansoFrame *frame);

/*
 * 以下访问函数不做越界检查：row 须小于 h->rows，index 须小于 extras_count，
 * 前提是帧来自本库（Rust 写入器保证各段在界内）。
 */

static inline const uint8_t *kanso_frame_base(const KansoFrameHeader *h) {
    return (const uint8_t *)h;
}

static inline const KansoLineMeta *kanso_frame_line_meta(const KansoFrameHeader *h, uint16_t row) {
    return (const KansoLineMeta *)(kanso_frame_base(h) + h->line_meta_offset) + row;
}

/* 第 row 行第 0 个单元格；该行共 h->cols 个，连续存放。 */
static inline const KansoCell *kanso_frame_row(const KansoFrameHeader *h, uint16_t row) {
    return (const KansoCell *)(kanso_frame_base(h) + h->cells_offset) + (size_t)row * h->cols;
}

/* 取单元格的附加信息；cell->extra 为 0 时返回 NULL。 */
static inline const KansoCellExtra *kanso_frame_extra(const KansoFrameHeader *h, const KansoCell *cell) {
    if (cell->extra == 0) {
        return NULL;
    }
    return (const KansoCellExtra *)(kanso_frame_base(h) + h->extras_offset) + (cell->extra - 1u);
}

/* 字形簇文本起点（UTF-8，不以 NUL 结尾，长度见 grapheme_len）；无字形簇时返回 NULL。 */
static inline const char *kanso_frame_grapheme(const KansoFrameHeader *h, const KansoCellExtra *extra) {
    if (extra->grapheme_len == 0) {
        return NULL;
    }
    return (const char *)(kanso_frame_base(h) + h->pool_offset + extra->grapheme_offset);
}

static inline uint32_t kanso_color_tag(uint32_t color) {
    return color >> KANSO_COLOR_TAG_SHIFT;
}

/* 颜色负载：索引（低 8 位）、0xRRGGBB 或语义类别（低 8 位），含义由标签决定。 */
static inline uint32_t kanso_color_payload(uint32_t color) {
    return color & 0x00FFFFFFu;
}

#ifdef __cplusplus
}
#endif

#endif /* KANSO_FRAME_H */
