//! Session-note writer used by the Stop and `PreCompact` hooks. Makes no LLM
//! calls.
//!
//! One note per session, rewritten on every Stop so a compaction never loses
//! it: frontmatter (session, project, files touched), the first prompt, the
//! last assistant message, and the files the transcript shows were read or
//! edited. Promoting a session note into a durable decision note is the
//! agent's job through the skill, not kvault's.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::brief;
use crate::config::{Config, GLOBAL_SESSIONS_DIR, expand_tilde};
use crate::error::{Error, Result};
use crate::pack;
use crate::store::Store;

/// Bytes read from the start of a transcript: start time and first prompt.
pub const TRANSCRIPT_HEAD: u64 = 128 * 1024;
/// Bytes read from the end: last message and files touched. Head plus tail
/// is the 1 MB ceiling the hook contract promises.
pub const TRANSCRIPT_TAIL: u64 = 896 * 1024;
/// How far the head read goes on when the first prompt has not been seen
/// within [`TRANSCRIPT_HEAD`]: one record holding a pasted image can be
/// larger than the whole head, and the note's title and file name both hang
/// on that first prompt.
pub const TRANSCRIPT_HEAD_MAX: u64 = 4 * 1024 * 1024;
/// Characters kept of the last assistant message.
pub const LAST_MESSAGE_CHARS: usize = 4_000;
/// Characters kept of the first prompt.
pub const PROMPT_CHARS: usize = 600;
/// The tag every session note carries.
pub const SESSION_TAG: &str = "session";
/// Characters of the first prompt line used as the note title.
const TITLE_CHARS: usize = 80;

pub struct CaptureInput<'a> {
    pub session_id: &'a str,
    pub cwd: &'a Path,
    pub transcript_path: Option<&'a Path>,
    /// Supplied by the Stop hook; `PreCompact` falls back to the transcript.
    pub last_assistant_message: Option<&'a str>,
}

/// What a transcript scan found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Transcript {
    /// Timestamp of the first record, as written (RFC 3339, UTC).
    pub started: Option<String>,
    pub first_prompt: Option<String>,
    /// Text of the last assistant message that had any.
    pub last_assistant: Option<String>,
    /// User prompts seen; approximate on transcripts over the read ceiling.
    pub prompts: usize,
    /// Path as the tool call gave it → counts.
    pub files: BTreeMap<String, Touch>,
    /// File tool calls, shell commands and assistant text, in order.
    pub actions: Vec<Action>,
    /// The middle of the transcript was not read: `files` and `actions`
    /// cover the end of the session only.
    pub partial: bool,
}

/// One thing the assistant did, kept so the query log can tell which search
/// results were used afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    /// Timestamp of the record, as written.
    pub at: Option<String>,
    pub kind: ActionKind,
    /// The file path, the command, or the text.
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    /// A file tool read or edited the path.
    File,
    Shell,
    Said,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Touch {
    pub edits: u32,
    pub reads: u32,
}

/// Write or refresh the session note. Returns its path, or `None` when there
/// is no session id or nothing worth recording.
///
/// # Errors
/// Returns an error if the transcript or the note's directory cannot be read
/// or written, or the index cannot be queried for the project.
pub fn run(config: &Config, store: &Store, input: &CaptureInput<'_>) -> Result<Option<PathBuf>> {
    let session = short_session(input.session_id);
    if session.is_empty() {
        return Ok(None);
    }
    let project_dir = project_dir(input.cwd);
    let project = brief::project_for(store, input.cwd)?.or_else(|| {
        project_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    });
    let transcript = match input.transcript_path {
        Some(p) if p.is_file() => scan_transcript(p)?,
        _ => Transcript::default(),
    };
    let last = input
        .last_assistant_message
        .filter(|m| !m.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| transcript.last_assistant.clone());
    if transcript.first_prompt.is_none() && last.is_none() && transcript.files.is_empty() {
        return Ok(None);
    }

    let now = jiff::Zoned::now();
    let started = transcript
        .started
        .as_deref()
        .and_then(|s| s.parse::<jiff::Timestamp>().ok())
        .map_or_else(|| now.clone(), |t| t.to_zoned(now.time_zone().clone()));
    let dir = session_dir(config, &project_dir, project.as_deref());
    let path = dir.join(format!("{}-{session}.md", started.date()));
    // A long session is scanned at its two ends only, so files touched in
    // the middle drop out of later scans: keep what the note already lists.
    let carried = if transcript.partial {
        files_in_note(&path)
    } else {
        Vec::new()
    };
    let note = render(&Note {
        session: &session,
        project: project.as_deref(),
        started: &started,
        updated: &now,
        transcript: &transcript,
        last: last.as_deref(),
        project_dir: &project_dir,
        carried: &carried,
    });
    std::fs::create_dir_all(&dir)?;
    write_atomic(&path, &note)?;
    Ok(Some(path))
}

