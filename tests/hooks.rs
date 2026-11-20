//! `kvault hook` end to end: stdin JSON in, JSON out, exit 0 always, notes
//! written where the design says.

#![allow(clippy::unwrap_used)]

mod common;

use std::path::Path;

use assert_cmd::Command;
use common::{Fixture, fixture, write};

struct Env {
    fx: Fixture,
    config: std::path::PathBuf,
    log: std::path::PathBuf,
}

fn env(fallback_dir: Option<&str>) -> Env {
    let mut fx = fixture();
    if let Some(d) = fallback_dir {
        d.clone_into(&mut fx.config.capture.fallback_dir);
    }
    let config = fx.base.join("config.toml");
    std::fs::write(&config, fx.config.to_toml().unwrap()).unwrap();
    let log = fx.base.join("hooks.log");
    Env { fx, config, log }
}

impl Env {
    fn kvault(&self) -> Command {
        let mut cmd = Command::cargo_bin("kvault").unwrap();
        cmd.env("KVAULT_CONFIG", &self.config);
        cmd.env("KVAULT_DB", self.fx.base.join("index.db"));
        cmd.env("KVAULT_LOG", &self.log);
        cmd
    }

    fn hook(&self, event: &str, stdin: &str) -> serde_json::Value {
        let out = self
            .kvault()
            .args(["hook", event])
            .write_stdin(stdin)
            .assert()
            .success();
        let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text:?}"))
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

fn transcript(path: &Path, cwd: &Path) {
    let lines = [
        serde_json::json!({"type":"mode","mode":"normal"}).to_string(),
        serde_json::json!({"parentUuid":null,"isSidechain":false,"type":"user","isMeta":true,
            "message":{"role":"user","content":"<local-command-caveat>x</local-command-caveat>"},
            "timestamp":"2026-10-28T16:45:25.275Z","cwd":cwd}).to_string(),
        serde_json::json!({"parentUuid":"a","isSidechain":false,"type":"user",
            "message":{"role":"user","content":"Switch the index to WAL mode please"},
            "timestamp":"2026-10-28T16:45:30.000Z","cwd":cwd}).to_string(),
        serde_json::json!({"parentUuid":"b","isSidechain":false,"type":"assistant",
            "message":{"role":"assistant","content":[
                {"type":"text","text":"Looking at the store."},
                {"type":"tool_use","id":"t1","name":"Read","input":{"file_path":cwd.join("src/store.rs")}}
            ]},"timestamp":"2026-10-28T16:45:40.000Z"}).to_string(),
        serde_json::json!({"parentUuid":"c","isSidechain":false,"type":"assistant",
            "message":{"role":"assistant","content":[
                {"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":cwd.join("src/store.rs"),"old_string":"a","new_string":"b"}}
            ]},"timestamp":"2026-10-28T16:46:00.000Z"}).to_string(),
        serde_json::json!({"parentUuid":"d","isSidechain":false,"type":"assistant",
            "message":{"role":"assistant","content":[{"type":"text","text":"Done from the transcript."}]},
            "timestamp":"2026-10-28T16:46:10.000Z"}).to_string(),
    ];
    write(path, &(lines.join("\n") + "\n"));
}

#[test]
fn session_start_injects_the_brief_for_the_project() {
    let e = env(None);
    let cwd = e.fx.base.join("proj-a");
    let v = e.hook(
        "session-start",
        &serde_json::json!({"session_id":"s1","cwd":cwd,"source":"startup","hook_event_name":"SessionStart"}).to_string(),
    );
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("no additionalContext in {v}"));
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    assert!(ctx.contains("proj-a"), "{ctx}");
    assert!(ctx.contains("Architecture"), "pinned note missing: {ctx}");
    assert!(ctx.len() <= kvault::brief::MAX_BYTES);
    assert_eq!(e.log(), "", "hook logged an error");
}

#[test]
fn session_start_is_silent_on_resume_and_fork() {
    let e = env(None);
    for source in ["resume", "fork"] {
        let v = e.hook(
            "session-start",
            &serde_json::json!({"session_id":"s1","cwd":e.fx.base.join("proj-a"),"source":source})
                .to_string(),
        );
        assert_eq!(v, serde_json::json!({}), "{source}");
    }
}

