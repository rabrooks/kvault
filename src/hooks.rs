//! Claude Code hook entry points: `kvault hook session-start|stop|pre-compact`.
//!
//! Contract: read `JSON` on stdin, print `JSON` on stdout, always exit 0. On
//! any failure print `{}` and append one line to the hook log so a week of
//! daily use can be audited. `SessionStart` output stays under Claude Code's
//! 10,000-character cap because the brief caps itself at
//! [`crate::brief::MAX_BYTES`]. Stop and `PreCompact` do no LLM work and
//! finish in milliseconds.

use std::io::{Read, Write as _};
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::brief::{self, BriefOptions};
use crate::capture::{self, CaptureInput};
use crate::cli::HookEvent;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::index::{self, IndexOptions};
use crate::store::Store;

/// Most stdin bytes a hook reads; Claude Code sends a few hundred.
const STDIN_MAX: u64 = 1 << 20;

/// The fields of Claude Code's hook input kvault uses; the rest are ignored.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct HookInput {
    pub session_id: String,
    pub transcript_path: Option<std::path::PathBuf>,
    pub cwd: Option<std::path::PathBuf>,
    pub hook_event_name: Option<String>,
    /// `SessionStart` only: `startup`, `resume`, `clear`, `compact`, `fork`.
    pub source: Option<String>,
    /// `Stop` only: the assistant's final text for the turn.
    pub last_assistant_message: Option<String>,
}

impl HookEvent {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "session-start",
            Self::Stop => "stop",
            Self::PreCompact => "pre-compact",
        }
    }
}

/// Run a hook end to end, stdin to the string to print. Never fails: an
/// error is logged and `{}` returned.
#[must_use]
pub fn run(event: HookEvent) -> String {
    match read_input().and_then(|input| handle(event, &input)) {
        Ok(v) => v.to_string(),
        Err(e) => {
            log(event, &format!("{e:#}"));
            "{}".to_owned()
        }
    }
}

fn read_input() -> Result<HookInput> {
    let mut buf = String::new();
    std::io::stdin()
        .lock()
        .take(STDIN_MAX)
        .read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        return Ok(HookInput::default());
    }
    serde_json::from_str(&buf).map_err(|e| Error::Other(anyhow::anyhow!("hook input: {e}")))
}

/// Handle one event with already-parsed input, loading config and index
/// from their usual locations.
///
/// # Errors
/// Returns an error if the config or index cannot be opened, or the event
/// handler fails; `run` turns that into a log line.
pub fn handle(event: HookEvent, input: &HookInput) -> Result<Value> {
    let config = Config::load()?;
    let db = Config::db_path()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("no data directory available")))?;
    let mut store = Store::open(&db)?;
    let cwd = match &input.cwd {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    };
    match event {
        HookEvent::SessionStart => session_start(&config, &mut store, input, &cwd),
        HookEvent::Stop | HookEvent::PreCompact => {
            capture_session(&config, &mut store, input, &cwd)
        }
    }
}

fn session_start(
    config: &Config,
    store: &mut Store,
    input: &HookInput,
    cwd: &Path,
) -> Result<Value> {
    // A resumed or forked session already has its context; re-injecting
    // would only break the cached prompt prefix.
    if matches!(input.source.as_deref(), Some("resume" | "fork")) {
        return Ok(json!({}));
    }
    if !config.roots.is_empty()
        && let Err(e) = index::run(
            config,
            store,
            &IndexOptions {
                quiet: true,
                ..IndexOptions::default()
            },
        )
    {
        // A stale brief beats none.
        log(HookEvent::SessionStart, &format!("index: {e:#}"));
    }
    let text = brief::run(
        store,
        &BriefOptions {
            cwd: cwd.to_path_buf(),
            budget: config.brief.budget,
            recent_days: config.brief.recent_days,
        },
    )?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    Ok(session_start_output(&text))
}

/// The `SessionStart` response that injects `text` as context.
#[must_use]
pub fn session_start_output(text: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": text,
        }
    })
}

fn capture_session(
    config: &Config,
    store: &mut Store,
    input: &HookInput,
    cwd: &Path,
) -> Result<Value> {
    if !config.capture.enabled {
        return Ok(json!({}));
    }
    let written = capture::run(
        config,
        store,
        &CaptureInput {
            session_id: &input.session_id,
            cwd,
            transcript_path: input.transcript_path.as_deref(),
            last_assistant_message: input.last_assistant_message.as_deref(),
        },
    )?;
    if let Some(path) = written {
        index::reindex_root_containing(config, store, &path)?;
    }
    Ok(json!({}))
}

/// Append one line to the hook log. Logging failures are ignored: a hook
/// must never fail because its log could not be written.
pub fn log(event: HookEvent, message: &str) {
    let Some(path) = Config::hook_log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(
            f,
            "{} {} {}",
            jiff::Timestamp::now(),
            event.as_str(),
            message.replace('\n', " ")
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn input_ignores_unknown_fields_and_defaults_the_rest() {
        let v: HookInput = serde_json::from_str(
            r#"{"session_id":"abc","cwd":"/x","hook_event_name":"Stop","stop_hook_active":false,"permission_mode":"default"}"#,
        )
        .unwrap();
        assert_eq!(v.session_id, "abc");
        assert_eq!(v.cwd.as_deref(), Some(Path::new("/x")));
        assert!(v.source.is_none());
        assert!(v.last_assistant_message.is_none());
    }

    #[test]
    fn session_start_output_shape() {
        let v = session_start_output("hello");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], "hello");
    }
}
