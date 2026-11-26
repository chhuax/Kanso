//! 终端装饰的绘制：时间戳 / 行号栏、命令分隔线、失败块色条、悬停链接下划线。
//!
//! 数据全部来自帧（`RenderGrid` 的行元数据与 `CommandBlock` 列表）；几何与格式化的纯逻辑在
//! `kanso_gpui::app::{gutter, blocks}`。对照 mac 版 `TerminalKit/TerminalRenderer`。

use std::sync::Arc;

use gpui::{
    App, Bounds, Hsla, Pixels, Point, SharedString, TextRun, Window, fill, point, px, rgba, size,
};
use kanso_frame::CommandBlock;
use kanso_gpui::app::blocks::{Stripe, block_at_line, stripe};
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

/// 命令提示符所在行的整行浅底色（在文字与选区之下，不覆盖时间戳栏）。
pub(crate) fn paint_prompt_backgrounds(
    d: &DecorInput,
    bounds: Bounds<Pixels>,
    origin: Point<Pixels>,
    window: &mut Window,
) {
    if d.grid.alt_screen || d.blocks.is_empty() {
        return;
    }
    let tint = overlay(d.palette.decorations.prompt_background);
    let x = f32::from(bounds.origin.x) + d.gutter_w;
    let width = (f32::from(bounds.size.width) - d.gutter_w).max(0.0);
    for (row, info) in d.grid.lines.iter().enumerate() {
        let abs = info.abs_line;
        let is_prompt = d.blocks.iter().any(|b| {
            let end = b.command_line.unwrap_or(b.end_line);
            b.start_line <= abs && abs <= end
        });
        if is_prompt {
            let y0 = row_y(origin, &d.metrics, row as u16);
            let y1 = row_y(origin, &d.metrics, row as u16 + 1);
            window.paint_quad(fill(
                Bounds::new(point(px(x), px(y0)), size(px(width), px(y1 - y0))),
                tint,
            ));
        }
    }
}

/// 栏、折叠树线与色条（在文字之上，半透明 / 细线不影响阅读）。
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
        // 左侧命令块折叠图标（[-]/[+]）与输出树线（| / └）；无输出但失败或运行中的块保留细色条。
        let fold_cx = snap(f32::from(bounds.origin.x) + d.gutter_w + 9.0, scale);
        let box_sz = 9.0_f32;
        let box_half = 4.0_f32;
        for block in &d.blocks {
            let failed = matches!(stripe(block), Some(Stripe::Failed));
            if block.foldable {
                let cmd_abs = block.command_line.unwrap_or(block.start_line);
                let color = if block.folded {
                    hsla(d.palette.cursor)
                } else if failed {
                    hsla(d.palette.ansi[1])
                } else {
                    hsla(deco.gutter_text)
                };
                if let Some((cmd_row, _)) = d
                    .grid
                    .lines
                    .iter()
                    .enumerate()
                    .find(|(_, info)| info.abs_line == cmd_abs)
                {
                    let cmd_y = row_y(origin, m, cmd_row as u16);
                    let bx = snap(fold_cx - box_half, scale);
                    let by = snap(cmd_y + (m.cell_h - box_sz) / 2.0, scale);
                    window.paint_quad(fill(
                        Bounds::new(point(px(bx), px(by)), size(px(box_sz), px(thin))),
                        color,
                    ));
                    window.paint_quad(fill(
                        Bounds::new(
                            point(px(bx), px(by + box_sz - thin)),
                            size(px(box_sz), px(thin)),
                        ),
                        color,
                    ));
                    window.paint_quad(fill(
                        Bounds::new(point(px(bx), px(by)), size(px(thin), px(box_sz))),
                        color,
                    ));
                    window.paint_quad(fill(
                        Bounds::new(
                            point(px(bx + box_sz - thin), px(by)),
                            size(px(thin), px(box_sz)),
                        ),
                        color,
                    ));
                    // 内部 -
                    window.paint_quad(fill(
                        Bounds::new(
                            point(px(bx + 2.0), px(snap(by + box_half, scale))),
                            size(px(box_sz - 4.0), px(thin)),
                        ),
                        color,
                    ));
                    if block.folded {
                        // 内部 +
                        window.paint_quad(fill(
                            Bounds::new(
                                point(px(snap(bx + box_half, scale)), px(by + 2.0)),
                                size(px(thin), px(box_sz - 4.0)),
                            ),
                            color,
                        ));
                    } else {
                        let stem_top = by + box_sz;
                        let stem_h = (cmd_y + m.cell_h - stem_top).max(0.0);
                        if stem_h > 0.0 {
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(px(fold_cx), px(stem_top)),
                                    size(px(thin), px(stem_h)),
                                ),
                                color,
                            ));
                        }
                    }
                }
                if !block.folded {
                    for (row, info) in d.grid.lines.iter().enumerate() {
                        let abs = info.abs_line;
                        if abs <= cmd_abs || abs > block.end_line {
                            continue;
                        }
                        let y0 = row_y(origin, m, row as u16);
                        if abs < block.end_line {
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(px(fold_cx), px(y0)),
                                    size(px(thin), px(m.cell_h)),
                                ),
                                color,
                            ));
                        } else {
                            let mid_y = snap(y0 + m.cell_h / 2.0, scale);
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(px(fold_cx), px(y0)),
                                    size(px(thin), px((mid_y - y0 + thin).max(0.0))),
                                ),
                                color,
                            ));
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(px(fold_cx), px(mid_y)),
                                    size(px(box_half + 1.0), px(thin)),
                                ),
                                color,
                            ));
                        }
                    }
                }
            } else if let Some(kind) = stripe(block) {
                let color = match kind {
                    Stripe::Failed => d.palette.ansi[1],
                    Stripe::Running => d.palette.cursor,
                };
                for (row, info) in d.grid.lines.iter().enumerate() {
                    if block.start_line <= info.abs_line && info.abs_line <= block.end_line {
                        let y0 = row_y(origin, m, row as u16);
                        window.paint_quad(fill(
                            Bounds::new(point(px(fold_cx), px(y0)), size(px(2.0), px(m.cell_h))),
                            hsla(color),
                        ));
                    }
                }
            }
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
