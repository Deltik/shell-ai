//! Progress indicator for shell-ai using indicatif.
//!
//! Shows a spinner with elapsed time in deciseconds during slow operations.
//! Only displays when stderr is a terminal.

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use is_terminal::IsTerminal;
use std::sync::Mutex;
use std::time::Duration;

/// Global active progress bar for coordination with the logger.
/// When set, the logger will suspend this bar before printing.
static ACTIVE_BAR: Mutex<Option<ProgressBar>> = Mutex::new(None);

/// Execute a closure while any active progress bar is suspended.
/// This should be called by the logger to avoid output conflicts.
pub fn with_suspended<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let guard = ACTIVE_BAR.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(ref bar) = *guard {
        bar.suspend(f)
    } else {
        f()
    }
}

/// A progress indicator that shows a spinner with elapsed time.
///
/// Example output: `⠹ Generating suggestions... 2.3s`
pub struct Progress {
    bar: ProgressBar,
}

impl Progress {
    /// Create a new progress indicator with the given message.
    ///
    /// Returns `None` if stderr is not a terminal (e.g., piped output).
    pub fn new(message: &str) -> Option<Self> {
        if !std::io::stderr().is_terminal() {
            return None;
        }

        let bar = ProgressBar::new_spinner();
        bar.set_draw_target(ProgressDrawTarget::stderr());

        // Style: spinner + message + elapsed time
        let style = ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg} {elapsed:.dim}")
            .expect("Invalid progress template")
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]);

        bar.set_style(style);
        bar.set_message(message.to_string());

        // Tick every 100ms for smooth animation and decisecond updates
        bar.enable_steady_tick(Duration::from_millis(100));

        // Register as the active progress bar
        *ACTIVE_BAR.lock().unwrap_or_else(|e| e.into_inner()) = Some(bar.clone());

        Some(Self { bar })
    }

    /// Update the progress message.
    pub fn set_message(&self, message: &str) {
        self.bar.set_message(message.to_string());
    }

    /// Finish the progress indicator and clear it from the terminal.
    ///
    /// Call this before printing results to avoid visual artifacts.
    pub fn finish_and_clear(&self) {
        // Unregister before finishing
        *ACTIVE_BAR.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.bar.finish_and_clear();
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        // Unregister on drop
        *ACTIVE_BAR.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.bar.finish_and_clear();
    }
}