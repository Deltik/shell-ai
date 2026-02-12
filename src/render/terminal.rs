//! Terminal renderer with synchronized output.
//!
//! Renders virtual buffers to the terminal using:
//! - Synchronized output (DEC Private Mode 2026) to prevent tearing
//! - Cell-level diffing to update only the specific cells that changed
//! - Wrap-aware rendering to preserve native terminal line wrapping
//! - Cursor positioning to skip unchanged regions efficiently

use super::{
    diff::{diff_row_cells, CellSpan},
    Color, Region, Row, Style, VirtualBuffer,
};
use crossterm::terminal;
use std::io::{self, Write};

/// Escape sequences for synchronized output (DEC Private Mode 2026).
const BSU: &str = "\x1b[?2026h"; // Begin Synchronized Update
const ESU: &str = "\x1b[?2026l"; // End Synchronized Update
const RESERVED_ROWS: u16 = 1;

/// Check if a row wraps naturally to the next row.
///
/// A row wraps when `VirtualBuffer::write_char()` advanced the cursor to the
/// next row because a character didn't fit, AND the row does not have a logical
/// line break. This replaces the previous heuristic that checked the last cell's
/// character, which failed when wrapping content happened to end with a space.
fn row_wraps(row: &Row) -> bool {
    row.wrapped && !row.line_break
}

/// Renders a virtual buffer to the terminal.
pub struct TerminalRenderer {
    /// Previous buffer for diffing.
    prev_buffer: Option<VirtualBuffer>,
    /// Terminal dimensions from last render.
    prev_size: (u16, u16),
    /// Number of rows we've painted (for cleanup).
    painted_rows: usize,
    /// Output buffer for batching writes.
    output: String,
    /// Current cursor row (relative to our render area start).
    cursor_row: usize,
    /// Current cursor column.
    cursor_col: usize,
}

impl TerminalRenderer {
    /// Create a new terminal renderer.
    pub fn new() -> Self {
        Self {
            prev_buffer: None,
            prev_size: (0, 0),
            painted_rows: 0,
            output: String::with_capacity(4096),
            cursor_row: 0,
            cursor_col: 0,
        }
    }

    /// Query current terminal size.
    pub fn term_size() -> (u16, u16) {
        terminal::size().unwrap_or((80, 24))
    }

    /// Get the number of painted rows from the last render.
    pub fn painted_rows(&self) -> usize {
        self.painted_rows
    }

    /// Render a buffer with regions to stderr.
    pub fn render(&mut self, buffer: &VirtualBuffer, regions: &[Region]) -> io::Result<()> {
        self.render_impl(buffer, regions, None)
    }

    /// Render a buffer and position the terminal cursor at a specific cell
    /// instead of parking it below content. The cursor positioning happens
    /// within the BSU/ESU synchronized update to prevent flicker.
    pub fn render_with_cursor(
        &mut self,
        buffer: &VirtualBuffer,
        regions: &[Region],
        cursor: (usize, usize),
    ) -> io::Result<()> {
        self.render_impl(buffer, regions, Some(cursor))
    }

