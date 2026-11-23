//! The query log: one `JSON` line per `search` or `recall`, per session
//! brief injected, per session that completed a turn, and per listed note
//! the session then used.
//!
//! It is the evidence for whether retrieval helps: how many sessions there
//! were, which of them searched, what came back and at what cost, and
//! whether any of it, or anything the brief listed, was opened afterwards. It lives
//! beside the hook log rather than in the index, which is derived and may be
//! deleted at any time. Writing it is best effort; a search never fails
//! because its log line could not be written.
//!
//! A result counts as used when, after it was listed and in the same session,
//! it was fetched with `kvault get`, its file was read or edited by a file
//! tool, its path appeared in a shell command, or the assistant's text
//! named it. Use lines are written by the Stop and `PreCompact` hooks from
//! the transcript, once per (query, result, how).

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::capture::{Action, ActionKind, Transcript};
use crate::config::Config;
use crate::error::Result;

/// Environment variable Claude Code sets to the session id in the shells it
/// runs, which is what ties a query to the transcript that used it.
pub const SESSION_ENV: &str = "CLAUDE_CODE_SESSION_ID";

/// Results recorded per query; ranks past this are one-line references.
pub const MAX_HITS: usize = 20;

/// A relative path shorter than this is too likely to match by accident.
const MIN_RELATIVE_LEN: usize = 12;

/// One result of a logged query.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoggedHit {
    /// Short chunk id, as printed.
    pub id: String,
    /// Display path, as printed.
    pub path: String,
    /// Absolute path of the file.
    pub file: PathBuf,
}

/// A line of the log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Query(QueryEvent),
    Use(UseEvent),
    Brief(BriefEvent),
    Session(SessionEvent),
}

impl Event {
    /// When the event was recorded.
    #[must_use]
    pub fn at(&self) -> &str {
        match self {
            Self::Query(e) => &e.at,
            Self::Use(e) => &e.at,
            Self::Brief(e) => &e.at,
            Self::Session(e) => &e.at,
        }
    }
}

/// The version stamped on new lines, so results from different builds are
/// not pooled blindly.
#[must_use]
pub fn version() -> Option<String> {
    Some(env!("CARGO_PKG_VERSION").to_owned())
}

/// A session brief was injected at `SessionStart`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BriefEvent {
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub session: String,
    /// `startup`, `clear` or `compact`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Tokens of the brief, by the index's counter.
    pub tokens: usize,
    /// Size of the index it was built from.
    pub docs: i64,
    pub chunks: i64,
    pub roots: i64,
    /// The notes the brief listed.
    pub hits: Vec<LoggedHit>,
}

