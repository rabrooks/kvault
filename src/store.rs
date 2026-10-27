//! `SQLite` store: schema, migrations, and the connection wrapper.
//!
//! The database is *derived*: deleting it and running `kvault index` must
//! reproduce it exactly. Schema changes bump `PRAGMA user_version` and add a
//! migration step; there is no down-migration, a rebuild is the rollback.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};

use crate::error::{Error, Result};

/// Current schema version. Bump with every DDL change.
pub const SCHEMA_VERSION: i64 = 1;

/// Minimum `SQLite` version: `FTS5` `contentless_delete` arrived in 3.43.0.
pub const MIN_SQLITE_VERSION: i32 = 3_043_000;

const SCHEMA_V1: &str = r#"
CREATE TABLE roots (
    id      INTEGER PRIMARY KEY,
    path    TEXT NOT NULL UNIQUE,
    kind    TEXT NOT NULL CHECK (kind IN ('memory','notes','reference','other')),
    weight  REAL NOT NULL DEFAULT 1.0,
    project TEXT
);

CREATE TABLE docs (
    id          INTEGER PRIMARY KEY,
    root_id     INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
    rel_path    TEXT NOT NULL,
    title       TEXT NOT NULL,
    blake3      TEXT NOT NULL,
    mtime_ns    INTEGER NOT NULL,
    size        INTEGER NOT NULL,
    frontmatter TEXT,
    kind        TEXT,
    created     TEXT,
    updated     TEXT,
    active      INTEGER NOT NULL DEFAULT 1,
    indexed_at  INTEGER NOT NULL,
    UNIQUE (root_id, rel_path)
);
CREATE INDEX docs_blake3 ON docs(blake3);
CREATE INDEX docs_updated ON docs(updated);
CREATE INDEX docs_title ON docs(title);

CREATE TABLE chunks (
    id           INTEGER PRIMARY KEY,
    doc_id       INTEGER NOT NULL REFERENCES docs(id) ON DELETE CASCADE,
    ordinal      INTEGER NOT NULL,
    heading_path TEXT NOT NULL DEFAULT '',
    level        INTEGER NOT NULL DEFAULT 0,
    start_line   INTEGER NOT NULL,
    end_line     INTEGER NOT NULL,
    byte_start   INTEGER NOT NULL,
    byte_end     INTEGER NOT NULL,
    tokens       INTEGER NOT NULL DEFAULT 0,
    blake3       TEXT NOT NULL,
    UNIQUE (doc_id, ordinal)
);
CREATE INDEX chunks_blake3 ON chunks(blake3);

-- Contentless FTS: bodies live only in the files and inside this index.
-- rowid is chunks.id so hits map straight back to a byte range.
CREATE VIRTUAL TABLE chunks_fts USING fts5(
    title, heading_path, body, path,
    content='', contentless_delete=1,
    tokenize="unicode61 remove_diacritics 2 tokenchars '/_-.'",
    prefix='2 3'
);

CREATE TABLE links (
    from_doc_id INTEGER NOT NULL REFERENCES docs(id) ON DELETE CASCADE,
    to_doc_id   INTEGER REFERENCES docs(id) ON DELETE SET NULL,
    target      TEXT NOT NULL,
    link_type   TEXT NOT NULL,
    line        INTEGER NOT NULL
);
CREATE INDEX links_from ON links(from_doc_id);
CREATE INDEX links_to ON links(to_doc_id);
CREATE INDEX links_target ON links(target);

CREATE TABLE tags (
    doc_id INTEGER NOT NULL REFERENCES docs(id) ON DELETE CASCADE,
    tag    TEXT NOT NULL,
    PRIMARY KEY (doc_id, tag)
);
CREATE INDEX tags_tag ON tags(tag);

