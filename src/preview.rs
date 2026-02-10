//! Streaming preview UI components for shell-ai.
//!
//! Provides real-time streaming preview showing AI response content as it arrives.
//! - `StreamingPreview`: For explain mode - shows parsed preview without borders
//! - `SuggestProgress`: For suggest mode - shows stacked progress for each suggestion
//!
//! Architecture:
//! - Uses a virtual buffer (2D grid of styled cells) for rendering
//! - Diff-based updates minimize terminal I/O
//! - Synchronized output (DEC 2026) prevents tearing
//! - Row-level diffing skips unchanged rows and only repaints modified ones

use crate::config::PreviewMode;
use crate::render::{Color, Region, Style, TerminalRenderer, VirtualBuffer};
use is_terminal::IsTerminal;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

/// Spinner characters for animation.
const SPINNER_CHARS: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Bouncing dots animation for truncation indicator.
/// The animation cycles through these frames to indicate ongoing activity.
const ELLIPSIS_FRAMES: &[&str] = &["·  ", "·· ", "···", " ··", "  ·"];

/// Get an animated ellipsis based on frame index.
pub fn animated_ellipsis(frame: usize) -> &'static str {
    ELLIPSIS_FRAMES[frame % ELLIPSIS_FRAMES.len()]
}

/// Separator line character.
const SEPARATOR_CHAR: char = '─';

/// Perforation separator for transitions between chunk types.
const PERFORATION_CHAR: char = '┄';

/// Lines reserved for chrome (spinner line + 2 separator lines).
const CHROME_LINES: usize = 3;

// ============================================================================
// Progress Header
// ============================================================================

/// Thinking phase for the progress header.
pub enum ThinkingPhase {
    /// No preamble received.
    None,
    /// Currently receiving preamble text.
    Active,
    /// Thinking finished, show duration.
    Done { secs: f64 },
}

/// Write text with waving color flourish in cyan.
/// The wave travels linearly (no wrap-around), so the highlight fades in from
/// the left, traverses the text, fades out to the right, then pauses briefly
/// before the next cycle. `pos` can be negative (wave approaching from left).
fn write_flourish(buffer: &mut VirtualBuffer, text: &str, pos: isize) {
    let chars: Vec<char> = text.chars().collect();
    for (j, &ch) in chars.iter().enumerate() {
        let dist = (j as isize - pos).unsigned_abs();

        let style = if dist == 0 {
            // \033[1;96m — bold + bright cyan
            Style {
                bold: true,
                fg: Some(Color::BrightCyan),
                ..Default::default()
            }
        } else if dist <= 2 {
            // \033[0;36m — normal cyan
            Style::fg(Color::Cyan)
        } else {
            // \033[2;36m — dim cyan
            Style {
                dim: true,
                fg: Some(Color::Cyan),
                ..Default::default()
            }
        };
        buffer.set_style(style);
        buffer.write_char(ch);
    }
    buffer.reset_style();
}

/// Write "thinking" with a pulsing dim/bold effect.
fn write_thinking_pulse(buffer: &mut VirtualBuffer, frame: usize) {
    // 8-frame cycle: dim dim default default bold bold default default
    let style = match frame % 8 {
        0 | 1 => Style::dim(),
        4 | 5 => Style {
            bold: true,
            ..Default::default()
        },
        _ => Style::default(),
    };
    buffer.set_style(style);
    buffer.write_str("thinking");
    buffer.set_style(Style::dim()); // restore dim for closing paren
}

/// Write the progress header line.
/// `label` is "Suggesting…" or "Explaining…".
/// `metadata_items` are `·`-separated entries inside parentheses.
fn write_progress_header(
    buffer: &mut VirtualBuffer,
    spinner_idx: usize,
    label: &str,
    metadata_items: &[String],
) {
    // Spinner (cyan)
    let spinner = SPINNER_CHARS[spinner_idx % SPINNER_CHARS.len()];
    buffer.set_style(Style::fg(Color::Cyan));
    buffer.write_char(spinner);
    buffer.reset_style();
    buffer.write_char(' ');

    // Label with waving flourish.
    // Cycle = label_len + 6: 2 entry frames (wave fading in from left) +
    // label_len traversal frames + 2 exit frames (tail fading off right) +
    // 2 fully dark frames. Effective position is offset by -2 so the wave
    // starts approaching from beyond the left edge.
    let label_len = label.chars().count();
    let cycle_len = label_len + 6;
    let wave_pos = (spinner_idx % cycle_len) as isize - 2;
    write_flourish(buffer, label, wave_pos);

    // Space + parenthesized metadata (dim)
    buffer.write_char(' ');
    buffer.set_style(Style::dim());
    buffer.write_char('(');
    for (i, item) in metadata_items.iter().enumerate() {
        if i > 0 {
            buffer.write_str(" \u{00b7} ");
        }
        if item == "thinking" {
            write_thinking_pulse(buffer, spinner_idx);
        } else {
            buffer.write_str(item);
        }
    }
    buffer.write_char(')');
    buffer.reset_style();
}

// ============================================================================
// Line Wrapping Utilities (Pure Functions)
// ============================================================================

/// Strip ANSI escape sequences from a string.
/// Returns a new string with all escape sequences removed.
pub fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Start of escape sequence
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                // Skip until we hit a letter (the terminator)
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            // Also handle other escape sequences like \x1b]...\x07 (OSC)
            else if chars.peek() == Some(&']') {
                chars.next();
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next == '\x07' || next == '\\' {
                        break;
                    }
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Calculate display width of a string, accounting for Unicode and ignoring ANSI escapes.
pub fn display_width(s: &str) -> usize {
    let stripped = strip_ansi(s);
    UnicodeWidthStr::width(stripped.as_str())
}

/// Calculate how many terminal lines a string will occupy when wrapped.
pub fn wrapped_line_count(s: &str, term_width: usize) -> usize {
    if term_width == 0 {
        return 1;
    }

    let count: usize = s.lines()
        .map(|line| {
            let width = display_width(line);
            if width == 0 {
                1
            } else {
                width.div_ceil(term_width)
            }
        })
        .sum();

    // Empty string or string with no lines should be at least 1 if we're measuring content
    if count == 0 && !s.is_empty() { 1 } else { count.max(if s.is_empty() { 0 } else { 1 }) }
}

/// Result of truncating a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncatedString {
    /// The truncated content (without any ellipsis indicator).
    pub content: String,
    /// Number of characters that were removed (0 if not truncated).
    pub chars_removed: usize,
    /// Display width of the removed portion.
    pub width_removed: usize,
}

impl TruncatedString {
    /// Returns true if the string was truncated.
    pub fn was_truncated(&self) -> bool {
        self.chars_removed > 0
    }
}


/// Truncate a string to fit within a given display width.
/// Returns the truncated string and metadata about what was removed.
/// The caller is responsible for adding any ellipsis indicator.
pub fn truncate_string(s: &str, max_width: usize) -> TruncatedString {
    let original_width = display_width(s);

    if original_width <= max_width {
        return TruncatedString {
            content: s.to_string(),
            chars_removed: 0,
            width_removed: 0,
        };
    }

    let mut result = String::new();
    let mut current_width = 0;
    let mut chars_kept = 0;
    let original_chars = s.chars().count();

    for c in s.chars() {
        let char_width = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if current_width + char_width > max_width {
            break;
        }
        result.push(c);
        current_width += char_width;
        chars_kept += 1;
    }

    TruncatedString {
        content: result,
        chars_removed: original_chars - chars_kept,
        width_removed: original_width - current_width,
    }
}


// ============================================================================
// StreamingPreview
// ============================================================================

