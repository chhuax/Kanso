//! 自绘元素：量行列、画背景 / 文字 / 光标 / 输入法预编辑，并把输入处理器注册给窗口。

use gpui::{
    App, BorderStyle, Bounds, Element, ElementId, ElementInputHandler, Entity, Font, FontStyle,
    FontWeight, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, PathBuilder,
    Pixels, Point, SharedString, StrikethroughStyle, Style, TextRun, TransformationMatrix,
    UnderlineStyle, Window, fill, outline, point, px, relative, rgb, size,
};
use kanso_frame::UNDERLINE_CURLY;
use kanso_gpui::ime::layout_preedit;
use kanso_gpui::render::{ChipRun, CursorKind, RenderGrid};
use kanso_gpui::theme::{Palette, Rgb8};

use super::decor;
use super::view::{Metrics, PAD, TerminalView, compute_grid};
use super::{START, log, memory};

/// 终端字体：取自显示设置（见 `display::terminal_font`）。
pub(crate) fn base_font() -> Font {
    super::display::terminal_font()
}

fn font_for(bold: bool, italic: bool) -> Font {
    let mut f = base_font();
    if bold {
        f.weight = FontWeight::BOLD;
    }
    if italic {
        f.style = FontStyle::Italic;
    }
    f
}

pub(super) fn hsla(c: Rgb8) -> Hsla {
    rgb(c.to_u32()).into()
}

pub(crate) struct TerminalElement {
    pub view: Entity<TerminalView>,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        // 窗口大小变化 → 重新算行列数 → 通知会话。
        let (metrics, current, gutter_w) = {
            let v = self.view.read(cx);
            (v.metrics, v.size, v.gutter_width())
        };
        if let Some(size) = compute_grid(
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
            &metrics,
            gutter_w,
        ) && size != current
        {
            let scale = window.scale_factor();
            self.view
                .update(cx, |v, cx| v.apply_resize(size, scale, cx));
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (grid, palette, metrics, marked, caret, focus, decor, gutter_w) = {
            let v = self.view.read(cx);
            (
                v.grid.clone(),
                v.palette.clone(),
                v.metrics,
                v.ime.marked_text().map(str::to_owned),
                v.ime.selected_range().start,
                v.focus.clone(),
                v.decor_input(),
                v.gutter_width(),
            )
        };
        // 正文区从时间戳栏右侧开始。
        let origin = bounds.origin + point(px(gutter_w + PAD), px(PAD));
        // 输入法：每帧在绘制阶段把处理器交给窗口，候选框定位与组字回调都经由它。
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );

        if let Some(grid) = grid {
            // 选中的命令块底色在文字背景之下。
            if let Some(d) = &decor {
                decor::paint_block_highlight(d, bounds, origin, window);
            }
            paint_grid(&grid, &palette, &metrics, origin, window, cx);
            if let Some(d) = &decor {
                decor::paint_overlays(d, bounds, origin, window, cx);
            }
            if let Some(text) = marked {
                paint_preedit(&text, caret, &grid, &palette, &metrics, origin, window, cx);
            }
        }

        let first = self.view.update(cx, |v, _| {
            v.origin = Some(origin);
            let first = !v.first_painted && v.grid.is_some();
            if first {
                v.first_painted = true;
            }
            first
        });
        if first {
            let ms = START.get().map_or(0, |s| s.elapsed().as_millis());
            log(&format!(
                "首帧已绘制：启动到首帧 {ms} ms，{}",
                memory::describe()
            ));
        }
    }
}

pub(super) fn cell_rect(
    origin: Point<Pixels>,
    m: &Metrics,
    row: u16,
    col: u16,
    cols: u16,
) -> Bounds<Pixels> {
    // 边界取整，避免相邻矩形之间出现缝。
    let x0 = (f32::from(col) * m.cell_w).round();
    let x1 = (f32::from(col + cols) * m.cell_w).round();
    let y0 = (f32::from(row) * m.cell_h).round();
    let y1 = (f32::from(row + 1) * m.cell_h).round();
    Bounds::new(
        point(origin.x + px(x0), origin.y + px(y0)),
        size(px(x1 - x0), px(y1 - y0)),
    )
}

/// 当前待输入提示符的小圆角标签。历史提示符不画这块底，只留缩小、变淡的文字。
fn paint_chip(
    chip: &ChipRun,
    palette: &Palette,
    m: &Metrics,
    origin: Point<Pixels>,
    stroke: Pixels,
    window: &mut Window,
) {
    if !chip.emphasized {
        return;
    }
    let mut bounds = cell_rect(origin, m, chip.row, chip.col, chip.cols);
    bounds.origin.y += stroke;
    bounds.size.height -= stroke * 2.;
    let radius = px(3.).min(bounds.size.height / 2.);
    window.paint_quad(gpui::quad(
        bounds,
        radius,
        hsla(palette.decorations.chip_background),
        stroke,
        hsla(palette.decorations.chip_border),
        BorderStyle::Solid,
    ));
}

