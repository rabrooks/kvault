//! End-to-end CLI smoke tests for the M0 skeleton.

#![allow(clippy::unwrap_used)]

use assert_cmd::Command;
use predicates::prelude::*;

fn kvault() -> Command {
    let mut cmd = Command::cargo_bin("kvault").unwrap();
    // Never touch the developer's real config or index.
    cmd.env("KVAULT_CONFIG", "/nonexistent/kvault-config.toml");
    cmd.env("KVAULT_DB", "/nonexistent/kvault-index.db");
    // Nor the session notes and logs under their home directory.
    cmd.env("HOME", "/nonexistent/home");
    for var in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
        cmd.env_remove(var);
    }
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

/// A home directory for `init`: config, index and logs all land inside it.
fn at_home(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("kvault").unwrap();
    cmd.env("HOME", home);
    for var in [
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "KVAULT_CONFIG",
        "KVAULT_DB",
        "KVAULT_LOG",
        "KVAULT_QUERY_LOG",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

fn note(path: &std::path::Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn init_with_no_notes_anywhere_says_how_to_add_them() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    at_home(home)
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "add    ~/.claude/projects/*/memory      (nothing there yet)",
        ))
        .stdout(predicate::str::contains("skip   ~/Projects/*/notes"))
        .stdout(predicate::str::contains("kvault root add <dir>"));
    let config = std::fs::read_to_string(home.join(".config/kvault/config.toml")).unwrap();
    assert_eq!(config.matches("[[roots]]").count(), 1, "{config}");
    // A second init does not clobber the config.
    at_home(home)
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("kvault root add"));
}

#[test]
fn init_takes_notes_paths_and_root_add_appends_more() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    note(
        &home.join("code/app/notes/deploy.md"),
        "# Deploy\n\nThe app deploys through the blue green pipeline.\n",
    );
    note(
        &home.join("code/site/notes/dns.md"),
        "# DNS\n\nThe site zone lives in the shared account.\n",
    );
    note(
        &home.join("work/handbook/oncall.md"),
        "# On call\n\nThe pager rotates weekly between the engineers.\n",
    );
    at_home(home)
        .args(["init", "--notes"])
        .arg(home.join("code/*/notes"))
        .assert()
        .success()
        .stdout(predicate::str::contains("add    ~/code/*/notes"))
        .stdout(predicate::str::contains("(2 dirs: app, site)"))
        .stdout(predicate::str::contains("2 files"))
        .stdout(predicate::str::contains("kvault root add <dir>").not());
    at_home(home)
        .args([
            "search",
            "pipeline",
            "--project",
            "app",
            "--format",
            "paths",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("app/notes/deploy.md"));

    at_home(home)
        .args(["root", "add"])
        .arg(home.join("work/handbook"))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "added  ~/work/handbook (notes, project from dir; 1 dir: handbook)",
        ))
        .stdout(predicate::str::contains("3 files"));
    at_home(home)
        .args([
            "search",
            "pager",
            "--project",
            "handbook",
            "--format",
            "paths",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("handbook/handbook/oncall.md"));
    let config = std::fs::read_to_string(home.join(".config/kvault/config.toml")).unwrap();
    assert!(
        config.ends_with(
            "\n[[roots]]\npath = \"~/work/handbook\"\nkind = \"notes\"\nproject_from = \"dir\"\n"
        ),
        "{config}"
    );

    // Already there; and a directory that does not exist yet is accepted.
    at_home(home)
        .args(["root", "add"])
        .arg(home.join("work/handbook/"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("already a configured root"));
    at_home(home)
        .args([
            "root",
            "add",
            "--kind",
            "reference",
            "--project-from",
            "none",
            "--weight",
            "0.8",
        ])
        .arg(home.join("later"))
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing matches yet"));
    let config = std::fs::read_to_string(home.join(".config/kvault/config.toml")).unwrap();
    assert!(
        config.contains("kind = \"reference\"\nweight = 0.8\nproject_from = \"none\""),
        "{config}"
    );
}

#[test]
fn root_add_needs_a_config() {
    let tmp = tempfile::tempdir().unwrap();
    at_home(tmp.path())
        .args(["root", "add", "/somewhere"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("kvault init"));
}