/// Configuration for StreamingPreview rendering.
pub struct ExplainPreviewConfig {
    pub term_width: usize,
    pub term_height: usize,
    pub elapsed_secs: f64,
    pub spinner_idx: usize,
    /// Number of characters received so far.
    pub char_count: usize,
    /// Optional status message (e.g., "backoff #2, 4.0s")
    pub status: Option<String>,
    /// Maximum preview display mode from user settings.
    pub max_preview_mode: PreviewMode,
    /// Current thinking/preamble phase.
    pub thinking: ThinkingPhase,
}

/// Determine the appropriate display mode for explain based on content and terminal size.
fn determine_explain_display_mode(config: &ExplainPreviewConfig) -> DisplayMode {
    let available = config.term_height.saturating_sub(CHROME_LINES);

    let fits_mode = if available >= 1 {
        DisplayMode::Full
    } else {
        DisplayMode::Minimal
    };

    // Cap at user's maximum preference
    cap_display_mode(fits_mode, config.max_preview_mode)
}

/// Render explain frame to a virtual buffer with regions.
///
/// This is the main rendering function that produces output suitable for
/// the new TerminalRenderer with synchronized output and diff-based updates.
fn render_explain_to_buffer(
    chunks: &[StreamChunk],
    config: &ExplainPreviewConfig,
) -> (VirtualBuffer, Vec<Region>) {
    let width = config.term_width as u16;
    let height = config.term_height as u16;
    let mut buffer = VirtualBuffer::new(width, height);
    let mut regions = Vec::new();

    let display_mode = determine_explain_display_mode(config);

    // Row 0: Header (chrome)
    let mut metadata = vec![
        format!("{:.1}s", config.elapsed_secs),
        format!("{} chars", config.char_count),
    ];
    if let Some(ref status) = config.status {
        metadata.push(status.clone());
    }
    match &config.thinking {
        ThinkingPhase::Active => metadata.push("thinking".to_string()),
        ThinkingPhase::Done { secs } => {
            metadata.push(format!("thought for {}s", *secs as u64));
        }
        ThinkingPhase::None => {}
    }
    write_progress_header(&mut buffer, config.spinner_idx, "Explaining\u{2026}", &metadata);

    regions.push(Region::new(0, 1));

    // Minimal mode: just the header line
    if display_mode == DisplayMode::Minimal {
        return (buffer, regions);
    }

    // Check if there's any content to display
    let has_content = chunks.iter().any(|c| !c.text.is_empty());

    if has_content {
        // Row 1: Top separator (chrome)
        buffer.newline();
        buffer.set_style(Style::dim());
        for _ in 0..config.term_width {
            buffer.write_char(SEPARATOR_CHAR);
        }
        buffer.reset_style();
        regions.push(Region::new(1, 2));

        // Compact mode: single truncated content line
        if display_mode == DisplayMode::Compact {
            buffer.newline();
            // Concatenate all content and show first line truncated
            let all_content: String = chunks.iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join("");
            let first_line = all_content.lines().next().unwrap_or("");
            let available_width = config.term_width.saturating_sub(3); // Reserve space for ellipsis
            let truncated = truncate_string(first_line, available_width);
            buffer.write_str(&truncated.content);
            if truncated.was_truncated() || all_content.contains('\n') {
                buffer.set_style(Style::dim());
                buffer.write_str(animated_ellipsis(config.spinner_idx));
                buffer.reset_style();
            }
            regions.push(Region::new(2, 3));

            // Bottom separator
            buffer.newline();
            buffer.set_style(Style::dim());
            for _ in 0..config.term_width {
                buffer.write_char(SEPARATOR_CHAR);
            }
            buffer.reset_style();
            regions.push(Region::new(3, 4));

            return (buffer, regions);
        }

        // Full mode: Content rows
        let content_start = 2;
        let available_lines = config.term_height.saturating_sub(CHROME_LINES);

        buffer.newline();
        // Start with 1 line used for the initial content row
        let mut lines_used = 1;
        let mut prev_chunk_type: Option<ChunkType> = None;
        let mut was_truncated = false;

        for chunk in chunks {
            // Insert perforation separator when transitioning between chunk types
            if let Some(prev_type) = prev_chunk_type {
                if prev_type != chunk.chunk_type {
                    // Write perforation line
                    if lines_used + 1 > available_lines {
                        was_truncated = true;
                        break;
                    }
                    buffer.newline();
                    lines_used += 1;
                    buffer.set_style(Style::dim());
                    for _ in 0..config.term_width {
                        buffer.write_char(PERFORATION_CHAR);
                    }
                    buffer.reset_style();
                    if lines_used + 1 > available_lines {
                        was_truncated = true;
                        break;
                    }
                    buffer.newline();
                    if !chunk.text.is_empty() {
                        lines_used += 1;
                    }
                }
            }
            prev_chunk_type = Some(chunk.chunk_type);

            let style = match chunk.chunk_type {
                ChunkType::Preamble => Style::dim(),
                ChunkType::Content => Style::default(),
            };
            buffer.set_style(style);

            for ch in chunk.text.chars() {
                if ch == '\n' {
                    if lines_used + 1 > available_lines {
                        was_truncated = true;
                        break;
                    }
                    lines_used += 1;
                    buffer.newline();
                } else {
                    // Track wrapping
                    let char_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    if buffer.cursor_col() + char_width > config.term_width {
                        if lines_used + 1 > available_lines {
                            was_truncated = true;
                            break;
                        }
                        lines_used += 1;
                    }
                    buffer.write_char(ch);
                }
            }

            if was_truncated {
                break;
            }
        }
        buffer.reset_style();

        let content_end = content_start + lines_used.min(available_lines);
        if content_end > content_start {
            regions.push(Region::new(content_start, content_end));
        }

        // Bottom separator (chrome)
        let separator_row = content_end;
        if separator_row < config.term_height {
            buffer.move_to(separator_row, 0);
            buffer.set_style(Style::dim());
            for _ in 0..config.term_width {
                buffer.write_char(SEPARATOR_CHAR);
            }
            if was_truncated && config.term_width >= 6 {
                buffer.move_to(separator_row, 1);
                buffer.write_char('┤');
                buffer.move_to(separator_row, 2);
                buffer.write_str(animated_ellipsis(config.spinner_idx));
                buffer.move_to(separator_row, 5);
                buffer.write_char('├');
            }
            buffer.reset_style();
            regions.push(Region::new(separator_row, separator_row + 1));
        }
    }

    (buffer, regions)
}

/// Type of streaming chunk for display styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkType {
    /// Main content - displayed normally.
    Content,
    /// Preamble/thinking - displayed dimmed.
    Preamble,
}

/// A chunk of streamed text with its display type.
#[derive(Debug, Clone)]
pub struct StreamChunk {
    pub chunk_type: ChunkType,
    pub text: String,
}

/// Append text to a chunks vector, merging with the last chunk if types match.
pub fn append_chunk(chunks: &mut Vec<StreamChunk>, chunk_type: ChunkType, text: String) {
    if let Some(last) = chunks.last_mut() {
        if last.chunk_type == chunk_type {
            last.text.push_str(&text);
            return;
        }
    }
    chunks.push(StreamChunk { chunk_type, text });
}

/// Real-time streaming preview for explain mode.
pub struct StreamingPreview {
    /// Ordered list of typed chunks (preserves streaming order).
    chunks: Arc<Mutex<Vec<StreamChunk>>>,
    char_count: Arc<Mutex<usize>>,
    status: Arc<Mutex<Option<String>>>,
    start_time: Instant,
    renderer: TerminalRenderer,
    spinner_idx: usize,
    last_render: Instant,
    /// Maximum preview display mode from user settings.
    max_preview_mode: PreviewMode,
    /// Whether currently receiving preamble text.
    is_thinking: Arc<Mutex<bool>>,
    /// When the current thinking phase started.
    thinking_start: Arc<Mutex<Option<Instant>>>,
    /// Accumulated thinking duration in seconds.
    thinking_total_secs: Arc<Mutex<f64>>,
}