CREATE TABLE observations (
    doc_id   INTEGER NOT NULL REFERENCES docs(id) ON DELETE CASCADE,
    chunk_id INTEGER REFERENCES chunks(id) ON DELETE CASCADE,
    line     INTEGER NOT NULL,
    category TEXT NOT NULL,
    text     TEXT NOT NULL
);

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// An open index database.
pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    /// Open (creating and migrating if needed) the database at `path`.
    ///
    /// # Errors
    /// Returns [`Error::Store`] if the file cannot be opened, `SQLite` is too
    /// old, or migration fails.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE;
        let conn = Connection::open_with_flags(path, flags).map_err(|e| Error::Store {
            path: path.to_path_buf(),
            source: e,
        })?;
        let store = Self {
            conn,
            path: path.to_path_buf(),
        };
        store.init()?;
        Ok(store)
    }

    /// Open an in-memory database (tests).
    ///
    /// # Errors
    /// Returns an error if `SQLite` cannot create the database or migrate it.
    pub fn open_in_memory() -> Result<Self> {
        let store = Self {
            conn: Connection::open_in_memory()?,
            path: PathBuf::from(":memory:"),
        };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<()> {
        if rusqlite::version_number() < MIN_SQLITE_VERSION {
            return Err(Error::Other(anyhow::anyhow!(
                "SQLite {} is older than the required 3.43.0",
                rusqlite::version()
            )));
        }
        self.conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )?;
        self.migrate()
    }

    fn migrate(&self) -> Result<()> {
        let current: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if current == SCHEMA_VERSION {
            return Ok(());
        }
        if current > SCHEMA_VERSION {
            return Err(Error::Other(anyhow::anyhow!(
                "index schema v{current} is newer than this kvault (v{SCHEMA_VERSION}); \
                 upgrade kvault or delete {}",
                self.path.display()
            )));
        }
        if current == 0 {
            self.conn.execute_batch(SCHEMA_V1)?;
            self.conn.execute(
                "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)",
                params![SCHEMA_VERSION.to_string()],
            )?;
            self.conn
                .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        // Future: `if current < 2 { ... }` steps, applied in order.
        Ok(())
    }

    #[must_use]
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Row counts for `kvault status`.
    ///
    /// # Errors
    /// Returns an error if a count query fails.
    pub fn counts(&self) -> Result<Counts> {
        let count = |table: &str| -> Result<i64> {
            Ok(self
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
        };
        Ok(Counts {
            roots: count("roots")?,
            docs: count("docs WHERE active = 1")?,
            chunks: count("chunks")?,
            links: count("links")?,
            tags: count("tags")?,
        })
    }

    /// Read a `meta` value.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()?)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub roots: i64,
    pub docs: i64,
    pub chunks: i64,
    pub links: i64,
    pub tags: i64,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn bundled_sqlite_is_new_enough() {
        assert!(
            rusqlite::version_number() >= MIN_SQLITE_VERSION,
            "{}",
            rusqlite::version()
        );
    }

    #[test]
    fn fresh_db_has_schema_v1() {
        let s = Store::open_in_memory().unwrap();
        let v: i64 = s
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(s.meta("schema_version").unwrap().as_deref(), Some("1"));
        let tables: Vec<String> = s
            .conn()
            .prepare("SELECT name FROM sqlite_master WHERE type IN ('table') ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for t in [
            "roots",
            "docs",
            "chunks",
            "chunks_fts",
            "links",
            "tags",
            "observations",
            "meta",
        ] {
            assert!(tables.contains(&t.to_owned()), "missing {t} in {tables:?}");
        }
        assert_eq!(s.counts().unwrap(), Counts::default());
    }

    #[test]
    fn contentless_fts_round_trips_a_query() {
        let s = Store::open_in_memory().unwrap();
        s.conn()
            .execute(
                "INSERT INTO chunks_fts(rowid, title, heading_path, body, path)
                 VALUES (7, 'Lambda patterns', 'AWS / Lambda', 'cold start mitigations', 'aws/lambda.md')",
                [],
            )
            .unwrap();
        let rowid: i64 = s
            .conn()
            .query_row(
                "SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH 'cold' ORDER BY bm25(chunks_fts)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rowid, 7);
        // tokenchars keeps '/' so path tokens are searchable as written
        let hit: i64 = s
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH 'path:\"aws/lambda.md\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit, 1);
        // contentless_delete: deleting by rowid works without a content table
        s.conn()
            .execute("DELETE FROM chunks_fts WHERE rowid = 7", [])
            .unwrap();
        let left: i64 = s
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH 'cold'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn open_on_disk_creates_parent_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/dir/index.db");
        Store::open(&path).unwrap();
        let again = Store::open(&path).unwrap();
        let v: i64 = again
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }
}
