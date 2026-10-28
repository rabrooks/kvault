//! Temporary corpus shared by the integration tests: two projects with
//! notes roots and one Claude memory root.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use kvault::config::{Config, ProjectFrom, RootConfig};
use kvault::index::{self, IndexOptions};
use kvault::roots::RootKind;
use kvault::store::Store;

pub struct Fixture {
    pub _tmp: tempfile::TempDir,
    pub base: PathBuf,
    pub config: Config,
    pub store: Store,
}

pub fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Sections are padded past the 40-token merge threshold so each heading
/// stays its own chunk.
pub fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().to_path_buf();
    write(
        &base.join("proj-a/notes/plan.md"),
        "# Plan\n\n\
         This plan document introduces the project and sets the stage for the phases \
         below, with enough words to stand as its own chunk of content on its own.\n\n\
         ## Phase one\n\n\
         Do the first thing with lambda cold start tuning and provisioned concurrency so \
         the function warms before traffic arrives. See [[decision-one]].\n\n\
         ## Phase two\n\n\
         Do the second thing: move the kubernetes pods onto the new node pool and verify \
         the services still answer on their cluster addresses afterwards.\n",
    );
    write(
        &base.join("proj-a/notes/sub/arch.md"),
        "---\ntitle: Architecture\ntags: [design, storage]\npinned: true\n---\n\
         # Architecture\n\n\
         The storage layer is sqlite with a single writer and WAL mode; readers never \
         block and the index file is disposable because markdown is the source of truth.\n",
    );
    write(
        &base.join("proj-a/notes/readme.md"),
        "# Readme\n\nShort note in project a.\n",
    );
    write(
        &base.join("proj-b/notes/other.md"),
        "# Other project\n\n\
         Unrelated content about kubernetes pods and services, including the horizontal \
         pod autoscaler and how it reacts to CPU pressure on the worker nodes.\n",
    );
    write(
        &base.join("proj-b/notes/readme.md"),
        "# Readme\n\nShort note in project b.\n",
    );
    write(
        &base.join("proj-b/notes/cross.md"),
        "---\ntags: [design]\n---\n# Cross reference\n\nSee [[plan]] for the phases of the other project.\n",
    );
    write(
        &base.join("mem/-Users-x-Projects-proj-a/memory/decision-one.md"),
        "---\nname: decision-one\ndescription: we chose sqlite over tantivy\n\
         metadata:\n  type: project\n---\n\n\
         We chose SQLite FTS5 over Tantivy because one store is simpler to operate, and a \
         second index would need its own consistency story and backup plan.\n",
    );
    write(
        &base.join("mem/-Users-x-Projects-proj-a/memory/lambda-timeouts.md"),
        "---\nname: lambda-timeouts\ndescription: Lambda timeout is 30 seconds\n---\n\n\
         The Lambda timeout is thirty seconds; cold start is mitigated by keeping the \
         function warm with a scheduled ping rather than by raising the timeout.\n",
    );
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
        ],
        ..Config::default()
    };
    let mut store = Store::open(&base.join("index.db")).unwrap();
    index::run(&config, &mut store, &IndexOptions::default()).unwrap();
    Fixture {
        _tmp: tmp,
        base,
        config,
        store,
    }
}