impl StreamingPreview {
    /// Create a new streaming preview.
    /// Returns None if stderr is not a TTY.
    pub fn new(max_preview_mode: PreviewMode) -> io::Result<Option<Self>> {
        if !io::stderr().is_terminal() {
            return Ok(None);
        }

        Ok(Some(Self {
            chunks: Arc::new(Mutex::new(Vec::new())),
            char_count: Arc::new(Mutex::new(0)),
            status: Arc::new(Mutex::new(None)),
            start_time: Instant::now(),
            renderer: TerminalRenderer::new(),
            spinner_idx: 0,
            last_render: Instant::now(),
            max_preview_mode,
            is_thinking: Arc::new(Mutex::new(false)),
            thinking_start: Arc::new(Mutex::new(None)),
            thinking_total_secs: Arc::new(Mutex::new(0.0)),
        }))
    }

    /// Get a clone of the chunks Arc for use in callbacks.
    pub fn chunks_handle(&self) -> Arc<Mutex<Vec<StreamChunk>>> {
        Arc::clone(&self.chunks)
    }

    /// Get a clone of the char count Arc for use in callbacks.
    pub fn char_count_handle(&self) -> Arc<Mutex<usize>> {
        Arc::clone(&self.char_count)
    }

    /// Get a clone of the status Arc for use in callbacks.
    pub fn status_handle(&self) -> Arc<Mutex<Option<String>>> {
        Arc::clone(&self.status)
    }

    /// Get a clone of the is_thinking Arc for use in callbacks.
    pub fn is_thinking_handle(&self) -> Arc<Mutex<bool>> {
        Arc::clone(&self.is_thinking)
    }

    /// Get a clone of the thinking_start Arc for use in callbacks.
    pub fn thinking_start_handle(&self) -> Arc<Mutex<Option<Instant>>> {
        Arc::clone(&self.thinking_start)
    }

    /// Get a clone of the thinking_total_secs Arc for use in callbacks.
    pub fn thinking_total_secs_handle(&self) -> Arc<Mutex<f64>> {
        Arc::clone(&self.thinking_total_secs)
    }

    /// Get the current chunks.
    pub fn get_chunks(&self) -> Vec<StreamChunk> {
        self.chunks.lock().map(|c| c.clone()).unwrap_or_default()
    }

    /// Get the current char count.
    pub fn get_char_count(&self) -> usize {
        self.char_count.lock().map(|c| *c).unwrap_or(0)
    }

    /// Get the current status.
    pub fn get_status(&self) -> Option<String> {
        self.status.lock().ok().and_then(|s| s.clone())
    }

    /// Render the current preview state.
    /// Rate-limited to ~12.5 renders/second (80ms).
    pub fn render(&mut self) -> io::Result<()> {
        let now = Instant::now();
        if now.duration_since(self.last_render).as_millis() < 80 {
            return Ok(());
        }
        self.last_render = now;

        self.render_inner()
    }

    fn render_inner(&mut self) -> io::Result<()> {
        let (width, height) = TerminalRenderer::term_size();
        let drawable_height = height.saturating_sub(1);
        if drawable_height == 0 || width == 0 {
            return Ok(());
        }

        // Compute thinking phase
        let is_thinking = self.is_thinking.lock().map(|t| *t).unwrap_or(false);
        let total_secs = self.thinking_total_secs.lock().map(|t| *t).unwrap_or(0.0);
        let thinking = if is_thinking {
            ThinkingPhase::Active
        } else if total_secs > 0.0 {
            ThinkingPhase::Done { secs: total_secs }
        } else {
            ThinkingPhase::None
        };

        let config = ExplainPreviewConfig {
            term_width: width as usize,
            term_height: drawable_height as usize,
            elapsed_secs: self.start_time.elapsed().as_secs_f64(),
            spinner_idx: self.spinner_idx,
            char_count: self.get_char_count(),
            status: self.get_status(),
            max_preview_mode: self.max_preview_mode,
            thinking,
        };
        self.spinner_idx = self.spinner_idx.wrapping_add(1);

        let chunks = self.get_chunks();
        let (buffer, regions) = render_explain_to_buffer(&chunks, &config);

        self.renderer.render(&buffer, &regions)
    }

    /// Clear the preview and prepare for final output.
    pub fn finish_and_clear(&mut self) -> io::Result<()> {
        self.renderer.clear()
    }
}

// ============================================================================
// SuggestProgress
// ============================================================================

/// State of a single suggestion slot.
#[derive(Debug, Clone, PartialEq)]
pub enum SlotState {
    Pending,
    Streaming { chars: usize, content: String },
    /// Waiting for retry after rate limiting (backoff).
    Waiting { attempt: u32, delay_ms: u64 },
    /// Retrying after backoff completed.
    Retrying { attempt: u32 },
    Complete { chars: usize, command: String },
    Error(String),
}

impl SlotState {
    pub fn is_pending_or_streaming(&self) -> bool {
        matches!(self, SlotState::Pending | SlotState::Streaming { .. } | SlotState::Waiting { .. } | SlotState::Retrying { .. })
    }

    pub fn char_count(&self) -> usize {
        match self {
            SlotState::Streaming { chars, .. } => *chars,
            SlotState::Complete { chars, .. } => *chars,
            _ => 0,
        }
    }
}

/// Plain text prefix for a slot line (everything before variable-length content).
/// Single source of truth for layout — used by both line counting and rendering.
fn slot_prefix(idx: usize, slot: &SlotState, spinner_idx: usize) -> String {
    let label = format!("[{}] ", idx + 1);
    let spinner = SPINNER_CHARS[spinner_idx % SPINNER_CHARS.len()];
    match slot {
        SlotState::Pending => format!("{}{} (pending)", label, spinner),
        SlotState::Waiting { attempt, delay_ms } => {
            format!("{}{} (backoff #{}, {:.1}s)", label, spinner, attempt, *delay_ms as f64 / 1000.0)
        }
        SlotState::Retrying { attempt } => format!("{}{} (retry #{})", label, spinner, attempt),
        SlotState::Streaming { chars, .. } => format!("{}{} ({} chars) ", label, spinner, chars),
        SlotState::Complete { chars, .. } => format!("{}✓ ({} chars) ", label, chars),
        SlotState::Error(_) => format!("{}✗ ", label),
    }
}

/// Variable-length content portion of a slot line.
fn slot_content(slot: &SlotState) -> &str {
    match slot {
        SlotState::Streaming { content, .. } => content,
        SlotState::Complete { command, .. } => command,
        SlotState::Error(err) => err,
        _ => "",
    }
}

/// Display mode for suggest progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DisplayMode {
    /// Single spinner line only
    Minimal = 0,
    /// One line per suggestion
    Compact = 1,
    /// Multi-line suggestions allowed
    Full = 2,
}

/// Configuration for SuggestProgress rendering.
#[derive(Debug, Clone)]
pub struct SuggestPreviewConfig {
    pub term_width: usize,
    pub term_height: usize,
    pub elapsed_secs: f64,
    pub spinner_idx: usize,
    /// Maximum preview display mode from user settings.
    pub max_preview_mode: PreviewMode,
}

/// Cap display mode to user's maximum preference.
/// Returns the more restrictive of the two modes (lower integer value).
fn cap_display_mode(fits: DisplayMode, max: PreviewMode) -> DisplayMode {
    let max_as_display = match max {
        PreviewMode::Minimal => DisplayMode::Minimal,
        PreviewMode::Compact => DisplayMode::Compact,
        PreviewMode::Full => DisplayMode::Full,
    };

    // Return the more restrictive mode (lower integer value wins)
    if (fits as u8) <= (max_as_display as u8) {
        fits
    } else {
        max_as_display
    }
}

