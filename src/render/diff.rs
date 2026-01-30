//! Buffer diffing for efficient updates.
//!
//! Compares rows cell-by-cell to find what changed, enabling minimal repaints.

use super::Row;

/// A contiguous span of changed cells within a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellSpan {
    /// First changed column (inclusive).
    pub start_col: usize,
    /// Last changed column (exclusive).
    pub end_col: usize,
}

/// Compare two rows cell-by-cell, returning spans of changed cells.
/// Returns empty vec if rows are identical.
pub fn diff_row_cells(prev: &Row, curr: &Row) -> Vec<CellSpan> {
    if prev.visual_eq(curr) {
        return Vec::new();
    }

    let len = prev.cells.len().min(curr.cells.len());
    let mut spans = Vec::new();
    let mut span_start: Option<usize> = None;

    for col in 0..len {
        let changed = prev.cells[col] != curr.cells[col];
        if changed {
            if span_start.is_none() {
                span_start = Some(col);
            }
        } else if let Some(start) = span_start {
            spans.push(CellSpan {
                start_col: start,
                end_col: col,
            });
            span_start = None;
        }
    }

    // Close final span if it extends to the end
    if let Some(start) = span_start {
        spans.push(CellSpan {
            start_col: start,
            end_col: len,
        });
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_row_cells_unchanged() {
        let row1 = Row::new(10);
        let row2 = Row::new(10);
        let spans = diff_row_cells(&row1, &row2);
        assert!(spans.is_empty());
    }

    #[test]
    fn test_diff_row_cells_single_change() {
        let row1 = Row::new(10);
        let mut row2 = Row::new(10);
        row2.cells[5].ch = Some('X');
        let spans = diff_row_cells(&row1, &row2);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0], CellSpan { start_col: 5, end_col: 6 });
    }

    #[test]
    fn test_diff_row_cells_two_spans() {
        let row1 = Row::new(10);
        let mut row2 = Row::new(10);
        row2.cells[0].ch = Some('A');
        row2.cells[9].ch = Some('Z');
        let spans = diff_row_cells(&row1, &row2);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0], CellSpan { start_col: 0, end_col: 1 });
        assert_eq!(spans[1], CellSpan { start_col: 9, end_col: 10 });
    }

    #[test]
    fn test_diff_row_cells_contiguous_change() {
        let row1 = Row::new(10);
        let mut row2 = Row::new(10);
        row2.cells[2].ch = Some('A');
        row2.cells[3].ch = Some('B');
        row2.cells[4].ch = Some('C');
        let spans = diff_row_cells(&row1, &row2);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0], CellSpan { start_col: 2, end_col: 5 });
    }

    #[test]
    fn test_diff_row_cells_erased() {
        // A cell that had content and is now empty should be detected as changed.
        let mut row1 = Row::new(10);
        row1.cells[3].ch = Some('X');
        let row2 = Row::new(10); // all cells empty (None)
        let spans = diff_row_cells(&row1, &row2);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0], CellSpan { start_col: 3, end_col: 4 });
    }

    #[test]
    fn test_diff_row_cells_empty_vs_space() {
        // An empty cell (None) becoming a space (Some(' ')) is a real change.
        let row1 = Row::new(10); // all None
        let mut row2 = Row::new(10);
        row2.cells[5].ch = Some(' ');
        let spans = diff_row_cells(&row1, &row2);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0], CellSpan { start_col: 5, end_col: 6 });
    }
}