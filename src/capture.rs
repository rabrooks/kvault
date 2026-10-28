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
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::brief;
use crate::config::{Config, expand_tilde};
use crate::error::{Error, Result};
use crate::pack;
use crate::store::Store;

/// Bytes read from the start of a transcript: start time and first prompt.
pub const TRANSCRIPT_HEAD: u64 = 128 * 1024;
/// Bytes read from the end: last message and files touched. Head plus tail
/// is the 1 MB ceiling the hook contract promises.
pub const TRANSCRIPT_TAIL: u64 = 896 * 1024;
/// Characters kept of the last assistant message.
pub const LAST_MESSAGE_CHARS: usize = 4_000;
/// Characters kept of the first prompt.
pub const PROMPT_CHARS: usize = 600;
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
    let note = render(&Note {
        session: &session,
        project: project.as_deref(),
        started: &started,
        updated: &now,
        transcript: &transcript,
        last: last.as_deref(),
        project_dir: &project_dir,
    });
    std::fs::create_dir_all(&dir)?;
    write_atomic(&path, &note)?;
    Ok(Some(path))
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
/// (normally `notes`) exists under the project, else the fallback dir with
/// a per-project subdirectory.
fn session_dir(config: &Config, project_dir: &Path, project: Option<&str>) -> PathBuf {
    let rel = Path::new(&config.capture.session_dir);
    let anchor = rel.components().next().map(|c| project_dir.join(c));
    if anchor.is_some_and(|p| p.is_dir()) {
        project_dir.join(rel)
    } else {
        expand_tilde(&config.capture.fallback_dir).join(project.unwrap_or("_global"))
    }
}

struct Note<'a> {
    session: &'a str,
    project: Option<&'a str>,
    started: &'a jiff::Zoned,
    updated: &'a jiff::Zoned,
    transcript: &'a Transcript,
    last: Option<&'a str>,
    project_dir: &'a Path,
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
    let title = n
        .transcript
        .first_prompt
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
    let files: Vec<(String, Touch)> = n
        .transcript
        .files
        .iter()
        .map(|(p, t)| (relative_to(p, n.project_dir), *t))
        .collect();
    let fm = Frontmatter {
        title: title.clone(),
        kind: "session",
        project: n.project,
        session: n.session,
        started: local_time(n.started),
        updated: local_time(n.updated),
        prompts: n.transcript.prompts,
        files_touched: files.iter().map(|(p, _)| p.clone()).collect(),
        tags: ["session"],
    };
    let mut out = String::new();
    out.push_str("---\n");
    // Serializing a struct of strings and a string list cannot fail.
    out.push_str(&serde_norway::to_string(&fm).unwrap_or_default());
    out.push_str("---\n");
    let _ = writeln!(out, "# {title}");
    if let Some(p) = &n.transcript.first_prompt {
        let _ = write!(
            out,
            "\n## Prompt\n\n{}\n",
            redact(&pack::truncate(p.trim(), PROMPT_CHARS))
        );
    }
    if let Some(last) = n.last {
        let _ = write!(
            out,
            "\n## Last response\n\n{}\n",
            redact(&pack::truncate(
                strip_private(last).trim(),
                LAST_MESSAGE_CHARS
            ))
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

/// Scan a Claude Code transcript (`JSON` lines). Reads at most the head and
/// tail ceilings; the gap, if any, only costs an exact prompt count.
///
/// # Errors
/// Returns an error if the file cannot be read.
pub fn scan_transcript(path: &Path) -> Result<Transcript> {
    let (head, tail) = read_head_tail(path)?;
    let mut t = Transcript::default();
    for line in head.lines() {
        scan_line(line, &mut t);
    }
    if let Some(tail) = tail {
        // The first tail line is almost certainly cut; a partial record never parses.
        for line in tail.lines().skip(1) {
            scan_line(line, &mut t);
        }
    }
    Ok(t)
}

fn read_head_tail(path: &Path) -> Result<(String, Option<String>)> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    if len <= TRANSCRIPT_HEAD + TRANSCRIPT_TAIL {
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        return Ok((String::from_utf8_lossy(&buf).into_owned(), None));
    }
    let mut head = vec![0; usize::try_from(TRANSCRIPT_HEAD).unwrap_or(usize::MAX)];
    f.read_exact(&mut head)?;
    f.seek(SeekFrom::Start(len - TRANSCRIPT_TAIL))?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail)?;
    Ok((
        String::from_utf8_lossy(&head).into_owned(),
        Some(String::from_utf8_lossy(&tail).into_owned()),
    ))
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
                        record_tool(b, t);
                    }
                }
                let text = text_blocks(blocks);
                if !text.trim().is_empty() {
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

fn record_tool(b: &Value, t: &mut Transcript) {
    let input = &b["input"];
    let (path, edit) = match b["name"].as_str() {
        Some("Edit" | "Write" | "MultiEdit") => (input["file_path"].as_str(), true),
        Some("NotebookEdit") => (input["notebook_path"].as_str(), true),
        Some("Read") => (input["file_path"].as_str(), false),
        _ => return,
    };
    let Some(path) = path else {
        return;
    };
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

/// Replace credential-shaped tokens with `[redacted]`: PEM lines, `Bearer x`,
/// AWS access keys, GitHub, Slack, `OpenAI` and Anthropic key prefixes, Google
/// API keys. Deliberately a short list; the note is a summary, not a vault.
#[must_use]
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if line.contains("-----BEGIN") {
            out.push_str("[redacted]");
            continue;
        }
        let mut after_bearer = false;
        for (j, word) in line.split(' ').enumerate() {
            if j > 0 {
                out.push(' ');
            }
            if after_bearer || is_secret_token(word) {
                out.push_str("[redacted]");
            } else {
                out.push_str(word);
            }
            after_bearer = word.eq_ignore_ascii_case("bearer");
        }
    }
    out
}

fn is_secret_token(word: &str) -> bool {
    let w = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-');
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
