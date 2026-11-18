//! 终端装饰的绘制：命令块选中底色、时间戳 / 行号栏、命令分隔线、失败块色条、悬停链接下划线。
//!
//! 数据全部来自帧（`RenderGrid` 的行元数据与 `CommandBlock` 列表）；几何与格式化的纯逻辑在
//! `kanso_gpui::app::{gutter, blocks}`。对照 mac 版 `TerminalKit/TerminalRenderer`。

use std::sync::Arc;

use gpui::{
    App, Bounds, Hsla, Pixels, Point, SharedString, TextRun, Window, fill, point, px, rgba, size,
};
use kanso_frame::CommandBlock;
use kanso_gpui::app::blocks::{Stripe, block_at_line, has_command, stripe, visible_rows};
use kanso_gpui::app::gutter::{self, line_text};
use kanso_gpui::app::links::LinkHit;
use kanso_gpui::app::settings::GutterMode;
use kanso_gpui::render::RenderGrid;
use kanso_gpui::theme::{Overlay, Palette};

use super::element::{base_font, cell_rect, hsla};
use super::view::Metrics;

/// 一次绘制需要的装饰数据（从视图里取出的快照）。
pub(crate) struct DecorInput {
    pub grid: Arc<RenderGrid>,
    pub palette: Arc<Palette>,
    pub metrics: Metrics,
    pub gutter_mode: GutterMode,
    /// 栏宽（逻辑像素）；关闭为 0。
    pub gutter_w: f32,
    pub blocks: Vec<CommandBlock>,
    pub selected_block: Option<u64>,
    pub hover: Option<LinkHit>,
    pub tz_offset_min: i32,
}

fn overlay(o: Overlay) -> Hsla {
    rgba((o.rgb.to_u32() << 8) | u32::from(o.alpha)).into()
}

/// 对齐设备像素：逻辑坐标 → 最近的整数物理像素。
fn snap(v: f32, scale: f32) -> f32 {
    (v * scale).round() / scale
}

fn row_y(origin: Point<Pixels>, m: &Metrics, row: u16) -> f32 {
    f32::from(origin.y) + (f32::from(row) * m.cell_h).round()
}

/// 选中命令块的整行底色（在文字背景之下）：从提示符行覆盖到块最后一行，
/// 左右铺满正文区（含左右留白），不覆盖时间戳栏。
pub(crate) fn paint_block_highlight(
    d: &DecorInput,
    bounds: Bounds<Pixels>,
    origin: Point<Pixels>,
    window: &mut Window,
) {
    if d.grid.alt_screen {
        return;
    }
    let Some(id) = d.selected_block else {
        return;
    };
    let Some(block) = d.blocks.iter().find(|b| b.id == id && has_command(b)) else {
        return;
    };
    let Some((r0, r1)) = visible_rows(block, d.grid.first_abs_line, d.grid.rows) else {
        return;
    };
    let x = f32::from(bounds.origin.x) + d.gutter_w;
    let y0 = row_y(origin, &d.metrics, r0);
    let y1 = row_y(origin, &d.metrics, r1 + 1);
    window.paint_quad(fill(
        Bounds::new(
            point(px(x), px(y0)),
            size(
                px((f32::from(bounds.size.width) - d.gutter_w).max(0.0)),
                px(y1 - y0),
            ),
        ),
        hsla(d.palette.decorations.block_selection),
    ));
}

