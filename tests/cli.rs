//! End-to-end CLI smoke tests for the M0 skeleton.

#![allow(clippy::unwrap_used)]

use assert_cmd::Command;
use predicates::prelude::*;

fn kvault() -> Command {
    let mut cmd = Command::cargo_bin("kvault").unwrap();
    // Never touch the developer's real config or index.
    cmd.env("KVAULT_CONFIG", "/nonexistent/kvault-config.toml");
    cmd.env("KVAULT_DB", "/nonexistent/kvault-index.db");
    cmd
}

#[test]
fn help_lists_the_v0_surface() {
    let out = kvault().arg("--help").assert().success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    for sub in [
        "init", "index", "search", "recall", "get", "brief", "add", "list", "links", "status",
        "bench", "hook",
    ] {
        assert!(text.contains(sub), "missing subcommand {sub} in help");
    }
}

#[test]
fn status_runs_with_defaults() {
    kvault()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("roots:   0 configured"))
        .stdout(predicate::str::contains("not built"));
}

#[test]
fn status_reads_config_and_expands_roots() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("p1/notes")).unwrap();
    std::fs::create_dir_all(tmp.path().join("p2/notes")).unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "[[roots]]\npath = \"{}/*/notes\"\nkind = \"notes\"\nproject_from = \"parent\"\n",
            tmp.path().display()
        ),
    )
    .unwrap();
    kvault()
        .env("KVAULT_CONFIG", &cfg)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("1 configured, 2 present"))
        .stdout(predicate::str::contains("p1"))
        .stdout(predicate::str::contains("p2"));
}

#[test]
fn status_opens_db_when_present() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("index.db");
    kvault::store::Store::open(&db).unwrap();
    kvault()
        .env("KVAULT_DB", &db)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("0 roots, 0 docs, 0 chunks"));
}

#[test]
fn unimplemented_commands_exit_3_with_milestone() {
    kvault()
        .args(["links", "anything"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("M5"));
}

#[test]
fn hooks_exit_zero_even_when_nothing_is_configured() {
    let tmp = tempfile::tempdir().unwrap();
    for event in ["session-start", "stop", "pre-compact"] {
        kvault()
            .env("KVAULT_DB", tmp.path().join("index.db"))
            .env("KVAULT_LOG", tmp.path().join("hooks.log"))
            .args(["hook", event])
            .write_stdin("{}")
            .assert()
            .success()
            .stdout(predicate::str::starts_with("{"));
    }
    // Nothing configured is not an error worth a log line.
    assert!(!tmp.path().join("hooks.log").exists());
}

#[test]
fn bad_config_is_a_clear_error() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[brief]\nbudgett = 1\n").unwrap();
    kvault()
        .env("KVAULT_CONFIG", &cfg)
        .arg("status")
        .assert()
        .failure()
        .stderr(predicate::str::contains("config file"))
        .stderr(predicate::str::contains("budgett"));
}