/// Determine the appropriate display mode based on content and terminal size.
pub fn determine_display_mode(slots: &[SlotState], config: &SuggestPreviewConfig) -> DisplayMode {
    let available = config.term_height.saturating_sub(CHROME_LINES);

    // Calculate lines needed for Full mode
    let full_lines: usize = slots.iter().enumerate().map(|(i, slot)| {
        let text = format!("{}{}", slot_prefix(i, slot, config.spinner_idx), slot_content(slot));
        wrapped_line_count(&text, config.term_width)
    }).sum();

    let fits_mode = if full_lines <= available {
        DisplayMode::Full
    } else if slots.len() <= available {
        DisplayMode::Compact
    } else {
        DisplayMode::Minimal
    };

    // Cap at user's maximum preference
    cap_display_mode(fits_mode, config.max_preview_mode)
}

/// Render suggest frame to a virtual buffer with regions.
fn render_suggest_to_buffer(
    slots: &[SlotState],
    config: &SuggestPreviewConfig,
) -> (VirtualBuffer, Vec<Region>) {
    let width = config.term_width as u16;
    let height = config.term_height as u16;
    let mut buffer = VirtualBuffer::new(width, height);
    let mut regions = Vec::new();

    let display_mode = determine_display_mode(slots, config);

    let pending_count = slots.iter().filter(|s| s.is_pending_or_streaming()).count();
    let total_chars: usize = slots.iter().map(|s| s.char_count()).sum();

    match display_mode {
        DisplayMode::Minimal => {
            // Single line header
            let metadata = vec![
                format!("{:.1}s", config.elapsed_secs),
                format!("{} chars", total_chars),
                format!("{}/{} pending", pending_count, slots.len()),
            ];
            write_progress_header(
                &mut buffer,
                config.spinner_idx,
                "Suggesting\u{2026}",
                &metadata,
            );

            regions.push(Region::new(0, 1));
        }
        DisplayMode::Compact | DisplayMode::Full => {
            // Row 0: Header (chrome)
            let metadata = vec![
                format!("{:.1}s", config.elapsed_secs),
                format!("{} chars", total_chars),
                format!("{} suggestions", slots.len()),
            ];
            write_progress_header(
                &mut buffer,
                config.spinner_idx,
                "Suggesting\u{2026}",
                &metadata,
            );

            regions.push(Region::new(0, 1));

            // Row 1: Top separator (chrome)
            buffer.newline();
            buffer.set_style(Style::dim());
            for _ in 0..config.term_width {
                buffer.write_char(SEPARATOR_CHAR);
            }
            buffer.reset_style();
            regions.push(Region::new(1, 2));

            // Content rows (each slot)
            let content_start = 2;
            let compact = display_mode == DisplayMode::Compact;
            let mut content_end = content_start;

            if !slots.is_empty() {
                // Move to first content row
                buffer.newline();
                for (i, slot) in slots.iter().enumerate() {
                    if i > 0 {
                        buffer.newline();
                    }
                    write_slot_to_buffer(
                        &mut buffer,
                        i,
                        slot,
                        config.spinner_idx,
                        compact,
                        config.term_width,
                    );
                    let end = buffer.cursor_row().saturating_add(1);
                    if end > content_end {
                        content_end = end;
                    }
                }
            }

            if content_end > content_start {
                regions.push(Region::new(content_start, content_end));
            }

            // Bottom separator (chrome)
            if content_end < config.term_height {
                buffer.move_to(content_end, 0);
                buffer.set_style(Style::dim());
                for _ in 0..config.term_width {
                    buffer.write_char(SEPARATOR_CHAR);
                }
                buffer.reset_style();
                regions.push(Region::new(content_end, content_end + 1));
            }
        }
    }

    (buffer, regions)
}

/// Write a single slot to the buffer.
fn write_slot_to_buffer(
    buffer: &mut VirtualBuffer,
    idx: usize,
    slot: &SlotState,
    spinner_idx: usize,
    compact: bool,
    term_width: usize,
) {
    let spinner = SPINNER_CHARS[spinner_idx % SPINNER_CHARS.len()];
    let prefix_width = display_width(&slot_prefix(idx, slot, spinner_idx));

    // Slot number [N] in dim
    let slot_label = format!("[{}]", idx + 1);
    buffer.set_style(Style::dim());
    buffer.write_str(&slot_label);
    buffer.reset_style();
    buffer.write_char(' ');

    match slot {
        SlotState::Pending => {
            buffer.set_style(Style::fg(Color::Cyan));
            buffer.write_char(spinner);
            buffer.reset_style();
            buffer.write_char(' ');
            buffer.set_style(Style::dim());
            buffer.write_str("(pending)");
            buffer.reset_style();
        }
        SlotState::Waiting { attempt, delay_ms } => {
            buffer.set_style(Style::fg(Color::Cyan));
            buffer.write_char(spinner);
            buffer.reset_style();
            buffer.write_char(' ');
            buffer.set_style(Style::fg(Color::Yellow));
            let secs = *delay_ms as f64 / 1000.0;
            buffer.write_str(&format!("(backoff #{}, {:.1}s)", attempt, secs));
            buffer.reset_style();
        }
        SlotState::Retrying { attempt } => {
            buffer.set_style(Style::fg(Color::Cyan));
            buffer.write_char(spinner);
            buffer.reset_style();
            buffer.write_char(' ');
            buffer.set_style(Style::fg(Color::Yellow));
            buffer.write_str(&format!("(retry #{})", attempt));
            buffer.reset_style();
        }
        SlotState::Streaming { chars, content } => {
            buffer.set_style(Style::fg(Color::Cyan));
            buffer.write_char(spinner);
            buffer.reset_style();
            buffer.write_char(' ');
            buffer.set_style(Style::dim());
            buffer.write_str(&format!("({} chars)", chars));
            buffer.reset_style();
            buffer.write_char(' ');

            buffer.set_style(Style::fg(Color::Cyan));
            if compact {
                let available = term_width.saturating_sub(prefix_width).saturating_sub(3);
                let first_line = content.lines().next().unwrap_or("");
                let truncated = truncate_string(first_line, available);
                buffer.write_str(&truncated.content);
                if truncated.was_truncated() || content.contains('\n') {
                    buffer.set_style(Style::dim());
                    buffer.write_str(animated_ellipsis(spinner_idx));
                }
            } else {
                buffer.write_str(content);
            }
            buffer.reset_style();
        }
        SlotState::Complete { chars, command } => {
            buffer.set_style(Style::fg(Color::Green));
            buffer.write_str("✓");
            buffer.reset_style();
            buffer.write_char(' ');
            buffer.set_style(Style::dim());
            buffer.write_str(&format!("({} chars)", chars));
            buffer.reset_style();
            buffer.write_char(' ');

            if compact {
                let available = term_width.saturating_sub(prefix_width).saturating_sub(3);
                let first_line = command.lines().next().unwrap_or("");
                let truncated = truncate_string(first_line, available);
                buffer.write_str(&truncated.content);
                if truncated.was_truncated() || command.contains('\n') {
                    buffer.set_style(Style::dim());
                    buffer.write_str("···");
                    buffer.reset_style();
                }
            } else {
                buffer.write_str(command);
            }
        }
        SlotState::Error(err) => {
            buffer.set_style(Style::fg(Color::Red));
            buffer.write_str("✗");
            buffer.reset_style();
            buffer.write_char(' ');

            buffer.set_style(Style::fg(Color::Red));
            if compact {
                let available = term_width.saturating_sub(prefix_width).saturating_sub(3);
                let first_line = err.lines().next().unwrap_or("");
                let truncated = truncate_string(first_line, available);
                buffer.write_str(&truncated.content);
                if truncated.was_truncated() || err.contains('\n') {
                    buffer.set_style(Style::dim());
                    buffer.write_str("···");
                }
            } else {
                buffer.write_str(err);
            }
            buffer.reset_style();
        }
    }
}

