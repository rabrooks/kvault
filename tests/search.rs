//! `search`, `get`, and `bench` over a temporary corpus, through the library
//! and through the binary.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use assert_cmd::Command;
use common::{Fixture, fixture, write};
use kvault::cli::SearchFormat;
use kvault::get::{self, Target};
use kvault::index::{self, IndexOptions};
use kvault::render;
use kvault::search::{self, Query, SearchOptions};
use predicates::prelude::*;

fn run(fx: &Fixture, q: &str, opts: &SearchOptions) -> Vec<search::Hit> {
    search::run(&fx.store, &Query::parse(q), opts).unwrap().hits
}

fn opts() -> SearchOptions {
    SearchOptions {
        limit: 10,
        ..SearchOptions::default()
    }
}

fn paths(hits: &[search::Hit]) -> Vec<&str> {
    hits.iter().map(|h| h.chunk.doc.rel_path.as_str()).collect()
}

#[test]
fn body_terms_find_the_right_section() {
    let fx = fixture();
    let hits = run(&fx, "lambda cold start", &opts());
    let top: Vec<&str> = paths(&hits).into_iter().take(2).collect();
    assert!(top.contains(&"plan.md"), "{top:?}");
    assert!(top.contains(&"lambda-timeouts.md"), "{top:?}");
    let plan = hits
        .iter()
        .find(|h| h.chunk.doc.rel_path == "plan.md")
        .unwrap();
    assert!(
        plan.chunk.heading_path.contains("Phase one"),
        "best chunk is the section that matches, got {:?}",
        plan.chunk.heading_path
    );
    assert!(plan.chunk.start_line > 1);
    assert_eq!(plan.display_path, "proj-a/notes/plan.md");
    assert_eq!(plan.chunk.id.len(), 8);
}

#[test]
fn title_match_ranks_first_and_explain_is_populated() {
    let fx = fixture();
    let hits = run(&fx, "architecture", &opts());
    assert_eq!(hits[0].chunk.doc.rel_path, "sub/arch.md");
    let streams: Vec<&str> = hits[0].explain.streams.iter().map(|s| s.stream).collect();
    assert!(streams.contains(&"fts"), "{streams:?}");
    assert!(streams.contains(&"title"), "{streams:?}");
    assert!(hits[0].explain.rrf > 0.0);
    // pinned, root weight 1.0 → 1.05
    assert!((hits[0].explain.authority - 1.05).abs() < 1e-9);
    assert!((hits[0].score - hits[0].explain.rrf * 1.05).abs() < 1e-12);
}

#[test]
fn phrases_and_negation_apply_per_chunk() {
    let fx = fixture();
    let hits = run(&fx, "\"cold start\" -provisioned", &opts());
    assert_eq!(paths(&hits), vec!["lambda-timeouts.md"]);
    let hits = run(&fx, "\"pod autoscaler\"", &opts());
    assert_eq!(paths(&hits), vec!["other.md"]);
    let hits = run(&fx, "\"start cold\"", &opts());
    assert!(hits.is_empty(), "phrase order matters");
}

#[test]
fn exclusions_apply_to_title_and_tag_streams() {
    let fx = fixture();
    // arch.md matches by title and by tag, but its only chunk says sqlite.
    assert!(run(&fx, "architecture -sqlite", &opts()).is_empty());
    // cross.md carries the design tag too and never mentions sqlite.
    assert_eq!(
        paths(&run(&fx, "design -sqlite", &opts())),
        vec!["cross.md"]
    );
    assert!(run(&fx, "lex: architecture NOT sqlite", &opts()).is_empty());
    assert_eq!(
        paths(&run(&fx, "lex: lambda NOT python", &opts())),
        vec!["lambda-timeouts.md", "plan.md"],
        "the word after NOT is not a wanted word"
    );

    let hits = run(&fx, "sqlite -tag:design", &opts());
    assert_eq!(paths(&hits), vec!["decision-one.md"]);
    let hits = run(&fx, "sqlite -tag:nonexistent", &opts());
    assert_eq!(hits.len(), 2);
}

