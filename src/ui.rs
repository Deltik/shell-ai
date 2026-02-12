//! Terminal UI widgets for shell-ai.
//!
//! Provides interactive prompts with both arrow key navigation and
//! number/letter shortcuts (similar to Claude Code's interface).

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
        let mut input = self.initial_value.clone();
        let mut cursor_pos = input.chars().count(); // char index, not byte offset

        loop {
            // Render prompt and current input
            execute!(
                stderr,
                cursor::MoveToColumn(0),
                terminal::Clear(ClearType::CurrentLine)
            )?;
            write!(stderr, "{} {}", self.prompt.cyan(), input)?;

            // Position cursor using display widths
            let prompt_width = UnicodeWidthStr::width(self.prompt.as_str()) + 1; // +1 for space
            let byte_offset = Self::char_to_byte(&input, cursor_pos);
            let input_width = UnicodeWidthStr::width(&input[..byte_offset]);
            execute!(stderr, cursor::MoveToColumn((prompt_width + input_width) as u16))?;
            stderr.flush()?;

            // Wait for key event
            if let Event::Key(key_event) = event::read()? {
                let ctrl = key_event.modifiers.contains(KeyModifiers::CONTROL);
                let alt = key_event.modifiers.contains(KeyModifiers::ALT);

                match (key_event.code, ctrl, alt) {
                    // Cancel
                    (KeyCode::Char('c'), true, _) | (KeyCode::Esc, _, _) => {
                        execute!(stderr, cursor::MoveToColumn(0), terminal::Clear(ClearType::CurrentLine))?;
                        return Ok(None);
                    }
                    // Confirm
                    (KeyCode::Enter, _, _) => {
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
}
