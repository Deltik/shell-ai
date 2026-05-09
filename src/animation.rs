//! Shared animation constants, timing helpers, and rendering primitives.
//!
//! Animation speeds are defined as durations in milliseconds, decoupled from
//! the render frame rate.  Both `preview.rs` and `shimmer.rs` derive per-frame
//! state from elapsed time using these helpers, so changing
//! [`RENDER_INTERVAL_MS`] adjusts the refresh rate without altering animation
//! speed.

use crate::render::{Color, Style, VirtualBuffer};

// ---------------------------------------------------------------------------
// Animation assets
// ---------------------------------------------------------------------------

/// Spinner characters for animation.
pub const SPINNER_CHARS: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Bouncing dots animation for the truncation indicator.
/// Each frame is 3 columns wide.
pub const ELLIPSIS_FRAMES: &[&str] = &["·  ", "·· ", "···", " ··", "  ·"];

// ---------------------------------------------------------------------------
// Time-based animation periods (milliseconds)
// ---------------------------------------------------------------------------

/// Interval between render frames.
pub const RENDER_INTERVAL_MS: u64 = 16;

/// Time per spinner character change.
pub const SPINNER_STEP_MS: u64 = 80;

/// Duration of one full shimmer traversal.
pub const SHIMMER_CYCLE_MS: u64 = 1600;

/// Time per ellipsis animation step.
pub const ELLIPSIS_STEP_MS: u64 = 320;

/// Time per thinking-pulse step.
pub const THINKING_STEP_MS: u64 = 80;

// ---------------------------------------------------------------------------
// Timing helpers
// ---------------------------------------------------------------------------

/// Current spinner character for the given elapsed time.
pub fn spinner_char(elapsed_ms: u64) -> char {
    SPINNER_CHARS[(elapsed_ms / SPINNER_STEP_MS) as usize % SPINNER_CHARS.len()]
}

/// Shimmer wave position for the given elapsed time and text length.
///
/// The travel distance and entry/exit margins are derived from
/// [`flourish_near_radius`] so the wave has room to fully fade in and out
/// regardless of how wide the glow is.
///
/// A sine ease-in-out curve makes the wave enter slowly, accelerate through
/// the middle of the text, and decelerate as it exits — giving the animation
/// a subtle breathing quality.  The margin is one position past the
/// near-glow boundary so the slow ease-in/out lingers in the dim
/// off-screen zone instead of pausing visibly on the first/last character.
pub fn shimmer_pos(elapsed_ms: u64, text_len: usize) -> isize {
    let margin = flourish_near_radius(text_len) + 1;
    let travel = text_len + 2 * margin;
    let t = (elapsed_ms % SHIMMER_CYCLE_MS) as f64 / SHIMMER_CYCLE_MS as f64;
    let eased = (1.0 - (t * std::f64::consts::PI).cos()) / 2.0;
    (eased * travel as f64) as isize - margin as isize
}

/// Current animated-ellipsis frame for the given elapsed time.
pub fn animated_ellipsis(elapsed_ms: u64) -> &'static str {
    ELLIPSIS_FRAMES[(elapsed_ms / ELLIPSIS_STEP_MS) as usize % ELLIPSIS_FRAMES.len()]
}

/// Thinking-pulse phase (0–7) for the given elapsed time.
pub fn thinking_pulse_phase(elapsed_ms: u64) -> usize {
    (elapsed_ms / THINKING_STEP_MS) as usize % 8
}

/// Number of unique frames before the animation cycle repeats, given a frame
/// interval in milliseconds.
///
/// This is `lcm(SPINNER_PERIOD, SHIMMER_CYCLE, ELLIPSIS_PERIOD) / interval`.
pub fn cycle_frame_count(frame_interval_ms: u64) -> usize {
    let spinner_period = SPINNER_STEP_MS * SPINNER_CHARS.len() as u64;
    let ellipsis_period = ELLIPSIS_STEP_MS * ELLIPSIS_FRAMES.len() as u64;
    let full_cycle = lcm(lcm(spinner_period, SHIMMER_CYCLE_MS), ellipsis_period);
    (full_cycle / frame_interval_ms) as usize
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

fn lcm(a: u64, b: u64) -> u64 {
    a / gcd(a, b) * b
}

// ---------------------------------------------------------------------------
// Rendering primitives (shared by preview.rs and shimmer.rs)
// ---------------------------------------------------------------------------

/// Near-glow radius scaled logarithmically with text length.
///
/// Returns 2 for short text (≤ 14 chars, matching the original hard-coded
/// behavior), then grows gently: 3 at ~20 chars, 4 at ~55 chars, 5 at ~148.
pub fn flourish_near_radius(text_len: usize) -> usize {
    if text_len < 2 {
        return 2;
    }
    ((text_len as f64).ln() as usize).max(2)
}

/// Compute the flourish style for a character at `dist` positions from the
/// wave center.  The glow radius scales logarithmically with `text_len` so
/// short labels get a tight highlight while long prompts get a wider sweep.
pub fn flourish_style(dist: usize, text_len: usize) -> Style {
    let near_radius = flourish_near_radius(text_len);
    let highlight_radius = near_radius / 3;

    if dist <= highlight_radius {
        Style {
            bold: true,
            fg: Some(Color::BrightCyan),
            ..Default::default()
        }
    } else if dist <= near_radius {
        Style::fg(Color::Cyan)
    } else {
        Style {
            dim: true,
            fg: Some(Color::Cyan),
            ..Default::default()
        }
    }
}

/// Write text with a waving color flourish in cyan to a [`VirtualBuffer`].
///
/// The wave travels linearly (no wrap-around): the highlight fades in from the
/// left, traverses the text, fades out to the right, then pauses briefly before
/// the next cycle.  `pos` can be negative (wave approaching from the left).
pub fn write_flourish(buffer: &mut VirtualBuffer, text: &str, pos: isize) {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    for (j, &ch) in chars.iter().enumerate() {
        let dist = (j as isize - pos).unsigned_abs();
        buffer.set_style(flourish_style(dist, len));
        buffer.write_char(ch);
    }
    buffer.reset_style();
}

/// Convert a [`Style`] to an SGR escape sequence string (e.g. `"\x1b[0;1;96m"`).
///
/// Always emits a reset (SGR 0) prefix so each call is self-contained.
pub fn style_to_sgr(style: &Style) -> String {
    let mut codes: Vec<u8> = vec![0]; // reset first
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
        codes.push(match fg {
            Color::Red => 31,
            Color::Green => 32,
            Color::Yellow => 33,
            Color::Cyan => 36,
            Color::BrightCyan => 96,
        });
    }
    if let Some(bg) = &style.bg {
        codes.push(match bg {
            Color::Red => 41,
            Color::Green => 42,
            Color::Yellow => 43,
            Color::Cyan => 46,
            Color::BrightCyan => 106,
        });
    }
    let mut out = String::from("\x1b[");
    for (i, code) in codes.iter().enumerate() {
        if i > 0 {
            out.push(';');
        }
        out.push_str(&code.to_string());
    }
    out.push('m');
    out
}