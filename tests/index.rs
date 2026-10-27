//! Indexer behaviour over a temporary corpus.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::path::Path;

use kvault::config::{Config, ProjectFrom, RootConfig};
use kvault::index::{self, IndexOptions};
use kvault::list::{self, ListFilter};
use kvault::roots::RootKind;
use kvault::store::Store;

struct Fixture {
    _tmp: tempfile::TempDir,
    base: std::path::PathBuf,
    config: Config,
    store: Store,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().to_path_buf();
    std::fs::create_dir_all(base.join("proj-a/notes/sub")).unwrap();
    std::fs::create_dir_all(base.join("proj-b/notes")).unwrap();
    std::fs::create_dir_all(base.join("mem/-Users-x-Projects-proj-a/memory")).unwrap();
    write(
        &base.join("proj-a/notes/plan.md"),
        "# Plan\n\nIntro words for the plan section go here ok.\n\n## Phase one\n\nDo the first thing. See [[decision-one]] and [arch](sub/arch.md).\n\n## Phase two\n\nDo the second thing with lambda cold start tuning.\n",
    );
    write(
        &base.join("proj-a/notes/sub/arch.md"),
        "---\ntitle: Architecture\ntags: [design, Arch]\n---\n# Architecture\n\n- [decision] use sqlite #storage\n- supersedes [[plan]]\n\nMore body text that is long enough to be its own chunk of content.\n",
    );
    write(
        &base.join("proj-b/notes/other.md"),
        "# Other project\n\nUnrelated content about kubernetes pods and services.\n",
    );
    write(
        &base.join("mem/-Users-x-Projects-proj-a/memory/decision-one.md"),
        "---\nname: decision-one\ndescription: we chose sqlite over tantivy\nmetadata:\n  type: project\n---\n\nWe chose SQLite FTS5 over Tantivy because one store is simpler.\n",
    );
    write(
        &base.join("mem/-Users-x-Projects-proj-a/memory/MEMORY.md"),
        "- [decision one](decision-one.md) — sqlite\n",
    );
    write(
        &base.join("proj-a/notes/.hidden.md"),
        "# hidden\nshould be skipped\n",
    );
    write(&base.join("proj-a/notes/readme.txt"), "not markdown");

    let config = Config {
        roots: vec![
            RootConfig {
                path: format!("{}/*/notes", base.display()),
                kind: RootKind::Notes,
                weight: 1.0,
                project_from: ProjectFrom::Parent,
            },
            RootConfig {
                path: format!("{}/mem/*/memory", base.display()),
                kind: RootKind::Memory,
                weight: 1.2,
                project_from: ProjectFrom::ClaudeSlug,
            },
            RootConfig {
                path: format!("{}/missing", base.display()),
                kind: RootKind::Reference,
                weight: 0.8,
                project_from: ProjectFrom::None,
            },
        ],
        ..Config::default()
    };
    let store = Store::open(&base.join("index.db")).unwrap();
    Fixture {
        _tmp: tmp,
        base,
        config,
        store,
    }
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
}

/// Push a file's mtime into the past so the racily-clean window does not apply.
fn age(path: &Path) {
    let t = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(t).unwrap();
}

fn age_all(base: &Path) {
    for entry in walkdir(base) {
        age(&entry);
    }
}

fn walkdir(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walkdir(&p));
        } else if p.extension().is_some_and(|x| x == "md") {
            out.push(p);
        }
    }
    out
}

fn count(store: &Store, sql: &str) -> i64 {
    store.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn first_run_indexes_everything_and_second_run_is_idempotent() {
    let mut fx = fixture();
    age_all(&fx.base);
    let r1 = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r1.roots, 3, "two notes roots + one memory root");
    assert_eq!(r1.scanned, 5, "hidden and .txt skipped");
    assert_eq!(r1.added, 5);
    assert_eq!(r1.unchanged, 0);
    assert!(r1.chunks >= 5);

    let before = (
        count(&fx.store, "SELECT COUNT(*) FROM docs"),
        count(&fx.store, "SELECT COUNT(*) FROM chunks"),
        count(&fx.store, "SELECT COUNT(*) FROM chunks_fts"),
        count(&fx.store, "SELECT COUNT(*) FROM links"),
        count(&fx.store, "SELECT COUNT(*) FROM tags"),
    );
    let r2 = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r2.unchanged, 5);
    assert_eq!(r2.added + r2.updated + r2.renamed + r2.removed, 0);
    assert_eq!(r2.chunks, 0, "nothing rewritten on a warm run");
    let after = (
        count(&fx.store, "SELECT COUNT(*) FROM docs"),
        count(&fx.store, "SELECT COUNT(*) FROM chunks"),
        count(&fx.store, "SELECT COUNT(*) FROM chunks_fts"),
        count(&fx.store, "SELECT COUNT(*) FROM links"),
        count(&fx.store, "SELECT COUNT(*) FROM tags"),
    );
    assert_eq!(before, after);
    assert!(fx.store.meta("last_index_at").unwrap().is_some());
}