    /// Shared render implementation. When `cursor` is `None`, parks the cursor
    /// below content (streaming mode). When `Some((row, col))`, positions the
    /// terminal cursor at that cell (interactive editing mode).
    fn render_impl(
        &mut self,
        buffer: &VirtualBuffer,
        regions: &[Region],
        cursor: Option<(usize, usize)>,
    ) -> io::Result<()> {
        let (width, height) = Self::term_size();
        let drawable_height = height.saturating_sub(RESERVED_ROWS);
        if drawable_height == 0 {
            return Ok(());
        }

        // Handle resize: clear and reset
        if self.prev_size != (width, height) {
            self.clear()?;
            self.prev_buffer = None;
            self.prev_size = (width, height);
        }

        let old_painted_rows = self.painted_rows;

        self.output.clear();
        self.output.push_str(BSU);

        // Move cursor back to start of our render area
        if self.cursor_row > 0 {
            self.output.push_str(&format!("\x1b[{}A\r", self.cursor_row));
        } else if self.cursor_col > 0 {
            // render_with_cursor() may leave the cursor mid-row on row 0;
            // emit \r so the actual terminal column matches the reset tracking.
            self.output.push('\r');
        }

        // Reset cursor tracking for this render pass
        self.cursor_row = 0;
        self.cursor_col = 0;

        let mut current_style = Style::default();

        let last_drawable_row = drawable_height.saturating_sub(1) as usize;

        for region in regions {
            self.render_region(
                buffer,
                region,
                &mut current_style,
                last_drawable_row,
            );
        }

        // Calculate where the bottom of the rendered area is
        let bottom_row = regions
            .iter()
            .map(|r| r.end_row)
            .max()
            .unwrap_or(0)
            .min(drawable_height as usize);

        // Move cursor to the bottom of the rendered area for consistent positioning.
        // Uses \r + N×\n (not CSI CUD) so the terminal scrolls at the bottom.
        if self.cursor_row < bottom_row {
            let down = bottom_row - self.cursor_row;
            self.output.push('\r');
            for _ in 0..down {
                self.output.push('\n');
            }
            self.cursor_row = bottom_row;
            self.cursor_col = 0;
        } else if self.cursor_row > bottom_row {
            let up = self.cursor_row - bottom_row;
            self.output.push_str(&format!("\r\x1b[{}A", up));
            self.cursor_row = bottom_row;
            self.cursor_col = 0;
        }

        // Clear any lines that were previously painted but are no longer part of the frame.
        self.emit_clear_excess_lines(bottom_row, old_painted_rows);

        // Park the cursor on the first blank line below the rendered area.
        if self.cursor_row != bottom_row {
            self.emit_move_to(bottom_row, 0);
        }
        self.output.push_str("\x1b[2K");

        // Reset style at end
        self.emit_reset();

        // Position terminal cursor: either at the edit point or parked below
        if let Some((target_row, target_col)) = cursor {
            self.emit_move_to(target_row, target_col);
        }

        self.output.push_str(ESU);

        // Write to stderr
        let mut stderr = io::stderr().lock();
        stderr.write_all(self.output.as_bytes())?;
        stderr.flush()?;

        // Store for next diff
        self.prev_buffer = Some(buffer.clone());
        self.painted_rows = regions
            .iter()
            .map(|r| r.end_row)
            .max()
            .unwrap_or(0)
            .min(drawable_height as usize);

        Ok(())
    }