/// Thread-safe slot state for sharing between callbacks and renderer.
pub type SharedSlots = Arc<Mutex<Vec<SlotState>>>;

/// Create shared slots for use with callbacks.
pub fn create_shared_slots(count: usize) -> SharedSlots {
    Arc::new(Mutex::new(vec![SlotState::Pending; count]))
}

/// Update a slot with streaming content (thread-safe).
pub fn update_shared_slot(slots: &SharedSlots, idx: usize, delta: &str) {
    if let Ok(mut slots) = slots.lock() {
        if let Some(slot) = slots.get_mut(idx) {
            match slot {
                SlotState::Streaming { chars, content } => {
                    *chars += delta.chars().count();
                    content.push_str(delta);
                }
                SlotState::Pending | SlotState::Waiting { .. } | SlotState::Retrying { .. } => {
                    // Transition from Pending, Waiting, or Retrying to Streaming
                    *slot = SlotState::Streaming {
                        chars: delta.chars().count(),
                        content: delta.to_string(),
                    };
                }
                _ => {}
            }
        }
    }
}

/// Mark a slot as complete (thread-safe).
/// Preserves the token count from the Streaming state.
pub fn complete_shared_slot(slots: &SharedSlots, idx: usize, command: String) {
    if let Ok(mut slots) = slots.lock() {
        if let Some(slot) = slots.get_mut(idx) {
            // Preserve token count from streaming state
            let chars = match slot {
                SlotState::Streaming { chars, .. } => *chars,
                _ => 0,
            };
            *slot = SlotState::Complete { chars, command };
        }
    }
}

/// Mark a slot as errored (thread-safe).
pub fn error_shared_slot(slots: &SharedSlots, idx: usize, error: String) {
    if let Ok(mut slots) = slots.lock() {
        if let Some(slot) = slots.get_mut(idx) {
            *slot = SlotState::Error(error);
        }
    }
}

/// Mark a slot as waiting for backoff retry (thread-safe).
pub fn backoff_shared_slot(slots: &SharedSlots, idx: usize, attempt: u32, delay_ms: u64) {
    if let Ok(mut slots) = slots.lock() {
        if let Some(slot) = slots.get_mut(idx) {
            *slot = SlotState::Waiting { attempt, delay_ms };
        }
    }
}

/// Mark a slot as retrying after backoff (thread-safe).
pub fn retrying_shared_slot(slots: &SharedSlots, idx: usize, attempt: u32) {
    if let Ok(mut slots) = slots.lock() {
        if let Some(slot) = slots.get_mut(idx) {
            *slot = SlotState::Retrying { attempt };
        }
    }
}

/// Stacked progress display for suggest mode.
pub struct SuggestProgress {
    shared_slots: SharedSlots,
    start_time: Instant,
    renderer: TerminalRenderer,
    spinner_idx: usize,
    last_render: Instant,
    /// Maximum preview display mode from user settings.
    max_preview_mode: PreviewMode,
}

impl SuggestProgress {
    /// Create a new suggest progress tracker.
    /// Returns None if stderr is not a TTY.
    pub fn new(count: usize, max_preview_mode: PreviewMode) -> io::Result<Option<Self>> {
        if !io::stderr().is_terminal() {
            return Ok(None);
        }

        Ok(Some(Self {
            shared_slots: create_shared_slots(count),
            start_time: Instant::now(),
            renderer: TerminalRenderer::new(),
            spinner_idx: 0,
            last_render: Instant::now(),
            max_preview_mode,
        }))
    }

    /// Get a clone of the shared slots for use in callbacks.
    pub fn shared_slots(&self) -> SharedSlots {
        Arc::clone(&self.shared_slots)
    }