/// A session completed a turn. Written once per session, by the first Stop
/// or `PreCompact` hook, so sessions can be counted whether or not they
/// searched.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionEvent {
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueryEvent {
    /// When the query ran (RFC 3339, UTC).
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `search` or `recall`.
    pub command: String,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// `recall` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Absent when the command was not run from a Claude Code session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Hits found, which may exceed the hits listed.
    pub total: usize,
    /// `recall` only: the reply was a weak match, references only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub weak: bool,
    /// `recall` only: the best share of the query in the top hits, in
    /// hundredths, when that is meaningful.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_pct: Option<u32>,
    /// `recall` only: tokens printed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<usize>,
    /// Milliseconds the command spent searching and packing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
    /// The hits shown, best first, at most [`MAX_HITS`].
    pub hits: Vec<LoggedHit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UseEvent {
    /// When the hook recorded it.
    pub at: String,
    pub session: String,
    /// `at` of the query or brief that listed the note.
    pub query_at: String,
    /// What listed it.
    #[serde(default)]
    pub from: Source,
    pub id: String,
    pub how: How,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    #[default]
    Query,
    Brief,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum How {
    /// `kvault get <id>`.
    Get,
    /// A file tool read or edited the file.
    File,
    /// A shell command named the file.
    Shell,
    /// The assistant's text named the file or the id.
    Cited,
}

/// A query to record, before the session and time are filled in.
pub struct NewQuery<'a> {
    pub command: &'a str,
    pub query: &'a str,
    pub project: Option<&'a str>,
    pub budget: Option<usize>,
    pub total: usize,
    pub weak: bool,
    /// In `[0, 1]`.
    pub coverage: Option<f64>,
    pub tokens: Option<usize>,
    pub elapsed: std::time::Duration,
    pub hits: Vec<LoggedHit>,
}

/// Append a query to the log at the configured location, if enabled.
/// Failures are ignored.
pub fn record(config: &Config, q: NewQuery<'_>) {
    if !config.query_log.enabled {
        return;
    }
    let Some(path) = Config::query_log_path() else {
        return;
    };
    let mut hits = q.hits;
    hits.truncate(MAX_HITS);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let coverage_pct = q
        .coverage
        .map(|c| (c.clamp(0.0, 1.0) * 100.0).round() as u32);
    let event = Event::Query(QueryEvent {
        at: jiff::Timestamp::now().to_string(),
        version: version(),
        command: q.command.to_owned(),
        query: q.query.to_owned(),
        project: q.project.map(str::to_owned),
        budget: q.budget,
        cwd: std::env::current_dir().ok(),
        session: std::env::var(SESSION_ENV).ok().filter(|s| !s.is_empty()),
        total: q.total,
        weak: q.weak,
        coverage_pct,
        tokens: q.tokens,
        ms: Some(u64::try_from(q.elapsed.as_millis()).unwrap_or(u64::MAX)),
        hits,
    });
    let _ = append(&path, &[event]);
}

/// Append a brief injection to the log at the configured location, if
/// enabled. Failures are ignored.
pub fn record_brief(config: &Config, mut event: BriefEvent) {
    if !config.query_log.enabled || event.session.is_empty() {
        return;
    }
    let Some(path) = Config::query_log_path() else {
        return;
    };
    event.hits.truncate(MAX_HITS);
    let _ = append(&path, &[Event::Brief(event)]);
}

/// Append events, one line each, in a single write so concurrent writers
/// do not interleave within a line.
///
/// # Errors
/// Returns an error if the file cannot be opened or written.
pub fn append(path: &Path, events: &[Event]) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let mut buf = String::new();
    for e in events {
        buf.push_str(&serde_json::to_string(e).map_err(|e| crate::Error::Other(e.into()))?);
        buf.push('\n');
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(buf.as_bytes())?;
    Ok(())
}

/// Every parseable line of the log; a missing file is an empty log.
///
/// # Errors
/// Returns an error if the file exists but cannot be read.
pub fn read(path: &Path) -> Result<Vec<Event>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    Ok(text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

/// The parseable lines of the log that mention `session`. The log holds
/// every session there has been and this runs on every turn of every one,
/// so lines are skipped on a substring check without being parsed; a false
/// positive (the id inside a path or a query) is parsed and then ignored by
/// the caller's own match on the session.
///
/// # Errors
/// Returns an error if the file exists but cannot be read.
pub fn read_session(path: &Path, session: &str) -> Result<Vec<Event>> {
    use std::io::BufRead as _;
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut events = Vec::new();
    let mut reader = std::io::BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if !line.windows(session.len()).any(|w| w == session.as_bytes()) {
            continue;
        }
        if let Ok(event) = serde_json::from_slice(&line) {
            events.push(event);
        }
    }
    Ok(events)
}

/// Record that this session completed a turn, once, and which notes its
/// queries and briefs listed that the transcript shows were then used.
/// Returns how many lines were added; none are repeated when the hook runs
/// again on a longer transcript.
///
/// # Errors
/// Returns an error if the log cannot be read or written.
pub fn mark_used(
    path: &Path,
    session: &str,
    cwd: Option<&Path>,
    transcript: &Transcript,
) -> Result<usize> {
    if session.is_empty() {
        return Ok(0);
    }
    let events = read_session(path, session)?;
    let known: HashSet<(&str, &str, How)> = events
        .iter()
        .filter_map(|e| match e {
            Event::Use(u) if u.session == session => {
                Some((u.query_at.as_str(), u.id.as_str(), u.how))
            }
            _ => None,
        })
        .collect();
    let now = jiff::Timestamp::now().to_string();
    let mut new = Vec::new();
    if !events
        .iter()
        .any(|e| matches!(e, Event::Session(s) if s.session == session))
    {
        new.push(Event::Session(SessionEvent {
            at: now.clone(),
            version: version(),
            session: session.to_owned(),
            cwd: cwd.map(Path::to_path_buf),
        }));
    }
    // (listed at, listed from where, by what, the notes)
    let listings = events.iter().filter_map(|e| match e {
        Event::Query(q) if q.session.as_deref() == Some(session) => {
            Some((&q.at, q.cwd.as_deref(), Source::Query, &q.hits))
        }
        Event::Brief(b) if b.session == session => {
            Some((&b.at, b.cwd.as_deref(), Source::Brief, &b.hits))
        }
        _ => None,
    });
    for (at, listed_cwd, from, hits) in listings {
        let later: Vec<&Action> = transcript
            .actions
            .iter()
            .filter(|a| is_after(a.at.as_deref(), at))
            .collect();
        for hit in hits {
            for how in uses(hit, listed_cwd, &later) {
                if !known.contains(&(at.as_str(), hit.id.as_str(), how)) {
                    new.push(Event::Use(UseEvent {
                        at: now.clone(),
                        session: session.to_owned(),
                        query_at: at.clone(),
                        from,
                        id: hit.id.clone(),
                        how,
                    }));
                }
            }
        }
    }
    append(path, &new)?;
    Ok(new.len())
}

/// Whether an action stamped `at` happened after `query_at`. An action
/// with no usable timestamp is not counted: a read that came before the
/// query is not a use of its results.
fn is_after(at: Option<&str>, query_at: &str) -> bool {
    let parse = |s: &str| s.parse::<jiff::Timestamp>().ok();
    match (at.and_then(parse), parse(query_at)) {
        (Some(a), Some(q)) => a > q,
        _ => false,
    }
}

/// The ways `later` actions used `hit`, each at most once.
fn uses(hit: &LoggedHit, cwd: Option<&Path>, later: &[&Action]) -> Vec<How> {
    let file = hit.file.to_string_lossy();
    let mut names: Vec<String> = vec![file.clone().into_owned()];
    if let Some(home) = directories::BaseDirs::new()
        && let Ok(rest) = hit.file.strip_prefix(home.home_dir())
    {
        names.push(format!("~/{}", rest.to_string_lossy()));
    }
    if let Some(rel) = cwd.and_then(|c| hit.file.strip_prefix(c).ok()) {
        let rel = rel.to_string_lossy();
        if rel.len() >= MIN_RELATIVE_LEN {
            names.push(rel.into_owned());
        }
    }
    let mentions = |text: &str| names.iter().any(|n| text.contains(n.as_str()));
    let mut found = Vec::new();
    let mut add = |how| {
        if !found.contains(&how) {
            found.push(how);
        }
    };
    for a in later {
        match a.kind {
            ActionKind::File => {
                if a.text == file || names.iter().any(|n| a.text.ends_with(n.as_str())) {
                    add(How::File);
                }
            }
            ActionKind::Shell => {
                if a.text.contains("kvault get") && a.text.contains(hit.id.as_str()) {
                    add(How::Get);
                } else if mentions(&a.text) {
                    add(How::Shell);
                }
            }
            ActionKind::Said => {
                if mentions(&a.text) || a.text.contains(&format!("#{}", hit.id)) {
                    add(How::Cited);
                }
            }
        }
    }
    found
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn query(at: &str, session: &str) -> Event {
        Event::Query(QueryEvent {
            at: at.to_owned(),
            version: version(),
            command: "recall".to_owned(),
            query: "replica sku".to_owned(),
            project: None,
            budget: Some(1500),
            cwd: Some(PathBuf::from("/work/app")),
            session: Some(session.to_owned()),
            total: 2,
            weak: false,
            coverage_pct: Some(80),
            tokens: Some(900),
            ms: Some(4),
            hits: vec![
                LoggedHit {
                    id: "aaaa1111".to_owned(),
                    path: "app/notes/plans/replica-sizing.md".to_owned(),
                    file: PathBuf::from("/work/app/notes/plans/replica-sizing.md"),
                },
                LoggedHit {
                    id: "bbbb2222".to_owned(),
                    path: "app/todo.md".to_owned(),
                    file: PathBuf::from("/work/app/todo.md"),
                },
            ],
        })
    }

    #[test]
    fn read_session_keeps_only_lines_naming_the_session() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("queries.jsonl");
        append(
            &path,
            &[
                query("2026-11-20T10:00:00Z", "sess-one"),
                query("2026-11-20T10:01:00Z", "sess-two"),
                query("2026-11-20T10:02:00Z", "sess-one"),
            ],
        )
        .unwrap();
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("not json but mentions sess-one\n");
        std::fs::write(&path, text).unwrap();
        assert_eq!(read(&path).unwrap().len(), 3);
        let mine = read_session(&path, "sess-one").unwrap();
        assert_eq!(mine.len(), 2);
        assert!(
            mine.iter()
                .all(|e| matches!(e, Event::Query(q) if q.session.as_deref() == Some("sess-one")))
        );
        assert!(
            read_session(&tmp.path().join("missing"), "x")
                .unwrap()
                .is_empty()
        );
    }

    fn action(at: &str, kind: ActionKind, text: &str) -> Action {
        Action {
            at: Some(at.to_owned()),
            kind,
            text: text.to_owned(),
        }
    }

    fn uses_in(path: &Path) -> Vec<(String, How)> {
        read(path)
            .unwrap()
            .into_iter()
            .filter_map(|e| match e {
                Event::Use(u) => Some((u.id, u.how)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn events_round_trip_one_per_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state/queries.jsonl");
        let q = query("2026-11-20T10:00:00Z", "s1");
        append(&path, std::slice::from_ref(&q)).unwrap();
        append(&path, std::slice::from_ref(&q)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.starts_with(r#"{"event":"query","at":"2026-11-20T10:00:00Z""#));
        assert_eq!(read(&path).unwrap(), vec![q.clone(), q]);
        assert!(read(&tmp.path().join("absent.jsonl")).unwrap().is_empty());
    }

    #[test]
    fn uses_after_the_query_are_marked_once() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("queries.jsonl");
        append(&path, &[query("2026-11-20T10:00:00Z", "s1")]).unwrap();
        let mut t = Transcript {
            actions: vec![
                // Before the query: not a use of its results.
                action(
                    "2026-11-20T09:59:00.000Z",
                    ActionKind::File,
                    "/work/app/todo.md",
                ),
                action(
                    "2026-11-20T10:00:05.250Z",
                    ActionKind::Shell,
                    "kvault get aaaa1111 | head",
                ),
                action(
                    "2026-11-20T10:00:09Z",
                    ActionKind::Shell,
                    "sed -n 1,40p notes/plans/replica-sizing.md",
                ),
                action(
                    "2026-11-20T10:00:20Z",
                    ActionKind::Said,
                    "The sizing note (#aaaa1111) says the replica drives the SKU.",
                ),
            ],
            ..Transcript::default()
        };
        // Three uses and the line that says the session happened.
        assert_eq!(mark_used(&path, "s1", None, &t).unwrap(), 4);
        assert_eq!(
            uses_in(&path),
            vec![
                ("aaaa1111".to_owned(), How::Get),
                ("aaaa1111".to_owned(), How::Shell),
                ("aaaa1111".to_owned(), How::Cited),
            ]
        );
        // The hook runs again on every Stop.
        assert_eq!(mark_used(&path, "s1", None, &t).unwrap(), 0);
        t.actions.push(action(
            "2026-11-20T10:01:00Z",
            ActionKind::File,
            "/work/app/todo.md",
        ));
        assert_eq!(mark_used(&path, "s1", None, &t).unwrap(), 1);
        assert_eq!(uses_in(&path).last().unwrap().1, How::File);
    }

    #[test]
    fn a_note_the_brief_listed_is_credited_to_the_brief() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("queries.jsonl");
        let brief = Event::Brief(BriefEvent {
            at: "2026-11-20T10:00:00Z".to_owned(),
            version: version(),
            session: "s1".to_owned(),
            source: Some("startup".to_owned()),
            project: Some("app".to_owned()),
            cwd: Some(PathBuf::from("/work/app")),
            tokens: 300,
            docs: 10,
            chunks: 40,
            roots: 2,
            hits: vec![LoggedHit {
                id: "cccc3333".to_owned(),
                path: "other/notes/cost-review.md".to_owned(),
                file: PathBuf::from("/work/other/notes/cost-review.md"),
            }],
        });
        append(&path, &[brief]).unwrap();
        let t = Transcript {
            actions: vec![action(
                "2026-11-20T10:05:00Z",
                ActionKind::Shell,
                "kvault get cccc3333 | head -80",
            )],
            ..Transcript::default()
        };
        mark_used(&path, "s1", Some(Path::new("/work/app")), &t).unwrap();
        mark_used(&path, "s1", Some(Path::new("/work/app")), &t).unwrap();
        let uses: Vec<UseEvent> = read(&path)
            .unwrap()
            .into_iter()
            .filter_map(|e| match e {
                Event::Use(u) => Some(u),
                _ => None,
            })
            .collect();
        assert_eq!(uses.len(), 1);
        assert_eq!((uses[0].from, uses[0].how), (Source::Brief, How::Get));
        assert_eq!(uses[0].query_at, "2026-11-20T10:00:00Z");
    }

    #[test]
    fn lines_written_before_a_field_existed_still_parse() {
        let old = r#"{"event":"query","at":"2026-11-20T10:00:00Z","command":"recall","query":"q","total":0,"hits":[]}
{"event":"use","at":"2026-11-20T10:01:00Z","session":"s","query_at":"2026-11-20T10:00:00Z","id":"aaaa1111","how":"get"}"#;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("queries.jsonl");
        std::fs::write(&path, old).unwrap();
        let events = read(&path).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[1], Event::Use(u) if u.from == Source::Query));
    }

    #[test]
    fn other_sessions_and_short_relative_paths_do_not_count() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("queries.jsonl");
        append(&path, &[query("2026-11-20T10:00:00Z", "s1")]).unwrap();
        let t = Transcript {
            actions: vec![
                action(
                    "2026-11-20T10:00:05Z",
                    ActionKind::Shell,
                    "kvault get aaaa1111",
                ),
                // A bare file name under the cwd is too short to trust.
                action("2026-11-20T10:00:06Z", ActionKind::Shell, "cat todo.md"),
            ],
            ..Transcript::default()
        };
        assert_eq!(mark_used(&path, "", None, &t).unwrap(), 0);
        mark_used(&path, "s2", None, &t).unwrap();
        assert!(uses_in(&path).is_empty(), "another session's query");
        mark_used(&path, "s1", None, &t).unwrap();
        assert_eq!(uses_in(&path), vec![("aaaa1111".to_owned(), How::Get)]);
        let sessions = read(&path)
            .unwrap()
            .iter()
            .filter(|e| matches!(e, Event::Session(_)))
            .count();
        assert_eq!(sessions, 2, "one line per session however often Stop runs");
    }
}