#[test]
fn metadata_titles_tags_projects_and_fts_are_populated() {
    let mut fx = fixture();
    age_all(&fx.base);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();

    let docs = list::run(&fx.store, &ListFilter::default()).unwrap();
    let by_path: std::collections::HashMap<_, _> =
        docs.iter().map(|d| (d.rel_path.as_str(), d)).collect();
    assert_eq!(by_path["plan.md"].title, "Plan");
    assert_eq!(by_path["plan.md"].project.as_deref(), Some("proj-a"));
    assert_eq!(by_path["sub/arch.md"].title, "Architecture");
    assert_eq!(
        by_path["decision-one.md"].project.as_deref(),
        Some("proj-a")
    );
    assert_eq!(by_path["decision-one.md"].root_kind, "memory");
    // memory file with no heading falls back to the stem
    assert_eq!(by_path["decision-one.md"].title, "decision-one");

    let tagged = list::run(
        &fx.store,
        &ListFilter {
            tag: Some("ARCH".into()),
            ..ListFilter::default()
        },
    )
    .unwrap();
    assert_eq!(tagged.len(), 1);
    assert_eq!(tagged[0].rel_path, "sub/arch.md");
    assert_eq!(
        count(&fx.store, "SELECT COUNT(*) FROM tags WHERE tag = 'storage'"),
        1
    );

    let obs: (String, String) = fx
        .store
        .conn()
        .query_row("SELECT category, text FROM observations", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(obs.0, "decision");
    assert!(obs.1.contains("sqlite"));

    // FTS hit maps back to a chunk whose byte range contains the term.
    let (doc_path, bs, be): (String, i64, i64) = fx
        .store
        .conn()
        .query_row(
            "SELECT d.rel_path, c.byte_start, c.byte_end FROM chunks_fts f
             JOIN chunks c ON c.id = f.rowid JOIN docs d ON d.id = c.doc_id
             WHERE chunks_fts MATCH 'cold' ORDER BY bm25(chunks_fts) LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(doc_path, "plan.md");
    let text = std::fs::read_to_string(fx.base.join("proj-a/notes/plan.md")).unwrap();
    let slice = &text[usize::try_from(bs).unwrap()..usize::try_from(be).unwrap()];
    assert!(slice.contains("cold start"), "{slice:?}");

    // Description is searchable through the title column.
    let n = count(
        &fx.store,
        "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH 'title:tantivy'",
    );
    assert_eq!(n, 1);

    let kinds = list::run(
        &fx.store,
        &ListFilter {
            kind: Some("memory".into()),
            ..ListFilter::default()
        },
    )
    .unwrap();
    assert_eq!(kinds.len(), 2);
}

#[test]
fn links_resolve_within_and_across_roots() {
    let mut fx = fixture();
    age_all(&fx.base);
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    let rows: Vec<(String, String, Option<String>)> = fx
        .store
        .conn()
        .prepare(
            "SELECT l.target, l.link_type, t.rel_path FROM links l
             LEFT JOIN docs t ON t.id = l.to_doc_id ORDER BY l.target",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "decision-one".to_owned(),
                "wiki".to_owned(),
                Some("decision-one.md".to_owned())
            ),
            (
                "decision-one.md".to_owned(),
                "md".to_owned(),
                Some("decision-one.md".to_owned())
            ),
            (
                "plan".to_owned(),
                "rel:supersedes".to_owned(),
                Some("plan.md".to_owned())
            ),
            (
                "sub/arch.md".to_owned(),
                "md".to_owned(),
                Some("sub/arch.md".to_owned())
            ),
        ]
    );
    assert_eq!(r.links_unresolved, 0);
}