fn paint_grid(
    grid: &RenderGrid,
    palette: &Palette,
    m: &Metrics,
    origin: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let line_height = px(m.cell_h);
    for bg in &grid.bgs {
        window.paint_quad(fill(
            cell_rect(origin, m, bg.row, bg.col, bg.cols),
            hsla(bg.color),
        ));
    }

    let stroke = px(1. / window.scale_factor());
    for chip in &grid.chips {
        paint_chip(chip, palette, m, origin, stroke, window);
    }
    for span in &grid.spans {
        let fg = hsla(span.fg);
        if span.text == "\u{e0b0}" {
            let cell = cell_rect(origin, m, span.row, span.col, span.cells);
            let mut path = PathBuilder::fill();
            path.move_to(cell.origin);
            path.line_to(point(cell.right(), cell.center().y));
            path.line_to(cell.bottom_left());
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, fg);
            }
            continue;
        }
        let icon = match span.text.as_str() {
            "\u{e0a0}" => Some("icons/git-branch.svg"),
            "\u{f07b}" => Some("icons/folder.svg"),
            _ => None,
        };
        if let Some(icon) = icon {
            let cell = cell_rect(origin, m, span.row, span.col, span.cells);
            let edge = cell.size.width.min(cell.size.height);
            let center = cell.center();
            let bounds = Bounds::new(
                point(center.x - edge / 2., center.y - edge / 2.),
                size(edge, edge),
            );
            let _ = window.paint_svg(bounds, icon.into(), TransformationMatrix::unit(), fg, cx);
            continue;
        }
        let run = TextRun {
            len: span.text.len(),
            font: font_for(span.bold, span.italic),
            color: fg,
            background_color: None,
            underline: (span.underline != 0).then(|| UnderlineStyle {
                color: Some(hsla(span.underline_color.unwrap_or(span.fg))),
                thickness: px(1.0),
                wavy: span.underline == UNDERLINE_CURLY,
            }),
            strikethrough: span.strike.then_some(StrikethroughStyle {
                thickness: px(1.0),
                color: Some(fg),
            }),
        };
        let force = span.simple.then(|| px(m.cell_w));
        let line = window.text_system().shape_line(
            SharedString::from(span.text.clone()),
            px(m.font_size),
            &[run],
            force,
        );
        let at = point(
            origin.x + px((f32::from(span.col) * m.cell_w).round()),
            origin.y + px((f32::from(span.row) * m.cell_h).round()),
        );
        let _ = line.paint(at, line_height, window, cx);
    }

    if let Some(c) = &grid.cursor {
        let r = cell_rect(origin, m, c.row, c.col, c.cells);
        let color = hsla(c.color);
        match c.kind {
            CursorKind::Block => {}
            CursorKind::Underline => {
                let h = px(2.0);
                window.paint_quad(fill(
                    Bounds::new(
                        point(r.origin.x, r.origin.y + r.size.height - h),
                        size(r.size.width, h),
                    ),
                    color,
                ));
            }
            CursorKind::Bar => {
                window.paint_quad(fill(
                    Bounds::new(r.origin, size(px(2.0), r.size.height)),
                    color,
                ));
            }
            CursorKind::Hollow => {
                window.paint_quad(outline(r, color, BorderStyle::Solid));
            }
        }
    }
}

/// 在光标处画输入法预编辑：盖住底下的字，下划线表示“尚未确认”，并画一根输入法光标。
#[allow(clippy::too_many_arguments)]
fn paint_preedit(
    text: &str,
    caret_utf16: usize,
    grid: &RenderGrid,
    palette: &Palette,
    m: &Metrics,
    origin: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let (cursor_col, cursor_row) = grid.cursor_cell;
    let layout = layout_preedit(text, caret_utf16, cursor_col, grid.cols);
    let fg = hsla(palette.foreground);
    for cell in &layout.cells {
        let r = cell_rect(origin, m, cursor_row, cell.col, cell.cells);
        window.paint_quad(fill(r, hsla(palette.background)));
        let run = TextRun {
            len: cell.text.len(),
            font: base_font(),
            color: fg,
            background_color: None,
            underline: Some(UnderlineStyle {
                color: Some(fg),
                thickness: px(1.0),
                wavy: false,
            }),
            strikethrough: None,
        };
        let line = window.text_system().shape_line(
            SharedString::from(cell.text.clone()),
            px(m.font_size),
            &[run],
            None,
        );
        let _ = line.paint(r.origin, px(m.cell_h), window, cx);
    }
    // 输入法光标。
    let caret_col = layout.caret_col.min(grid.cols.saturating_sub(1));
    let r = cell_rect(origin, m, cursor_row, caret_col, 1);
    let x = if layout.caret_col > caret_col {
        r.origin.x + r.size.width - px(2.0)
    } else {
        r.origin.x
    };
    window.paint_quad(fill(
        Bounds::new(point(x, r.origin.y), size(px(2.0), r.size.height)),
        hsla(palette.cursor),
    ));
}