#[test]
fn bad_input_exits_zero_prints_empty_object_and_logs() {
    let e = env(None);
    let v = e.hook("session-start", "this is not json");
    assert_eq!(v, serde_json::json!({}));
    let log = e.log();
    assert!(log.contains("session-start hook input"), "{log}");
    assert_eq!(log.lines().count(), 1);
}

#[test]
fn empty_input_still_answers() {
    let e = env(None);
    // No cwd in the input: the brief is for the process cwd, which belongs
    // to no project, so it is the global brief.
    let v = e.hook("stop", "");
    assert_eq!(v, serde_json::json!({}));
    assert_eq!(e.log(), "");
}

#[test]
fn stop_writes_one_session_note_per_session_and_indexes_it() {
    let e = env(None);
    let cwd = e.fx.base.join("proj-a");
    let t = e.fx.base.join("transcript.jsonl");
    transcript(&t, &cwd);
    let input = serde_json::json!({
        "session_id":"deadbeef-1234-5678","cwd":cwd,"transcript_path":t,
        "hook_event_name":"Stop","stop_hook_active":false,
        "last_assistant_message":"Switched to WAL mode. <private>password hunter2</private> AKIAIOSFODNN7EXAMPLE was in the env."
    })
    .to_string();

    let v = e.hook("stop", &input);
    assert_eq!(v, serde_json::json!({}));
    assert_eq!(e.log(), "", "hook logged an error");

    let sessions = cwd.join("notes/sessions");
    let files: Vec<_> = std::fs::read_dir(&sessions)
        .unwrap()
        .map(|d| d.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let path = &files[0];
    let name = path.file_name().unwrap().to_string_lossy();
    assert!(name.ends_with("-deadbeef.md"), "{name}");
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.starts_with("---\ntitle: Switch the index to WAL mode please\nkind: session\nproject: proj-a\nsession: deadbeef\n"), "{text}");
    assert!(text.contains("files_touched:\n- src/store.rs\n"), "{text}");
    assert!(
        text.contains("## Last response\n\nSwitched to WAL mode.  [redacted] was in the env.\n"),
        "{text}"
    );
    assert!(!text.contains("hunter2"));
    assert!(
        text.contains("- src/store.rs (edited ×1, read ×1)"),
        "{text}"
    );

    // Indexed: searchable right away, scoped to the project.
    e.kvault()
        .args([
            "search",
            "WAL mode",
            "--project",
            "proj-a",
            "--format",
            "paths",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("sessions/"));

    // A second Stop in the same session rewrites the same file.
    e.hook("stop", &input);
    assert_eq!(std::fs::read_dir(&sessions).unwrap().count(), 1);
}

#[test]
fn pre_compact_uses_the_transcript_for_the_last_message() {
    let e = env(None);
    let cwd = e.fx.base.join("proj-a");
    let t = e.fx.base.join("transcript.jsonl");
    transcript(&t, &cwd);
    e.hook(
        "pre-compact",
        &serde_json::json!({"session_id":"cafe0001","cwd":cwd,"transcript_path":t,"trigger":"auto"}).to_string(),
    );
    assert_eq!(e.log(), "");
    let dir = cwd.join("notes/sessions");
    let path = std::fs::read_dir(&dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let text = std::fs::read_to_string(path).unwrap();
    assert!(
        text.contains("## Last response\n\nDone from the transcript.\n"),
        "{text}"
    );
}

#[test]
fn stop_falls_back_when_the_project_has_no_notes_dir() {
    let fallback_holder = tempfile::tempdir().unwrap();
    let fallback = fallback_holder.path().join("sessions");
    let e = env(Some(&fallback.to_string_lossy()));
    let cwd = e.fx.base.join("loose");
    std::fs::create_dir_all(&cwd).unwrap();
    let t = e.fx.base.join("transcript.jsonl");
    transcript(&t, &cwd);
    e.hook(
        "stop",
        &serde_json::json!({"session_id":"abcd0000","cwd":cwd,"transcript_path":t,"last_assistant_message":"ok"}).to_string(),
    );
    assert_eq!(e.log(), "");
    let dir = fallback.join("loose");
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|d| d.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].ends_with("-abcd0000.md"));
    assert!(!e.fx.base.join("loose/notes").exists());
    // No root was configured for it, yet the note is indexed under the
    // project the directory is named for.
    e.kvault()
        .args(["list", "--project", "loose"])
        .assert()
        .success()
        .stdout(predicates::str::contains(names[0].as_str()))
        .stdout(predicates::str::contains("1 documents"));
    e.kvault()
        .args(["search", "WAL mode", "--format", "paths"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "loose/loose/{}",
            names[0]
        )));
}

