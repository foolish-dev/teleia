//! Pre-TUI loading splash. Renders a single line to stderr with the
//! telia wordmark and the current boot phase, redrawing in place as
//! the phase label changes. The line is wiped on `finish` (and on
//! `Drop`) so nothing leaks into the terminal scrollback before
//! [`crate::tui::run`] enters its alt-screen.
//!
//! Pure decoration — no input handling, no spinner animation. Adding
//! a tick task would mean a second async handle and a render
//! mutex; the boot sequence is already busy doing real work, so a
//! static "label changes when the phase changes" splash is enough.

use crossterm::{
    cursor, queue,
    terminal::{Clear, ClearType},
};
use std::io::{stderr, Write};

pub struct Boot {
    drawn: bool,
}

impl Boot {
    pub fn new() -> Self {
        let mut b = Self { drawn: false };
        b.step("starting");
        b
    }

    /// Replace the visible label. Safe to call any number of times.
    pub fn step(&mut self, label: &str) {
        let mut out = stderr();
        let _ = queue!(out, cursor::MoveToColumn(0), Clear(ClearType::CurrentLine),);
        let _ = write!(out, "  λ τέλεια  ·  {label}");
        let _ = out.flush();
        self.drawn = true;
    }

    /// Wipe the splash line. Idempotent.
    pub fn finish(mut self) {
        self.wipe();
    }

    fn wipe(&mut self) {
        if !self.drawn {
            return;
        }
        let mut out = stderr();
        let _ = queue!(out, cursor::MoveToColumn(0), Clear(ClearType::CurrentLine),);
        let _ = out.flush();
        self.drawn = false;
    }
}

impl Drop for Boot {
    fn drop(&mut self) {
        // Belt-and-braces for early-return paths (`?` propagation
        // before `finish()` runs); without this the splash line
        // would survive into whatever error message follows.
        self.wipe();
    }
}