#[test]
fn tag_only_queries_browse_the_filtered_documents() {
    let fx = fixture();
    let hits = run(&fx, "tag:design", &opts());
    let mut found = paths(&hits);
    found.sort_unstable();
    assert_eq!(found, vec!["cross.md", "sub/arch.md"]);
    assert_eq!(hits[0].explain.streams[0].stream, "browse");
    assert_eq!(
        paths(&run(&fx, "tag:storage", &opts())),
        vec!["sub/arch.md"]
    );
    assert!(run(&fx, "tag:storage -tag:design", &opts()).is_empty());
    assert!(run(&fx, "tag:nonexistent", &opts()).is_empty());
}

#[test]
fn identical_sections_in_different_files_get_distinct_ids() {
    let mut fx = fixture();
    let body = "# Template\n\nBoilerplate zebra paragraph shared by every note made from it.\n";
    write(&fx.base.join("proj-a/notes/tpl.md"), body);
    write(&fx.base.join("proj-b/notes/tpl.md"), body);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    let hits = run(&fx, "zebra", &opts());
    assert_eq!(hits.len(), 2);
    assert_ne!(hits[0].chunk.id, hits[1].chunk.id);
    for h in &hits {
        let out = get::run(&fx.store, &Target::Id(h.chunk.id.clone()), false).unwrap();
        assert!(out.contains(&h.display_path), "{out}");
    }
}

#[test]
fn filters_restrict_every_stream() {
    let fx = fixture();
    let hits = run(
        &fx,
        "kubernetes",
        &SearchOptions {
            project: Some("proj-b".into()),
            ..opts()
        },
    );
    assert_eq!(paths(&hits), vec!["other.md"]);

    let hits = run(
        &fx,
        "sqlite",
        &SearchOptions {
            tags: vec!["Storage".into()],
            ..opts()
        },
    );
    assert_eq!(paths(&hits), vec!["sub/arch.md"]);

    let hits = run(&fx, "tag:design sqlite", &opts());
    assert_eq!(paths(&hits), vec!["sub/arch.md"], "inline tag filter");

    let hits = run(
        &fx,
        "sqlite",
        &SearchOptions {
            kind: Some("memory".into()),
            ..opts()
        },
    );
    assert_eq!(paths(&hits), vec!["decision-one.md"]);

    let hits = run(
        &fx,
        "sqlite",
        &SearchOptions {
            path: Some("sub/*".into()),
            ..opts()
        },
    );
    assert_eq!(paths(&hits), vec!["sub/arch.md"]);

    let hits = run(
        &fx,
        "sqlite",
        &SearchOptions {
            root: Some(fx.base.join("proj-a/notes")),
            ..opts()
        },
    );
    assert_eq!(paths(&hits), vec!["sub/arch.md"]);
    let hits = run(
        &fx,
        "sqlite",
        &SearchOptions {
            root: Some(fx.base.join("proj-a/notes/")),
            ..opts()
        },
    );
    assert_eq!(paths(&hits), vec!["sub/arch.md"], "trailing slash");

    let recent = run(
        &fx,
        "sqlite",
        &SearchOptions {
            since: Some("1h".into()),
            ..opts()
        },
    );
    assert_eq!(recent.len(), 2);
    let future = run(
        &fx,
        "sqlite",
        &SearchOptions {
            since: Some("2999-01-01".into()),
            ..opts()
        },
    );
    assert!(future.is_empty());
    let err = search::run(
        &fx.store,
        &Query::parse("sqlite"),
        &SearchOptions {
            since: Some("soon".into()),
            ..opts()
        },
    );
    assert!(err.is_err());
}

