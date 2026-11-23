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
            RootConfig::new(
                format!("{}/*/notes", base.display()),
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
            RootConfig {
                weight: 0.8,
                ..RootConfig::new(
                    format!("{}/missing", base.display()),
                    RootKind::Reference,
                    ProjectFrom::None,
                )
            },
        ],
        capture: kvault::config::CaptureConfig {
            fallback_dir: base.join("sessions").to_string_lossy().into_owned(),
            ..Default::default()
        },
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
    // Links are re-resolved from scratch every run, so `[[plan]]` no longer
    // matches: the stem is now `plan-renamed` and the title `Plan v2`. A full
    // rebuild would give the same answer.
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
    let to: Option<i64> = fx
        .store
        .conn()
        .query_row(
            "SELECT to_doc_id FROM links WHERE link_type = 'rel:supersedes'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        to,
        Some(plan_id),
        "back at plan.md the link resolves by path again"
    );
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
fn absent_root_is_hidden_returns_for_free_and_can_be_pruned() {
    let mut fx = fixture();
    age_all(&fx.base);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    let listed = |store: &Store| {
        list::run(store, &ListFilter::default())
            .unwrap()
            .into_iter()
            .map(|d| d.rel_path)
            .collect::<Vec<_>>()
    };
    let search = |store: &Store, q: &str| {
        kvault::search::run(
            store,
            &kvault::search::Query::parse(q),
            &kvault::search::SearchOptions {
                limit: 10,
                ..Default::default()
            },
        )
        .unwrap()
        .hits
        .len()
    };
    assert_eq!(listed(&fx.store).len(), 5);
    assert_eq!(search(&fx.store, "kubernetes"), 1);

    // The directory goes away (unmounted, moved, deleted).
    let gone = fx.base.join("proj-b/notes");
    let parked = fx.base.join("parked");
    std::fs::rename(&gone, &parked).unwrap();
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!((r.roots_missing, r.removed), (1, 0));
    assert_eq!(listed(&fx.store).len(), 4, "its documents are not listed");
    assert_eq!(search(&fx.store, "kubernetes"), 0, "nor found");
    assert_eq!(
        fx.store.absent_roots().unwrap(),
        vec![(gone.to_string_lossy().into_owned(), 1)]
    );
    assert_eq!(fx.store.counts().unwrap().docs, 4);
    assert_eq!(
        count(&fx.store, "SELECT COUNT(*) FROM docs WHERE active = 1"),
        5,
        "the rows are kept"
    );

    // It comes back: visible again without re-reading a file.
    std::fs::rename(&parked, &gone).unwrap();
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!((r.roots_missing, r.added, r.chunks), (0, 0, 0));
    assert_eq!(search(&fx.store, "kubernetes"), 1);
    assert!(fx.store.absent_roots().unwrap().is_empty());

    // Gone for good: prune drops the rows, the search index included.
    std::fs::rename(&gone, &parked).unwrap();
    let chunks_before = count(&fx.store, "SELECT COUNT(*) FROM chunks");
    let opts = IndexOptions {
        prune: true,
        ..Default::default()
    };
    let r = index::run(&fx.config, &mut fx.store, &opts).unwrap();
    assert_eq!((r.roots_pruned, r.roots_missing), (1, 0));
    assert!(r.to_string().ends_with("; 1 absent roots pruned"), "{r}");
    assert_eq!(count(&fx.store, "SELECT COUNT(*) FROM docs"), 4);
    assert!(count(&fx.store, "SELECT COUNT(*) FROM chunks") < chunks_before);
    assert_eq!(
        count(
            &fx.store,
            "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH 'kubernetes'"
        ),
        0
    );
    assert!(fx.store.absent_roots().unwrap().is_empty());
}

#[test]
fn rename_onto_a_soft_deleted_path_does_not_violate_uniqueness() {
    let mut fx = fixture();
    age_all(&fx.base);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    let a = fx.base.join("proj-b/notes/other.md");
    let b = fx.base.join("proj-b/notes/b.md");
    write(
        &b,
        "# B\n\nA second document in project b with enough text to chunk.\n",
    );
    age(&b);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    // Delete a, index (a is soft-deleted), then move b onto a's path.
    std::fs::remove_file(&a).unwrap();
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r.removed, 1);
    std::fs::rename(&b, &a).unwrap();
    age(&a);
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r.renamed, 1);
    // Every later run must still work.
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r.unchanged, 5);
    let titles: Vec<String> = fx
        .store
        .conn()
        .prepare("SELECT title FROM docs WHERE rel_path = 'other.md' AND root_id = (SELECT id FROM roots WHERE path LIKE '%proj-b%')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        titles,
        vec!["B".to_owned()],
        "exactly one row holds the path"
    );
}

