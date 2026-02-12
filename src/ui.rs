//! Terminal UI widgets for shell-ai.
//!
//! Provides interactive prompts with both arrow key navigation and
//! number/letter shortcuts (similar to Claude Code's interface).

use crate::render::{Color, Region, Style, TerminalRenderer, VirtualBuffer};
use colored::Colorize;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{self, ClearType},
};
use std::io::{self, Write};
use unicode_width::UnicodeWidthStr;

/// RAII guard that enables terminal raw mode on creation and disables it on drop.
/// Ensures raw mode is always restored, even on panic or early return.
struct RawModeGuard;

impl RawModeGuard {
    fn new() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

/// An option in an interactive select menu.
#[derive(Clone)]
pub struct SelectOption {
    /// The key to press for this option ('1', '2', 'g', 'n', etc.)
    pub key: char,
    /// The display label for this option
    pub label: String,
}

impl SelectOption {
    pub fn new(key: char, label: impl Into<String>) -> Self {
        Self {
            key,
            label: label.into(),
        }
    }
}

/// Help line segments: (text, is_key). Keys are styled cyan, others dimmed.
const HELP_SEGMENTS: &[(&str, bool)] = &[
    ("↑↓", true),
    ("/", false),
    ("jk", true),
    (" navigate ", false),
    ("•", false),
    (" ", false),
    ("key", true),
    ("/", false),
    ("Enter", true),
    (" select ", false),
    ("•", false),
    (" ", false),
    ("Esc", true),
    (" quit", false),
];


/// Write the help line with styling to a writer.
fn write_help_line(w: &mut impl Write) -> io::Result<()> {
    for (text, is_key) in HELP_SEGMENTS {
        if *is_key {
            write!(w, "{}", text.cyan())?;
        } else {
            write!(w, "{}", text.dimmed())?;
        }
    }
    Ok(())
}

/// Interactive select menu with arrow navigation and keyboard shortcuts.
///
/// Supports:
/// - Arrow up/down: Move highlight between options
/// - Number/letter keys: Jump directly to and select that option
/// - Enter: Confirm currently highlighted option
/// - Escape/Ctrl+C: Cancel
pub struct InteractiveSelect {
    prompt: String,
    options: Vec<SelectOption>,
    selected: usize,
}

impl InteractiveSelect {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            options: Vec::new(),
            selected: 0,
        }
    }

    /// Add an option with a key and label.
    pub fn option(mut self, key: char, label: impl Into<String>) -> Self {
        self.options.push(SelectOption::new(key, label));
        self
    }

    /// Run the interactive selection and return the index of the selected option.
    ///
    /// Returns `None` if the user cancelled (Escape/Ctrl+C/q).
    pub fn run(&mut self) -> io::Result<Option<usize>> {
        let result = {
            let _guard = RawModeGuard::new()?;
            self.run_inner()
        };

        // Clear the menu after selection
        execute!(io::stderr(), cursor::MoveToColumn(0))?;

        result
    }

    fn run_inner(&mut self) -> io::Result<Option<usize>> {
        let mut stderr = io::stderr();
        let mut first_render = true;

        loop {
            // Clear and redraw
            self.render(&mut stderr, first_render)?;
            first_render = false;

            // Wait for key event
            if let Event::Key(key_event) = event::read()? {
                match self.handle_key(key_event) {
                    KeyAction::Select(idx) => {
                        // Check if the selected option is a quit option (preserves display)
                        if self.options.get(idx).map(|o| o.key) == Some('q') {
                            write!(stderr, "\r\n")?;
                            stderr.flush()?;
                            return Ok(Some(idx));
                        }
                        // Clear the menu before returning
                        self.clear_menu(&mut stderr)?;
                        return Ok(Some(idx));
                    }
                    KeyAction::Cancel => {
                        write!(stderr, "\r\n")?;
                        stderr.flush()?;
                        return Ok(None);
                    }
                    KeyAction::MoveUp => {
                        if self.selected > 0 {
                            self.selected -= 1;
                        } else {
                            self.selected = self.options.len().saturating_sub(1);
                        }
                    }
                    KeyAction::MoveDown => {
                        if self.selected < self.options.len().saturating_sub(1) {
                            self.selected += 1;
                        } else {
                            self.selected = 0;
                        }
                    }
                    KeyAction::None => {}
                }
            }
        }
    }

    fn handle_key(&self, key: KeyEvent) -> KeyAction {
        // Handle Ctrl+C
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return KeyAction::Cancel;
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => KeyAction::MoveUp,
            KeyCode::Down | KeyCode::Char('j') => KeyAction::MoveDown,
            KeyCode::Enter => {
                if self.selected < self.options.len() {
                    KeyAction::Select(self.selected)
                } else {
                    KeyAction::None
                }
            }
            KeyCode::Esc => KeyAction::Cancel,
            KeyCode::Char(c) => {
                // '?' is used as a display label for the 10th+ suggestion but
                // is not an actionable shortcut — users navigate with arrows.
                if c == '?' {
                    return KeyAction::None;
                }
                // Check if this character matches any option key, return its index
                if let Some(idx) = self.options.iter().position(|o| o.key == c) {
                    KeyAction::Select(idx)
                } else {
                    KeyAction::None
                }
            }
            _ => KeyAction::None,
        }
    }

    fn render(&self, w: &mut impl Write, first_render: bool) -> io::Result<()> {
        // Move cursor back to start of menu if not first render
        if !first_render {
            let lines = self.calculate_total_lines();
            execute!(w, cursor::MoveUp(lines as u16))?;
        }

        // Move to column 0 and clear from cursor down
        execute!(w, cursor::MoveToColumn(0), terminal::Clear(ClearType::FromCursorDown))?;

        // Print prompt
        write!(w, "{}\r\n", self.prompt.white().bold())?;

        // Print options
        for (i, opt) in self.options.iter().enumerate() {
            let is_selected = i == self.selected;

            let key_display = format!("{}", opt.key);
            let key_styled = if is_selected {
                format!("[{}]", key_display).cyan().bold().to_string()
            } else {
                format!(" {} ", key_display).cyan().to_string()
            };

            let label_for_display = opt.label.replace('\n', "\r\n");
            let label_styled = if is_selected {
                label_for_display.bold().to_string()
            } else {
                label_for_display
            };

            write!(w, "  {} {}\r\n", key_styled, label_styled)?;
        }

        // Print help line
        write!(w, "\r\n")?;
        write_help_line(w)?;
        write!(w, "\r\n")?;

        w.flush()?;
        Ok(())
    }

    /// Calculate the total number of terminal lines the menu will occupy,
    /// accounting for line wrapping and embedded newlines.
    fn calculate_total_lines(&self) -> usize {
        let term_width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

        let mut total_lines = 0;

        // Prompt line
        total_lines += Self::lines_needed(&self.prompt, term_width);

        // Option lines (first line has "  [X] " prefix = 6 chars, continuation lines don't)
        for opt in &self.options {
            total_lines += Self::lines_needed_with_prefix(&opt.label, term_width, 6);
        }

        // Blank line + help line
        let help_text: String = HELP_SEGMENTS.iter().map(|(s, _)| *s).collect();
        total_lines += 1; // blank line
        total_lines += Self::lines_needed(&help_text, term_width);

        total_lines
    }

    /// Calculate how many terminal lines a string will occupy,
    /// accounting for embedded newlines and line wrapping.
    fn lines_needed(s: &str, term_width: usize) -> usize {
        if s.is_empty() || term_width == 0 {
            return 1;
        }
        s.split('\n')
            .map(|line| {
                if line.is_empty() {
                    1
                } else {
                    line.width().div_ceil(term_width)
                }
            })
            .sum()
    }

    /// Calculate lines needed for a string with a prefix on the first line only.
    fn lines_needed_with_prefix(s: &str, term_width: usize, prefix_len: usize) -> usize {
        if term_width == 0 {
            return 1;
        }
        let mut lines: Vec<&str> = s.split('\n').collect();
        if lines.is_empty() {
            return 1;
        }

        let mut total = 0;

        // First line includes prefix
        let first_line = lines.remove(0);
        let first_width = prefix_len + first_line.width();
        total += if first_width == 0 { 1 } else { first_width.div_ceil(term_width) };

        // Remaining lines have no prefix
        for line in lines {
            let width = line.width();
            total += if width == 0 { 1 } else { width.div_ceil(term_width) };
        }

        total
    }

    fn clear_menu(&self, w: &mut impl Write) -> io::Result<()> {
        let lines_to_clear = self.calculate_total_lines();
        execute!(
            w,
            cursor::MoveUp(lines_to_clear as u16),
            terminal::Clear(ClearType::FromCursorDown)
        )?;
        Ok(())
    }
}