#[test]
fn per_doc_collapses_chunks_into_siblings() {
    let fx = fixture();
    let hits = run(&fx, "thing", &opts());
    let plan: Vec<&search::Hit> = hits
        .iter()
        .filter(|h| h.chunk.doc.rel_path == "plan.md")
        .collect();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].siblings.len(), 1, "the other phase is a sibling");

    let hits = run(
        &fx,
        "thing",
        &SearchOptions {
            per_chunk: true,
            ..opts()
        },
    );
    let plan: Vec<&search::Hit> = hits
        .iter()
        .filter(|h| h.chunk.doc.rel_path == "plan.md")
        .collect();
    assert_eq!(plan.len(), 2);
    assert!(plan.iter().all(|h| h.siblings.is_empty()));
}

#[test]
fn limit_and_empty_query() {
    let fx = fixture();
    let hits = run(
        &fx,
        "thing sqlite kubernetes",
        &SearchOptions { limit: 2, ..opts() },
    );
    assert_eq!(hits.len(), 2);
    assert!(search::run(&fx.store, &Query::parse(""), &opts()).is_err());
    let err = search::run(&fx.store, &Query::parse("lex: AND"), &opts()).unwrap_err();
    assert!(err.to_string().contains("bad query syntax"), "{err}");

    let none = search::run(&fx.store, &Query::parse("zzz-nothing"), &opts()).unwrap();
    assert!(none.hits.is_empty());
    for format in [SearchFormat::Ids, SearchFormat::Paths] {
        assert_eq!(
            render::render(&none, format, false).unwrap(),
            "",
            "no trailing newline for no results"
        );
    }
}

#[test]
fn get_by_id_path_and_window() {
    let fx = fixture();
    let hit = run(&fx, "\"cold start\" tuning", &opts())
        .into_iter()
        .find(|h| h.chunk.doc.rel_path == "plan.md")
        .unwrap();

    let out = get::run(&fx.store, &Target::Id(hit.chunk.id.clone()), false).unwrap();
    assert!(out.starts_with(&format!("# {}  proj-a/notes/plan.md:", hit.chunk.id)));
    assert!(out.contains("› Plan / Phase one"));
    assert!(out.contains("## Phase one"));
    assert!(!out.contains("Phase two"));
    assert!(!out.contains("[stale"));

    let whole = get::run(&fx.store, &Target::Id(hit.chunk.id.clone()), true).unwrap();
    assert!(whole.contains("Phase two"));
    assert!(whole.contains("    1  # Plan"));

    let window = get::run(&fx.store, &Target::parse("proj-a/notes/plan.md:5:2"), false).unwrap();
    assert!(window.contains("    5  ## Phase one"), "{window}");
    assert!(!window.contains("    7  "), "{window}");

    let by_abs = get::run(
        &fx.store,
        &Target::parse(&fx.base.join("proj-b/notes/other.md").display().to_string()),
        false,
    )
    .unwrap();
    assert!(by_abs.contains("autoscaler"));

    let err = get::run(&fx.store, &Target::parse("readme.md"), false).unwrap_err();
    assert!(err.to_string().contains("matches 2 documents"), "{err}");
    let ok = get::run(&fx.store, &Target::parse("proj-b/notes/readme.md"), false).unwrap();
    assert!(ok.contains("project b"));

    let err = get::run(&fx.store, &Target::Id("ffffff".into()), false).unwrap_err();
    assert!(err.to_string().contains("no chunk with id"), "{err}");
}

#[test]
fn stale_files_are_flagged_not_misquoted() {
    let mut fx = fixture();
    let hit = run(&fx, "architecture", &opts()).remove(0);
    let path = fx.base.join("proj-a/notes/sub/arch.md");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.insert_str(0, "<!-- edited after indexing -->\n");
    std::fs::write(&path, text).unwrap();
    let f = std::fs::File::options().write(true).open(&path).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(10))
        .unwrap();

    let snip = render::snippet(&hit, &["sqlite".into()]).unwrap();
    assert!(snip.stale);
    let out = get::run(&fx.store, &Target::Id(hit.chunk.id.clone()), false).unwrap();
    assert!(out.contains("[stale"), "{out}");

    // Reindexing clears the flag.
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    let hit = run(&fx, "architecture", &opts()).remove(0);
    let snip = render::snippet(&hit, &["sqlite".into()]).unwrap();
    assert!(!snip.stale);
}

