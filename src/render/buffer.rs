//! Virtual buffer implementation.
//!
//! A 2D grid of styled cells representing what should be on screen.
//! Handles character writing, wrapping, and wide character support.

use super::{Cell, Row, Style};

/// A 2D buffer of styled cells representing what should be on screen.
#[derive(Debug, Clone)]
pub struct VirtualBuffer {
    /// Rows of cells.
    rows: Vec<Row>,
    /// Buffer width in columns.
    width: u16,
    /// Buffer height in rows.
    height: u16,
    /// Current write position (column).
    cursor_col: usize,
    /// Current write position (row).
    cursor_row: usize,
    /// Current style for new characters.
    current_style: Style,
}

impl VirtualBuffer {
    /// Create a new buffer filled with spaces.
    pub fn new(width: u16, height: u16) -> Self {
        let rows = (0..height).map(|_| Row::new(width as usize)).collect();
        Self {
            rows,
            width,
            height,
            cursor_col: 0,
            cursor_row: 0,
            current_style: Style::default(),
        }
    }

    /// Get buffer width.
    #[cfg(test)]
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Get current cursor row.
    pub fn cursor_row(&self) -> usize {
        self.cursor_row
    }

    /// Get current cursor column.
    pub fn cursor_col(&self) -> usize {
        self.cursor_col
    }

    /// Set the current style for subsequent writes.
    pub fn set_style(&mut self, style: Style) {
        self.current_style = style;
    }

    /// Reset style to default.
    pub fn reset_style(&mut self) {
        self.current_style = Style::default();
    }

    /// Move cursor to specific position.
    /// Clamps to valid range. No-op if buffer has zero dimensions.
    pub fn move_to(&mut self, row: usize, col: usize) {
        if self.width == 0 || self.height == 0 {
            return;
        }
        self.cursor_row = row.min(self.height as usize - 1);
        self.cursor_col = col.min(self.width as usize - 1);
    }

    /// Move cursor to start of next row, marking the current row as ending
    /// with a logical line break.
    /// If at bottom, cursor stays at last row.
    pub fn newline(&mut self) {
        if let Some(row) = self.rows.get_mut(self.cursor_row) {
            row.line_break = true;
        }
        self.cursor_col = 0;
        if self.cursor_row < self.height as usize - 1 {
            self.cursor_row += 1;
        }
    }

    /// Advance cursor to next column.
    /// Does NOT auto-wrap - wrapping is handled by write_char before writing.
    /// The cursor may end up at column == width, which is fine - the next
    /// write_char will wrap if needed, or newline() will move to the next row.
    fn advance_cursor(&mut self, width: usize) {
        self.cursor_col += width;
    }

    /// Write a character at cursor position, advance cursor.
    /// Handles wide characters (CJK, emoji) by using continuation cells.
    /// Wraps by advancing to next row (no actual newline emitted).
    pub fn write_char(&mut self, ch: char) {
        if self.cursor_row >= self.height as usize {
            return;
        }

        let char_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);

        // Handle zero-width characters (combining marks, etc.)
        if char_width == 0 {
            // Don't advance cursor for zero-width chars
            // In a full implementation, we might combine with previous char
            return;
        }

        // Check if character fits on current row
        if self.cursor_col + char_width > self.width as usize {
            // Mark current row as wrapping to the next
            if let Some(row) = self.rows.get_mut(self.cursor_row) {
                row.wrapped = true;
            }
            // Wrap to next row
            self.cursor_col = 0;
            if self.cursor_row < self.height as usize - 1 {
                self.cursor_row += 1;
            } else {
                return; // At bottom, can't write
            }
        }

        // Write the main cell
        if let Some(row) = self.rows.get_mut(self.cursor_row) {
            if let Some(cell) = row.cells.get_mut(self.cursor_col) {
                *cell = Cell {
                    ch: Some(ch),
                    width: char_width as u8,
                    style: self.current_style,
                    is_continuation: false,
                };
            }

            // For wide characters, mark next cell as continuation
            if char_width == 2 && self.cursor_col + 1 < self.width as usize {
                if let Some(next_cell) = row.cells.get_mut(self.cursor_col + 1) {
                    *next_cell = Cell::continuation();
                    next_cell.style = self.current_style;
                }
            }
        }