    /// Get the current slots (for testing/inspection).
    fn slots(&self) -> Vec<SlotState> {
        self.shared_slots.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Render the current progress state.
    /// Rate-limited to ~12.5 renders/second (80ms).
    pub fn render(&mut self) -> io::Result<()> {
        let now = Instant::now();
        if now.duration_since(self.last_render).as_millis() < 80 {
            return Ok(());
        }
        self.last_render = now;

        self.render_inner()
    }

    fn render_inner(&mut self) -> io::Result<()> {
        let (width, height) = TerminalRenderer::term_size();
        let drawable_height = height.saturating_sub(1);
        if drawable_height == 0 || width == 0 {
            return Ok(());
        }

        let config = SuggestPreviewConfig {
            term_width: width as usize,
            term_height: drawable_height as usize,
            elapsed_secs: self.start_time.elapsed().as_secs_f64(),
            spinner_idx: self.spinner_idx,
            max_preview_mode: self.max_preview_mode,
        };
        self.spinner_idx = self.spinner_idx.wrapping_add(1);

        let slots = self.slots();
        let (buffer, regions) = render_suggest_to_buffer(&slots, &config);

        self.renderer.render(&buffer, &regions)
    }

    /// Clear the progress display.
    pub fn finish_and_clear(&mut self) -> io::Result<()> {
        self.renderer.clear()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // display_width tests
    // ========================================================================

    #[test]
    fn test_display_width_empty() {
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn test_display_width_ascii() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width("hello world"), 11);
    }

    #[test]
    fn test_display_width_unicode_cjk() {
        // CJK characters are 2 columns wide
        assert_eq!(display_width("日"), 2);
        assert_eq!(display_width("日本語"), 6);
        assert_eq!(display_width("中文"), 4);
    }

    #[test]
    fn test_display_width_mixed() {
        // "hello" (5) + "日本" (4) = 9
        assert_eq!(display_width("hello日本"), 9);
    }

    #[test]
    fn test_display_width_emoji() {
        // Most emoji are 2 columns wide
        assert_eq!(display_width("👍"), 2);
    }

    // ========================================================================
    // ANSI escape sequence tests
    // ========================================================================

    #[test]
    fn test_strip_ansi_no_escapes() {
        assert_eq!(strip_ansi("hello"), "hello");
    }

    #[test]
    fn test_strip_ansi_simple_color() {
        // Red text: \x1b[31m ... \x1b[0m
        assert_eq!(strip_ansi("\x1b[31mhello\x1b[0m"), "hello");
    }

    #[test]
    fn test_strip_ansi_multiple_escapes() {
        // Bold red: \x1b[1;31m
        assert_eq!(strip_ansi("\x1b[1;31mbold red\x1b[0m normal"), "bold red normal");
    }

    #[test]
    fn test_strip_ansi_256_color() {
        // 256 color: \x1b[38;5;196m (bright red)
        assert_eq!(strip_ansi("\x1b[38;5;196mcolored\x1b[0m"), "colored");
    }

    #[test]
    fn test_display_width_with_ansi() {
        // "hello" with red color codes should still be 5 columns
        let colored = "\x1b[31mhello\x1b[0m";
        assert_eq!(display_width(colored), 5);
    }

    #[test]
    fn test_display_width_ansi_and_unicode() {
        // Colored CJK text
        let colored_cjk = "\x1b[32m日本語\x1b[0m";
        assert_eq!(display_width(colored_cjk), 6); // 3 CJK chars * 2 width
    }

    #[test]
    fn test_wrapped_line_count_with_ansi() {
        // 80 'x' chars with color codes should still be 1 line
        let colored = format!("\x1b[31m{}\x1b[0m", "x".repeat(80));
        assert_eq!(wrapped_line_count(&colored, 80), 1);
    }

    // ========================================================================
    // wrapped_line_count tests
    // ========================================================================

    #[test]
    fn test_wrapped_lines_empty() {
        assert_eq!(wrapped_line_count("", 80), 0);
    }

    #[test]
    fn test_wrapped_lines_short() {
        assert_eq!(wrapped_line_count("hello", 80), 1);
    }

    #[test]
    fn test_wrapped_lines_exact_width() {
        let line = "x".repeat(80);
        assert_eq!(wrapped_line_count(&line, 80), 1);
    }

    #[test]
    fn test_wrapped_lines_overflow_by_one() {
        let line = "x".repeat(81);
        assert_eq!(wrapped_line_count(&line, 80), 2);
    }

    #[test]
    fn test_wrapped_lines_double_width() {
        let line = "x".repeat(160);
        assert_eq!(wrapped_line_count(&line, 80), 2);
    }

    #[test]
    fn test_wrapped_lines_with_newlines() {
        assert_eq!(wrapped_line_count("abc\ndef", 80), 2);
        assert_eq!(wrapped_line_count("a\nb\nc", 80), 3);
        assert_eq!(wrapped_line_count("line1\nline2\nline3", 80), 3);
    }

    #[test]
    fn test_wrapped_lines_newline_and_wrap() {
        // First line wraps to 2, second line is 1
        let content = format!("{}\nshort", "x".repeat(160));
        assert_eq!(wrapped_line_count(&content, 80), 3);
    }

    #[test]
    fn test_wrapped_lines_unicode_wrap() {
        // 40 CJK chars = 80 columns = exactly 1 line on 80-col terminal
        let line = "日".repeat(40);
        assert_eq!(wrapped_line_count(&line, 80), 1);

        // 41 CJK chars = 82 columns = 2 lines on 80-col terminal
        let line = "日".repeat(41);
        assert_eq!(wrapped_line_count(&line, 80), 2);
    }

    #[test]
    fn test_wrapped_lines_zero_width() {
        // Edge case: zero-width terminal
        assert_eq!(wrapped_line_count("hello", 0), 1);
    }

    #[test]
    fn test_wrapped_lines_width_one() {
        // 1-column terminal: each character is a line
        assert_eq!(wrapped_line_count("abc", 1), 3);
    }

    // ========================================================================
    // truncate_string tests
    // ========================================================================

    #[test]
    fn test_truncate_string_fits() {
        let result = truncate_string("hello", 10);
        assert_eq!(result.content, "hello");
        assert!(!result.was_truncated());
        assert_eq!(result.chars_removed, 0);
    }

    #[test]
    fn test_truncate_string_exact() {
        let result = truncate_string("hello", 5);
        assert_eq!(result.content, "hello");
        assert!(!result.was_truncated());
    }

    #[test]
    fn test_truncate_string_exceeds() {
        let result = truncate_string("hello world", 8);
        assert!(result.was_truncated());
        assert!(display_width(&result.content) <= 8);
        assert!(result.chars_removed > 0);
    }

    #[test]
    fn test_truncate_string_very_short_max() {
        let result = truncate_string("hello", 3);
        assert!(display_width(&result.content) <= 3);
        // Content should be truncated to fit
    }

    #[test]
    fn test_truncate_string_unicode() {
        // "日本語" is 6 columns
        let result = truncate_string("日本語test", 8);
        assert!(display_width(&result.content) <= 8);
    }


    // ========================================================================
    // SlotState tests
    // ========================================================================

    #[test]
    fn test_slot_state_pending() {
        let slot = SlotState::Pending;
        assert!(slot.is_pending_or_streaming());
        assert_eq!(slot.char_count(), 0);
    }

    #[test]
    fn test_slot_state_streaming() {
        let slot = SlotState::Streaming {
            chars: 5,
            content: "test".to_string(),
        };
        assert!(slot.is_pending_or_streaming());
        assert_eq!(slot.char_count(), 5);
    }

    #[test]
    fn test_slot_state_complete() {
        let slot = SlotState::Complete { chars: 0, command: "ls -la".to_string() };
        assert!(!slot.is_pending_or_streaming());
        assert_eq!(slot.char_count(), 0);
    }

    #[test]
    fn test_slot_state_error() {
        let slot = SlotState::Error("connection failed".to_string());
        assert!(!slot.is_pending_or_streaming());
        assert_eq!(slot.char_count(), 0);
    }

    #[test]
    fn test_slot_state_waiting() {
        let slot = SlotState::Waiting { attempt: 2, delay_ms: 4000 };
        // Waiting counts as pending/streaming for progress tracking
        assert!(slot.is_pending_or_streaming());
        assert_eq!(slot.char_count(), 0);
    }

    #[test]
    fn test_slot_state_retrying() {
        let slot = SlotState::Retrying { attempt: 3 };
        // Retrying counts as pending/streaming for progress tracking
        assert!(slot.is_pending_or_streaming());
        assert_eq!(slot.char_count(), 0);
    }

    // ========================================================================
    // DisplayMode selection tests
    // ========================================================================

    #[test]
    fn test_display_mode_full_small_content() {
        let slots = vec![
            SlotState::Complete { chars: 0, command: "ls".to_string() },
            SlotState::Complete { chars: 0, command: "pwd".to_string() },
        ];
        let config = SuggestPreviewConfig {
            term_width: 80,
            term_height: 24,
            elapsed_secs: 1.0,
            spinner_idx: 0,
            max_preview_mode: PreviewMode::Full,
        };
        assert_eq!(determine_display_mode(&slots, &config), DisplayMode::Full);
    }

    #[test]
    fn test_display_mode_compact_medium_content() {
        // Create slots with long commands that would wrap in Full mode
        let slots: Vec<SlotState> = (0..5)
            .map(|i| SlotState::Complete { chars: 0, command: format!("command_{} with a very long argument list that will wrap multiple times on an 80 column terminal display", i) })
            .collect();
        let config = SuggestPreviewConfig {
            term_width: 80,
            term_height: 12, // Small terminal
            elapsed_secs: 1.0,
            spinner_idx: 0,
            max_preview_mode: PreviewMode::Full,
        };
        assert_eq!(determine_display_mode(&slots, &config), DisplayMode::Compact);
    }

    #[test]
    fn test_display_mode_minimal_tiny_terminal() {
        let slots = vec![
            SlotState::Pending,
            SlotState::Pending,
            SlotState::Pending,
            SlotState::Pending,
            SlotState::Pending,
            SlotState::Pending,
            SlotState::Pending,
            SlotState::Pending,
            SlotState::Pending,
            SlotState::Pending,
        ];
        let config = SuggestPreviewConfig {
            term_width: 80,
            term_height: 8, // Very small terminal, can't fit 10 slots
            elapsed_secs: 1.0,
            spinner_idx: 0,
            max_preview_mode: PreviewMode::Full,
        };
        assert_eq!(determine_display_mode(&slots, &config), DisplayMode::Minimal);
    }


    // ========================================================================
    // append_chunk tests
    // ========================================================================

    #[test]
    fn test_append_chunk_to_empty() {
        let mut chunks = Vec::new();
        append_chunk(&mut chunks, ChunkType::Content, "hello".to_string());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_type, ChunkType::Content);
        assert_eq!(chunks[0].text, "hello");
    }

