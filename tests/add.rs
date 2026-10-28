//! `kvault add` end to end: target root selection, frontmatter, optimistic
//! overwrite, memory-root pointer, immediate indexing.

#![allow(clippy::unwrap_used)]

mod common;

use assert_cmd::Command;
use common::{Fixture, fixture};
use predicates::prelude::*;

struct Env {
    fx: Fixture,
    config: std::path::PathBuf,
}

fn env() -> Env {
    let fx = fixture();
    let config = fx.base.join("config.toml");
    std::fs::write(&config, fx.config.to_toml().unwrap()).unwrap();
    Env { fx, config }
}

impl Env {
    fn kvault(&self) -> Command {
        let mut cmd = Command::cargo_bin("kvault").unwrap();
        cmd.env("KVAULT_CONFIG", &self.config);
        cmd.env("KVAULT_DB", self.fx.base.join("index.db"));
        cmd
    }
}

fn hash_line(stdout: &[u8]) -> String {
    let text = String::from_utf8(stdout.to_vec()).unwrap();
    text.lines()
        .find_map(|l| l.strip_prefix("blake3"))
        .unwrap_or_else(|| panic!("no blake3 line in {text}"))
        .trim()
        .to_owned()
}

#[test]
fn add_from_cwd_writes_into_the_project_notes_root_and_indexes() {
    let e = env();
    let cwd = e.fx.base.join("proj-a/src");
    std::fs::create_dir_all(&cwd).unwrap();
    e.kvault()
        .current_dir(&cwd)
        .args([
            "add",
            "--title",
            "Lambda decision",
            "--kind",
            "decision",
            "--tags",
            "aws, lambda",
        ])
        .write_stdin("We keep provisioned concurrency on.\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("wrote"))
        .stdout(predicate::str::contains("proj-a/notes/lambda-decision.md"))
        .stdout(predicate::str::contains("not indexed").not());
    let text = std::fs::read_to_string(e.fx.base.join("proj-a/notes/lambda-decision.md")).unwrap();
    assert!(
        text.starts_with("---\ntitle: Lambda decision\nkind: decision\nproject: proj-a\ntags:\n- aws\n- lambda\ncreated: "),
        "{text}"
    );
    assert!(
        text.ends_with("---\n# Lambda decision\n\nWe keep provisioned concurrency on.\n"),
        "{text}"
    );
    e.kvault()
        .args([
            "search",
            "provisioned concurrency",
            "--format",
            "paths",
            "--kind",
            "decision",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("lambda-decision.md"));
}

#[test]
fn add_refuses_to_overwrite_without_the_current_hash() {
    let e = env();
    let out = e
        .kvault()
        .args(["add", "--title", "Topic", "--project", "proj-b"])
        .write_stdin("first")
        .assert()
        .success();
    let hash = hash_line(&out.get_output().stdout);
    let path = e.fx.base.join("proj-b/notes/topic.md");
    assert!(path.exists());

    e.kvault()
        .args(["add", "--title", "Topic", "--project", "proj-b"])
        .write_stdin("second")
        .assert()
        .failure()
        .stderr(predicate::str::contains("exists; pass --expected-hash"));
    e.kvault()
        .args([
            "add",
            "--title",
            "Topic",
            "--project",
            "proj-b",
            "--expected-hash",
            "0000000000000000",
        ])
        .write_stdin("second")
        .assert()
        .failure()
        .stderr(predicate::str::contains("changed since it was read"));
    assert!(std::fs::read_to_string(&path).unwrap().contains("first"));

    // A prefix of the hash is enough; the file is replaced in place.
    e.kvault()
        .args([
            "add",
            "--title",
            "Topic",
            "--project",
            "proj-b",
            "--expected-hash",
            &hash[..12],
        ])
        .write_stdin("second")
        .assert()
        .success()
        .stdout(predicate::str::contains("replaced"));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("second") && !text.contains("first"), "{text}");
}

#[test]
fn add_into_a_memory_root_uses_claude_format_and_memory_md() {
    let e = env();
    let mem = e.fx.base.join("mem/-Users-x-Projects-proj-a/memory");
    e.kvault()
        .args([
            "add",
            "--title",
            "Prefer WAL",
            "--kind",
            "feedback",
            "--root",
            &mem.to_string_lossy(),
        ])
        .write_stdin("Always open the index in WAL mode.\n\n**Why:** readers never block.")
        .assert()
        .success()
        .stdout(predicate::str::contains("prefer-wal.md"));
    let text = std::fs::read_to_string(mem.join("prefer-wal.md")).unwrap();
    assert_eq!(
        text,
        "---\nname: prefer-wal\ndescription: Always open the index in WAL mode.\ntitle: Prefer WAL\nmetadata:\n  type: feedback\n---\n\nAlways open the index in WAL mode.\n\n**Why:** readers never block.\n"
    );
    let index = std::fs::read_to_string(mem.join("MEMORY.md")).unwrap();
    assert_eq!(
        index,
        "- [Prefer WAL](prefer-wal.md) — Always open the index in WAL mode.\n"
    );
    e.kvault()
        .args([
            "search", "WAL mode", "--format", "paths", "--kind", "memory",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("prefer-wal.md"));
}

#[test]
fn add_outside_any_project_needs_a_target() {
    let e = env();
    let loose = e.fx.base.join("loose");
    std::fs::create_dir_all(&loose).unwrap();
    e.kvault()
        .current_dir(&loose)
        .args(["add", "--title", "X"])
        .write_stdin("body")
        .assert()
        .failure()
        .stderr(predicate::str::contains("pass --project or --root"));
    e.kvault()
        .args(["add", "--title", "X", "--project", "nope"])
        .write_stdin("body")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no configured root belongs to project nope",
        ));
    // An explicit --root outside every configured root is written but not indexed.
    e.kvault()
        .args(["add", "--title", "X", "--root", &loose.to_string_lossy()])
        .write_stdin("body")
        .assert()
        .success()
        .stdout(predicate::str::contains("not indexed"));
    assert!(loose.join("x.md").exists());
}

#[test]
fn add_rejects_an_empty_body() {
    let e = env();
    e.kvault()
        .args(["add", "--title", "X", "--project", "proj-a"])
        .write_stdin("  \n")
        .assert()
        .failure()
        .stderr(predicate::str::contains("nothing to add"));
}
