//! Pre-computed shimmer animation for shell integration keybindings.
//!
//! The `_shimmer` hidden subcommand outputs shell variable assignments that
//! contain pre-computed animation frames.  Shell integration scripts `eval`
//! this output and then loop over the frames with a simple `sleep` + `printf`.
//!
//! Each frame is rendered into a [`VirtualBuffer`] using the same
//! [`animation::write_flourish`] routine as the native preview, then
//! diff-encoded via [`diff_row_cells`] so only changed columns are emitted
//! (using CHA positioning), wrapped in BSU/ESU for tear-free display.

use crate::animation;
use crate::render::{diff_row_cells, Color, Row, Style, VirtualBuffer};
use clap::{Parser, ValueEnum};
use unicode_width::UnicodeWidthChar;

// ---------------------------------------------------------------------------
// ANSI constants
// ---------------------------------------------------------------------------

const BSU: &str = "\x1b[?2026h";
const ESU: &str = "\x1b[?2026l";
const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";
const CR_CLEAR_LINE: &str = "\r\x1b[K";

// ---------------------------------------------------------------------------
// CLI arguments
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
pub struct ShimmerArgs {
    /// Target shell for output encoding.
    #[arg(long, value_enum, default_value = "bash")]
    shell: ShellKind,

    /// Terminal width in columns.
    #[arg(long, default_value = "80")]
    cols: usize,

    /// Frame interval in milliseconds (controls refresh rate).
    #[arg(long, default_value_t = animation::RENDER_INTERVAL_MS)]
    frame_interval: u64,

    /// Text to animate.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    text: Vec<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ShellKind {
    Bash,
    Zsh,
    Fish,
    #[clap(name = "powershell")]
    PowerShell,
}

// ---------------------------------------------------------------------------
// VirtualBuffer-based frame rendering
// ---------------------------------------------------------------------------

/// Measure the display width of `text`, respecting unicode character widths.
/// Returns `(total_display_cols, char_count)`.
fn measure_display(text: &str) -> (usize, usize) {
    let mut cols = 0;
    let mut count = 0;
    for ch in text.chars() {
        cols += ch.width().unwrap_or(0).max(1);
        count += 1;
    }
    (cols, count)
}

/// Truncate `text` to fit within `max_cols` display columns.
/// Returns the truncated string.
fn truncate_to_cols(text: &str, max_cols: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0).max(1);
        if used + w > max_cols {
            break;
        }
        out.push(ch);
        used += w;
    }
    out
}

/// Render one animation frame into a single-row [`VirtualBuffer`].
///
/// Uses the same [`animation::write_flourish`] as the native preview so
/// the styling is pixel-identical.
fn render_frame_to_buffer(
    cols: usize,
    display_text: &str,
    display_char_count: usize,
    elapsed_ms: u64,
    truncated: bool,
) -> VirtualBuffer {
    let mut buf = VirtualBuffer::new(cols as u16, 1);

    // Spinner (bold cyan)
    buf.set_style(Style {
        bold: true,
        fg: Some(Color::Cyan),
        ..Default::default()
    });
    buf.write_char(animation::spinner_char(elapsed_ms));
    buf.reset_style();
    buf.write_char(' ');

    // Shimmer flourish — identical code path to preview.rs
    let wave_pos = animation::shimmer_pos(elapsed_ms, display_char_count);
    animation::write_flourish(&mut buf, display_text, wave_pos);

    // Animated ellipsis (if truncated)
    if truncated {
        let ellipsis = animation::animated_ellipsis(elapsed_ms);
        // Style the ellipsis as part of the flourish — its logical position
        // is one past the last visible text character.
        let dist = (display_char_count as isize - wave_pos).unsigned_abs();
        buf.set_style(animation::flourish_style(dist, display_char_count));
        buf.write_str(ellipsis);
        buf.reset_style();
    }

    buf
}

/// Serialize an entire row to an ANSI string (for the initial full render).
fn row_to_ansi(row: &Row) -> String {
    let mut out = String::with_capacity(row.cells.len() * 6);
    out.push_str(HIDE_CURSOR);
    out.push_str(BSU);
    out.push_str(CR_CLEAR_LINE);

    let mut current_style = Style::default();
    let mut first = true;
    for cell in &row.cells {
        if cell.is_continuation {
            continue;
        }
        let ch = match cell.ch {
            Some(c) => c,
            None => break, // rest of row is empty
        };
        if first || cell.style != current_style {
            out.push_str(&animation::style_to_sgr(&cell.style));
            current_style = cell.style;
            first = false;
        }
        out.push(ch);
    }

    out.push_str("\x1b[0m");
    out.push_str(ESU);
    out
}