/// 栏、分隔线与色条（在文字之上，半透明 / 细线不影响阅读）。
pub(crate) fn paint_overlays(
    d: &DecorInput,
    bounds: Bounds<Pixels>,
    origin: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let scale = window.scale_factor();
    let thin = 1.0 / scale;
    let m = &d.metrics;
    let deco = &d.palette.decorations;
    let sep = overlay(deco.command_separator);
    let left = f32::from(bounds.origin.x);

    // 栏与正文之间的竖线：贯穿整个高度，全屏程序期间也保留，版面不跳动。
    if d.gutter_w > 0.0 {
        window.paint_quad(fill(
            Bounds::new(
                point(px(snap(left + d.gutter_w, scale)), bounds.origin.y),
                size(px(thin), bounds.size.height),
            ),
            sep,
        ));
    }

    if !d.grid.alt_screen {
        // 命令分隔线：提示符行上方 1 个设备像素，横跨整个视图（含栏）。同一行只画一条由核心去重保证。
        for (row, info) in d.grid.lines.iter().enumerate() {
            if info.prompt && !info.first_prompt {
                let y = snap(row_y(origin, m, row as u16), scale);
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.origin.x, px(y)),
                        size(bounds.size.width, px(thin)),
                    ),
                    sep,
                ));
            }
        }

        // 块的左侧色条：失败红 / 运行中强调色，画在栏与正文之间的左内边距里，不与时间戳重叠。
        for block in &d.blocks {
            let color = match stripe(block) {
                Some(Stripe::Failed) => d.palette.ansi[1],
                Some(Stripe::Running) => d.palette.cursor,
                None => continue,
            };
            let Some((r0, r1)) = visible_rows(block, d.grid.first_abs_line, d.grid.rows) else {
                continue;
            };
            let y0 = row_y(origin, m, r0);
            let y1 = row_y(origin, m, r1 + 1);
            window.paint_quad(fill(
                Bounds::new(
                    point(px(snap(f32::from(origin.x) - 4.0, scale)), px(y0)),
                    size(px(2.0), px(y1 - y0)),
                ),
                hsla(color),
            ));
        }

        if d.gutter_w > 0.0 {
            paint_gutter(d, bounds, origin, window, cx);
        }
    }

    // 悬停的链接：单元格底部一条细线。
    if let Some(link) = &d.hover {
        for &(row, c0, c1) in &link.segments {
            let r = cell_rect(origin, m, row, c0, c1.saturating_sub(c0).max(1));
            window.paint_quad(fill(
                Bounds::new(
                    point(r.origin.x, r.origin.y + r.size.height - px(2.0)),
                    size(r.size.width, px(1.0)),
                ),
                hsla(d.palette.foreground),
            ));
        }
    }
}

/// 每行的时间与行号。栏字体比正文小一点，行内垂直居中；光标所在行高亮；空白行不显示。
fn paint_gutter(
    d: &DecorInput,
    bounds: Bounds<Pixels>,
    origin: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let m = &d.metrics;
    let deco = &d.palette.decorations;
    let color = hsla(deco.gutter_text);
    let cursor_line = overlay(deco.gutter_cursor_line);
    let left = f32::from(bounds.origin.x);
    for (row, info) in d.grid.lines.iter().enumerate() {
        let row = row as u16;
        let y = row_y(origin, m, row);
        if info.cursor {
            window.paint_quad(fill(
                Bounds::new(
                    point(px(left), px(y)),
                    size(px(d.gutter_w), px(m.cell_h.round())),
                ),
                cursor_line,
            ));
        }
        let Some(text) = line_text(
            d.gutter_mode,
            info.blank,
            info.timestamp_ms,
            info.abs_line,
            d.grid.line_number_base,
            d.tz_offset_min,
        ) else {
            continue;
        };
        let run = TextRun {
            len: text.len(),
            font: base_font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window.text_system().shape_line(
            SharedString::from(text),
            px(m.gutter_font_size),
            &[run],
            Some(px(m.gutter_cell_w)),
        );
        // 左侧留 1 个栏字符的空白。
        let at = point(px(left + m.gutter_cell_w), px(y));
        let _ = line.paint(at, px(m.cell_h), window, cx);
    }
}

/// 右键位置所在的命令块（有命令的才算）。
pub(crate) fn block_for_row<'a>(
    grid: &RenderGrid,
    blocks: &'a [CommandBlock],
    row: u16,
) -> Option<&'a CommandBlock> {
    if grid.alt_screen {
        return None;
    }
    let abs = grid.lines.get(usize::from(row))?.abs_line;
    block_at_line(blocks, abs)
}

/// 栏的字符数对应的像素宽（栏字体的单元格宽 × 栏格数，取整）。
pub(crate) fn gutter_width(mode: GutterMode, gutter_cell_w: f32) -> f32 {
    (gutter::width_cells(mode) as f32 * gutter_cell_w).round()
}