    #[test]
    fn test_append_chunk_same_type_merges() {
        let mut chunks = vec![StreamChunk {
            chunk_type: ChunkType::Content,
            text: "hello".to_string(),
        }];
        append_chunk(&mut chunks, ChunkType::Content, " world".to_string());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "hello world");
    }

    #[test]
    fn test_append_chunk_different_type_creates_new() {
        let mut chunks = vec![StreamChunk {
            chunk_type: ChunkType::Preamble,
            text: "thinking...".to_string(),
        }];
        append_chunk(&mut chunks, ChunkType::Content, "{\"data\":".to_string());
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chunk_type, ChunkType::Preamble);
        assert_eq!(chunks[1].chunk_type, ChunkType::Content);
    }

    #[test]
    fn test_append_chunk_alternating_types() {
        let mut chunks = Vec::new();
        append_chunk(&mut chunks, ChunkType::Preamble, "pre1".to_string());
        append_chunk(&mut chunks, ChunkType::Content, "content".to_string());
        append_chunk(&mut chunks, ChunkType::Preamble, "pre2".to_string());
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].text, "pre1");
        assert_eq!(chunks[1].text, "content");
        assert_eq!(chunks[2].text, "pre2");
    }

    // ========================================================================
    // Regression tests for line counting with newlines
    // ========================================================================

    #[test]
    fn test_wrapped_line_count_trailing_newline() {
        // A string with a trailing newline should count the same as without
        // because .lines() doesn't include a trailing empty element
        assert_eq!(wrapped_line_count("hello", 80), 1);
        assert_eq!(wrapped_line_count("hello\n", 80), 1);
        // But embedded newlines DO add lines
        assert_eq!(wrapped_line_count("hello\nworld", 80), 2);
        assert_eq!(wrapped_line_count("hello\nworld\n", 80), 2);
    }

    // ========================================================================
    // VirtualBuffer rendering tests
    // ========================================================================

    #[test]
    fn test_render_explain_to_buffer_empty() {
        let config = ExplainPreviewConfig {
            term_width: 80,
            term_height: 24,
            elapsed_secs: 1.0,
            spinner_idx: 0,
            char_count: 0,
            status: None,
            max_preview_mode: PreviewMode::Full,
            thinking: ThinkingPhase::None,
        };
        let (buffer, regions) = render_explain_to_buffer(&[], &config);

        // Should have header region only
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].start_row, 0);
        assert_eq!(regions[0].end_row, 1);

        // Buffer should have content
        assert!(buffer.row(0).is_some());
    }

    #[test]
    fn test_render_explain_to_buffer_with_content() {
        let config = ExplainPreviewConfig {
            term_width: 80,
            term_height: 24,
            elapsed_secs: 1.0,
            spinner_idx: 0,
            char_count: 10,
            status: None,
            max_preview_mode: PreviewMode::Full,
            thinking: ThinkingPhase::None,
        };
        let chunks = vec![
            StreamChunk { chunk_type: ChunkType::Content, text: "test content".to_string() },
        ];
        let (buffer, regions) = render_explain_to_buffer(&chunks, &config);

        // Should have: header, top separator, content, bottom separator
        assert!(regions.len() >= 3);

        // First region should be header row
        assert_eq!(regions[0].start_row, 0);
        assert_eq!(regions[0].end_row, 1);

        // Content region should exist (starts at row 2)
        let has_content = regions.iter().any(|r| r.start_row == 2);
        assert!(has_content, "Should have a content region");

        // Check buffer has the content
        let cell = buffer.row(2).map(|r| &r.cells[0]);
        assert!(cell.is_some());
    }

    #[test]
    fn test_render_explain_truncation_preserves_separators() {
        let config = ExplainPreviewConfig {
            term_width: 40,
            term_height: 5,
            elapsed_secs: 1.0,
            spinner_idx: 0,
            char_count: 10,
            status: None,
            max_preview_mode: PreviewMode::Full,
            thinking: ThinkingPhase::None,
        };
        // Content must exceed available_lines (term_height - CHROME_LINES = 2)
        // at this width, so >80 chars to wrap to 3+ lines.
        let chunks = vec![
            StreamChunk {
                chunk_type: ChunkType::Content,
                text: "a".repeat(100), // wraps to 3 lines at width 40, triggers truncation
            },
        ];
        let (buffer, regions) = render_explain_to_buffer(&chunks, &config);

        let has_top_separator = regions.iter().any(|r| {
            r.start_row == 1 && r.end_row == 2
        });
        assert!(has_top_separator, "top separator should exist");

        let bottom_row = config.term_height - 1;
        let has_bottom_separator = regions.iter().any(|r| {
            r.start_row == bottom_row && r.end_row == bottom_row + 1
        });
        assert!(has_bottom_separator, "bottom separator should exist");

        let top_cell = &buffer.row(1).expect("top separator row exists").cells[0];
        assert_eq!(top_cell.ch, Some(SEPARATOR_CHAR));

        let bottom_row_cells = &buffer
            .row(bottom_row)
            .expect("bottom separator row exists")
            .cells;
        let bottom_left = &bottom_row_cells[1];
        let bottom_right = &bottom_row_cells[5];
        assert_eq!(bottom_left.ch, Some('┤'));
        assert_eq!(bottom_right.ch, Some('├'));
    }

    #[test]
    fn test_render_explain_tiny_terminal_wraps_header() {
        // On very narrow terminals, the header wraps into subsequent rows
        // instead of clipping. Verify this doesn't panic and produces output.
        let config = ExplainPreviewConfig {
            term_width: 8,
            term_height: 10,
            elapsed_secs: 1.0,
            spinner_idx: 0,
            char_count: 10,
            status: None,
            max_preview_mode: PreviewMode::Full,
            thinking: ThinkingPhase::None,
        };
        let chunks = vec![StreamChunk {
            chunk_type: ChunkType::Content,
            text: "hello".to_string(),
        }];
        let (buffer, regions) = render_explain_to_buffer(&chunks, &config);

        // Header should start on row 0
        let first_cell = &buffer.row(0).unwrap().cells[0];
        assert_eq!(first_cell.ch, Some(SPINNER_CHARS[0]));

        // Should still produce regions
        assert!(!regions.is_empty());
    }

    #[test]
    fn test_render_suggest_tiny_terminal_wraps_header() {
        let config = SuggestPreviewConfig {
            term_width: 8,
            term_height: 10,
            elapsed_secs: 1.0,
            spinner_idx: 0,
            max_preview_mode: PreviewMode::Full,
        };
        let slots = vec![SlotState::Pending, SlotState::Pending];
        let (buffer, regions) = render_suggest_to_buffer(&slots, &config);

        // Header should start on row 0
        let first_cell = &buffer.row(0).unwrap().cells[0];
        assert_eq!(first_cell.ch, Some(SPINNER_CHARS[0]));

        // Should still produce regions
        assert!(!regions.is_empty());
    }

    #[test]
    fn test_render_suggest_to_buffer() {
        let config = SuggestPreviewConfig {
            term_width: 80,
            term_height: 24,
            elapsed_secs: 1.0,
            spinner_idx: 0,
            max_preview_mode: PreviewMode::Full,
        };
        let slots = vec![
            SlotState::Complete { chars: 5, command: "ls -la".to_string() },
            SlotState::Pending,
        ];
        let (buffer, regions) = render_suggest_to_buffer(&slots, &config);

        // Should have regions
        assert!(!regions.is_empty());

        // Buffer should have content
        assert!(buffer.row(0).is_some());
    }

    // ========================================================================
    // slot_prefix / slot_content tests
    // ========================================================================

    /// Extract plain text from a VirtualBuffer row, trimming trailing spaces.
    fn buffer_row_text(buffer: &VirtualBuffer, row: usize) -> String {
        match buffer.row(row) {
            Some(r) => r.cells.iter()
                .filter(|c| !c.is_continuation)
                .map(|c| c.ch.unwrap_or(' '))
                .collect::<String>()
                .trim_end()
                .to_string(),
            None => String::new(),
        }
    }

    #[test]
    fn test_slot_prefix_all_states() {
        let spinner = SPINNER_CHARS[0]; // ⠋

        assert_eq!(
            slot_prefix(0, &SlotState::Pending, 0),
            format!("[1] {} (pending)", spinner),
        );
        assert_eq!(
            slot_prefix(0, &SlotState::Streaming { chars: 42, content: "x".into() }, 0),
            format!("[1] {} (42 chars) ", spinner),
        );
        assert_eq!(
            slot_prefix(0, &SlotState::Complete { chars: 5, command: "ls".into() }, 0),
            "[1] ✓ (5 chars) ",
        );
        assert_eq!(
            slot_prefix(0, &SlotState::Error("err".into()), 0),
            "[1] ✗ ",
        );
        assert_eq!(
            slot_prefix(0, &SlotState::Waiting { attempt: 2, delay_ms: 4000 }, 0),
            format!("[1] {} (backoff #2, 4.0s)", spinner),
        );
        assert_eq!(
            slot_prefix(0, &SlotState::Retrying { attempt: 3 }, 0),
            format!("[1] {} (retry #3)", spinner),
        );
    }

    #[test]
    fn test_slot_prefix_multi_digit_index() {
        let spinner = SPINNER_CHARS[0];
        // idx=9 → slot number 10
        assert_eq!(
            slot_prefix(9, &SlotState::Pending, 0),
            format!("[10] {} (pending)", spinner),
        );
        assert_eq!(
            slot_prefix(9, &SlotState::Complete { chars: 7, command: "ls".into() }, 0),
            "[10] ✓ (7 chars) ",
        );
        // idx=99 → slot number 100
        assert_eq!(
            slot_prefix(99, &SlotState::Streaming { chars: 1, content: String::new() }, 0),
            format!("[100] {} (1 chars) ", spinner),
        );
    }

    #[test]
    fn test_slot_content_extraction() {
        assert_eq!(slot_content(&SlotState::Pending), "");
        assert_eq!(slot_content(&SlotState::Waiting { attempt: 1, delay_ms: 1000 }), "");
        assert_eq!(slot_content(&SlotState::Retrying { attempt: 1 }), "");
        assert_eq!(
            slot_content(&SlotState::Streaming { chars: 5, content: "hello".into() }),
            "hello",
        );
        assert_eq!(
            slot_content(&SlotState::Complete { chars: 5, command: "ls -la".into() }),
            "ls -la",
        );
        assert_eq!(
            slot_content(&SlotState::Error("broken".into())),
            "broken",
        );
    }

    #[test]
    fn test_slot_prefix_width_accounts_for_multi_digit() {
        let single = slot_prefix(0, &SlotState::Pending, 0);   // [1]
        let double = slot_prefix(9, &SlotState::Pending, 0);   // [10]
        let triple = slot_prefix(99, &SlotState::Pending, 0);  // [100]

        assert_eq!(display_width(&double) - display_width(&single), 1);
        assert_eq!(display_width(&triple) - display_width(&single), 2);
    }

    // ========================================================================
    // Rendering consistency tests
    // ========================================================================

    #[test]
    fn test_complete_slot_renders_char_count() {
        let slot = SlotState::Complete { chars: 42, command: "ls -la".into() };
        let mut buffer = VirtualBuffer::new(80, 1);
        write_slot_to_buffer(&mut buffer, 0, &slot, 0, false, 80);

        let text = buffer_row_text(&buffer, 0);
        assert!(text.contains("(42 chars)"), "expected char count in: {}", text);
        assert!(text.contains("✓"), "expected checkmark in: {}", text);
        assert!(text.contains("ls -la"), "expected command in: {}", text);
    }

    #[test]
    fn test_streaming_slot_renders_content() {
        let slot = SlotState::Streaming { chars: 10, content: "cmake --build".into() };
        let mut buffer = VirtualBuffer::new(80, 1);
        write_slot_to_buffer(&mut buffer, 0, &slot, 0, false, 80);

        let text = buffer_row_text(&buffer, 0);
        assert!(text.contains("(10 chars)"), "expected char count in: {}", text);
        assert!(text.contains("cmake --build"), "expected content in: {}", text);
    }

    #[test]
    fn test_compact_truncation_preserves_prefix() {
        let long_cmd = "a".repeat(200);
        let slot = SlotState::Complete { chars: 5, command: long_cmd.clone() };
        let mut buffer = VirtualBuffer::new(60, 1);
        write_slot_to_buffer(&mut buffer, 0, &slot, 0, true, 60);

        let text = buffer_row_text(&buffer, 0);
        // Prefix should be intact
        assert!(text.starts_with("[1] ✓ (5 chars)"), "prefix missing in: {}", text);
        // Content should be truncated (total <= 60 chars)
        assert!(display_width(&text) <= 60, "text too wide: {}", display_width(&text));
        // Should end with ellipsis
        assert!(text.contains("···"), "expected ellipsis in: {}", text);
    }

    #[test]
    fn test_compact_truncation_multi_digit_slot() {
        let long_cmd = "b".repeat(200);
        let slot = SlotState::Complete { chars: 5, command: long_cmd };

        // Single-digit slot
        let mut buf1 = VirtualBuffer::new(60, 1);
        write_slot_to_buffer(&mut buf1, 0, &slot, 0, true, 60);
        let text1 = buffer_row_text(&buf1, 0);

        // Double-digit slot — prefix is 1 char wider, so content should be 1 char shorter
        let mut buf2 = VirtualBuffer::new(60, 1);
        write_slot_to_buffer(&mut buf2, 9, &slot, 0, true, 60);
        let text2 = buffer_row_text(&buf2, 0);

        assert!(text2.starts_with("[10]"), "expected [10] prefix in: {}", text2);
        // Both should fit within terminal width
        assert!(display_width(&text1) <= 60);
        assert!(display_width(&text2) <= 60);
        // The double-digit version should have less content (or equal if both max out)
        let content1_len = text1.find("···").unwrap_or(text1.len());
        let content2_len = text2.find("···").unwrap_or(text2.len());
        assert!(content2_len <= content1_len + 1, "multi-digit slot should truncate more");
    }

    #[test]
    fn test_display_mode_multi_digit_slots() {
        // Regression test for the [_] bug: 10 slots with short commands should
        // fit in Full mode on a 24-line terminal (10 slots + 3 chrome = 13 lines).
        let slots: Vec<SlotState> = (0..10)
            .map(|_| SlotState::Complete { chars: 5, command: "ls".into() })
            .collect();
        let config = SuggestPreviewConfig {
            term_width: 80,
            term_height: 24,
            elapsed_secs: 1.0,
            spinner_idx: 0,
            max_preview_mode: PreviewMode::Full,
        };
        assert_eq!(determine_display_mode(&slots, &config), DisplayMode::Full);
    }

    #[test]
    fn test_line_count_matches_rendering() {
        // Verify that the line count computed by determine_display_mode's logic
        // matches the actual rows consumed by write_slot_to_buffer.
        let slots = vec![
            SlotState::Complete { chars: 10, command: "short".into() },
            SlotState::Streaming { chars: 5, content: "x".repeat(200) },
            SlotState::Error("something went wrong with a long error message".into()),
        ];
        let width: usize = 40;

        for (i, slot) in slots.iter().enumerate() {
            let text = format!("{}{}", slot_prefix(i, slot, 0), slot_content(slot));
            let predicted = wrapped_line_count(&text, width);

            let mut buffer = VirtualBuffer::new(width as u16, predicted as u16 + 1);
            write_slot_to_buffer(&mut buffer, i, slot, 0, false, width);
            let rendered = buffer.cursor_row() + 1; // cursor is 0-indexed

            assert_eq!(
                predicted, rendered,
                "slot {} ({:?}): predicted {} lines but rendered {}",
                i, std::mem::discriminant(slot), predicted, rendered,
            );
        }
    }
}
