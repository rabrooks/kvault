//! `recall` and `brief` over the shared fixture: budget invariants, tiers,
//! trimming, project detection, and the CLI.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use assert_cmd::Command;
use common::{Fixture, fixture};
use kvault::brief::{self, BriefOptions};
use kvault::cli::RecallFormat;
use kvault::pack::{ByteHeuristic, Tier, TokenCounter};
use kvault::recall::{self, Recall, RecallOptions};
use kvault::search::Query;
use predicates::prelude::*;

fn run(fx: &Fixture, q: &str, budget: usize) -> Recall {
    run_as(fx, q, budget, RecallFormat::Md)
}

fn run_as(fx: &Fixture, q: &str, budget: usize, format: RecallFormat) -> Recall {
    recall::run(
        &fx.store,
        &Query::parse(q),
        &RecallOptions {
            budget,
            project: None,
            format,
        },
    )
    .unwrap()
}

fn assert_within(r: &Recall) {
    assert!(
        r.tokens <= r.budget,
        "budget {}: {} tokens\n{}",
        r.budget,
        r.tokens,
        r.text
    );
    assert_eq!(
        ByteHeuristic.count(&r.text),
        r.tokens,
        "reported tokens are the rendered text's"
    );
}

#[test]
fn recall_fits_every_budget_and_is_generous_when_it_can_be() {
    let fx = fixture();
    let q = "sqlite kubernetes lambda";
    let total = run(&fx, q, 5000).entries.len();
    assert!(total >= 5, "{total}");
    for budget in [0, 20, 50, 100, 200, 400, 800, 1600, 5000] {
        let r = run(&fx, q, budget);
        assert_within(&r);
        assert_eq!(
            r.entries.len() + r.omitted.len(),
            total,
            "every hit accounted for"
        );
        if budget < 20 {
            assert_eq!(r.text, "");
            assert_eq!(r.omitted.len(), total);
        }
    }
    let r = run(&fx, q, 5000);
    assert!(r.omitted.is_empty());
    assert_eq!(r.entries[0].tier, Tier::L2);
    assert_eq!(r.entries[2].tier, Tier::L2);
    assert_eq!(r.entries[3].tier, Tier::L1);
    assert!(r.text.contains(&format!("({total} of {total} hits)")));
    assert!(!r.text.contains("omitted"));
    assert!(r.text.ends_with("Fetch more: `kvault get <id>`\n"));

    let r = run(&fx, q, 90);
    assert!(!r.omitted.is_empty());
    assert!(
        r.text
            .contains(&format!("{} more hits omitted.", r.omitted.len()))
    );
    assert!(!r.entries.is_empty(), "rank 1 is shed last");
}

#[test]
fn recall_text_is_trimmed_and_tiers_render_as_documented() {
    let fx = fixture();
    let r = run(&fx, "\"cold start\"", 5000);
    let top = &r.entries[0];
    let full = top.text.as_deref().unwrap();
    assert!(!full.contains("\n\n\n"), "blank runs collapsed:\n{full}");
    assert!(!full.contains(" \n"), "no trailing whitespace");
    assert!(!full.starts_with("---"), "no frontmatter");
    assert!(full.ends_with('\n') && !full.ends_with("\n\n"));
    // Header line: id, display path with lines, heading path without the
    // repeated title.
    let header = r.text.lines().nth(2).unwrap();
    assert!(header.starts_with(&format!("#{}  ", top.id)), "{header}");
    assert!(header.contains(&top.path));
    assert!(!header.contains("Plan › Plan"), "{header}");

    let r = run(&fx, "\"cold start\"", 120);
    let top = &r.entries[0];
    assert_eq!(top.tier, Tier::L1);
    let preview = top.text.as_deref().unwrap();
    assert!((1..=2).contains(&preview.lines().count()), "{preview}");
    assert!(preview.lines().all(|l| l.starts_with("  ")));

    let none = run(&fx, "zzz-nothing", 500);
    assert!(none.text.starts_with("No results for"));
    assert!(none.entries.is_empty());
}