    /// Render a region with cell-level diffing and wrap-aware rendering.
    ///
    /// When a row's wrap structure changes between frames (e.g., a single-row
    /// suggestion expands to wrap across multiple rows), a full-row repaint
    /// establishes the correct terminal line structure (natural wraps vs `\r\n`
    /// line breaks). Full repaints cascade through wrapping rows so that
    /// wrap-pending state flows naturally from one row to the next.
    ///
    /// When structure is unchanged, cell-level diffing updates only the specific
    /// cells that changed, keeping terminal I/O minimal.
    fn render_region(
        &mut self,
        buffer: &VirtualBuffer,
        region: &Region,
        current_style: &mut Style,
        last_drawable_row: usize,
    ) {
        // Pre-scan: detect erasures (prev Some → curr None) and mark the
        // entire wrapped line group for full repaint.  Clearing a single
        // physical line with \x1b[2K severs the terminal's wrap chain, so
        // every physical row in the logical line must be repainted together.
        let region_len = region.end_row.saturating_sub(region.start_row);
        let mut force_repaint = vec![false; region_len];
        if let Some(prev_buf) = &self.prev_buffer {
            for row_idx in region.start_row..region.end_row {
                let local = row_idx - region.start_row;
                if force_repaint[local] {
                    continue; // already marked by an earlier erasure in this group
                }
                if let (Some(prev), Some(curr)) = (prev_buf.row(row_idx), buffer.row(row_idx)) {
                    let has_erasure = prev
                        .cells
                        .iter()
                        .zip(curr.cells.iter())
                        .any(|(p, c)| p.ch.is_some() && c.ch.is_none() && !c.is_continuation);
                    if has_erasure {
                        // Walk backward to the first row of the wrapped group.
                        let mut group_start = row_idx;
                        while group_start > region.start_row {
                            if let Some(above) = buffer.row(group_start - 1) {
                                if row_wraps(above) {
                                    group_start -= 1;
                                } else {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                        // Mark the group start; in_full_repaint_chain cascades forward.
                        force_repaint[group_start - region.start_row] = true;
                    }
                }
            }
        }

        // Tracks whether the previous row was fully repainted and ended in
        // wrap-pending state. When true, the next row MUST also be fully
        // repainted so its first character triggers the natural wrap.
        let mut in_full_repaint_chain = false;

        for row_idx in region.start_row..region.end_row {
            let prev_row = self
                .prev_buffer
                .as_ref()
                .and_then(|b| b.row(row_idx));
            let curr_row = buffer.row(row_idx);

            let Some(curr) = curr_row else { continue };

            // Decide: full repaint or cell-level diff?
            let structure_changed = match prev_row {
                None => true, // First render — no previous structure
                Some(prev) => row_wraps(prev) != row_wraps(curr),
            };

            let erasure_repaint = force_repaint[row_idx - region.start_row];

            if structure_changed || in_full_repaint_chain || erasure_repaint {
                // Full row repaint to establish correct line structure.
                self.emit_full_row(
                    curr,
                    row_idx,
                    region,
                    current_style,
                    last_drawable_row,
                    in_full_repaint_chain,
                );

                // Continue the chain if this row wraps to the next
                in_full_repaint_chain = row_wraps(curr)
                    && row_idx + 1 < region.end_row
                    && row_idx < last_drawable_row;
            } else {
                // Cell-level diff: structure is unchanged, update only changed cells.
                in_full_repaint_chain = false;
                let spans = diff_row_cells(prev_row.unwrap(), curr);
                if spans.is_empty() {
                    continue;
                }
                for span in &spans {
                    self.emit_move_to(row_idx, span.start_col);
                    self.emit_cell_span(curr, span, current_style);
                }
            }
        }
    }

    /// Full-row repaint: clear line, write cells, establish line ending.
    ///
    /// Used for first render and when a row's wrap structure changes. Handles
    /// both natural wrapping (cursor advances to next row automatically) and
    /// explicit line breaks (`\r\n`).
    ///
    /// For non-wrapping rows, only writes up to the last non-blank cell.
    /// Writing trailing spaces would fill the terminal row to its full width,
    /// causing the terminal to treat it as wrapping for text selection purposes,
    /// even when followed by `\r\n`.
    fn emit_full_row(
        &mut self,
        curr: &Row,
        row_idx: usize,
        region: &Region,
        current_style: &mut Style,
        last_drawable_row: usize,
        from_wrap_chain: bool,
    ) {
        if !from_wrap_chain {
            // Position cursor and clear line
            self.emit_move_to(row_idx, 0);
            self.output.push_str("\x1b[2K");
        }
        // else: cursor is in wrap-pending state from previous row;
        // the first character write triggers the wrap naturally.

        // Determine how to end this row
        let wraps_to_next = row_wraps(curr)
            && row_idx + 1 < region.end_row
            && row_idx < last_drawable_row;

        // For wrapping rows, write all cells — filling the terminal row to its
        // full width triggers the natural wrap. For non-wrapping rows, stop at
        // the last non-blank cell so the terminal sees a partially-filled row
        // (the initial \x1b[2K already cleared any old content).
        let span_end = if wraps_to_next {
            curr.cells.len()
        } else {
            curr.cells
                .iter()
                .rposition(|c| c.ch.is_some() || c.is_continuation)
                .map_or(0, |idx| idx + 1)
        };

        let full_span = CellSpan {
            start_col: 0,
            end_col: span_end,
        };
        self.emit_cell_span(curr, &full_span, current_style);

        if wraps_to_next {
            // Terminal wraps naturally — next character goes to next row
            self.cursor_row = row_idx + 1;
            self.cursor_col = 0;
        } else {
            if from_wrap_chain {
                // Line wasn't pre-cleared with \x1b[2K]; clear any remaining
                // old content to the right of what we just wrote.
                self.output.push_str("\x1b[K");
            }
            if row_idx < last_drawable_row {
                self.output.push_str("\r\n");
                self.cursor_row += 1;
                self.cursor_col = 0;
            } else {
                self.output.push('\r');
                self.cursor_col = 0;
            }
        }
    }

    /// Clear all painted rows and reset state.
    ///
    /// Handles the cursor being at any position (not just the parking line).
    /// After `render_with_cursor()`, the cursor is at the edit position, so
    /// we first move to the parking line before clearing upward.
    pub fn clear(&mut self) -> io::Result<()> {
        if self.painted_rows > 0 {
            let mut stderr = io::stderr().lock();

            write!(stderr, "{}", BSU)?;

            // Move from current position to parking line (below last painted row)
            let parking_row = self.painted_rows;
            if self.cursor_row < parking_row {
                let down = parking_row - self.cursor_row;
                write!(stderr, "\r")?;
                for _ in 0..down {
                    writeln!(stderr)?;
                }
            } else if self.cursor_row > parking_row {
                let up = self.cursor_row - parking_row;
                write!(stderr, "\r\x1b[{}A", up)?;
            }

            // Now at parking line — move up and clear each painted row
            for _ in 0..self.painted_rows {
                write!(stderr, "\x1b[A\x1b[2K")?;
            }
            write!(stderr, "\x1b[2K")?; // Clear parking line
            write!(stderr, "{}", ESU)?;
            stderr.flush()?;

            self.painted_rows = 0;
        }
        self.prev_buffer = None;
        self.cursor_row = 0;
        self.cursor_col = 0;
        Ok(())
    }

    /// Position cursor at specific row and column.
    ///
    /// Downward movement uses `\r` + N×`\n` instead of CSI CUD (`\x1b[NB`)
    /// because `\n` scrolls the terminal at the bottom margin, while CUD does
    /// not.  Without scrolling, `cursor_row` drifts from the physical cursor
    /// position, progressively mis-positioning all subsequent output.
    fn emit_move_to(&mut self, target_row: usize, target_col: usize) {
        // Move to target row if needed
        if target_row > self.cursor_row {
            let down = target_row - self.cursor_row;
            self.output.push('\r');
            for _ in 0..down {
                self.output.push('\n');
            }
            self.cursor_col = 0; // Row movement resets column via \r
        } else if target_row < self.cursor_row {
            let up = self.cursor_row - target_row;
            self.output.push_str(&format!("\r\x1b[{}A", up));
            self.cursor_col = 0;
        }
        // Same row: no vertical movement, keep current cursor_col

        // Move to target column
        if target_col > self.cursor_col {
            let right = target_col - self.cursor_col;
            self.output.push_str(&format!("\x1b[{}C", right));
        } else if target_col < self.cursor_col {
            self.output.push('\r');
            if target_col > 0 {
                self.output.push_str(&format!("\x1b[{}C", target_col));
            }
        }
        // target_col == cursor_col: no horizontal movement needed

        self.cursor_row = target_row;
        self.cursor_col = target_col;
    }

    /// Emit only the cells within a span, skipping continuation cells.
    ///
    /// Empty cells (`ch: None`) advance the cursor with CUF instead of
    /// writing a space, so the position stays blank and non-selectable.
    fn emit_cell_span(&mut self, row: &Row, span: &CellSpan, current_style: &mut Style) {
        let mut col = span.start_col;
        while col < span.end_col {
            let cell = &row.cells[col];

            if cell.is_continuation {
                col += 1;
                continue;
            }

            if let Some(ch) = cell.ch {
                if cell.style != *current_style {
                    self.emit_style(&cell.style);
                    *current_style = cell.style;
                }
                self.output.push(ch);
                self.cursor_col += cell.width as usize;
                col += 1;
            } else {
                // Batch consecutive empty cells into one CUF sequence.
                let start = col;
                col += 1;
                while col < span.end_col
                    && row.cells[col].ch.is_none()
                    && !row.cells[col].is_continuation
                {
                    col += 1;
                }
                let skip = col - start;
                self.output.push_str(&format!("\x1b[{}C", skip));
                self.cursor_col += skip;
            }
        }
    }

    fn emit_clear_excess_lines(&mut self, bottom_row: usize, old_painted_rows: usize) {
        if old_painted_rows <= bottom_row {
            return;
        }

        let extra = old_painted_rows - bottom_row;

        for i in 0..extra {
            self.output.push_str("\x1b[2K");
            if i + 1 < extra {
                self.output.push_str("\r\n");
            }
        }

        self.output.push('\r');
        if extra > 1 {
            self.output.push_str(&format!("\x1b[{}A", extra - 1));
        }

        self.cursor_row = bottom_row;
        self.cursor_col = 0;
    }

    /// Emit style change sequences (SGR).
    fn emit_style(&mut self, style: &Style) {
        // Build SGR sequence
        let mut codes: Vec<u8> = Vec::new();

        // Reset first if we need to clear attributes
        codes.push(0);

        if style.bold {
            codes.push(1);
        }
        if style.dim {
            codes.push(2);
        }
        if style.italic {
            codes.push(3);
        }
        if style.underline {
            codes.push(4);
        }

        if let Some(fg) = &style.fg {
            match fg {
                Color::Red => codes.push(31),
                Color::Green => codes.push(32),
                Color::Yellow => codes.push(33),
                Color::Cyan => codes.push(36),
                Color::BrightCyan => codes.push(96),
            }
        }

        if let Some(bg) = &style.bg {
            match bg {
                Color::Red => codes.push(41),
                Color::Green => codes.push(42),
                Color::Yellow => codes.push(43),
                Color::Cyan => codes.push(46),
                Color::BrightCyan => codes.push(106),
            }
        }

        // Emit combined SGR sequence
        if !codes.is_empty() {
            self.output.push_str("\x1b[");
            for (i, code) in codes.iter().enumerate() {
                if i > 0 {
                    self.output.push(';');
                }
                self.output.push_str(&code.to_string());
            }
            self.output.push('m');
        }
    }

    /// Reset all styling (SGR 0).
    fn emit_reset(&mut self) {
        self.output.push_str("\x1b[0m");
    }
}

impl Default for TerminalRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_renderer_new() {
        let renderer = TerminalRenderer::new();
        assert!(renderer.prev_buffer.is_none());
        assert_eq!(renderer.painted_rows, 0);
        assert_eq!(renderer.cursor_row, 0);
        assert_eq!(renderer.cursor_col, 0);
    }

    #[test]
    fn test_emit_style_dim() {
        let mut renderer = TerminalRenderer::new();
        renderer.emit_style(&Style::dim());
        assert!(renderer.output.contains("\x1b["));
        assert!(renderer.output.contains("2")); // dim code
    }

    #[test]
    fn test_emit_style_color() {
        let mut renderer = TerminalRenderer::new();
        renderer.emit_style(&Style::fg(Color::Red));
        assert!(renderer.output.contains("31")); // red foreground
    }

    #[test]
    fn test_emit_reset() {
        let mut renderer = TerminalRenderer::new();
        renderer.emit_reset();
        assert_eq!(renderer.output, "\x1b[0m");
    }

    #[test]
    fn test_emit_move_to_down() {
        let mut renderer = TerminalRenderer::new();
        renderer.emit_move_to(3, 5);
        assert_eq!(renderer.cursor_row, 3);
        assert_eq!(renderer.cursor_col, 5);
        assert!(renderer.output.contains("\x1b[3B") || renderer.output.contains("\r\n"));
        assert!(renderer.output.contains("\x1b[5C"));
    }

    #[test]
    fn test_emit_move_to_up() {
        let mut renderer = TerminalRenderer::new();
        renderer.cursor_row = 5;
        renderer.cursor_col = 0;
        renderer.emit_move_to(2, 0);
        assert_eq!(renderer.cursor_row, 2);
        assert!(renderer.output.contains("\x1b[3A"));
    }

    #[test]
    fn test_emit_move_to_same_row_forward() {
        let mut renderer = TerminalRenderer::new();
        renderer.cursor_row = 3;
        renderer.cursor_col = 5;
        renderer.emit_move_to(3, 10);
        assert_eq!(renderer.cursor_row, 3);
        assert_eq!(renderer.cursor_col, 10);
        assert_eq!(renderer.output, "\x1b[5C");
    }

    #[test]
    fn test_emit_move_to_same_row_no_move() {
        let mut renderer = TerminalRenderer::new();
        renderer.cursor_row = 3;
        renderer.cursor_col = 7;
        renderer.emit_move_to(3, 7);
        assert_eq!(renderer.cursor_row, 3);
        assert_eq!(renderer.cursor_col, 7);
        assert!(renderer.output.is_empty());
    }

    #[test]
    fn test_emit_cell_span_single_char() {
        let mut renderer = TerminalRenderer::new();
        let mut row = Row::new(10);
        row.cells[3].ch = Some('X');
        let span = CellSpan { start_col: 3, end_col: 4 };
        let mut style = Style::default();

        renderer.cursor_col = 3;
        renderer.emit_cell_span(&row, &span, &mut style);

        assert_eq!(renderer.output, "X");
        assert_eq!(renderer.cursor_col, 4);
    }

    #[test]
    fn test_row_wraps() {
        let row = Row::new(5);
        assert!(!row_wraps(&row)); // default row — no wrap

        let mut row2 = Row::new(5);
        row2.wrapped = true;
        assert!(row_wraps(&row2)); // wrapped, no line_break — wraps

        let mut row3 = Row::new(5);
        row3.wrapped = true;
        row3.line_break = true;
        assert!(!row_wraps(&row3)); // wrapped but line_break — no wrap

        let mut row4 = Row::new(5);
        row4.cells[4].ch = Some('X');
        assert!(!row_wraps(&row4)); // last cell filled but not marked wrapped — no wrap
    }

    #[test]
    fn test_full_row_repaint_wrapping() {
        let mut renderer = TerminalRenderer::new();
        let mut buffer = VirtualBuffer::new(5, 2);
        // Fill row 0 completely: "abcde" — this triggers wrapping
        buffer.write_str("abcde");
        // Row 1 starts with "fg"
        buffer.write_str("fg");

        let mut style = Style::default();
        let region = Region::new(0, 2);

        // Full repaint of wrapping row 0
        renderer.emit_full_row(
            buffer.row(0).unwrap(),
            0,
            &region,
            &mut style,
            1,
            false,
        );

        // After wrapping row: cursor should be at (1, 0)
        assert_eq!(renderer.cursor_row, 1);
        assert_eq!(renderer.cursor_col, 0);
        // Output should contain all chars of row 0
        assert!(renderer.output.contains("abcde"));
        // Should NOT contain \r\n (natural wrap, not line break)
        assert!(!renderer.output.contains("\r\n"));
    }

    #[test]
    fn test_emit_clear_excess_lines_shrinks_frame() {
        let mut renderer = TerminalRenderer::new();
        renderer.cursor_row = 1;
        renderer.cursor_col = 0;

        renderer.emit_clear_excess_lines(1, 3);

        assert!(
            renderer
                .output
                .contains("\x1b[2K\r\n\x1b[2K"),
            "should clear trailing lines using \\n for scrolling at bottom margin"
        );
        assert!(renderer.output.contains("\x1b[1A"));
        assert_eq!(renderer.cursor_row, 1);
        assert_eq!(renderer.cursor_col, 0);
    }

    #[test]
    fn test_emit_cell_span_empty_cells_use_cuf() {
        // Empty cells (ch: None) should emit CUF (cursor forward) instead of
        // a space character, so the position stays blank and non-selectable.
        let mut renderer = TerminalRenderer::new();
        let row = Row::new(10); // all cells empty (None)
        let span = CellSpan {
            start_col: 3,
            end_col: 6,
        };
        let mut style = Style::default();

        renderer.cursor_col = 3;
        renderer.emit_cell_span(&row, &span, &mut style);

        // Should emit CUF 3, not three spaces
        assert_eq!(renderer.output, "\x1b[3C");
        assert_eq!(renderer.cursor_col, 6);
    }

    #[test]
    fn test_emit_cell_span_mixed_chars_and_empty() {
        // A span with characters and empty cells should write chars and
        // use CUF to skip empty cells.
        let mut renderer = TerminalRenderer::new();
        let mut row = Row::new(10);
        row.cells[0].ch = Some('A');
        // cells[1] and cells[2] stay None
        row.cells[3].ch = Some('B');
        let span = CellSpan {
            start_col: 0,
            end_col: 4,
        };
        let mut style = Style::default();

        renderer.cursor_col = 0;
        renderer.emit_cell_span(&row, &span, &mut style);

        assert_eq!(renderer.output, "A\x1b[2CB");
        assert_eq!(renderer.cursor_col, 4);
    }
}