/// The `files_touched` of the session note at `path`, if there is one.
fn files_in_note(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let (fm, _) = crate::markdown::split_frontmatter(&text);
    fm.extra
        .get("files_touched")
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .filter_map(|f| f.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// First eight characters of the session id, keeping only filename-safe ones.
#[must_use]
pub fn short_session(id: &str) -> String {
    id.chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect()
}

/// The directory a working directory's session notes belong to: the nearest
/// ancestor holding `.git` (a file in worktrees, a directory otherwise),
/// else the working directory itself.
#[must_use]
pub fn project_dir(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|p| p.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

/// `<project>/<session_dir>` when the first component of `session_dir`
/// (normally `notes`) exists under the project and is indexed, else the
/// fallback dir with a per-project subdirectory. A `notes/` directory that
/// no configured root covers is somebody else's (a third-party checkout
/// with its own `notes/`): a session note written there would be an
/// untracked file in their repository that kvault never indexes.
fn session_dir(config: &Config, project_dir: &Path, project: Option<&str>) -> PathBuf {
    let rel = Path::new(&config.capture.session_dir);
    let anchor = rel.components().next().map(|c| project_dir.join(c));
    if anchor.is_some_and(|p| p.is_dir() && under_a_root(config, &p)) {
        project_dir.join(rel)
    } else {
        expand_tilde(&config.capture.fallback_dir).join(project.unwrap_or(GLOBAL_SESSIONS_DIR))
    }
}

/// Whether an existing directory is a configured root or inside one.
fn under_a_root(config: &Config, dir: &Path) -> bool {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let dir = canonical(dir);
    crate::roots::expand_config(config)
        .iter()
        .any(|root| dir.starts_with(canonical(&root.path)))
}

struct Note<'a> {
    session: &'a str,
    project: Option<&'a str>,
    started: &'a jiff::Zoned,
    updated: &'a jiff::Zoned,
    transcript: &'a Transcript,
    last: Option<&'a str>,
    project_dir: &'a Path,
    /// Files an earlier version of this note listed, relative to
    /// `project_dir`, kept when the scan no longer reaches them.
    carried: &'a [String],
}

#[derive(Serialize)]
struct Frontmatter<'a> {
    title: String,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<&'a str>,
    session: &'a str,
    started: String,
    updated: String,
    prompts: usize,
    files_touched: Vec<String>,
    tags: [&'static str; 1],
}

fn local_time(z: &jiff::Zoned) -> String {
    z.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

fn render(n: &Note<'_>) -> String {
    // Scrub before deriving the title or truncating: the title is a prompt
    // line, and a secret cut short by truncation no longer looks like one.
    let prompt = n
        .transcript
        .first_prompt
        .as_deref()
        .map(scrub)
        .filter(|p| !p.is_empty());
    let last = n.last.map(scrub).filter(|l| !l.is_empty());
    let title = prompt
        .as_deref()
        .and_then(|p| p.lines().find(|l| !l.trim().is_empty()))
        .map_or_else(
            || format!("Session {}", n.session),
            |l| {
                pack::truncate(
                    &l.split_whitespace().collect::<Vec<_>>().join(" "),
                    TITLE_CHARS,
                )
            },
        );
    let mut files: Vec<(String, Touch)> = n
        .transcript
        .files
        .iter()
        .map(|(p, t)| (relative_to(p, n.project_dir), *t))
        .collect();
    for old in n.carried {
        if !files.iter().any(|(p, _)| p == old) {
            files.push((old.clone(), Touch::default()));
        }
    }
    let fm = Frontmatter {
        title: title.clone(),
        kind: "session",
        project: n.project,
        session: n.session,
        started: local_time(n.started),
        updated: local_time(n.updated),
        prompts: n.transcript.prompts,
        files_touched: files.iter().map(|(p, _)| p.clone()).collect(),
        tags: [SESSION_TAG],
    };
    let mut out = String::new();
    out.push_str("---\n");
    // Serializing a struct of strings and a string list cannot fail.
    out.push_str(&serde_norway::to_string(&fm).unwrap_or_default());
    out.push_str("---\n");
    let _ = writeln!(out, "# {title}");
    if let Some(p) = &prompt {
        let _ = write!(out, "\n## Prompt\n\n{}\n", pack::truncate(p, PROMPT_CHARS));
    }
    if let Some(last) = &last {
        let _ = write!(
            out,
            "\n## Last response\n\n{}\n",
            pack::truncate(last, LAST_MESSAGE_CHARS)
        );
    }
    if !files.is_empty() {
        out.push_str("\n## Files touched\n\n");
        for (p, t) in &files {
            let mut what = Vec::new();
            if t.edits > 0 {
                what.push(format!("edited ×{}", t.edits));
            }
            if t.reads > 0 {
                what.push(format!("read ×{}", t.reads));
            }
            if what.is_empty() {
                what.push("earlier in the session".to_owned());
            }
            let _ = writeln!(out, "- {p} ({})", what.join(", "));
        }
    }
    out
}

fn relative_to(path: &str, dir: &Path) -> String {
    Path::new(path)
        .strip_prefix(dir)
        .map_or_else(|_| path.to_owned(), |p| p.to_string_lossy().into_owned())
}

/// Scan a Claude Code transcript (`JSON` lines). Reads the head and the
/// tail, up to their ceilings; the gap, if any, costs an exact prompt count
/// and the files touched in the middle (see [`Transcript::partial`]).
///
/// The head is read in whole lines, and past its ceiling until the first
/// prompt has been seen (within [`TRANSCRIPT_HEAD_MAX`]): the start time and
/// the first prompt name the note, so they must not depend on where the
/// tail happens to begin.
///
/// # Errors
/// Returns an error if the file cannot be read.
pub fn scan_transcript(path: &Path) -> Result<Transcript> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let whole = len <= TRANSCRIPT_HEAD + TRANSCRIPT_TAIL;
    let mut reader = BufReader::new(file);
    let mut t = Transcript::default();
    let mut pos = 0;
    let mut line = Vec::new();
    while pos < len {
        let head_done =
            pos >= TRANSCRIPT_HEAD && (t.first_prompt.is_some() || pos >= TRANSCRIPT_HEAD_MAX);
        if !whole && head_done {
            break;
        }
        let limit = if whole {
            u64::MAX
        } else {
            TRANSCRIPT_HEAD_MAX.saturating_sub(pos).max(1)
        };
        line.clear();
        let read = (&mut reader).take(limit).read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        pos += read as u64;
        // A record cut by the limit never parses.
        scan_line(&String::from_utf8_lossy(&line), &mut t);
    }
    if pos < len {
        let start = pos.max(len.saturating_sub(TRANSCRIPT_TAIL));
        t.partial = start > pos;
        reader.seek(SeekFrom::Start(start))?;
        let mut tail = Vec::new();
        reader.read_to_end(&mut tail)?;
        // After a jump the first line is almost certainly cut; a partial
        // record never parses.
        for line in String::from_utf8_lossy(&tail)
            .lines()
            .skip(usize::from(t.partial))
        {
            scan_line(line, &mut t);
        }
    }
    Ok(t)
}

/// The parts of a transcript record kvault reads.
#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    message: Option<Message>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default, rename = "isSidechain")]
    sidechain: bool,
    #[serde(default, rename = "isMeta")]
    meta: bool,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Value,
}

/// Fold one transcript line into the scan. Lines that cannot be user or
/// assistant records are skipped on a substring check, which is far cheaper
/// than parsing them; a false positive is parsed and then ignored by type.
pub fn scan_line(line: &str, t: &mut Transcript) {
    if !(line.contains("\"type\":\"user\"") || line.contains("\"type\":\"assistant\"")) {
        return;
    }
    let Ok(rec) = serde_json::from_str::<Record>(line) else {
        return;
    };
    if rec.sidechain {
        return;
    }
    if t.started.is_none() {
        t.started.clone_from(&rec.timestamp);
    }
    let Some(msg) = rec.message else {
        return;
    };
    match rec.kind.as_str() {
        "user" if !rec.meta => {
            let text = match &msg.content {
                Value::String(s) => s.clone(),
                Value::Array(blocks) => text_blocks(blocks),
                _ => String::new(),
            };
            if is_prompt(&text) {
                t.prompts += 1;
                if t.first_prompt.is_none() {
                    t.first_prompt = Some(text);
                }
            }
        }
        "assistant" => {
            if let Value::Array(blocks) = &msg.content {
                for b in blocks {
                    if b["type"] == "tool_use" {
                        record_tool(b, rec.timestamp.as_deref(), t);
                    }
                }
                let text = text_blocks(blocks);
                if !text.trim().is_empty() {
                    t.actions.push(Action {
                        at: rec.timestamp.clone(),
                        kind: ActionKind::Said,
                        text: text.clone(),
                    });
                    t.last_assistant = Some(text);
                }
            }
        }
        _ => {}
    }
}

fn text_blocks(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Slash commands and their caveats are user records too; they are not prompts.
fn is_prompt(s: &str) -> bool {
    let s = s.trim_start();
    !s.is_empty() && !s.starts_with("<command-") && !s.starts_with("<local-command")
}

fn record_tool(b: &Value, at: Option<&str>, t: &mut Transcript) {
    let input = &b["input"];
    let mut act = |kind, text: &str| {
        t.actions.push(Action {
            at: at.map(str::to_owned),
            kind,
            text: text.to_owned(),
        });
    };
    if b["name"] == "Bash" {
        if let Some(cmd) = input["command"].as_str() {
            act(ActionKind::Shell, cmd);
        }
        return;
    }
    let (path, edit) = match b["name"].as_str() {
        Some("Edit" | "Write" | "MultiEdit") => (input["file_path"].as_str(), true),
        Some("NotebookEdit") => (input["notebook_path"].as_str(), true),
        Some("Read") => (input["file_path"].as_str(), false),
        _ => return,
    };
    let Some(path) = path else {
        return;
    };
    act(ActionKind::File, path);
    let touch = t.files.entry(path.to_owned()).or_default();
    if edit {
        touch.edits += 1;
    } else {
        touch.reads += 1;
    }
}

/// Remove `<private>…</private>` spans; an unclosed span runs to the end.
#[must_use]
pub fn strip_private(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<private>") {
        out.push_str(&rest[..start]);
        rest = match rest[start..].find("</private>") {
            Some(end) => &rest[start + end + "</private>".len()..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// What a session note may quote of a prompt or a response: `<private>`
/// spans dropped, credentials redacted, surrounding whitespace trimmed.
fn scrub(text: &str) -> String {
    redact(&strip_private(text)).trim().to_owned()
}

const REDACTED: &str = "[redacted]";

/// Replace credentials with `[redacted]`:
///
/// - PEM blocks, from the `-----BEGIN` line through the `-----END` line, or
///   through the last line of key material when the block is cut short;
/// - the value after `Bearer`;
/// - AWS access keys, GitHub, Slack, `OpenAI` and Anthropic key prefixes and
///   Google API keys, wherever they sit in a word (`KEY=sk-…`,
///   `https://user:ghp_…@host`);
/// - the value assigned to a name ending in `key`, `token`, `secret`,
///   `password`, `passwd` or `pwd` (`DB_PASSWORD=…`, `"api_key": "…"`).
///
/// Deliberately a short list; the note is a summary, not a vault.
#[must_use]
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_pem = false;
    for (i, line) in text.split('\n').enumerate() {
        if in_pem {
            if line.contains("-----END") {
                in_pem = false;
                continue;
            }
            if is_pem_body(line) {
                continue;
            }
            // Not key material: the block was cut short or never was one.
            in_pem = false;
        }
        if i > 0 {
            out.push('\n');
        }
        if let Some(begin) = line.find("-----BEGIN") {
            out.push_str(REDACTED);
            in_pem = !line[begin..].contains("-----END");
            continue;
        }
        redact_line(line, &mut out);
    }
    out
}

/// A line that can sit inside a PEM block: blank, base64, or one of the
/// headers of an encrypted key.
fn is_pem_body(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("Proc-Type:")
        || line.starts_with("DEK-Info:")
        || line
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

/// A character of a credential-shaped token or of an assigned name.
const fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Where a redacted value stops: whitespace or a closing quote.
fn value_end(s: &str) -> usize {
    s.find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
        .unwrap_or(s.len())
}

fn redact_line(line: &str, out: &mut String) {
    let mut rest = line;
    while let Some(start) = rest.find(is_token_char) {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest.find(|c: char| !is_token_char(c)).unwrap_or(rest.len());
        let (run, after) = rest.split_at(end);
        rest = after;
        if is_secret_token(run) {
            out.push_str(REDACTED);
            continue;
        }
        out.push_str(run);
        let value_at = if run.eq_ignore_ascii_case("bearer") {
            bearer_value_start(rest)
        } else if is_secret_name(run) {
            assigned_value_start(rest)
        } else {
            None
        };
        if let Some(at) = value_at {
            let len = value_end(&rest[at..]);
            if len > 0 {
                out.push_str(&rest[..at]);
                out.push_str(REDACTED);
                rest = &rest[at + len..];
            }
        }
    }
    out.push_str(rest);
}

/// `Bearer <value>`: the offset of the value after the whitespace.
fn bearer_value_start(rest: &str) -> Option<usize> {
    let at = rest.find(|c: char| !c.is_whitespace())?;
    (at > 0).then_some(at)
}

/// The offset of the value in what follows an assigned name: `=value`,
/// `="value"`, or after a quoted name `": "value"`.
fn assigned_value_start(rest: &str) -> Option<usize> {
    let is_quote = |c: char| c == '"' || c == '\'';
    let mut chars = rest.char_indices().peekable();
    let quoted_name = chars.next_if(|&(_, c)| is_quote(c)).is_some();
    if quoted_name {
        while chars.next_if(|&(_, c)| c == ' ').is_some() {}
    }
    chars.next_if(|&(_, c)| c == '=' || (quoted_name && c == ':'))?;
    if quoted_name {
        while chars.next_if(|&(_, c)| c == ' ').is_some() {}
    }
    chars.next_if(|&(_, c)| is_quote(c));
    Some(chars.peek().map_or(rest.len(), |&(i, _)| i))
}

fn is_secret_name(run: &str) -> bool {
    let name = run.to_ascii_lowercase();
    ["key", "token", "secret", "password", "passwd", "pwd"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

fn is_secret_token(w: &str) -> bool {
    let aws = w.len() == 20
        && w.starts_with("AKIA")
        && w.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    let google = w.len() == 39 && w.starts_with("AIza");
    aws || google
        || w.starts_with("ghp_")
        || w.starts_with("gho_")
        || w.starts_with("github_pat_")
        || w.starts_with("xoxb-")
        || w.starts_with("xoxp-")
        || w.starts_with("sk-ant-")
        || (w.starts_with("sk-") && w.len() >= 20)
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::Io(e)
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn user(text: &str, ts: &str) -> String {
        serde_json::json!({"parentUuid": null, "isSidechain": false, "type": "user",
            "message": {"role": "user", "content": text}, "timestamp": ts, "cwd": "/p"})
        .to_string()
    }

    fn assistant(blocks: &Value) -> String {
        serde_json::json!({"parentUuid": "x", "isSidechain": false, "type": "assistant",
            "message": {"role": "assistant", "content": blocks}, "timestamp": "2026-10-28T16:50:00.000Z"})
        .to_string()
    }

    #[test]
    fn scan_collects_prompt_last_text_and_files() {
        let lines = [
            r#"{"type":"mode","mode":"normal","sessionId":"s"}"#.to_owned(),
            user(
                "<command-name>/clear</command-name>",
                "2026-10-28T16:45:25.275Z",
            ),
            user("Fix the indexer", "2026-10-28T16:45:30.000Z"),
            assistant(&serde_json::json!([
                {"type":"text","text":"Looking."},
                {"type":"tool_use","name":"Read","input":{"file_path":"/p/src/index.rs"}}
            ])),
            assistant(&serde_json::json!([
                {"type":"tool_use","name":"Edit","input":{"file_path":"/p/src/index.rs","old_string":"a","new_string":"b"}}
            ])),
            user("thanks", "2026-10-28T16:55:00.000Z"),
            assistant(&serde_json::json!([{"type":"text","text":"Done: fixed it."}])),
        ];
        let mut t = Transcript::default();
        for l in &lines {
            scan_line(l, &mut t);
        }
        assert_eq!(t.started.as_deref(), Some("2026-10-28T16:45:25.275Z"));
        assert_eq!(t.first_prompt.as_deref(), Some("Fix the indexer"));
        assert_eq!(t.prompts, 2);
        assert_eq!(t.last_assistant.as_deref(), Some("Done: fixed it."));
        assert_eq!(
            t.files.get("/p/src/index.rs"),
            Some(&Touch { edits: 1, reads: 1 })
        );
    }

    #[test]
    fn sidechain_and_meta_records_are_ignored() {
        let mut t = Transcript::default();
        scan_line(
            r#"{"isSidechain":true,"type":"user","message":{"content":"sub"},"timestamp":"t"}"#,
            &mut t,
        );
        assert_eq!(t, Transcript::default());
        // A meta record (a slash command's caveat) is not a prompt but it is
        // usually the first record, so it still supplies the start time.
        scan_line(
            r#"{"isMeta":true,"type":"user","message":{"content":"meta"},"timestamp":"t"}"#,
            &mut t,
        );
        assert_eq!(
            t,
            Transcript {
                started: Some("t".to_owned()),
                ..Transcript::default()
            }
        );
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let mut t = Transcript::default();
        scan_line(r#"{"type":"user","message":"#, &mut t);
        scan_line("not json at all", &mut t);
        assert_eq!(t, Transcript::default());
    }

    #[test]
    fn private_spans_are_stripped() {
        assert_eq!(strip_private("a <private>b</private> c"), "a  c");
        assert_eq!(strip_private("a <private>b"), "a ");
        assert_eq!(strip_private("plain"), "plain");
    }

    #[test]
    fn secrets_are_redacted() {
        let text = "key AKIAIOSFODNN7EXAMPLE here\nAuthorization: Bearer abc.def\n-----BEGIN RSA PRIVATE KEY-----\ntoken ghp_abcdefghijklmnop end\nsk-short ok";
        let r = redact(text);
        assert_eq!(
            r,
            "key [redacted] here\nAuthorization: Bearer [redacted]\n[redacted]\ntoken [redacted] end\nsk-short ok"
        );
    }

    #[test]
    fn pem_blocks_are_redacted_whole() {
        let pem = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\nc2VjcmV0\n-----END RSA PRIVATE KEY-----\nafter";
        assert_eq!(redact(pem), "before\n[redacted]\nafter");
        // Cut short (truncated paste): the key material still goes, the
        // text after it stays.
        assert_eq!(
            redact("x\n-----BEGIN PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\nc2VjcmV0+/=="),
            "x\n[redacted]"
        );
        assert_eq!(
            redact(
                "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\n\nMIIE\nand then some prose"
            ),
            "[redacted]\nand then some prose"
        );
        assert_eq!(
            redact("-----BEGIN X----- abc -----END X-----\nkept"),
            "[redacted]\nkept"
        );
    }

    #[test]
    fn secrets_inside_words_are_redacted() {
        for (text, want) in [
            (
                "export OPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuv then test",
                "export OPENAI_API_KEY=[redacted] then test",
            ),
            ("token:ghp_abcdefghijklmnop", "token:[redacted]"),
            (
                "git clone https://me:ghp_abcdefghijklmnop@github.com/x/y",
                "git clone https://me:[redacted]@github.com/x/y",
            ),
            ("Bearer\teyJhbGciOi.J9.x-y_z", "Bearer\t[redacted]"),
            (
                r#"{"Authorization":"Bearer abc.def+/=","n":1}"#,
                r#"{"Authorization":"Bearer [redacted]","n":1}"#,
            ),
            ("DB_PASSWORD=hunter2 ./run", "DB_PASSWORD=[redacted] ./run"),
            (r#"password="hunter 2""#, r#"password="[redacted] 2""#),
            (
                r#"{"api_key": "abc123", "x": 1}"#,
                r#"{"api_key": "[redacted]", "x": 1}"#,
            ),
            ("client_secret='s3cr3t'", "client_secret='[redacted]'"),
        ] {
            assert_eq!(redact(text), want, "{text}");
        }
    }

    #[test]
    fn ordinary_text_is_not_redacted() {
        for text in [
            "the primary key = id, and the token: a unit of text",
            "max_tokens=1500 --budget=1500",
            "a task-oriented-and-rather-long-hyphenated-word",
            "the bearer.",
            "let key = map.get(&token)?;",
            "naïve café: key= (empty)",
        ] {
            assert_eq!(redact(text), text);
        }
    }

    #[test]
    fn title_and_prompt_are_scrubbed_before_truncation() {
        let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz0123456789";
        let filler = "word ".repeat(PROMPT_CHARS / 5 - 4);
        let t = Transcript {
            first_prompt: Some(format!(
                "export OPENAI_API_KEY={secret} and <private>my address</private> run\n{filler}{secret}"
            )),
            prompts: 1,
            ..Transcript::default()
        };
        let now = jiff::Zoned::now();
        let out = render(&Note {
            session: "abcd1234",
            project: None,
            started: &now,
            updated: &now,
            transcript: &t,
            last: None,
            project_dir: Path::new("/p"),
            carried: &[],
        });
        assert!(
            out.contains("\n# export OPENAI_API_KEY=[redacted] and run\n"),
            "{out}"
        );
        assert!(!out.contains("sk-proj"), "{out}");
        assert!(!out.contains("my address"), "{out}");

        // A prompt that is nothing but a private span leaves no title or section.
        let t = Transcript {
            first_prompt: Some("<private>all of it</private>".to_owned()),
            prompts: 1,
            ..Transcript::default()
        };
        let out = render(&Note {
            session: "abcd1234",
            project: None,
            started: &now,
            updated: &now,
            transcript: &t,
            last: None,
            project_dir: Path::new("/p"),
            carried: &[],
        });
        assert!(out.contains("\n# Session abcd1234\n"), "{out}");
        assert!(!out.contains("## Prompt"), "{out}");
    }

    fn record(kind: &str, at: &str, content: &Value) -> String {
        serde_json::json!({"type": kind, "isSidechain": false, "timestamp": at,
            "message": {"role": kind, "content": content}})
        .to_string()
    }

    fn read_call(path: &str) -> Value {
        serde_json::json!([{"type": "tool_use", "id": "t", "name": "Read",
            "input": {"file_path": path}}])
    }

    /// A first prompt larger than the head (a pasted image) used to leave
    /// the head with nothing, so the note's start time and title came from
    /// wherever the tail began, and moved as the transcript grew.
    #[test]
    fn a_first_prompt_larger_than_the_head_still_names_the_session() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.jsonl");
        let image = "A".repeat(usize::try_from(TRANSCRIPT_HEAD).unwrap() * 2);
        let mut text = record(
            "user",
            "2026-10-28T16:45:30.000Z",
            &Value::String(format!("Describe this screenshot {image}")),
        );
        text.push('\n');
        text.push_str(&record(
            "assistant",
            "2026-10-28T16:46:00.000Z",
            &read_call("/p/early.rs"),
        ));
        text.push('\n');
        let filler = record(
            "assistant",
            "2026-10-28T17:00:00.000Z",
            &serde_json::json!([{"type": "text", "text": "x".repeat(4000)}]),
        );
        let mut scans = Vec::new();
        for rounds in [300, 600] {
            for _ in 0..rounds {
                text.push_str(&filler);
                text.push('\n');
            }
            text.push_str(&record(
                "assistant",
                "2026-10-29T09:00:00.000Z",
                &read_call("/p/late.rs"),
            ));
            text.push('\n');
            std::fs::write(&path, &text).unwrap();
            scans.push(scan_transcript(&path).unwrap());
        }
        for t in &scans {
            assert!(t.partial);
            assert_eq!(t.started.as_deref(), Some("2026-10-28T16:45:30.000Z"));
            assert!(
                t.first_prompt
                    .as_deref()
                    .is_some_and(|p| p.starts_with("Describe this screenshot")),
            );
            assert!(t.files.contains_key("/p/late.rs"));
        }
        assert!(
            !scans[1].files.contains_key("/p/early.rs"),
            "the middle of a long transcript is not read"
        );
    }

    #[test]
    fn a_short_transcript_is_read_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.jsonl");
        let text = [
            record(
                "user",
                "2026-10-28T16:45:30.000Z",
                &Value::String("Fix it".into()),
            ),
            record(
                "assistant",
                "2026-10-28T16:46:00.000Z",
                &read_call("/p/a.rs"),
            ),
        ]
        .join("\n");
        std::fs::write(&path, text).unwrap();
        let t = scan_transcript(&path).unwrap();
        assert!(!t.partial);
        assert_eq!(t.first_prompt.as_deref(), Some("Fix it"));
        assert!(t.files.contains_key("/p/a.rs"));
    }

    /// Files an earlier version of the note listed stay listed when a long
    /// transcript's scan no longer reaches them.
    #[test]
    fn files_from_the_earlier_note_are_carried() {
        let mut t = Transcript {
            first_prompt: Some("Fix it".to_owned()),
            prompts: 1,
            partial: true,
            ..Transcript::default()
        };
        t.files
            .insert("/p/src/late.rs".to_owned(), Touch { edits: 1, reads: 0 });
        let now = jiff::Zoned::now();
        let carried = ["src/early.rs".to_owned(), "src/late.rs".to_owned()];
        let out = render(&Note {
            session: "abcd1234",
            project: None,
            started: &now,
            updated: &now,
            transcript: &t,
            last: None,
            project_dir: Path::new("/p"),
            carried: &carried,
        });
        assert!(
            out.contains("files_touched:\n- src/late.rs\n- src/early.rs\n"),
            "{out}"
        );
        assert!(
            out.contains("- src/late.rs (edited ×1)\n- src/early.rs (earlier in the session)\n"),
            "{out}"
        );

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("note.md");
        std::fs::write(&path, &out).unwrap();
        assert_eq!(files_in_note(&path), ["src/late.rs", "src/early.rs"]);
        assert!(files_in_note(&tmp.path().join("missing.md")).is_empty());
    }

    #[test]
    fn short_session_keeps_eight_safe_chars() {
        assert_eq!(short_session("da5fb1ee-d800-4ba5"), "da5fb1ee");
        assert_eq!(short_session("../x"), "x");
        assert_eq!(short_session(""), "");
    }

    #[test]
    fn render_has_frontmatter_title_and_sections() {
        let mut t = Transcript {
            first_prompt: Some("Fix the  indexer: now\nmore".to_owned()),
            prompts: 1,
            ..Transcript::default()
        };
        t.files
            .insert("/p/src/a.rs".to_owned(), Touch { edits: 2, reads: 0 });
        let now = jiff::Zoned::now();
        let out = render(&Note {
            session: "abcd1234",
            project: Some("p"),
            started: &now,
            updated: &now,
            transcript: &t,
            last: Some("All <private>secret</private> done"),
            project_dir: Path::new("/p"),
            carried: &[],
        });
        assert!(
            out.starts_with(
                "---\ntitle: 'Fix the indexer: now'\nkind: session\nproject: p\nsession: abcd1234\n"
            ),
            "{out}"
        );
        assert!(out.contains("files_touched:\n- src/a.rs\n"), "{out}");
        assert!(
            out.contains("# Fix the indexer: now\n\n## Prompt\n\nFix the  indexer: now\nmore\n"),
            "{out}"
        );
        assert!(out.contains("## Last response\n\nAll  done\n"), "{out}");
        assert!(
            out.contains("## Files touched\n\n- src/a.rs (edited ×2)\n"),
            "{out}"
        );
    }

    #[test]
    fn project_dir_stops_at_git() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        std::fs::write(repo.join(".git"), "gitdir: elsewhere").unwrap();
        assert_eq!(project_dir(&repo.join("src/deep")), repo);
        let loose = tmp.path().join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        assert_eq!(project_dir(&loose), loose);
    }
}
