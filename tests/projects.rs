//! Project aliases and archive demotion, over a fixture shaped like a real
//! workspace: a notes root whose parent names the project `mas`, and a
//! Claude memory root whose slug names it `work-mas`.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use assert_cmd::Command;
use common::write;
use kvault::brief::{self, BriefOptions};
use kvault::config::{Config, ProjectConfig, ProjectFrom, RootConfig};
use kvault::index::{self, IndexOptions};
use kvault::list::{self, ListFilter};
use kvault::roots::RootKind;
use kvault::search::{self, Query, SearchOptions};
use kvault::store::Store;
use predicates::prelude::*;

struct Workspace {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    config: Config,
    store: Store,
}

/// `base/work/mas/mas-notes` (project `mas` from its parent) and
/// `base/mem/-Users-x-Projects-work-mas/memory` (project `work-mas` from the
/// slug), indexed with `config` adjusted by `tweak`.
fn workspace(tweak: impl FnOnce(&mut Config)) -> Workspace {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().to_path_buf();
    let notes = base.join("work/mas/mas-notes");
    write(
        &notes.join("plans/replica-reinitialize.md"),
        "# Replica reinitialize plan\n\n\
         Reinitialize the read replica index after the schema change: drop the replica \
         index, rebuild it from the primary, then verify row counts before reopening reads. \
         The rollout covers every region and is owned by the platform team this sprint.\n",
    );
    write(
        &notes.join("plans/archive/replica-reinitialize-v1.md"),
        "# Replica reinitialize plan (v1)\n\n\
         Reinitialize the replica index: drop the replica index and rebuild it from the \
         primary, then verify counts.\n",
    );
    write(
        &notes.join("investigations/archive/failover-runbook.md"),
        "# Failover runbook\n\n\
         ## Failover runbook steps\n\n\
         The failover runbook: promote the standby, repoint the connection strings, and \
         confirm the failover completed before paging anyone else about the runbook.\n",
    );
    write(
        &notes.join("investigations/latency.md"),
        "# Latency investigation\n\n\
         Requests slowed during the evening peak. We checked the connection pool, the \
         query plans, the cache hit rate, the network path between regions, the garbage \
         collector pauses, and, briefly, whether a failover had happened; it had not.\n",
    );
    write(
        &base.join("mem/-Users-x-Projects-work-mas/memory/replica-is-sku-driver.md"),
        "---\nname: replica-is-sku-driver\ndescription: the ctmdata replica drives the SKU\n---\n\n\
         The ctmdata replica is what drives the database SKU; sizing the primary alone \
         under-provisions the replica and the nightly reports time out.\n",
    );
    let mut config = Config {
        roots: vec![
            RootConfig::new(
                notes.to_string_lossy().into_owned(),
                RootKind::Notes,
                ProjectFrom::Parent,
            ),
            RootConfig {
                weight: 1.2,
                ..RootConfig::new(
                    format!("{}/mem/*/memory", base.display()),
                    RootKind::Memory,
                    ProjectFrom::ClaudeSlug,
                )
            },
        ],
        projects: BTreeMap::from([(
            "mas".to_owned(),
            ProjectConfig {
                aliases: vec!["work-mas".to_owned()],
            },
        )]),
        ..Config::default()
    };
    tweak(&mut config);
    config.validate().unwrap();
    let mut store = Store::open(&base.join("index.db")).unwrap();
    index::run(&config, &mut store, &IndexOptions::default()).unwrap();
    Workspace {
        _tmp: tmp,
        base,
        config,
        store,
    }
}

fn search(ws: &Workspace, q: &str, project: Option<&str>) -> Vec<search::Hit> {
    let opts = SearchOptions {
        limit: 10,
        project: project.map(str::to_owned),
        ..SearchOptions::default()
    };
    search::run(&ws.store, &Query::parse(q), &opts)
        .unwrap()
        .hits
}

fn paths(hits: &[search::Hit]) -> Vec<&str> {
    hits.iter().map(|h| h.chunk.doc.rel_path.as_str()).collect()
}

fn brief_header(ws: &Workspace) -> String {
    let text = brief::run(
        &ws.store,
        &BriefOptions {
            cwd: ws.base.join("work/mas"),
            budget: 2000,
            recent_days: 14,
        },
    )
    .unwrap();
    text.lines().next().unwrap().to_owned()
}

#[test]
fn without_an_alias_the_two_roots_are_different_projects() {
    let ws = workspace(|c| c.projects.clear());
    assert!(
        brief_header(&ws).starts_with("# kvault: mas (4 notes, 0 memory facts"),
        "the gap the alias closes: {}",
        brief_header(&ws)
    );
}