/// Active `(rel_path, title)` rows of the proj-b notes root, by path.
fn proj_b_docs(store: &Store) -> Vec<(String, String)> {
    store
        .conn()
        .prepare(
            "SELECT d.rel_path, d.title FROM docs d JOIN roots r ON r.id = d.root_id
             WHERE r.path LIKE '%proj-b%' AND d.active = 1 ORDER BY d.rel_path",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

const TEMPLATE: &str = "# Template\n\nBoilerplate every new note in this project starts from.\n";
const OLD: &str = "# Old\n\nA document that is about to be renamed onto another path.\n";

/// proj-b holds `other.md`, a soft-deleted `p.md` (the template) and, when
/// `with_old`, a live `old.md`.
fn with_soft_deleted_template(with_old: bool) -> Fixture {
    let mut fx = fixture();
    let notes = fx.base.join("proj-b/notes");
    write(&notes.join("p.md"), TEMPLATE);
    if with_old {
        write(&notes.join("old.md"), OLD);
    }
    age_all(&fx.base);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    std::fs::remove_file(notes.join("p.md")).unwrap();
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r.removed, 1);
    fx
}

fn pairs(docs: &[(&str, &str)]) -> Vec<(String, String)> {
    docs.iter()
        .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
        .collect()
}

/// A soft-deleted row deleted to make way for a rename must not be handed to
/// a later new file with the same content: that used to fail the run, and
/// every run after it, on a foreign key. `copy` sorts after or before the
/// rename target, which used to fail differently (an error, or a file left
/// with no row).
#[test]
fn rename_onto_a_soft_deleted_path_next_to_a_copy_of_its_content() {
    for copy in ["z.md", "a.md"] {
        let mut fx = with_soft_deleted_template(true);
        let notes = fx.base.join("proj-b/notes");
        std::fs::rename(notes.join("old.md"), notes.join("p.md")).unwrap();
        write(&notes.join(copy), TEMPLATE);
        age_all(&fx.base);
        let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
        assert_eq!((r.renamed, r.added, r.removed), (1, 1, 0), "{copy}");
        let mut want = pairs(&[
            ("other.md", "Other project"),
            ("p.md", "Old"),
            (copy, "Template"),
        ]);
        want.sort();
        assert_eq!(proj_b_docs(&fx.store), want, "{copy}");
        // Every later run must still work, and find nothing to do.
        let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
        assert_eq!(r.added + r.updated + r.renamed + r.removed, 0, "{copy}");
        assert_eq!(
            count(&fx.store, "SELECT COUNT(*) FROM docs WHERE active = 0"),
            0
        );
    }
}

/// A soft-deleted row reused for a new file with its old content must not be
/// reused again when its old path comes back with different content: one row
/// used to end up holding the other file's text.
#[test]
fn restored_path_and_a_copy_of_its_old_content_get_their_own_rows() {
    let mut fx = with_soft_deleted_template(false);
    let notes = fx.base.join("proj-b/notes");
    write(&notes.join("a.md"), TEMPLATE);
    write(
        &notes.join("p.md"),
        "# Rewritten\n\nThe restored file says something else entirely now.\n",
    );
    age_all(&fx.base);
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!((r.added, r.renamed, r.removed), (2, 0, 0));
    assert_eq!(
        proj_b_docs(&fx.store),
        pairs(&[
            ("a.md", "Template"),
            ("other.md", "Other project"),
            ("p.md", "Rewritten")
        ])
    );
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r.added + r.updated + r.renamed + r.removed, 0);
}

#[test]
fn identical_content_files_are_all_soft_deleted() {
    let mut fx = fixture();
    age_all(&fx.base);
    let dir = fx.base.join("proj-b/notes");
    for n in ["dup1.md", "dup2.md", "dup3.md"] {
        write(
            &dir.join(n),
            "# Same\n\nIdentical content in every copy of this note.\n",
        );
        age(&dir.join(n));
    }
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    for n in ["dup1.md", "dup2.md", "dup3.md"] {
        std::fs::remove_file(dir.join(n)).unwrap();
    }
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r.removed, 3);
    assert_eq!(
        count(
            &fx.store,
            "SELECT COUNT(*) FROM docs WHERE active = 1 AND rel_path LIKE 'dup%'"
        ),
        0
    );
}

