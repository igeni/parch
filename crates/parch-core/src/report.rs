//! Diagnostics, verbose output and the progress bar (all on stderr).

use std::fmt::Display;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

/// Before the task size is known: processed bytes and speed only.
const STYLE_UNKNOWN: &str = "{elapsed_precise} {spinner} {bytes} {bytes_per_sec} {msg}";
/// Processed bytes (before compression) relative to the task size.
const STYLE_TOTAL: &str = "{elapsed_precise} [{wide_bar}] {percent:>3}% {bytes}/{total_bytes} {bytes_per_sec} ETA {eta}";

pub struct Reporter {
    quiet: bool,
    verbose: u8,
    warnings: AtomicUsize,
    errors: AtomicUsize,
    bar: ProgressBar,
}

impl Reporter {
    pub fn new(quiet: bool, verbose: u8, progress: bool) -> Self {
        let target = if progress && !quiet && verbose == 0 { ProgressDrawTarget::stderr() } else { ProgressDrawTarget::hidden() };
        Self::with_draw_target(quiet, verbose, target)
    }

    /// Reporter drawing its progress bar to `target`.
    pub fn with_draw_target(quiet: bool, verbose: u8, target: ProgressDrawTarget) -> Self {
        let bar = ProgressBar::with_draw_target(None, target);
        bar.set_style(ProgressStyle::with_template(STYLE_UNKNOWN).expect("valid template"));
        Self { quiet, verbose, warnings: AtomicUsize::new(0), errors: AtomicUsize::new(0), bar }
    }

    pub fn bar(&self) -> &ProgressBar {
        &self.bar
    }

    pub fn progress_visible(&self) -> bool {
        !self.bar.is_hidden()
    }

    /// Shows that the task size is being counted.
    pub fn start_counting(&self) {
        self.bar.set_message("counting files...");
        self.bar.enable_steady_tick(Duration::from_millis(200));
    }

    /// Sets the task size (bytes before compression) and switches to the percentage view.
    pub fn set_total(&self, total: u64) {
        self.bar.disable_steady_tick();
        self.bar.set_length(total);
        self.bar.set_message("");
        self.bar.set_style(ProgressStyle::with_template(STYLE_TOTAL).expect("valid template").progress_chars("=> "));
    }

    pub fn verbose(&self) -> u8 {
        if self.quiet { 0 } else { self.verbose }
    }

    pub fn warn(&self, msg: impl Display) {
        self.warnings.fetch_add(1, Ordering::Relaxed);
        if !self.quiet {
            self.print(format_args!("parch: warning: {msg}"));
        }
    }

    pub fn error(&self, msg: impl Display) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        self.print(format_args!("parch: error: {msg}"));
    }

    /// Printed only in verbose mode.
    pub fn info(&self, msg: impl Display) {
        if self.verbose() > 0 {
            self.print(format_args!("{msg}"));
        }
    }

    fn print(&self, msg: std::fmt::Arguments<'_>) {
        self.bar.suspend(|| eprintln!("{msg}"));
    }

    pub fn warnings(&self) -> usize {
        self.warnings.load(Ordering::Relaxed)
    }

    pub fn errors(&self) -> usize {
        self.errors.load(Ordering::Relaxed)
    }

    /// 0 — success, 1 — warnings, 2 — errors.
    pub fn exit_code(&self) -> u8 {
        if self.errors() > 0 {
            2
        } else if self.warnings() > 0 {
            1
        } else {
            0
        }
    }

    pub fn finish(&self) {
        self.bar.finish_and_clear();
    }
}
