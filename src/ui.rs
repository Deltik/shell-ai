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

    /// Run the interactive selection and return the selected key.
    ///
    /// Returns `None` if the user cancelled (Escape/Ctrl+C).
    pub fn run(&mut self) -> io::Result<Option<char>> {
        terminal::enable_raw_mode()?;
        let result = self.run_inner();
        terminal::disable_raw_mode()?;

        // Clear the menu after selection
        execute!(io::stderr(), cursor::MoveToColumn(0))?;

        result
    }

    fn run_inner(&mut self) -> io::Result<Option<char>> {
        let mut stderr = io::stderr();
        let mut first_render = true;

        loop {
            // Clear and redraw
            self.render(&mut stderr, first_render)?;
            first_render = false;

            // Wait for key event
            if let Event::Key(key_event) = event::read()? {
                match self.handle_key(key_event) {
                    KeyAction::Select(key) => {
                        // Clear the menu before returning
                        self.clear_menu(&mut stderr)?;
                        return Ok(Some(key));
                    }
                    KeyAction::Cancel => {
                        self.clear_menu(&mut stderr)?;
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
                if let Some(opt) = self.options.get(self.selected) {
                    KeyAction::Select(opt.key)
                } else {
                    KeyAction::None
                }
            }
            KeyCode::Esc => KeyAction::Cancel,
            KeyCode::Char(c) => {
                // Check if this character matches any option key
                if let Some(opt) = self.options.iter().find(|o| o.key == c) {
                    KeyAction::Select(opt.key)
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

            let label_styled = if is_selected {
                opt.label.clone().bold().to_string()
            } else {
                opt.label.clone()
            };

            write!(w, "  {} {}\r\n", key_styled, label_styled)?;
        }

        // Print help line
        write!(
            w,
            "\r\n{}\r\n",
            "↑↓/jk navigate • key/Enter select • Esc cancel".dimmed()
        )?;

        w.flush()?;
        Ok(())
    }

    /// Calculate the total number of terminal lines the menu will occupy,
    /// accounting for line wrapping.
    fn calculate_total_lines(&self) -> usize {
        let term_width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

        let mut total_lines = 0;

        // Prompt line
        total_lines += Self::lines_needed(&self.prompt, term_width);

        // Option lines (each has "  [X] " prefix = 6 chars)
        for opt in &self.options {
            let line_len = 6 + opt.label.len();
            total_lines += (line_len + term_width - 1) / term_width; // ceiling division
        }

        // Blank line + help line
        let help_text = "↑↓/jk navigate • key/Enter select • Esc cancel";
        total_lines += 1; // blank line
        total_lines += Self::lines_needed(help_text, term_width);

        total_lines
    }

    /// Calculate how many terminal lines a string will occupy.
    fn lines_needed(s: &str, term_width: usize) -> usize {
        if s.is_empty() || term_width == 0 {
            return 1;
        }
        (s.len() + term_width - 1) / term_width // ceiling division
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
    Select(char),
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
        terminal::enable_raw_mode()?;
        let result = self.run_inner();
        terminal::disable_raw_mode()?;
        result
    }

    fn run_inner(&self) -> io::Result<Option<String>> {
        let mut stderr = io::stderr();
        let mut input = self.initial_value.clone();
        let mut cursor_pos = input.len();

        loop {
            // Render prompt and current input
            execute!(
                stderr,
                cursor::MoveToColumn(0),
                terminal::Clear(ClearType::CurrentLine)
            )?;
            write!(stderr, "{} {}", self.prompt.cyan(), input)?;

            // Position cursor
            let prompt_len = self.prompt.len() + 1; // +1 for space
            execute!(stderr, cursor::MoveToColumn((prompt_len + cursor_pos) as u16))?;
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
                        cursor_pos = input.len();
                    }
                    // Kill to beginning: Ctrl+U
                    (KeyCode::Char('u'), true, _) => {
                        input.drain(..cursor_pos);
                        cursor_pos = 0;
                    }
                    // Kill to end: Ctrl+K
                    (KeyCode::Char('k'), true, _) => {
                        input.truncate(cursor_pos);
                    }
                    // Delete word backward: Ctrl+W or Alt+Backspace
                    (KeyCode::Char('w'), true, _) | (KeyCode::Backspace, _, true) => {
                        let new_pos = find_word_boundary_backward(&input, cursor_pos);
                        input.drain(new_pos..cursor_pos);
                        cursor_pos = new_pos;
                    }
                    // Delete word forward: Alt+D
                    (KeyCode::Char('d'), _, true) => {
                        let end_pos = find_word_boundary_forward(&input, cursor_pos);
                        input.drain(cursor_pos..end_pos);
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
                            input.remove(cursor_pos - 1);
                            cursor_pos -= 1;
                        }
                    }
                    // Delete
                    (KeyCode::Delete, _, _) | (KeyCode::Char('d'), true, _) => {
                        if cursor_pos < input.len() {
                            input.remove(cursor_pos);
                        }
                    }
                    // Move left
                    (KeyCode::Left, _, _) | (KeyCode::Char('b'), true, _) => {
                        if cursor_pos > 0 {
                            cursor_pos -= 1;
                        }
                    }
                    // Move right
                    (KeyCode::Right, _, _) | (KeyCode::Char('f'), true, _) => {
                        if cursor_pos < input.len() {
                            cursor_pos += 1;
                        }
                    }
                    // Regular character input
                    (KeyCode::Char(c), false, false) => {
                        input.insert(cursor_pos, c);
                        cursor_pos += 1;
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Find the position of the previous word boundary (for backward word operations).
fn find_word_boundary_backward(s: &str, from: usize) -> usize {
    if from == 0 {
        return 0;
    }
    let bytes = s.as_bytes();
    let mut pos = from;

    // Skip any whitespace immediately before cursor
    while pos > 0 && bytes[pos - 1].is_ascii_whitespace() {
        pos -= 1;
    }
    // Skip the word (non-whitespace)
    while pos > 0 && !bytes[pos - 1].is_ascii_whitespace() {
        pos -= 1;
    }
    pos
}

/// Find the position of the next word boundary (for forward word operations).
fn find_word_boundary_forward(s: &str, from: usize) -> usize {
    let len = s.len();
    if from >= len {
        return len;
    }
    let bytes = s.as_bytes();
    let mut pos = from;

    // Skip current word (non-whitespace)
    while pos < len && !bytes[pos].is_ascii_whitespace() {
        pos += 1;
    }
    // Skip whitespace after the word
    while pos < len && bytes[pos].is_ascii_whitespace() {
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