enum KeyAction {
    Select(usize),
    Cancel,
    MoveUp,
    MoveDown,
    None,
}

/// Simple text input prompt with readline-style shortcuts.
///
/// Supports:
/// - Basic text editing (backspace, delete, typing)
/// - Arrow keys for cursor movement
/// - Home/End or Ctrl+A/Ctrl+E for line start/end
/// - Ctrl+U to kill to beginning, Ctrl+K to kill to end
/// - Ctrl+W or Alt+Backspace to delete word backward
/// - Ctrl+Left/Right or Alt+B/Alt+F for word movement
/// - Enter to confirm, Escape/Ctrl+C to cancel
pub struct TextInput {
    prompt: String,
    initial_value: String,
}

impl TextInput {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            initial_value: String::new(),
        }
    }

    /// Set an initial value for the input.
    pub fn with_initial_value(mut self, value: impl Into<String>) -> Self {
        self.initial_value = value.into();
        self
    }

    /// Run the text input and return the entered text.
    ///
    /// Returns `None` if the user cancelled (Escape/Ctrl+C).
    pub fn run(&self) -> io::Result<Option<String>> {
        let _guard = RawModeGuard::new()?;
        self.run_inner()
    }

    /// Convert a char index to a byte offset in the string.
    fn char_to_byte(s: &str, char_idx: usize) -> usize {
        s.char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(s.len())
    }

    fn run_inner(&self) -> io::Result<Option<String>> {
        let mut stderr = io::stderr();
        let mut renderer = TerminalRenderer::new();
        let mut input = self.initial_value.clone();
        let mut cursor_pos = input.chars().count(); // char index, not byte offset

        loop {
            let (width, height) = TerminalRenderer::term_size();
            let drawable_height = height.saturating_sub(1); // Reserve 1 for parking line
            if width == 0 || drawable_height == 0 {
                if let Event::Key(_) = event::read()? {}
                continue;
            }

            // Try multi-line render — use one extra row so overflow is detectable
            // (VirtualBuffer clips at its height, so without the extra row
            // content_rows would always equal drawable_height and scroll mode
            // could never trigger)
            let buf_height = drawable_height.saturating_add(1);
            let (buffer, content_rows, cursor_row, cursor_col) =
                self.build_multiline_buffer(&input, cursor_pos, width, buf_height);

            let cursor_visual_row = if content_rows <= drawable_height as usize {
                // Multi-line mode: content fits
                let regions = vec![Region::new(0, content_rows)];
                renderer.render_with_cursor(&buffer, &regions, (cursor_row, cursor_col))?;
                cursor_row
            } else {
                // Horizontal scroll fallback
                let (buffer, regions, col) =
                    self.build_scroll_buffer(&input, cursor_pos, width);
                renderer.render_with_cursor(&buffer, &regions, (0, col))?;
                0
            };

            // Wait for key event
            if let Event::Key(key_event) = event::read()? {
                let ctrl = key_event.modifiers.contains(KeyModifiers::CONTROL);
                let alt = key_event.modifiers.contains(KeyModifiers::ALT);

                match (key_event.code, ctrl, alt) {
                    // Cancel
                    (KeyCode::Char('c'), true, _) | (KeyCode::Esc, _, _) => {
                        renderer.clear()?;
                        return Ok(None);
                    }
                    // Confirm
                    (KeyCode::Enter, _, _) => {
                        // Move cursor from edit position to below content
                        let parking_row = renderer.painted_rows();
                        let down = parking_row.saturating_sub(cursor_visual_row);
                        if down > 0 {
                            write!(stderr, "\r")?;
                            for _ in 0..down {
                                writeln!(stderr)?;
                            }
                        }
                        write!(stderr, "\r\n")?;
                        stderr.flush()?;
                        return Ok(Some(input));
                    }
                    // Beginning of line: Ctrl+A or Home
                    (KeyCode::Char('a'), true, _) | (KeyCode::Home, _, _) => {
                        cursor_pos = 0;
                    }
                    // End of line: Ctrl+E or End
                    (KeyCode::Char('e'), true, _) | (KeyCode::End, _, _) => {
                        cursor_pos = input.chars().count();
                    }
                    // Kill to beginning: Ctrl+U
                    (KeyCode::Char('u'), true, _) => {
                        let byte_off = Self::char_to_byte(&input, cursor_pos);
                        input.drain(..byte_off);
                        cursor_pos = 0;
                    }
                    // Kill to end: Ctrl+K
                    (KeyCode::Char('k'), true, _) => {
                        let byte_off = Self::char_to_byte(&input, cursor_pos);
                        input.truncate(byte_off);
                    }
                    // Delete word backward: Ctrl+W or Alt+Backspace
                    (KeyCode::Char('w'), true, _) | (KeyCode::Backspace, _, true) => {
                        let new_pos = find_word_boundary_backward(&input, cursor_pos);
                        let start = Self::char_to_byte(&input, new_pos);
                        let end = Self::char_to_byte(&input, cursor_pos);
                        input.drain(start..end);
                        cursor_pos = new_pos;
                    }
                    // Delete word forward: Alt+D
                    (KeyCode::Char('d'), _, true) => {
                        let end_pos = find_word_boundary_forward(&input, cursor_pos);
                        let start = Self::char_to_byte(&input, cursor_pos);
                        let end = Self::char_to_byte(&input, end_pos);
                        input.drain(start..end);
                    }
                    // Move word backward: Ctrl+Left or Alt+B
                    (KeyCode::Left, true, _) | (KeyCode::Char('b'), _, true) => {
                        cursor_pos = find_word_boundary_backward(&input, cursor_pos);
                    }
                    // Move word forward: Ctrl+Right or Alt+F
                    (KeyCode::Right, true, _) | (KeyCode::Char('f'), _, true) => {
                        cursor_pos = find_word_boundary_forward(&input, cursor_pos);
                    }
                    // Simple backspace
                    (KeyCode::Backspace, _, _) => {
                        if cursor_pos > 0 {
                            let byte_off = Self::char_to_byte(&input, cursor_pos - 1);
                            input.remove(byte_off);
                            cursor_pos -= 1;
                        }
                    }
                    // Delete
                    (KeyCode::Delete, _, _) | (KeyCode::Char('d'), true, _) => {
                        if cursor_pos < input.chars().count() {
                            let byte_off = Self::char_to_byte(&input, cursor_pos);
                            input.remove(byte_off);
                        }
                    }
                    // Move left
                    (KeyCode::Left, _, _) | (KeyCode::Char('b'), true, _) => {
                        cursor_pos = cursor_pos.saturating_sub(1);
                    }
                    // Move right
                    (KeyCode::Right, _, _) | (KeyCode::Char('f'), true, _) => {
                        if cursor_pos < input.chars().count() {
                            cursor_pos += 1;
                        }
                    }
                    // Regular character input
                    (KeyCode::Char(c), false, false) => {
                        let byte_off = Self::char_to_byte(&input, cursor_pos);
                        input.insert(byte_off, c);
                        cursor_pos += 1;
                    }
                    _ => {}
                }
            }
        }
    }

    /// Build a VirtualBuffer with the prompt and input wrapping across multiple
    /// lines, and capture the visual cursor position within the buffer.
    fn build_multiline_buffer(
        &self,
        input: &str,
        cursor_pos: usize,
        width: u16,
        height: u16,
    ) -> (VirtualBuffer, usize, usize, usize) {
        // Returns (buffer, content_rows, cursor_row, cursor_col)

        let mut buf = VirtualBuffer::new(width, height);

        // Write prompt in cyan bold
        buf.set_style(Style {
            fg: Some(Color::Cyan),
            bold: true,
            ..Default::default()
        });
        buf.write_str(&self.prompt);
        buf.reset_style();
        buf.write_char(' ');

        // Write input up to cursor, capture visual position
        let byte_offset = Self::char_to_byte(input, cursor_pos);
        buf.write_str(&input[..byte_offset]);

        // Capture cursor visual position
        // Edge case: cursor at exact end of a full row should be on the next line
        let (cursor_row, cursor_col) = if cursor_pos > 0
            && buf.cursor_col() >= width as usize
        {
            (buf.cursor_row() + 1, 0)
        } else {
            (buf.cursor_row(), buf.cursor_col())
        };

        // Write rest of input
        buf.write_str(&input[byte_offset..]);

        // Content rows = last row with content + 1
        // Also include cursor_row in case cursor is on an empty line below content
        let last_content_row = if buf.cursor_col() > 0 || buf.cursor_row() > 0 {
            buf.cursor_row() + 1
        } else {
            1
        };
        let content_rows = last_content_row.max(cursor_row + 1);

        (buf, content_rows, cursor_row, cursor_col)
    }

    /// Build a single-row VirtualBuffer with horizontal scrolling when
    /// multi-line content would exceed the terminal height.
    fn build_scroll_buffer(
        &self,
        input: &str,
        cursor_pos: usize,
        width: u16,
    ) -> (VirtualBuffer, Vec<Region>, usize) {
        // Returns (buffer, regions, cursor_col)

        let mut buf = VirtualBuffer::new(width, 1);
        let width = width as usize;

        // Write prompt
        buf.set_style(Style {
            fg: Some(Color::Cyan),
            bold: true,
            ..Default::default()
        });
        buf.write_str(&self.prompt);
        buf.reset_style();
        buf.write_char(' ');
        let prompt_cols = buf.cursor_col();

        // Calculate input display widths
        let input_chars: Vec<char> = input.chars().collect();
        let char_widths: Vec<usize> = input_chars
            .iter()
            .map(|c| unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0))
            .collect();
        let cumulative: Vec<usize> = std::iter::once(0)
            .chain(char_widths.iter().scan(0, |acc, &w| {
                *acc += w;
                Some(*acc)
            }))
            .collect();
        let total_input_width = *cumulative.last().unwrap_or(&0);
        let cursor_x = cumulative[cursor_pos]; // display column of cursor in input

        let available = width.saturating_sub(prompt_cols);
        if total_input_width <= available {
            // Fits without scrolling
            buf.write_str(input);
            let cursor_col = prompt_cols + cursor_x;
            return (buf, vec![Region::new(0, 1)], cursor_col);
        }

        // Reserve last column for the cursor so typed characters don't
        // visually overlap it (the terminal cursor sits on a cell, not between cells)
        let available = available.saturating_sub(1);

        // Need scrolling — reserve 1 col for each overflow indicator
        let has_left = cursor_x > available.saturating_sub(2);
        let has_right = cursor_x < total_input_width.saturating_sub(1);

        let left_indicator: usize = if has_left { 1 } else { 0 };
        let right_indicator: usize = if has_right { 1 } else { 0 };
        let viewport_width = available.saturating_sub(left_indicator + right_indicator);

        if viewport_width == 0 {
            return (buf, vec![Region::new(0, 1)], prompt_cols);
        }

        // Calculate viewport start (in display columns) to keep cursor visible
        // Center cursor in viewport, clamped to valid range
        let viewport_start = cursor_x
            .saturating_sub(viewport_width / 2)
            .min(total_input_width.saturating_sub(viewport_width));

        // Write left indicator
        if has_left {
            buf.set_style(Style::dim());
            buf.write_char('\u{2026}'); // …
            buf.reset_style();
        }

        // Write visible characters
        for (i, &ch) in input_chars.iter().enumerate() {
            let char_end = cumulative[i + 1];
            let char_start = cumulative[i];
            if char_end <= viewport_start {
                continue;
            }
            if char_start >= viewport_start + viewport_width {
                break;
            }
            buf.write_char(ch);
        }

        // Write right indicator
        if has_right {
            buf.set_style(Style::dim());
            buf.write_char('\u{2026}'); // …
            buf.reset_style();
        }

        // Cursor column in terminal
        let cursor_col = prompt_cols + left_indicator + cursor_x.saturating_sub(viewport_start);

        (buf, vec![Region::new(0, 1)], cursor_col)
    }
}

