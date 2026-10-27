//! Claude Code hook entry points: `kvault hook session-start|stop|pre-compact`.
//!
//! Contract: read `JSON` on stdin, print `JSON` on stdout, always exit 0, log
//! failures to the state dir. `SessionStart` output must stay under the
//! documented 10,000-character cap.
//!
//! Implemented in M4.

/// Hard cap on `SessionStart` `additionalContext`, below Claude Code's 10,000.
pub const BRIEF_MAX_CHARS: usize = 8_000;