#[test]
fn modify_rename_delete_and_restore() {
    let mut fx = fixture();
    age_all(&fx.base);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    let plan_id: i64 = fx
        .store
        .conn()
        .query_row("SELECT id FROM docs WHERE rel_path = 'plan.md'", [], |r| {
            r.get(0)
        })
        .unwrap();

    // Modify: same path, new content → updated, same id, chunks rewritten.
    let plan = fx.base.join("proj-a/notes/plan.md");
    write(
        &plan,
        "# Plan v2\n\nCompletely new content about retries and backoff policies here.\n",
    );
    age(&plan);
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!((r.updated, r.added, r.removed), (1, 0, 0));
    let title: String = fx
        .store
        .conn()
        .query_row("SELECT title FROM docs WHERE id = ?1", [plan_id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(title, "Plan v2");
    assert_eq!(
        count(
            &fx.store,
            "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH 'cold'"
        ),
        0
    );
    assert_eq!(
        count(
            &fx.store,
            "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH 'backoff'"
        ),
        1
    );

    // Touch without content change: mtime differs, hash same → unchanged, no rewrite.
    let t = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&plan)
        .unwrap()
        .set_modified(t)
        .unwrap();
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r.unchanged, 5);
    assert_eq!(r.chunks, 0);

    // Rename: same content, new path → renamed, id kept, FTS path updated.
    let renamed = fx.base.join("proj-a/notes/plan-renamed.md");
    std::fs::rename(&plan, &renamed).unwrap();
    age(&renamed);
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!((r.renamed, r.added, r.removed), (1, 0, 0));
    let (path, active): (String, i64) = fx
        .store
        .conn()
        .query_row(
            "SELECT rel_path, active FROM docs WHERE id = ?1",
            [plan_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((path.as_str(), active), ("plan-renamed.md", 1));
    assert_eq!(
        count(
            &fx.store,
            "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH 'path:\"plan-renamed.md\"'"
        ),
        1
    );
    // The incoming `supersedes [[plan]]` link keeps pointing at the same doc id.
    let to: Option<i64> = fx
        .store
        .conn()
        .query_row(
            "SELECT to_doc_id FROM links WHERE link_type = 'rel:supersedes'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(to, Some(plan_id));

    // Delete: → removed, soft-deleted, chunks gone, links to it nulled.
    std::fs::remove_file(&renamed).unwrap();
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r.removed, 1);
    assert_eq!(index::doc_active(&fx.store, plan_id).unwrap(), Some(false));
    assert_eq!(
        count(
            &fx.store,
            &format!("SELECT COUNT(*) FROM chunks WHERE doc_id = {plan_id}")
        ),
        0
    );
    assert_eq!(
        count(
            &fx.store,
            "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH 'backoff'"
        ),
        0
    );
    let to: Option<i64> = fx
        .store
        .conn()
        .query_row(
            "SELECT to_doc_id FROM links WHERE link_type = 'rel:supersedes'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(to, None);
    assert_eq!(
        list::run(&fx.store, &ListFilter::default()).unwrap().len(),
        4
    );

    // Restore at the original path with the same content → rename-detected back onto the id.
    write(
        &plan,
        "# Plan v2\n\nCompletely new content about retries and backoff policies here.\n",
    );
    age(&plan);
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r.added + r.renamed, 1);
    assert_eq!(index::doc_active(&fx.store, plan_id).unwrap(), Some(true));
    let docs_total = count(&fx.store, "SELECT COUNT(*) FROM docs");
    assert_eq!(docs_total, 5, "no duplicate row was created");
}

#[test]
fn full_rebuild_matches_incremental_chunk_hashes() {
    let mut fx = fixture();
    age_all(&fx.base);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    let hashes = |s: &Store| -> Vec<String> {
        s.conn()
            .prepare("SELECT c.blake3 FROM chunks c JOIN docs d ON d.id = c.doc_id ORDER BY d.rel_path, c.ordinal")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    let h1 = hashes(&fx.store);
    drop(fx.store);
    std::fs::remove_file(fx.base.join("index.db")).unwrap();
    let mut fresh = Store::open(&fx.base.join("index.db")).unwrap();
    index::run(
        &fx.config,
        &mut fresh,
        &IndexOptions {
            full: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(h1, hashes(&fresh));
}

#[test]
fn only_root_limits_the_run_and_missing_root_is_not_a_mass_delete() {
    let mut fx = fixture();
    age_all(&fx.base);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    let opts = IndexOptions {
        only_root: Some(fx.base.join("proj-b/notes")),
        ..Default::default()
    };
    let r = index::run(&fx.config, &mut fx.store, &opts).unwrap();
    assert_eq!(r.roots, 1);
    assert_eq!(r.scanned, 1);

    // Remove the whole proj-b notes dir from the config: its docs stay.
    let mut cfg = fx.config.clone();
    cfg.roots.remove(0);
    let r = index::run(&cfg, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r.roots_missing, 2, "two notes roots no longer configured");
    assert_eq!(r.removed, 0);
    assert_eq!(
        count(&fx.store, "SELECT COUNT(*) FROM docs WHERE active = 1"),
        5
    );
}

#[test]
fn parser_version_change_forces_reparse() {
    let mut fx = fixture();
    age_all(&fx.base);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    fx.store
        .conn()
        .execute(
            "UPDATE meta SET value = '0' WHERE key = 'parser_version'",
            [],
        )
        .unwrap();
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r.updated, 5, "every file re-parsed");
    assert!(r.chunks > 0);
    assert_eq!(
        fx.store.meta("parser_version").unwrap().as_deref(),
        Some(index::PARSER_VERSION)
    );
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r.unchanged, 5);
}