        self.advance_cursor(char_width);
    }

    /// Write a string at cursor position.
    /// Handles wrapping across rows without inserting newline characters.
    pub fn write_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.write_char(ch);
        }
    }

    /// Get a row by index.
    pub fn row(&self, idx: usize) -> Option<&Row> {
        self.rows.get(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(buf: &VirtualBuffer, row: usize, col: usize) -> &Cell {
        &buf.row(row).unwrap().cells[col]
    }

    #[test]
    fn test_buffer_new() {
        let buf = VirtualBuffer::new(80, 24);
        assert_eq!(buf.width(), 80);
        assert!(buf.row(23).is_some());
        assert!(buf.row(24).is_none());
        assert_eq!(buf.cursor_row(), 0);
        assert_eq!(buf.cursor_col(), 0);
    }

    #[test]
    fn test_buffer_write_char() {
        let mut buf = VirtualBuffer::new(10, 3);
        buf.write_char('H');
        buf.write_char('i');

        assert_eq!(cell(&buf, 0, 0).ch, Some('H'));
        assert_eq!(cell(&buf, 0, 1).ch, Some('i'));
        assert_eq!(buf.cursor_col(), 2);
    }

    #[test]
    fn test_buffer_write_str() {
        let mut buf = VirtualBuffer::new(10, 3);
        buf.write_str("Hello");

        assert_eq!(cell(&buf, 0, 0).ch, Some('H'));
        assert_eq!(cell(&buf, 0, 4).ch, Some('o'));
        assert_eq!(buf.cursor_col(), 5);
    }

    #[test]
    fn test_buffer_write_wraps_without_newline() {
        let mut buf = VirtualBuffer::new(5, 3);
        buf.write_str("Hello World");

        // Row 0: "Hello"
        assert_eq!(cell(&buf, 0, 0).ch, Some('H'));
        assert_eq!(cell(&buf, 0, 4).ch, Some('o'));

        // Row 1: " Worl" (continuation, no \n)
        assert_eq!(cell(&buf, 1, 0).ch, Some(' '));
        assert_eq!(cell(&buf, 1, 4).ch, Some('l'));

        // Row 2: "d    " (continuation)
        assert_eq!(cell(&buf, 2, 0).ch, Some('d'));

        // Rows that wrapped should be marked
        assert!(buf.row(0).unwrap().wrapped);
        assert!(buf.row(1).unwrap().wrapped);
        assert!(!buf.row(2).unwrap().wrapped); // last row didn't wrap
    }

    #[test]
    fn test_buffer_explicit_newline_advances_row() {
        let mut buf = VirtualBuffer::new(10, 3);
        buf.write_str("line1");
        buf.newline();
        buf.write_str("line2");

        assert_eq!(cell(&buf, 0, 0).ch, Some('l'));
        assert_eq!(cell(&buf, 0, 4).ch, Some('1'));
        assert_eq!(cell(&buf, 1, 0).ch, Some('l'));
        assert_eq!(cell(&buf, 1, 4).ch, Some('2'));

        // newline() should mark the row with a line break
        assert!(buf.row(0).unwrap().line_break);
        assert!(!buf.row(1).unwrap().line_break);
    }

    #[test]
    fn test_buffer_wide_char() {
        let mut buf = VirtualBuffer::new(10, 1);
        buf.write_str("日本語");

        // Each CJK char takes 2 cells
        assert_eq!(cell(&buf, 0, 0).ch, Some('日'));
        assert_eq!(cell(&buf, 0, 0).width, 2);
        assert!(cell(&buf, 0, 1).is_continuation);

        assert_eq!(cell(&buf, 0, 2).ch, Some('本'));
        assert!(cell(&buf, 0, 3).is_continuation);

        assert_eq!(cell(&buf, 0, 4).ch, Some('語'));
        assert!(cell(&buf, 0, 5).is_continuation);

        assert_eq!(buf.cursor_col(), 6);
    }

    #[test]
    fn test_buffer_wide_char_wrap() {
        // Wide char at end of row that doesn't fit should wrap
        let mut buf = VirtualBuffer::new(5, 2);
        buf.write_str("xxxx"); // 4 chars, col 4
        buf.write_char('日'); // 2-wide char doesn't fit, should wrap

        // Row 0 should have "xxxx" + empty cell (wide char didn't fit)
        assert_eq!(cell(&buf, 0, 4).ch, None);

        // Row 1 should start with '日'
        assert_eq!(cell(&buf, 1, 0).ch, Some('日'));
        assert!(cell(&buf, 1, 1).is_continuation);
    }

    #[test]
    fn test_buffer_move_to() {
        let mut buf = VirtualBuffer::new(10, 5);
        buf.move_to(2, 5);
        assert_eq!(buf.cursor_row(), 2);
        assert_eq!(buf.cursor_col(), 5);

        buf.write_char('X');
        assert_eq!(cell(&buf, 2, 5).ch, Some('X'));
    }

    #[test]
    fn test_buffer_style() {
        let mut buf = VirtualBuffer::new(10, 1);
        buf.set_style(Style::dim());
        buf.write_char('A');

        assert!(cell(&buf, 0, 0).style.dim);
    }

}
