//! Rendering engine for terminal UI.
//!
//! This module provides a multi-buffer architecture for smooth terminal rendering:
//! - `Style`, `Color`, `Cell`, `Row` - Core data types for styled text
//! - `VirtualBuffer` - 2D grid of styled cells
//! - `Region` - Row ranges to render
//! - `TerminalRenderer` - Diff-based rendering with synchronized output (DEC 2026)
//!
//! Key design principles:
//! - Row-level diffing skips unchanged rows and only repaints modified ones
//! - Synchronized output (BSU/ESU) prevents tearing during updates
//! - Diff-based updates minimize terminal I/O

mod buffer;
mod diff;
mod region;
mod terminal;

pub use buffer::VirtualBuffer;
pub use diff::diff_row_cells;
pub use region::Region;
pub use terminal::TerminalRenderer;

/// Text styling attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    /// Foreground color.
    pub fg: Option<Color>,
    /// Background color.
    pub bg: Option<Color>,
    /// Bold text.
    pub bold: bool,
    /// Dimmed text.
    pub dim: bool,
    /// Italic text.
    pub italic: bool,
    /// Underlined text.
    pub underline: bool,
}

impl Style {
    /// Create a dimmed style.
    pub fn dim() -> Self {
        Self {
            dim: true,
            ..Default::default()
        }
    }

    /// Create a style with foreground color.
    pub fn fg(color: Color) -> Self {
        Self {
            fg: Some(color),
            ..Default::default()
        }
    }
}

/// Terminal colors (only colors actually used in the codebase).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Red,
    Green,
    Yellow,
    Cyan,
    BrightCyan,
}

/// A single cell in the virtual buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The character to display, or `None` for an empty cell.
    pub ch: Option<char>,
    /// Display width of this character (0, 1, or 2).
    pub width: u8,
    /// Style for this cell.
    pub style: Style,
    /// If true, this cell is a continuation of a wide character in the previous cell.
    /// When rendering, skip this cell (the previous cell's char covers it).
    pub is_continuation: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: None,
            width: 1,
            style: Style::default(),
            is_continuation: false,
        }
    }
}

impl Cell {
    /// Create a continuation cell (for wide characters).
    pub fn continuation() -> Self {
        Self {
            ch: None,
            width: 0,
            style: Style::default(),
            is_continuation: true,
        }
    }
}

/// A single row in the virtual buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Cells in this row. Length always equals buffer width.
    pub cells: Vec<Cell>,
    /// Whether this row ends with a logical line break.
    /// Set by `VirtualBuffer::newline()`. When true, the renderer emits a line
    /// break after this row instead of letting the terminal wrap naturally.
    pub line_break: bool,
    /// Whether content on this row wraps to the next row.
    /// Set by `VirtualBuffer::write_char()` when a character doesn't fit and
    /// the cursor advances to the next row. The renderer uses this (combined
    /// with `line_break`) to decide between natural terminal wrapping and
    /// explicit `\r\n`.
    pub wrapped: bool,
}

impl Row {
    /// Create a new row filled with default (space) cells.
    pub fn new(width: usize) -> Self {
        Self {
            cells: vec![Cell::default(); width],
            line_break: false,
            wrapped: false,
        }
    }

    /// Check if two rows are visually identical (cell content only).
    pub fn visual_eq(&self, other: &Row) -> bool {
        self.cells == other.cells
    }

    /// Check if the row is empty (all default cells, no structural flags).
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        !self.line_break
            && !self.wrapped
            && self
                .cells
                .iter()
                .all(|c| c.ch.is_none() && c.style == Style::default() && !c.is_continuation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_style_default() {
        let style = Style::default();
        assert_eq!(style.fg, None);
        assert_eq!(style.bg, None);
        assert!(!style.bold);
        assert!(!style.dim);
    }

    #[test]
    fn test_style_builders() {
        let style = Style::dim();
        assert!(style.dim);

        let style = Style::fg(Color::Red);
        assert_eq!(style.fg, Some(Color::Red));
    }

    #[test]
    fn test_cell_default() {
        let cell = Cell::default();
        assert_eq!(cell.ch, None);
        assert_eq!(cell.width, 1);
        assert!(!cell.is_continuation);
    }

    #[test]
    fn test_cell_continuation() {
        let cell = Cell::continuation();
        assert!(cell.is_continuation);
        assert_eq!(cell.width, 0);
    }

    #[test]
    fn test_row_new() {
        let row = Row::new(10);
        assert_eq!(row.cells.len(), 10);
        assert!(row.is_empty());
    }

    #[test]
    fn test_row_visual_eq() {
        let row1 = Row::new(5);
        let row2 = Row::new(5);
        assert!(row1.visual_eq(&row2));
    }
}
