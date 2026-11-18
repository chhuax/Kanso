//! 像素与行列的换算。

use kanso_types::geometry::{CellSide, GridSize, ViewportPoint};

/// 终端区域的最小尺寸：再小 shell 与全屏程序都无法正常工作。
pub const MIN_COLS: u16 = 2;
/// 最小行数。
pub const MIN_ROWS: u16 = 1;

/// 由可用像素和单元格像素尺寸算行列数（向下取整，夹在 `MIN_*`..=`u16::MAX`）。
/// 单元格尺寸无效（非正或非有限）时返回 `None`。
pub fn grid_size(width: f32, height: f32, cell_w: f32, cell_h: f32) -> Option<GridSize> {
    if !(cell_w.is_finite() && cell_h.is_finite() && cell_w > 0.0 && cell_h > 0.0) {
        return None;
    }
    if !(width.is_finite() && height.is_finite()) {
        return None;
    }
    let cols = (width / cell_w)
        .floor()
        .clamp(f32::from(MIN_COLS), f32::from(u16::MAX));
    let rows = (height / cell_h)
        .floor()
        .clamp(f32::from(MIN_ROWS), f32::from(u16::MAX));
    Some(GridSize {
        cols: cols as u16,
        rows: rows as u16,
    })
}

/// 把相对网格左上角的像素坐标换成视口单元格坐标；超出范围的夹到边缘。
/// `side` 取单元格内的左半 / 右半（选区边界要用）。
pub fn cell_at(x: f32, y: f32, cell_w: f32, cell_h: f32, size: GridSize) -> ViewportPoint {
    let fx = if cell_w > 0.0 { x / cell_w } else { 0.0 };
    let fy = if cell_h > 0.0 { y / cell_h } else { 0.0 };
    let max_col = f32::from(size.cols.saturating_sub(1));
    let max_row = f32::from(size.rows.saturating_sub(1));
    let col = fx.floor().clamp(0.0, max_col);
    let row = fy.floor().clamp(0.0, max_row);
    let side = if fx < 0.0 {
        CellSide::Left
    } else if fx >= f32::from(size.cols) || fx - fx.floor() >= 0.5 {
        CellSide::Right
    } else {
        CellSide::Left
    };
    ViewportPoint {
        col: col as u16,
        row: row as u16,
        side,
    }
}

/// 滚轮的纵向像素位移换成“行数”（正数 = 看更早的内容，与 `Core::scroll` 的约定一致）。
pub fn scroll_lines(delta_y_px: f32, cell_h: f32) -> f32 {
    if cell_h > 0.0 && delta_y_px.is_finite() {
        delta_y_px / cell_h
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_size_floors() {
        let g = grid_size(800.0, 600.0, 8.0, 16.0).unwrap();
        assert_eq!((g.cols, g.rows), (100, 37));
        let g = grid_size(807.9, 623.9, 8.0, 16.0).unwrap();
        assert_eq!((g.cols, g.rows), (100, 38));
    }

    #[test]
    fn grid_size_clamps_and_rejects_bad_metrics() {
        let g = grid_size(1.0, 1.0, 8.0, 16.0).unwrap();
        assert_eq!((g.cols, g.rows), (MIN_COLS, MIN_ROWS));
        let g = grid_size(1.0e9, 1.0e9, 1.0, 1.0).unwrap();
        assert_eq!((g.cols, g.rows), (u16::MAX, u16::MAX));
        assert!(grid_size(100.0, 100.0, 0.0, 10.0).is_none());
        assert!(grid_size(100.0, 100.0, 5.0, f32::NAN).is_none());
        assert!(grid_size(f32::NAN, 100.0, 5.0, 5.0).is_none());
    }

    #[test]
    fn cell_at_maps_and_clamps() {
        let size = GridSize { cols: 10, rows: 5 };
        let p = cell_at(0.0, 0.0, 8.0, 16.0, size);
        assert_eq!((p.col, p.row, p.side), (0, 0, CellSide::Left));
        let p = cell_at(13.0, 17.0, 8.0, 16.0, size);
        assert_eq!((p.col, p.row, p.side), (1, 1, CellSide::Right));
        let p = cell_at(1000.0, 1000.0, 8.0, 16.0, size);
        assert_eq!((p.col, p.row), (9, 4));
        let p = cell_at(1000.0, 0.0, 8.0, 16.0, size);
        assert_eq!(p.side, CellSide::Right);
        let p = cell_at(-5.0, -5.0, 8.0, 16.0, size);
        assert_eq!((p.col, p.row, p.side), (0, 0, CellSide::Left));
    }

    #[test]
    fn scroll_lines_sign_and_guard() {
        assert_eq!(scroll_lines(32.0, 16.0), 2.0);
        assert_eq!(scroll_lines(-8.0, 16.0), -0.5);
        assert_eq!(scroll_lines(8.0, 0.0), 0.0);
        assert_eq!(scroll_lines(f32::INFINITY, 16.0), 0.0);
    }
}