#[test]
fn recall_json_is_budgeted_on_its_own_bytes() {
    let fx = fixture();
    for budget in [20, 150, 400, 1000, 5000] {
        let r = run_as(&fx, "sqlite kubernetes lambda", budget, RecallFormat::Json);
        assert_within(&r);
        if r.text.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(&r.text).unwrap();
        assert_eq!(v["budget"], budget);
        assert_eq!(v["entries"].as_array().unwrap().len(), r.entries.len());
        assert_eq!(v["omitted"], r.omitted.len());
        assert!(v["entries"][0]["tier"].is_string());
        assert!(v["entries"][0].get("explain").is_none(), "lean entries");
    }
    // JSON framing costs more than markdown, so it fits fewer entries.
    let md = run_as(&fx, "sqlite kubernetes lambda", 400, RecallFormat::Md);
    let json = run_as(&fx, "sqlite kubernetes lambda", 400, RecallFormat::Json);
    assert!(json.entries.len() <= md.entries.len());
}

#[test]
fn recall_never_quotes_a_stale_chunk_in_full() {
    let fx = fixture();
    let path = fx.base.join("proj-a/notes/plan.md");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.insert_str(0, "<!-- edited after indexing -->\n");
    std::fs::write(&path, text).unwrap();
    let f = std::fs::File::options().write(true).open(&path).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(10))
        .unwrap();

    let r = run(&fx, "provisioned concurrency", 5000);
    let plan = r
        .entries
        .iter()
        .find(|e| e.path.ends_with("plan.md"))
        .unwrap();
    assert!(plan.stale);
    assert_eq!(plan.tier, Tier::L1, "a changed file gets a preview at most");
    assert!(r.text.contains("[stale]"));
}

/// Deterministic LCG so the test needs no extra crate.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next()).unwrap() % n
    }
}

#[test]
fn recall_never_exceeds_budget_over_a_thousand_random_queries() {
    let fx = fixture();
    let vocab = [
        "sqlite",
        "kubernetes",
        "lambda",
        "cold",
        "start",
        "plan",
        "phase",
        "pods",
        "storage",
        "timeout",
        "tantivy",
        "architecture",
        "readme",
        "project",
        "warm",
        "index",
        "node",
        "autoscaler",
        "decision",
        "markdown",
        "truth",
        "zebra",
    ];
    let mut rng = Lcg(0x5eed);
    for i in 0..1000 {
        let n = 1 + rng.below(3);
        let words: Vec<&str> = (0..n).map(|_| vocab[rng.below(vocab.len())]).collect();
        let mut q = words.join(" ");
        if i % 7 == 0 {
            q = format!("\"{q}\"");
        }
        let budget = 30 + rng.below(3000);
        let r = run(&fx, &q, budget);
        assert_within(&r);
    }
}

