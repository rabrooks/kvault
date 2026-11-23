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
        .stderr(predicate::str::contains(
            "already holds this note; read it, then pass --expected-hash",
        ));
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

/// Two titles can make the same file name. The second is refused, and the
/// refusal must not hand over the hash that would replace the first.
#[test]
fn add_refuses_a_different_note_with_the_same_file_name() {
    let e = env();
    let out = e
        .kvault()
        .args(["add", "--title", "C notes", "--project", "proj-b"])
        .write_stdin("about c")
        .assert()
        .success();
    let hash = hash_line(&out.get_output().stdout);
    let path = e.fx.base.join("proj-b/notes/c-notes.md");

    e.kvault()
        .args(["add", "--title", "C++ notes", "--project", "proj-b"])
        .write_stdin("about c++")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "is taken by a different note (\"C notes\"); choose another title",
        ))
        .stderr(predicate::str::contains("expected-hash").not())
        .stderr(predicate::str::contains(&hash[..12]).not());
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("about c\n")
    );

    // A hand-written file with no frontmatter title is someone else's too.
    let plain = e.fx.base.join("proj-b/notes/plain.md");
    std::fs::write(&plain, "# Plain\n\nwritten by hand\n").unwrap();
    e.kvault()
        .args(["add", "--title", "Plain", "--project", "proj-b"])
        .write_stdin("generated")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "is taken by a different note (untitled)",
        ));
    assert!(
        std::fs::read_to_string(&plain)
            .unwrap()
            .contains("written by hand")
    );
}

#[test]
fn expected_hash_must_be_long_enough_and_have_something_to_replace() {
    let e = env();
    let add = |hash: &str| {
        let mut cmd = e.kvault();
        cmd.args([
            "add",
            "--title",
            "Fresh",
            "--project",
            "proj-b",
            "--expected-hash",
            hash,
        ])
        .write_stdin("body");
        cmd
    };
    add("abc")
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs at least 8 hex digits"));
    add("0123456789abcdef")
        .assert()
        .failure()
        .stderr(predicate::str::contains("nothing to replace"));
    assert!(!e.fx.base.join("proj-b/notes/fresh.md").exists());
}

/// `MEMORY.md` is Claude Code's own index: whatever is in it stays, even when
/// it is not valid UTF-8 or lacks a final newline.
#[test]
fn memory_md_is_appended_to_never_rewritten() {
    let e = env();
    let mem = e.fx.base.join("mem/-Users-x-Projects-proj-a/memory");
    let before: &[u8] = b"- [old](old.md) \xff broken byte\n- [other](other.md) no newline";
    std::fs::write(mem.join("MEMORY.md"), before).unwrap();
    let add = |title: &str| {
        e.kvault()
            .args(["add", "--title", title, "--root", &mem.to_string_lossy()])
            .write_stdin("Always open the index in WAL mode.")
            .assert()
            .success();
    };
    add("Prefer WAL");
    let mut want = before.to_vec();
    want.extend_from_slice(
        "\n- [Prefer WAL](prefer-wal.md) — Always open the index in WAL mode.\n".as_bytes(),
    );
    assert_eq!(std::fs::read(mem.join("MEMORY.md")).unwrap(), want);

    // A title's line break cannot split the pointer line.
    add("Two\nlines");
    let text = String::from_utf8_lossy(&std::fs::read(mem.join("MEMORY.md")).unwrap()).into_owned();
    assert!(
        text.ends_with("\n- [Two lines](two-lines.md) — Always open the index in WAL mode.\n"),
        "{text}"
    );
}