/// Produce a diff-encoded ANSI string between two rows, using
/// [`diff_row_cells`] from the render module.
fn row_diff_to_ansi(prev: &Row, curr: &Row) -> String {
    let spans = diff_row_cells(prev, curr);
    if spans.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(128);
    out.push_str(BSU);

    for span in &spans {
        // CHA to the start column (1-based)
        out.push_str(&format!("\x1b[{}G", span.start_col + 1));
        let mut current_style: Option<Style> = None;
        for col in span.start_col..span.end_col {
            let cell = &curr.cells[col];
            if cell.is_continuation {
                continue;
            }
            if current_style.map_or(true, |s| s != cell.style) {
                out.push_str(&animation::style_to_sgr(&cell.style));
                current_style = Some(cell.style);
            }
            if let Some(ch) = cell.ch {
                out.push(ch);
            }
        }
    }

    out.push_str("\x1b[0m");
    out.push_str(ESU);
    out
}

// ---------------------------------------------------------------------------
// Shell encoding
// ---------------------------------------------------------------------------

fn shell_escape_bash(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() * 2);
    out.push_str("$'");
    for ch in raw.chars() {
        match ch {
            '\x1b' => out.push_str("\\x1b"),
            '\r' => out.push_str("\\r"),
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    out.push('\'');
    out
}

fn shell_escape_fish(raw: &str) -> String {
    let mut out = String::new();
    let mut in_quote = false;
    for ch in raw.chars() {
        match ch {
            '\x1b' | '\r' => {
                if in_quote {
                    out.push('\'');
                    in_quote = false;
                }
                if ch == '\x1b' {
                    out.push_str("\\x1b");
                } else {
                    out.push_str("\\r");
                }
            }
            '\'' => {
                if in_quote {
                    out.push('\'');
                    in_quote = false;
                }
                out.push_str("\\'");
            }
            '\\' => {
                if !in_quote {
                    out.push('\'');
                    in_quote = true;
                }
                out.push_str("\\\\");
            }
            _ => {
                if !in_quote {
                    out.push('\'');
                    in_quote = true;
                }
                out.push(ch);
            }
        }
    }
    if in_quote {
        out.push('\'');
    }
    if out.is_empty() {
        out.push_str("''");
    }
    out
}

fn shell_escape_powershell(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() * 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '\x1b' => out.push_str("`e"),
            '\r' => out.push_str("`r"),
            '"' => out.push_str("`\""),
            '`' => out.push_str("``"),
            '$' => out.push_str("`$"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn format_output(
    shell: ShellKind,
    init: &str,
    frames: &[String],
    cleanup: &str,
    interval_secs: f64,
) -> String {
    let escape = |s: &str| match shell {
        ShellKind::Bash | ShellKind::Zsh => shell_escape_bash(s),
        ShellKind::Fish => shell_escape_fish(s),
        ShellKind::PowerShell => shell_escape_powershell(s),
    };

    let mut out = String::new();

    match shell {
        ShellKind::Bash | ShellKind::Zsh => {
            out.push_str(&format!("_shai_init={}\n", escape(init)));
            out.push_str("_shai_frames=(");
            for (i, frame) in frames.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                out.push_str(&escape(frame));
            }
            out.push_str(")\n");
            out.push_str(&format!("_shai_n={}\n", frames.len()));
            out.push_str(&format!("_shai_interval={:.4}\n", interval_secs));
            out.push_str(&format!("_shai_cleanup={}\n", escape(cleanup)));
        }
        ShellKind::Fish => {
            out.push_str(&format!("set _shai_init {}\n", escape(init)));
            out.push_str("set _shai_frames");
            for frame in frames {
                out.push(' ');
                out.push_str(&escape(frame));
            }
            out.push('\n');
            out.push_str(&format!("set _shai_n {}\n", frames.len()));
            out.push_str(&format!("set _shai_interval {:.4}\n", interval_secs));
            out.push_str(&format!("set _shai_cleanup {}\n", escape(cleanup)));
        }
        ShellKind::PowerShell => {
            out.push_str(&format!("$_shai_init = {}\n", escape(init)));
            out.push_str("$_shai_frames = @(");
            for (i, frame) in frames.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&escape(frame));
            }
            out.push_str(")\n");
            out.push_str(&format!("$_shai_n = {}\n", frames.len()));
            out.push_str(&format!("$_shai_interval = {:.4}\n", interval_secs));
            out.push_str(&format!("$_shai_cleanup = {}\n", escape(cleanup)));
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run(args: ShimmerArgs) -> anyhow::Result<()> {
    let text = args.text.join(" ");
    let cols = args.cols;
    let interval = args.frame_interval;

    // col 1 = spinner, col 2 = space, remaining = text + optional ellipsis
    let text_budget = cols.saturating_sub(2);

    if text_budget == 0 {
        let cleanup = format!("{CR_CLEAR_LINE}{SHOW_CURSOR}");
        print!(
            "{}",
            format_output(
                args.shell,
                "",
                &[String::new()],
                &cleanup,
                interval as f64 / 1000.0,
            )
        );
        return Ok(());
    }

    // Measure display width and decide on truncation.
    let (full_width, _) = measure_display(&text);
    let truncated = full_width > text_budget;
    let max_text_cols = if truncated {
        text_budget.saturating_sub(3) // reserve 3 for animated ellipsis
    } else {
        text_budget
    };

    let display_text = truncate_to_cols(&text, max_text_cols);
    let (_, display_char_count) = measure_display(&display_text);

    // Compute cycle length and render all frames into VirtualBuffers
    let n_frames = animation::cycle_frame_count(interval);
    let mut rows: Vec<Row> = Vec::with_capacity(n_frames);
    for i in 0..n_frames {
        let elapsed_ms = i as u64 * interval;
        let buf = render_frame_to_buffer(
            cols,
            &display_text,
            display_char_count,
            elapsed_ms,
            truncated,
        );
        rows.push(buf.row(0).unwrap().clone());
    }

    // Frame 0: full render (init)
    let init = row_to_ansi(&rows[0]);

    // Diff frames: each frame's diff from the previous one
    let mut diff_frames: Vec<String> = Vec::with_capacity(n_frames);
    for i in 0..n_frames {
        let prev_idx = if i == 0 { n_frames - 1 } else { i - 1 };
        diff_frames.push(row_diff_to_ansi(&rows[prev_idx], &rows[i]));
    }

    let cleanup = format!("{CR_CLEAR_LINE}{SHOW_CURSOR}");
    let output = format_output(
        args.shell,
        &init,
        &diff_frames,
        &cleanup,
        interval as f64 / 1000.0,
    );
    print!("{}", output);

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_measure_display_ascii() {
        let (cols, count) = measure_display("Hello");
        assert_eq!(cols, 5);
        assert_eq!(count, 5);
    }

    #[test]
    fn test_measure_display_wide() {
        let (cols, count) = measure_display("中文");
        assert_eq!(cols, 4); // 2 + 2
        assert_eq!(count, 2);
    }

    #[test]
    fn test_truncate_to_cols() {
        assert_eq!(truncate_to_cols("Hello, world!", 5), "Hello");
        assert_eq!(truncate_to_cols("中文AB", 5), "中文A"); // 2+2+1 = 5
    }

    #[test]
    fn test_render_frame_to_buffer_basic() {
        let buf = render_frame_to_buffer(20, "Hi", 2, 0, false);
        let row = buf.row(0).unwrap();
        // First cell should be spinner
        assert_eq!(row.cells[0].ch, Some(animation::spinner_char(0)));
        // Second cell should be space
        assert_eq!(row.cells[1].ch, Some(' '));
        // Third cell should be 'H'
        assert_eq!(row.cells[2].ch, Some('H'));
    }

    #[test]
    fn test_row_to_ansi_contains_bsu_esu() {
        let buf = render_frame_to_buffer(20, "X", 1, 0, false);
        let row = buf.row(0).unwrap();
        let ansi = row_to_ansi(row);
        assert!(ansi.contains(BSU));
        assert!(ansi.contains(ESU));
        assert!(ansi.contains(HIDE_CURSOR));
    }

    #[test]
    fn test_row_diff_skips_unchanged() {
        let buf_a = render_frame_to_buffer(20, "Hello", 5, 0, false);
        let buf_b = render_frame_to_buffer(20, "Hello", 5, 80, false);
        let row_a = buf_a.row(0).unwrap();
        let row_b = buf_b.row(0).unwrap();
        let diff = row_diff_to_ansi(row_a, row_b);
        // Diff should exist (spinner changed) but be shorter than full render
        assert!(diff.len() < row_to_ansi(row_b).len());
    }

    #[test]
    fn test_shell_escape_bash() {
        assert_eq!(shell_escape_bash("hello"), "$'hello'");
        assert_eq!(shell_escape_bash("\x1b[1m"), "$'\\x1b[1m'");
        assert_eq!(shell_escape_bash("it's"), "$'it\\'s'");
    }

    #[test]
    fn test_shell_escape_powershell() {
        let result = shell_escape_powershell("\x1b[1m");
        assert_eq!(result, "\"`e[1m\"");
    }

    #[test]
    fn test_cycle_frame_count() {
        assert_eq!(animation::cycle_frame_count(80), 20);
        assert_eq!(animation::cycle_frame_count(40), 40);
    }
}