#[test]
fn an_alias_joins_both_roots_under_the_canonical_name() {
    let ws = workspace(|_| {});
    let header = brief_header(&ws);
    assert!(
        header.starts_with("# kvault: mas (4 notes, 1 memory facts"),
        "{header}"
    );

    for name in ["mas", "work-mas"] {
        let hits = search(&ws, "replica", Some(name));
        assert!(
            paths(&hits).contains(&"replica-is-sku-driver.md")
                && paths(&hits).contains(&"plans/replica-reinitialize.md"),
            "--project {name}: {:?}",
            paths(&hits)
        );
        let listed = list::run(
            &ws.store,
            &ListFilter {
                project: Some(name.to_owned()),
                ..ListFilter::default()
            },
        )
        .unwrap();
        assert_eq!(listed.len(), 5, "list --project {name}");
    }
    let memory = search(&ws, "sku driver", Some("mas"));
    assert_eq!(
        memory[0].display_path, "mas/memory/replica-is-sku-driver.md",
        "the memory root displays under the canonical name"
    );

    // A directory named after the alias resolves to the canonical project.
    let alias_dir = ws.base.join("elsewhere/work-mas");
    std::fs::create_dir_all(&alias_dir).unwrap();
    assert_eq!(
        brief::project_for(&ws.store, &alias_dir)
            .unwrap()
            .as_deref(),
        Some("mas")
    );
}

#[test]
fn archived_notes_rank_below_close_live_ones() {
    let undemoted = workspace(|c| {
        for r in &mut c.roots {
            r.demote.clear();
        }
    });
    let q = "replica reinitialize index";
    assert_eq!(
        paths(&search(&undemoted, q, Some("mas")))[0],
        "plans/archive/replica-reinitialize-v1.md",
        "without demotion the shorter archived plan wins on bm25, so the test below means something"
    );

    let ws = workspace(|_| {});
    let hits = search(&ws, q, Some("mas"));
    assert_eq!(
        &paths(&hits)[..2],
        [
            "plans/replica-reinitialize.md",
            "plans/archive/replica-reinitialize-v1.md"
        ]
    );
    assert_eq!(hits[0].explain.demoted, None);
    let archived = &hits[1];
    assert_eq!(archived.explain.demoted, Some(0.9));
    assert!((archived.explain.authority - 0.9).abs() < 1e-12);
}

#[test]
fn a_clearly_better_archived_hit_still_wins() {
    let ws = workspace(|_| {});
    let hits = search(&ws, "failover runbook", None);
    assert_eq!(
        paths(&hits)[0],
        "investigations/archive/failover-runbook.md",
        "demotion is a tiebreak, not a filter: {:?}",
        paths(&hits)
    );
    assert!(hits[0].explain.demoted.is_some());
}

#[test]
fn demote_globs_and_factor_are_configurable() {
    let ws = workspace(|c| {
        c.roots[0].demote = vec!["investigations/**".to_owned()];
        c.roots[0].demote_factor = 0.95;
    });
    let hits = search(&ws, "failover", None);
    let by_path = |p: &str| hits.iter().find(|h| h.chunk.doc.rel_path == p).unwrap();
    assert_eq!(
        by_path("investigations/latency.md").explain.demoted,
        Some(0.95)
    );
    let hits = search(&ws, "replica reinitialize index", None);
    assert!(
        hits.iter().all(|h| h.explain.demoted.is_none()),
        "plans/archive is no longer matched once the default is replaced"
    );
}

fn cli(ws: &Workspace) -> Command {
    let cfg = ws.base.join("config.toml");
    std::fs::write(&cfg, ws.config.to_toml().unwrap()).unwrap();
    let mut cmd = Command::cargo_bin("kvault").unwrap();
    cmd.env("KVAULT_CONFIG", &cfg);
    cmd.env("KVAULT_DB", ws.base.join("index.db"));
    cmd
}

#[test]
fn cli_explain_shows_demotion_and_config_round_trips() {
    let ws = workspace(|_| {});
    let text = ws.config.to_toml().unwrap();
    assert!(text.contains("[projects.mas]"), "{text}");
    assert_eq!(toml::from_str::<Config>(&text).unwrap(), ws.config);

    cli(&ws)
        .args(["search", "replica reinitialize index", "--explain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("(demoted ×0.90)"));
    cli(&ws)
        .args(["brief", "--cwd"])
        .arg(ws.base.join("work/mas"))
        .assert()
        .success()
        .stdout(predicate::str::starts_with(
            "# kvault: mas (4 notes, 1 memory facts",
        ));
}