/// Find the char-index of the previous word boundary (for backward word operations).
/// Accepts and returns char indices (not byte offsets).
fn find_word_boundary_backward(s: &str, from: usize) -> usize {
    if from == 0 {
        return 0;
    }
    let chars: Vec<char> = s.chars().collect();
    let mut pos = from;

    // Skip any whitespace immediately before cursor
    while pos > 0 && chars[pos - 1].is_ascii_whitespace() {
        pos -= 1;
    }
    // Skip the word (non-whitespace)
    while pos > 0 && !chars[pos - 1].is_ascii_whitespace() {
        pos -= 1;
    }
    pos
}

/// Find the char-index of the next word boundary (for forward word operations).
/// Accepts and returns char indices (not byte offsets).
fn find_word_boundary_forward(s: &str, from: usize) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    if from >= len {
        return len;
    }
    let mut pos = from;

    // Skip current word (non-whitespace)
    while pos < len && !chars[pos].is_ascii_whitespace() {
        pos += 1;
    }
    // Skip whitespace after the word
    while pos < len && chars[pos].is_ascii_whitespace() {
        pos += 1;
    }
    pos
}

// ============================================================================
// Clipboard Utilities
// ============================================================================

/// Copy text to the system clipboard.
///
/// Prints a success message on success, or logs a warning on failure.
pub fn copy_to_clipboard(text: &str) {
    match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text)) {
        Ok(_) => println!("Command copied to clipboard."),
        Err(e) => log::warn!("Failed to copy to clipboard: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lines_needed_ascii_exact_fit() {
        // 80 ASCII chars should be exactly 1 line at term_width=80
        let line = "a".repeat(80);
        assert_eq!(InteractiveSelect::lines_needed(&line, 80), 1);
    }

    #[test]
    fn test_lines_needed_ascii_one_over() {
        // 81 ASCII chars should wrap to 2 lines at term_width=80
        let line = "a".repeat(81);
        assert_eq!(InteractiveSelect::lines_needed(&line, 80), 2);
    }

    #[test]
    fn test_lines_needed_utf8_exact_fit() {
        // "↑" is 3 bytes but 1 display column
        // 80 arrows = 80 display columns = 1 line
        let line = "↑".repeat(80);
        assert_eq!(InteractiveSelect::lines_needed(&line, 80), 1);
    }

    #[test]
    fn test_lines_needed_with_prefix_exact_fit() {
        // prefix=6, label=74 ASCII chars = 80 total = 1 line
        let label = "a".repeat(74);
        assert_eq!(InteractiveSelect::lines_needed_with_prefix(&label, 80, 6), 1);
    }

    #[test]
    fn test_lines_needed_with_prefix_utf8_exact_fit() {
        // prefix=6, 74 arrows (74 display cols, 222 bytes) = 80 display cols = 1 line
        let label = "↑".repeat(74);
        assert_eq!(InteractiveSelect::lines_needed_with_prefix(&label, 80, 6), 1);
    }

    #[test]
    fn test_lines_needed_wide_chars() {
        // CJK characters are 2 display columns each
        // 40 CJK chars = 80 display columns = 1 line
        let line = "你".repeat(40);
        assert_eq!(InteractiveSelect::lines_needed(&line, 80), 1);
    }

    #[test]
    fn test_lines_needed_emoji() {
        // Emoji "🎉" is 4 bytes but 2 display columns
        // 40 emoji = 80 display columns = 1 line
        let line = "🎉".repeat(40);
        assert_eq!(InteractiveSelect::lines_needed(&line, 80), 1);
    }

    // ========================================================================
    // Helper for TextInput rendering tests
    // ========================================================================

    /// Extract visible characters from a buffer row as a String.
    fn row_text(buf: &VirtualBuffer, row: usize) -> String {
        let r = buf.row(row).unwrap();
        r.cells
            .iter()
            .filter_map(|c| if c.is_continuation { None } else { c.ch })
            .collect()
    }

    /// Create a TextInput with a short prompt for testing.
    fn test_input(prompt: &str) -> TextInput {
        TextInput::new(prompt)
    }

    // ========================================================================
    // build_multiline_buffer tests
    // ========================================================================

    #[test]
    fn test_multiline_empty_input() {
        let ti = test_input(">");
        // prompt ">" + space = 2 cols; empty input
        let (_buf, content_rows, cursor_row, cursor_col) =
            ti.build_multiline_buffer("", 0, 80, 24);
        assert_eq!(content_rows, 1);
        assert_eq!(cursor_row, 0);
        assert_eq!(cursor_col, 2); // after "> "
    }

    #[test]
    fn test_multiline_short_input_cursor_at_end() {
        let ti = test_input(">");
        // prompt ">" + space = 2 cols; "hello" = 5 cols; total = 7
        let (_buf, content_rows, cursor_row, cursor_col) =
            ti.build_multiline_buffer("hello", 5, 80, 24);
        assert_eq!(content_rows, 1);
        assert_eq!(cursor_row, 0);
        assert_eq!(cursor_col, 7); // 2 (prompt) + 5 (input)
    }

    #[test]
    fn test_multiline_cursor_at_start() {
        let ti = test_input(">");
        let (_buf, _content_rows, cursor_row, cursor_col) =
            ti.build_multiline_buffer("hello", 0, 80, 24);
        assert_eq!(cursor_row, 0);
        assert_eq!(cursor_col, 2); // right after "> "
    }

    #[test]
    fn test_multiline_cursor_in_middle() {
        let ti = test_input(">");
        let (_buf, _content_rows, cursor_row, cursor_col) =
            ti.build_multiline_buffer("hello", 3, 80, 24);
        assert_eq!(cursor_row, 0);
        assert_eq!(cursor_col, 5); // 2 + 3
    }

    #[test]
    fn test_multiline_wraps_to_second_line() {
        let ti = test_input(">");
        // width=10, prompt "> " = 2 cols, leaves 8 cols on first line
        // "abcdefghij" = 10 chars; first 8 on row 0, "ij" on row 1
        let (buf, content_rows, cursor_row, cursor_col) =
            ti.build_multiline_buffer("abcdefghij", 10, 10, 24);
        assert_eq!(content_rows, 2);
        assert_eq!(cursor_row, 1);
        assert_eq!(cursor_col, 2); // "ij" takes 2 cols on row 1
        assert_eq!(row_text(&buf, 0), "> abcdefgh");
        assert_eq!(row_text(&buf, 1), "ij");
    }

    #[test]
    fn test_multiline_cursor_on_wrapped_line() {
        let ti = test_input(">");
        // width=10, "> " = 2 cols, "abcdefghij" wraps; cursor at pos 9 = 'j' on row 1
        let (_buf, _content_rows, cursor_row, cursor_col) =
            ti.build_multiline_buffer("abcdefghij", 9, 10, 24);
        assert_eq!(cursor_row, 1);
        assert_eq!(cursor_col, 1); // 'j' is at col 1 on row 1
    }

    #[test]
    fn test_multiline_cursor_at_exact_row_boundary() {
        let ti = test_input(">");
        // width=10, "> " = 2 cols, 8 chars fills row 0 exactly
        // cursor at pos 8 should land on row 1, col 0
        let (_buf, content_rows, cursor_row, cursor_col) =
            ti.build_multiline_buffer("abcdefgh", 8, 10, 24);
        assert_eq!(cursor_row, 1);
        assert_eq!(cursor_col, 0);
        assert_eq!(content_rows, 2); // row 1 needed for cursor
    }

    #[test]
    fn test_multiline_wide_chars() {
        let ti = test_input(">");
        // width=10, "> " = 2 cols, "日本語" = 6 cols (3 chars × 2)
        let (buf, content_rows, cursor_row, cursor_col) =
            ti.build_multiline_buffer("日本語", 3, 10, 24);
        assert_eq!(content_rows, 1);
        assert_eq!(cursor_row, 0);
        assert_eq!(cursor_col, 8); // 2 + 6
        assert_eq!(row_text(&buf, 0), "> 日本語");
    }

    #[test]
    fn test_multiline_wide_char_wraps_at_edge() {
        let ti = test_input(">");
        // width=5, "> " = 2 cols, 3 cols remain; "日" needs 2 cols
        // First "日" at cols 2-3, second "日" doesn't fit (needs col 4-5 but col 4
        // is the last), so it wraps to row 1
        let (buf, content_rows, _cursor_row, _cursor_col) =
            ti.build_multiline_buffer("日日", 2, 5, 24);
        assert_eq!(content_rows, 2);
        assert_eq!(row_text(&buf, 0), "> 日");
        assert_eq!(row_text(&buf, 1), "日");
    }

    #[test]
    fn test_multiline_overflow_detected() {
        let ti = test_input(">");
        // width=5, height=2 (drawable_height+1 = 3 in practice, but test directly)
        // "> " = 2 cols, "abcdefghijklmno" = 15 chars
        // Row 0: "> abc" (5 cols), Row 1: "defgh" (5 cols), Row 2: "ijklm" ...
        // With height=2, buffer clips at row 1, but content needs 4+ rows
        // content_rows should exceed height=2 to trigger scroll fallback
        let (_buf, content_rows, _cursor_row, _cursor_col) =
            ti.build_multiline_buffer("abcdefghijklmno", 15, 5, 3);
        assert!(
            content_rows > 2,
            "content_rows ({content_rows}) should exceed drawable height (2) to trigger scroll mode"
        );
    }

    #[test]
    fn test_multiline_exact_fit_no_overflow() {
        let ti = test_input(">");
        // width=10, height=2, "> " = 2 cols
        // "abcdefgh" = 8 chars fits exactly on row 0 (2 + 8 = 10)
        // Cursor at end goes to row 1 col 0, content_rows = 2
        // With height=2, this should NOT overflow (content_rows <= 2)
        let (_buf, content_rows, cursor_row, _cursor_col) =
            ti.build_multiline_buffer("abcdefgh", 8, 10, 2);
        assert_eq!(content_rows, 2); // cursor on row 1
        assert_eq!(cursor_row, 1);
    }

    #[test]
    fn test_multiline_content_rows_includes_cursor_line() {
        let ti = test_input(">");
        // Cursor at end of a full row should produce an extra content row
        // even though no input characters are on that row
        let (_buf, content_rows, cursor_row, cursor_col) =
            ti.build_multiline_buffer("abcdefgh", 8, 10, 24);
        assert_eq!(cursor_row, 1);
        assert_eq!(cursor_col, 0);
        assert!(content_rows >= 2, "must include the row where the cursor sits");
    }

    // ========================================================================
    // build_scroll_buffer tests
    // ========================================================================

    #[test]
    fn test_scroll_short_input_no_scrolling() {
        let ti = test_input(">");
        // width=80, "> " = 2 cols, "hi" = 2 cols; fits easily
        let (buf, regions, cursor_col) = ti.build_scroll_buffer("hi", 2, 80);
        assert_eq!(regions, vec![Region::new(0, 1)]);
        assert_eq!(cursor_col, 4); // 2 + 2
        assert_eq!(row_text(&buf, 0), "> hi");
    }

    #[test]
    fn test_scroll_cursor_at_start() {
        let ti = test_input(">");
        let (_buf, _regions, cursor_col) = ti.build_scroll_buffer("hi", 0, 80);
        assert_eq!(cursor_col, 2); // right after "> "
    }

    #[test]
    fn test_scroll_long_input_has_indicators() {
        let ti = test_input(">");
        // width=20, "> " = 2 cols, available = 18, cursor reserved = 17
        // 30 chars of input; cursor in middle
        let input = "a".repeat(30);
        let (buf, _regions, _cursor_col) = ti.build_scroll_buffer(&input, 15, 20);
        let text = row_text(&buf, 0);
        // Should have ellipsis indicators when content overflows
        assert!(
            text.contains('\u{2026}'),
            "scrolling buffer should contain ellipsis indicator"
        );
    }

    #[test]
    fn test_scroll_cursor_within_terminal_width() {
        let ti = test_input(">");
        // Regression: cursor_col must never exceed width-1
        let input = "x".repeat(200);
        let width: u16 = 40;
        for cursor_pos in [0, 50, 100, 199, 200] {
            let (_buf, _regions, cursor_col) =
                ti.build_scroll_buffer(&input, cursor_pos, width);
            assert!(
                cursor_col < width as usize,
                "cursor_col ({cursor_col}) must be < width ({width}) at cursor_pos={cursor_pos}"
            );
        }
    }

    #[test]
    fn test_scroll_cursor_at_end_no_right_indicator() {
        let ti = test_input(">");
        // Cursor at end of input — no content to the right, so no right "…"
        let input = "a".repeat(50);
        let len = input.chars().count();
        let (buf, _regions, _cursor_col) = ti.build_scroll_buffer(&input, len, 30);
        let text = row_text(&buf, 0);
        // The last visible non-space char should NOT be "…"
        let chars: Vec<char> = text.chars().collect();
        let last = chars.last().unwrap();
        assert_ne!(*last, '\u{2026}', "no right indicator when cursor is at end");
    }

    #[test]
    fn test_scroll_cursor_at_start_no_left_indicator() {
        let ti = test_input(">");
        // Cursor at start of input — no content to the left, so no left "…"
        let input = "a".repeat(50);
        let (buf, _regions, _cursor_col) = ti.build_scroll_buffer(&input, 0, 30);
        let text = row_text(&buf, 0);
        assert!(
            !text.starts_with("> \u{2026}"),
            "no left indicator when cursor is at start"
        );
    }

    #[test]
    fn test_scroll_wide_chars() {
        let ti = test_input(">");
        // width=10, "> " = 2 cols, "日本語日本語" = 12 cols; overflows
        let input = "日本語日本語";
        let len = input.chars().count();
        let (_buf, _regions, cursor_col) = ti.build_scroll_buffer(input, len, 10);
        assert!(
            cursor_col < 10,
            "cursor_col ({cursor_col}) must be < width (10) with wide chars"
        );
    }

    #[test]
    fn test_scroll_cursor_reserved_cell() {
        let ti = test_input(">");
        // Regression: the last cell of the line is reserved for the cursor.
        // With cursor at end of a long input, cursor_col should be at most
        // width - 1, leaving the cursor on an empty cell rather than on
        // content.
        let input = "b".repeat(100);
        let len = input.chars().count();
        let width: u16 = 20;
        let (_buf, _regions, cursor_col) = ti.build_scroll_buffer(&input, len, width);
        assert!(
            cursor_col < width as usize,
            "cursor must fit within terminal width"
        );
    }

    // ========================================================================
    // char_to_byte tests
    // ========================================================================

    #[test]
    fn test_char_to_byte_ascii() {
        assert_eq!(TextInput::char_to_byte("hello", 0), 0);
        assert_eq!(TextInput::char_to_byte("hello", 3), 3);
        assert_eq!(TextInput::char_to_byte("hello", 5), 5);
    }

    #[test]
    fn test_char_to_byte_multibyte() {
        // "日本語" — each char is 3 bytes
        assert_eq!(TextInput::char_to_byte("日本語", 0), 0);
        assert_eq!(TextInput::char_to_byte("日本語", 1), 3);
        assert_eq!(TextInput::char_to_byte("日本語", 2), 6);
        assert_eq!(TextInput::char_to_byte("日本語", 3), 9);
    }

    #[test]
    fn test_char_to_byte_past_end() {
        assert_eq!(TextInput::char_to_byte("hi", 10), 2);
    }
}
