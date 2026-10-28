//! A memory root's `MEMORY.md` index never outranks the fact files it links
//! to, and stays findable when it is the only match.

#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use assert_cmd::Command;
use common::write;
use kvault::config::{Config, ProjectFrom, RootConfig};
use kvault::get::{self, Target};
use kvault::index::{self, IndexOptions};
use kvault::roots::RootKind;
use kvault::search::{self, Query, SearchOptions};
use kvault::store::Store;
use predicates::prelude::*;

struct Corpus {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    config: Config,
    store: Store,
}

/// One memory root with an index and two facts, plus a notes root holding
/// a file that is also called `MEMORY.md` but is not an index.
fn corpus() -> Corpus {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().to_path_buf();
    let mem = base.join("mem/-Users-x-Projects-p/memory");
    // Each hook repeats its fact's vocabulary, so the section holds every
    // query term even though no single fact does.
    write(
        &mem.join("MEMORY.md"),
        "# Memory Index\n\n\
         ## Roadmap\n\n\
         - [Portal rebuild](portal-rebuild.md): candidate portal sends CTM status by name; a rename breaks it\n\
         - [Replica sizing](replica-sizing.md): the replica drives the SKU, not the primary\n\
         - [Pager rota](pager-rota.md): who is on call for the vendor bridge this quarter\n",
    );
    write(
        &mem.join("portal-rebuild.md"),
        "---\nname: portal-rebuild\ndescription: candidate portal rebuild status\n---\n\n\
         The rebuilt candidate portal sends the CTM status by name rather than by numeric \
         id, so the mapping table must list every status label the vendor emits.\n",
    );
    write(
        &mem.join("replica-sizing.md"),
        "---\nname: replica-sizing\ndescription: replica drives the SKU\n---\n\n\
         The read replica, not the primary, sets the database SKU; size from its CPU.\n",
    );
    write(
        &mem.join("pager-rota.md"),
        "---\nname: pager-rota\ndescription: on-call rota\n---\n\n\
         Rotation alternates weekly between the two platform engineers.\n",
    );
    write(
        &base.join("p/notes/MEMORY.md"),
        "# Scratch\n\nA notes file that happens to be named like the index: pager bridge.\n",
    );
    let config = Config {
        roots: vec![
            RootConfig {
                weight: 1.2,
                ..RootConfig::new(
                    format!("{}/mem/*/memory", base.display()),
                    RootKind::Memory,
                    ProjectFrom::ClaudeSlug,
                )
            },
            RootConfig::new(
                format!("{}/*/notes", base.display()),
                RootKind::Notes,
                ProjectFrom::Parent,
            ),
        ],
        ..Config::default()
    };
    let mut store = Store::open(&base.join("index.db")).unwrap();
    index::run(&config, &mut store, &IndexOptions::default()).unwrap();
    Corpus {
        _tmp: tmp,
        base,
        config,
        store,
    }
}

fn search(c: &Corpus, q: &str) -> Vec<search::Hit> {
    let opts = SearchOptions {
        limit: 10,
        ..SearchOptions::default()
    };
    search::run(&c.store, &Query::parse(q), &opts).unwrap().hits
}

fn display(hits: &[search::Hit]) -> Vec<&str> {
    hits.iter().map(|h| h.display_path.as_str()).collect()
}

#[test]
fn the_fact_outranks_the_index_line_pointing_at_it() {
    let c = corpus();
    let hits = search(&c, "CTM status name rename");
    assert_eq!(
        &display(&hits)[..2],
        ["p/memory/portal-rebuild.md", "p/memory/MEMORY.md"],
        "{:?}",
        display(&hits)
    );
    let fact = &hits[0];
    let index = &hits[1];
    assert!(
        index.explain.streams.iter().any(|s| s.stream == "strict"),
        "only the index holds every term, so only it is in the strict stream"
    );
    let cap = index.explain.index_cap.as_ref().unwrap();
    assert_eq!(cap.below, fact.chunk.id);
    assert!(
        cap.rrf_before > fact.explain.rrf,
        "uncapped, the index would have ranked first"
    );
    assert!(index.explain.rrf < fact.explain.rrf);
    assert!(fact.explain.index_cap.is_none());
}

#[test]
fn the_index_is_kept_when_it_is_the_only_route() {
    let c = corpus();
    // "vendor bridge" appears only in the pager-rota hook, not in the fact.
    let hits = search(&c, "vendor bridge");
    assert_eq!(display(&hits)[0], "p/memory/MEMORY.md");
    assert!(hits[0].explain.index_cap.is_none());

    let text = get::run(&c.store, &Target::parse("p/memory/MEMORY.md"), true).unwrap();
    assert!(text.contains("Portal rebuild"), "{text}");
}

#[test]
fn a_notes_file_named_like_the_index_is_not_capped() {
    let c = corpus();
    let hits = search(&c, "pager bridge");
    let notes = hits
        .iter()
        .find(|h| h.display_path == "p/notes/MEMORY.md")
        .unwrap();
    assert!(notes.explain.index_cap.is_none());
}

#[test]
fn cli_explain_says_why_the_index_ranked_low() {
    let c = corpus();
    let cfg = c.base.join("config.toml");
    std::fs::write(&cfg, c.config.to_toml().unwrap()).unwrap();
    Command::cargo_bin("kvault")
        .unwrap()
        .env("KVAULT_CONFIG", &cfg)
        .env("KVAULT_DB", c.base.join("index.db"))
        .args(["search", "CTM status name rename", "--explain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("(index, capped from "));
}