#[cfg(unix)]
#[test]
fn unreadable_subdirectory_skips_deletions_for_that_root() {
    use std::os::unix::fs::PermissionsExt;
    let mut fx = fixture();
    age_all(&fx.base);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    let sub = fx.base.join("proj-a/notes/sub");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default());
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let r = r.unwrap();
    assert!(r.errors >= 1);
    assert_eq!(
        r.removed, 0,
        "files under the unreadable dir must not be soft-deleted"
    );
    assert_eq!(
        index::doc_active(&fx.store, doc_id(&fx.store, "sub/arch.md")).unwrap(),
        Some(true)
    );
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r.errors, 0);
    assert_eq!(r.removed, 0);
}

fn doc_id(store: &Store, rel_path: &str) -> i64 {
    store
        .conn()
        .query_row("SELECT id FROM docs WHERE rel_path = ?1", [rel_path], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn only_root_with_no_match_is_an_error() {
    let mut fx = fixture();
    let opts = IndexOptions {
        only_root: Some(fx.base.join("nope")),
        ..Default::default()
    };
    let err = index::run(&fx.config, &mut fx.store, &opts).unwrap_err();
    assert!(err.to_string().contains("not a configured root"), "{err}");
    assert!(fx.store.meta("last_index_at").unwrap().is_none());
}

#[test]
fn parser_version_is_tracked_per_root() {
    let mut fx = fixture();
    age_all(&fx.base);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    fx.store
        .conn()
        .execute("UPDATE roots SET parser_version = '0'", [])
        .unwrap();
    // Re-parse only proj-b; the other roots must still be marked stale.
    let opts = IndexOptions {
        only_root: Some(fx.base.join("proj-b/notes")),
        ..Default::default()
    };
    let r = index::run(&fx.config, &mut fx.store, &opts).unwrap();
    assert_eq!(r.updated, 1);
    let stale = count(
        &fx.store,
        &format!(
            "SELECT COUNT(*) FROM roots WHERE parser_version IS NOT '{}'",
            index::PARSER_VERSION
        ),
    );
    assert_eq!(stale, 2);
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(
        r.updated, 4,
        "remaining roots re-parsed on the next full run"
    );
}

#[test]
fn frontmatter_dates_are_normalised_for_ordering() {
    let mut fx = fixture();
    age_all(&fx.base);
    let dir = fx.base.join("proj-b/notes");
    write(
        &dir.join("old.md"),
        "---\ndate: 2020-01-05\n---\n# Old\n\nold content long enough to be a chunk of text\n",
    );
    write(
        &dir.join("mid.md"),
        "---\nupdated: 2023-06-01 12:00\n---\n# Mid\n\nmid content long enough to be a chunk of text\n",
    );
    age(&dir.join("old.md"));
    age(&dir.join("mid.md"));
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    let rows: Vec<(String, Option<String>)> = fx
        .store
        .conn()
        .prepare("SELECT rel_path, updated FROM docs WHERE rel_path IN ('old.md','mid.md') ORDER BY rel_path")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(rows[0].1.as_deref(), Some("2023-06-01T12:00:00Z"));
    // `date:` sets created, not updated; updated falls back to the filesystem.
    assert!(rows[1].1.as_deref().unwrap().starts_with("20"));
    let recent = list::run(
        &fx.store,
        &ListFilter {
            recent_days: Some(30),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(recent.iter().all(|d| d.rel_path != "mid.md"));
    let all = list::run(&fx.store, &ListFilter::default()).unwrap();
    assert_eq!(
        all.last().unwrap().rel_path,
        "mid.md",
        "oldest updated sorts last"
    );
}

#[test]
fn non_utf8_file_is_indexed_lossily_not_fatal() {
    let mut fx = fixture();
    age_all(&fx.base);
    let bad = fx.base.join("proj-b/notes/latin1.md");
    std::fs::write(
        &bad,
        b"# Caf\xe9\n\nbody with a stray byte \xff here and more words after\n",
    )
    .unwrap();
    age(&bad);
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(r.errors, 0);
    assert_eq!(
        count(
            &fx.store,
            "SELECT COUNT(*) FROM docs WHERE rel_path = 'latin1.md' AND active = 1"
        ),
        1
    );
}

/// Only Windows separates paths with a backslash. Elsewhere it is part of a
/// name, and folding it made two files claim one path, failing every run.
#[cfg(unix)]
#[test]
fn a_backslash_in_a_file_name_is_not_a_separator() {
    let mut fx = fixture();
    let notes = fx.base.join("proj-b/notes");
    std::fs::create_dir_all(notes.join("a")).unwrap();
    write(
        &notes.join("a/b.md"),
        "# Slash\n\nThe file in a directory.\n",
    );
    write(
        &notes.join("a\\b.md"),
        "# Backslash\n\nThe file with an odd name.\n",
    );
    age_all(&fx.base);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    let docs = proj_b_docs(&fx.store);
    assert!(
        docs.contains(&("a/b.md".to_owned(), "Slash".to_owned())),
        "{docs:?}"
    );
    assert!(
        docs.contains(&("a\\b.md".to_owned(), "Backslash".to_owned())),
        "{docs:?}"
    );
}

/// A file indexed from lossily decoded bytes must be readable the same way.
#[test]
fn a_non_utf8_file_can_be_read_back() {
    let mut fx = fixture();
    let bad = fx.base.join("proj-b/notes/latin1.md");
    std::fs::write(
        &bad,
        b"# Caf\xe9\n\nbody with a stray byte \xff here and more words after\n",
    )
    .unwrap();
    age_all(&fx.base);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    let text = kvault::get::run(
        &fx.store,
        &kvault::get::Target::parse("proj-b/notes/latin1.md"),
        true,
    )
    .unwrap();
    assert!(text.contains("more words after"), "{text}");
    let packed = kvault::recall::run(
        &fx.store,
        &kvault::search::Query::parse("stray byte"),
        &kvault::recall::RecallOptions {
            budget: 1500,
            project: None,
            format: kvault::cli::RecallFormat::Md,
        },
    )
    .unwrap();
    assert!(packed.text.contains("more words after"), "{}", packed.text);
}

/// A root is on the current parser only when every file in it was re-parsed.
#[cfg(unix)]
#[test]
fn an_unreadable_file_keeps_its_root_marked_for_reparse() {
    use std::os::unix::fs::PermissionsExt;
    let mut fx = fixture();
    age_all(&fx.base);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    fx.store
        .conn()
        .execute("UPDATE roots SET parser_version = '0'", [])
        .unwrap();
    let locked = fx.base.join("proj-b/notes/other.md");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(r.unwrap().errors >= 1);
    let stale = |store: &Store| {
        count(
            store,
            &format!(
                "SELECT COUNT(*) FROM roots WHERE path LIKE '%proj-%' AND parser_version IS NOT '{}'",
                index::PARSER_VERSION
            ),
        )
    };
    assert_eq!(stale(&fx.store), 1, "only the root with the unread file");
    let r = index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!((r.errors, r.updated), (0, 1));
    assert_eq!(stale(&fx.store), 0);
}

/// A memory fact that is nothing but frontmatter is found by its description.
#[test]
fn a_frontmatter_only_fact_is_searchable() {
    let mut fx = fixture();
    write(
        &fx.base
            .join("mem/-Users-x-Projects-proj-a/memory/quorum.md"),
        "---\nname: quorum\ndescription: the zookeeper ensemble needs a quorum of three\n---\n",
    );
    age_all(&fx.base);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    let hits = kvault::search::run(
        &fx.store,
        &kvault::search::Query::parse("zookeeper quorum"),
        &kvault::search::SearchOptions::default(),
    )
    .unwrap()
    .hits;
    assert_eq!(hits[0].chunk.doc.rel_path, "quorum.md");
    let text = kvault::get::run(
        &fx.store,
        &kvault::get::Target::Id(hits[0].chunk.id.clone()),
        true,
    )
    .unwrap();
    assert!(text.contains("zookeeper ensemble"), "{text}");
}

/// An unchanged corpus is not rewritten link by link on every run, and a
/// link ambiguous across roots resolves the same way whatever order the
/// documents were indexed in.
#[test]
fn links_are_only_written_when_their_target_changes() {
    let mut fx = fixture();
    age_all(&fx.base);
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    let resolved = |store: &Store| -> Vec<(String, Option<i64>)> {
        store
            .conn()
            .prepare("SELECT target, to_doc_id FROM links ORDER BY rowid")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    let before = resolved(&fx.store);
    assert!(before.iter().any(|(_, to)| to.is_some()));
    fx.store
        .conn()
        .execute_batch(
            "CREATE TEMP TABLE link_writes(n INTEGER);
             CREATE TEMP TRIGGER count_link_writes AFTER UPDATE ON main.links
             BEGIN INSERT INTO link_writes VALUES (1); END;",
        )
        .unwrap();
    index::run(&fx.config, &mut fx.store, &IndexOptions::default()).unwrap();
    assert_eq!(count(&fx.store, "SELECT COUNT(*) FROM link_writes"), 0);
    assert_eq!(resolved(&fx.store), before);
}