/// The log lines as `JSON` values.
fn query_log(e: &Env) -> Vec<serde_json::Value> {
    std::fs::read_to_string(e.fx.base.join("queries.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn queries_are_logged_and_stop_marks_the_results_that_were_used() {
    let e = env(None);
    let cwd = e.fx.base.join("proj-a");
    for (command, session) in [("recall", Some("sess-0001")), ("search", None)] {
        let mut cmd = e.kvault();
        cmd.current_dir(&cwd).args([command, "lambda cold start"]);
        match session {
            Some(s) => cmd.env("CLAUDE_CODE_SESSION_ID", s),
            None => cmd.env_remove("CLAUDE_CODE_SESSION_ID"),
        };
        cmd.assert().success();
    }
    let log = query_log(&e);
    assert_eq!(log.len(), 2, "{log:?}");
    let recall = &log[0];
    assert_eq!(recall["event"], "query");
    assert_eq!(recall["command"], "recall");
    assert_eq!(recall["query"], "lambda cold start");
    assert_eq!(recall["budget"], 1500);
    assert_eq!(recall["session"], "sess-0001");
    assert!(log[1].get("session").is_none(), "run outside a session");
    let hits = recall["hits"].as_array().unwrap();
    assert!(hits.len() >= 2, "{hits:?}");
    let plan = hits
        .iter()
        .find(|h| h["path"].as_str().unwrap().ends_with("notes/plan.md"))
        .unwrap();
    let fact = hits
        .iter()
        .find(|h| h["path"].as_str().unwrap().ends_with("lambda-timeouts.md"))
        .unwrap();

    // The session then fetches one hit by id and reads the other's file.
    let t = e.fx.base.join("transcript.jsonl");
    let later = "2999-01-01T00:00:00.000Z";
    let lines = [
        serde_json::json!({"type":"user","message":{"role":"user","content":"why is it slow"},
            "timestamp":"2026-10-28T16:45:30.000Z"}).to_string(),
        serde_json::json!({"type":"assistant","timestamp":later,"message":{"role":"assistant","content":[
            {"type":"tool_use","id":"t1","name":"Bash","input":{"command":format!("kvault get {}", fact["id"].as_str().unwrap())}},
            {"type":"tool_use","id":"t2","name":"Read","input":{"file_path":plan["file"]}}
        ]}}).to_string(),
    ];
    write(&t, &(lines.join("\n") + "\n"));
    let input = serde_json::json!({"session_id":"sess-0001","cwd":cwd,"transcript_path":t,
        "last_assistant_message":"done"})
    .to_string();
    e.hook("stop", &input);
    e.hook("stop", &input);
    assert_eq!(e.log(), "");
    let mut uses: Vec<(String, String)> = query_log(&e)
        .iter()
        .filter(|l| l["event"] == "use")
        .map(|l| {
            assert_eq!(l["session"], "sess-0001");
            assert_eq!(l["query_at"], recall["at"]);
            (
                l["id"].as_str().unwrap().to_owned(),
                l["how"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    uses.sort();
    let mut expected = vec![
        (fact["id"].as_str().unwrap().to_owned(), "get".to_owned()),
        (plan["id"].as_str().unwrap().to_owned(), "file".to_owned()),
    ];
    expected.sort();
    assert_eq!(uses, expected, "each use once, however often Stop runs");
}

#[test]
fn briefs_and_sessions_are_logged_and_report_summarises_them() {
    let e = env(None);
    let cwd = e.fx.base.join("proj-a");
    let start =
        serde_json::json!({"session_id":"sess-0002","cwd":cwd,"source":"startup"}).to_string();
    e.hook("session-start", &start);
    let log = query_log(&e);
    assert_eq!(log.len(), 1, "{log:?}");
    let brief = &log[0];
    assert_eq!(brief["event"], "brief");
    assert_eq!(brief["session"], "sess-0002");
    assert_eq!(brief["project"], "proj-a");
    assert_eq!(brief["source"], "startup");
    assert!(brief["tokens"].as_u64().unwrap() > 0);
    assert!(brief["docs"].as_i64().unwrap() >= 8);
    assert!(brief["version"].is_string());
    let listed = brief["hits"].as_array().unwrap();
    assert!(!listed.is_empty());

    // The session reads a note the brief listed, and never searches.
    let t = e.fx.base.join("transcript.jsonl");
    let lines = [
        serde_json::json!({"type":"user","message":{"role":"user","content":"carry on"},
            "timestamp":"2026-10-28T16:45:30.000Z"}).to_string(),
        serde_json::json!({"type":"assistant","timestamp":"2999-01-01T00:00:00.000Z","message":{"role":"assistant","content":[
            {"type":"tool_use","id":"t1","name":"Read","input":{"file_path":listed[0]["file"]}}
        ]}}).to_string(),
    ];
    write(&t, &(lines.join("\n") + "\n"));
    let stop = serde_json::json!({"session_id":"sess-0002","cwd":cwd,"transcript_path":t,
        "last_assistant_message":"done"})
    .to_string();
    e.hook("stop", &stop);
    e.hook("stop", &stop);
    assert_eq!(e.log(), "");
    let log = query_log(&e);
    let of = |kind: &str| log.iter().filter(|l| l["event"] == kind).count();
    assert_eq!(
        (of("brief"), of("session"), of("use"), of("query")),
        (1, 1, 1, 0)
    );
    let used = log.iter().find(|l| l["event"] == "use").unwrap();
    assert_eq!(used["from"], "brief");
    assert_eq!(used["how"], "file");
    assert_eq!(used["query_at"], brief["at"]);

    let out = e.kvault().args(["report"]).assert().success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        text.contains("sessions  1 given a brief, 1 completed a turn"),
        "{text}"
    );
    assert!(
        text.contains("0 searched (0%), 0 used a result (0%), 1 used a note from the brief (100%)"),
        "{text}"
    );
    assert!(text.contains("uses      brief:file 1"), "{text}");
    assert!(
        !text.contains("proj-a"),
        "no project names in the summary:\n{text}"
    );
    let json = e.kvault().args(["report", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_slice(&json.get_output().stdout).unwrap();
    assert_eq!(v["sessions"]["followed_the_brief"], 1);
    assert_eq!(v["briefs"]["injected"], 1);
    e.kvault()
        .args(["report", "--full", "--since", "1d"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"event\":\"brief\""));
    e.kvault()
        .args(["report", "--since", "soon"])
        .assert()
        .failure();
}

#[test]
fn query_log_can_be_disabled() {
    let mut e = env(None);
    e.fx.config.query_log.enabled = false;
    std::fs::write(&e.config, e.fx.config.to_toml().unwrap()).unwrap();
    e.kvault().args(["recall", "lambda"]).assert().success();
    assert!(query_log(&e).is_empty());
}

#[test]
fn stop_writes_nothing_without_a_session_or_content() {
    let e = env(None);
    let cwd = e.fx.base.join("proj-a");
    e.hook(
        "stop",
        &serde_json::json!({"session_id":"","cwd":cwd,"last_assistant_message":"x"}).to_string(),
    );
    e.hook(
        "stop",
        &serde_json::json!({"session_id":"s2","cwd":cwd}).to_string(),
    );
    assert!(!cwd.join("notes/sessions").exists());
    assert_eq!(e.log(), "");
}

#[test]
fn capture_can_be_disabled() {
    let mut e = env(None);
    e.fx.config.capture.enabled = false;
    std::fs::write(&e.config, e.fx.config.to_toml().unwrap()).unwrap();
    let cwd = e.fx.base.join("proj-a");
    e.hook(
        "stop",
        &serde_json::json!({"session_id":"s3","cwd":cwd,"last_assistant_message":"x"}).to_string(),
    );
    assert!(!cwd.join("notes/sessions").exists());
}