#[test]
fn brief_detects_the_project_and_fits_its_limits() {
    let fx = fixture();
    let opts = |cwd: std::path::PathBuf, budget: usize| BriefOptions {
        cwd,
        budget,
        recent_days: 14,
    };
    let text = brief::run(&fx.store, &opts(fx.base.join("proj-a/notes/sub"), 2000)).unwrap();
    let header = text.lines().next().unwrap();
    assert!(
        header.starts_with("# kvault: proj-a (3 notes, 2 memory facts, indexed "),
        "{header}"
    );
    assert!(text.contains("Pinned / rules:\n  #"), "{text}");
    assert!(
        text.contains("sub/arch.md \"Architecture\" — The storage layer"),
        "{text}"
    );
    assert!(text.contains("Recent (14d) in this project:\n"), "{text}");
    assert!(
        text.contains("proj-a/notes/plan.md — This plan document"),
        "{text}"
    );
    assert!(
        text.contains("Related elsewhere:\n  #") && text.contains("proj-b/notes/cross.md"),
        "cross.md links to plan.md and shares the design tag:\n{text}"
    );
    assert!(
        !text.contains("proj-b/notes/readme.md"),
        "unrelated docs stay out"
    );
    assert!(text.ends_with("Save: `kvault add`\n"));
    assert!(text.len() <= brief::MAX_BYTES);
    assert!(ByteHeuristic.count(&text) <= 2000);
    // Only one entry per document even though arch.md is pinned and recent.
    assert_eq!(text.matches("sub/arch.md").count(), 1);

    let again = brief::run(&fx.store, &opts(fx.base.join("proj-a"), 2000)).unwrap();
    let body = |s: &str| s.split_once('\n').unwrap().1.to_owned();
    assert_eq!(
        body(&text),
        body(&again),
        "project from the project dir itself, byte-stable below the header"
    );
    assert!(again.starts_with("# kvault: proj-a (3 notes, 2 memory facts, indexed "));

    for budget in [0, 40, 60, 100, 300] {
        let small = brief::run(&fx.store, &opts(fx.base.join("proj-a"), budget)).unwrap();
        assert!(
            ByteHeuristic.count(&small) <= budget,
            "budget {budget}:\n{small}"
        );
        if !small.is_empty() {
            assert!(small.starts_with("# kvault: proj-a"));
            assert!(small.ends_with("Save: `kvault add`\n"));
        }
        if budget == 100 {
            assert!(
                small.contains("Pinned / rules:"),
                "pinned survives longest:\n{small}"
            );
            assert!(
                !small.contains("Related elsewhere"),
                "related dropped first:\n{small}"
            );
        }
    }

    let global = brief::run(&fx.store, &opts(fx.base.clone(), 2000)).unwrap();
    assert!(
        global.starts_with("# kvault: all projects (6 notes, 2 memory facts"),
        "{global}"
    );
    assert!(global.contains("Recent (14d):\n"));
    assert!(!global.contains("Related elsewhere"));
    assert!(
        !global.contains("Pinned / rules:"),
        "a project's pinned notes are not global rules:\n{global}"
    );
}

fn cli(fx: &Fixture) -> Command {
    let cfg = fx.base.join("config.toml");
    std::fs::write(&cfg, fx.config.to_toml().unwrap()).unwrap();
    let mut cmd = Command::cargo_bin("kvault").unwrap();
    cmd.env("KVAULT_CONFIG", &cfg);
    cmd.env("KVAULT_DB", fx.base.join("index.db"));
    cmd
}

#[test]
fn cli_recall_and_brief() {
    let fx = fixture();
    cli(&fx)
        .args(["recall", "cold start", "--budget", "300"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("## recall \"cold start\""))
        .stdout(predicate::str::contains("Fetch more: `kvault get <id>`"));
    let out = cli(&fx)
        .args([
            "recall",
            "cold start",
            "--budget",
            "300",
            "--format",
            "json",
        ])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["budget"], 300);
    assert!(ByteHeuristic.count(&String::from_utf8_lossy(&out.get_output().stdout)) <= 300);
    cli(&fx)
        .args([
            "recall",
            "cold start",
            "--budget",
            "5",
            "--project",
            "proj-a",
        ])
        .assert()
        .success()
        .stdout("")
        .stderr(predicate::str::contains("too small"));
    cli(&fx)
        .args(["recall", ""])
        .assert()
        .failure()
        .stderr(predicate::str::contains("empty query"));

    cli(&fx)
        .args(["brief", "--cwd", fx.base.join("proj-a").to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("# kvault: proj-a ("))
        .stdout(predicate::str::contains("Pinned / rules:"));
    cli(&fx)
        .args([
            "brief",
            "--cwd",
            fx.base.join("proj-a").to_str().unwrap(),
            "--budget",
            "50",
        ])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("# kvault: proj-a ("));
}
