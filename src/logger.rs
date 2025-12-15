//! Custom logger for shell-ai with colored stderr output.
//!
//! Provides visual distinction for different log levels:
//! - ERROR: red bold [error]
//! - WARN: yellow [warn]
//! - INFO: cyan [info]
//! - DEBUG: dimmed [debug] (only with --debug or SHAI_DEBUG=true)
//! - TRACE: dimmed [trace] (only with --debug or SHAI_DEBUG=true)

use crate::config::DebugLevel;
use colored::{Color, Colorize};
use is_terminal::IsTerminal;
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

/// Global logger instance
static LOGGER: ShellAiLogger = ShellAiLogger;

/// Flag to track if debug mode is enabled (can be updated after init)
static DEBUG_MODE: AtomicBool = AtomicBool::new(false);

/// Guard to ensure logger is only initialized once
static INIT: Once = Once::new();

/// Custom logger that outputs colored messages to stderr
struct ShellAiLogger;

impl Log for ShellAiLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        let debug = DEBUG_MODE.load(Ordering::Relaxed);
        match metadata.level() {
            Level::Error | Level::Warn | Level::Info => true,
            Level::Debug | Level::Trace => debug,
        }
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let (prefix, color, bold) = match record.level() {
            Level::Error => ("[error]", Color::Red, true),
            Level::Warn => ("[warn]", Color::Yellow, false),
            Level::Info => ("[info]", Color::Cyan, false),
            Level::Debug => ("[debug]", Color::White, false),
            Level::Trace => ("[trace]", Color::White, false),
        };

        let styled_prefix = if bold {
            prefix.color(color).bold()
        } else if matches!(record.level(), Level::Debug | Level::Trace) {
            prefix.color(color).dimmed()
        } else {
            prefix.color(color).clear()
        };

        // Suspend any active progress bar while printing to avoid conflicts
        crate::progress::with_suspended(|| {
            eprintln!("{} {}", styled_prefix, record.args());
        });
    }

    fn flush(&self) {}
}

/// Initialize the logger.
///
/// Should be called once at the very start of main, before config loading.
/// This registers the logger so that log macros work immediately.
/// If stderr is not a terminal, colors will be disabled.
///
/// Call `set_debug()` later to enable debug/trace output.
pub fn init() {
    INIT.call_once(|| {
        // Disable colors if stderr is not a terminal
        if !std::io::stderr().is_terminal() {
            colored::control::set_override(false);
        }

        // Start with Info level; set_debug() can upgrade to Debug/Trace later
        log::set_logger(&LOGGER)
            .map(|()| log::set_max_level(LevelFilter::Info))
            .expect("Failed to initialize logger");
    });
}

/// Update the debug setting after initialization.
///
/// Call this after CLI parsing to enable debug/trace output.
///
/// - `None` = Info level (default)
/// - `Some(DebugLevel)` = Set to specified level
pub fn set_debug(level: Option<DebugLevel>) {
    match level {
        Some(lvl) => {
            // Enable debug mode for Debug and Trace levels
            if matches!(lvl, DebugLevel::Debug | DebugLevel::Trace) {
                DEBUG_MODE.store(true, Ordering::Relaxed);
            }
            log::set_max_level(lvl.to_level_filter());
        }
        None => {
            // Explicitly default to Info when no debug level is set
            log::set_max_level(LevelFilter::Info);
        }
    }
}