#[test]
fn bench_scores_fixture_and_beats_grep() {
    let fx = fixture();
    let q = fx.base.join("queries.jsonl");
    std::fs::write(
        &q,
        concat!(
            "# fixture bench\n",
            "{\"query\": \"lambda cold start tuning\", \"expected\": [\"plan.md#phase one\"]}\n",
            "{\"query\": \"why sqlite instead of tantivy\", \"expected\": [\"decision-one.md\"]}\n",
            "{\"query\": \"pod autoscaler\", \"expected\": [\"proj-b/notes/other.md\"]}\n",
            "{\"query\": \"architecture storage\", \"expected\": [\"sub/arch.md\"]}\n",
        ),
    )
    .unwrap();
    let cases = kvault::bench::load(&q).unwrap();
    assert_eq!(cases.len(), 4);
    let report = kvault::bench::run(
        &fx.store,
        &cases,
        &kvault::bench::BenchOptions {
            limit: 5,
            baseline: true,
        },
    )
    .unwrap();
    let m = &report.kvault.metrics;
    assert_eq!(m.n, 4);
    assert!((m.hit5 - 1.0).abs() < f64::EPSILON, "{m:?}");
    assert_eq!(m.sec5, Some((1.0, 1)));
    assert!(m.mrr > 0.9, "{m:?}");
    let g = &report.baseline.as_ref().unwrap().metrics;
    assert!(m.mrr >= g.mrr, "kvault {m:?} vs grep {g:?}");
    let table = report.render(true);
    assert!(table.contains("kvault"));
    assert!(table.contains("grep"));
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
fn cli_search_get_and_bench_round_trip() {
    let fx = fixture();
    let out = cli(&fx)
        .args(["search", "cold start", "--format", "ids"])
        .assert()
        .success();
    let ids = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let id = ids.lines().next().unwrap();
    assert_eq!(id.len(), 8);

    cli(&fx)
        .args(["get", id])
        .assert()
        .success()
        .stdout(predicate::str::contains("cold start"));

    cli(&fx)
        .args(["search", "cold start", "--explain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("rrf "))
        .stdout(predicate::str::contains("streams:"));

    let out = cli(&fx)
        .args(["search", "sqlite", "--format", "json", "-n", "1"])
        .assert()
        .success();
    let json: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(json["hits"].as_array().unwrap().len(), 1);
    assert!(json["hits"][0]["snippet"]["text"].is_string());

    cli(&fx)
        .args(["search", "nothing-matches-this-zzz"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no results"));

    cli(&fx)
        .args(["search", "sqlite", "--format", "paths", "--kind", "memory"])
        .assert()
        .success()
        .stdout("proj-a/memory/decision-one.md\n");

    cli(&fx)
        .args(["search", "sqlite", "--since", "later"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--since"));

    let q = fx.base.join("q.jsonl");
    std::fs::write(
        &q,
        "{\"query\": \"pod autoscaler\", \"expected\": [\"other.md\"]}\n",
    )
    .unwrap();
    cli(&fx)
        .args(["bench", q.to_str().unwrap(), "--min-hit5", "1.0"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hit@5"));
    std::fs::write(
        &q,
        "{\"query\": \"pod autoscaler\", \"expected\": [\"missing.md\"]}\n",
    )
    .unwrap();
    cli(&fx)
        .args(["bench", q.to_str().unwrap(), "--min-hit5", "0.5"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("below"));
}
