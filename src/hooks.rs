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
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::brief::{self, BriefOptions};
use crate::capture::{self, CaptureInput};
use crate::cli::HookEvent;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::index::{self, IndexOptions};
use crate::pack::{ByteHeuristic, TokenCounter};
use crate::querylog;
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

/// How long `SessionStart` waits for another process's write lock on the
/// index before skipping its own index run. The hook has five seconds in
/// all, and the index's usual busy timeout is the same five: left alone, a
/// busy index would use up the hook's whole time and the session would get
/// no brief at all, where a slightly stale one was there for the reading.
const SESSION_START_BUSY_TIMEOUT: Duration = Duration::from_secs(1);

/// Run a hook end to end, stdin to the string to print. Never fails: an
/// error, or a panic, is logged and `{}` returned.
#[must_use]
pub fn run(event: HookEvent) -> String {
    let outcome = std::panic::catch_unwind(|| read_input().and_then(|input| handle(event, &input)));
    match outcome {
        Ok(Ok(v)) => v.to_string(),
        Ok(Err(e)) => {
            log(event, &format!("{e:#}"));
            "{}".to_owned()
        }
        Err(panic) => {
            let what = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown".to_owned());
            log(event, &format!("panic: {what}"));
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
            capture_session(event, &config, &mut store, input, &cwd);
            Ok(json!({}))
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
    store.conn().busy_timeout(SESSION_START_BUSY_TIMEOUT)?;
    if !config.effective_roots().is_empty()
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
    let brief = brief::build(
        store,
        &BriefOptions {
            cwd: cwd.to_path_buf(),
            budget: config.brief.budget,
            recent_days: config.brief.recent_days,
        },
    )?;
    if brief.text.trim().is_empty() {
        return Ok(json!({}));
    }
    let counts = store.counts()?;
    querylog::record_brief(
        config,
        querylog::BriefEvent {
            at: jiff::Timestamp::now().to_string(),
            version: querylog::version(),
            session: input.session_id.clone(),
            source: input.source.clone(),
            project: brief.project,
            cwd: Some(cwd.to_path_buf()),
            tokens: ByteHeuristic.count(&brief.text),
            docs: counts.docs,
            chunks: counts.chunks,
            roots: counts.roots,
            hits: brief
                .items
                .into_iter()
                .map(|i| querylog::LoggedHit {
                    id: i.id,
                    path: i.path,
                    file: i.file,
                })
                .collect(),
        },
    );
    Ok(session_start_output(&brief.text))
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

/// Write the session note and record which listed notes were used. The two
/// are independent: a note that cannot be written (read-only directory, full
/// disk, busy index) must not cost the turn its use tracking, so each
/// failure is logged and the other half still runs.
fn capture_session(
    event: HookEvent,
    config: &Config,
    store: &mut Store,
    input: &HookInput,
    cwd: &Path,
) {
    if config.capture.enabled
        && let Err(e) = write_session_note(config, store, input, cwd)
    {
        log(event, &format!("capture: {e:#}"));
    }
    if config.query_log.enabled
        && let Some(log_path) = Config::query_log_path()
        && let Some(transcript) = input.transcript_path.as_deref().filter(|p| p.is_file())
        && let Err(e) = capture::scan_transcript(transcript)
            .and_then(|scan| querylog::mark_used(&log_path, &input.session_id, Some(cwd), &scan))
    {
        log(event, &format!("query log: {e:#}"));
    }
}

fn write_session_note(
    config: &Config,
    store: &mut Store,
    input: &HookInput,
    cwd: &Path,
) -> Result<()> {
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
    Ok(())
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
