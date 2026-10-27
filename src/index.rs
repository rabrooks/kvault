//! Indexer: walk roots, detect changes by mtime/size then `BLAKE3`, chunk, upsert.
//!
//! Rules (from the design doc):
//! - a file whose `(mtime, size)` match the stored row and whose mtime is
//!   older than 2 s is skipped without reading (git's racily-clean rule);
//! - a changed file is re-hashed; an unchanged hash only refreshes mtime;
//! - a new path whose hash matches a vanished path in the same root is a
//!   rename and keeps its `docs.id`;
//! - vanished files are soft-deleted (`active = 0`) and their chunks removed;
//! - a root that is missing on disk is skipped, never treated as mass delete;
//! - the whole run is one transaction.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use rusqlite::{OptionalExtension, Transaction, params};

use crate::config::Config;
use crate::error::Result;
use crate::markdown::{self, Parsed};
use crate::roots::{self, Root};
use crate::store::Store;

/// Files modified within this window are re-read even if mtime/size match.
const RACY_WINDOW: Duration = Duration::from_secs(2);

/// Identifier of the token counter used for `chunks.tokens`, stored in `meta`.
pub const TOKENIZER_ID: &str = "bytes36";

/// Bump whenever `markdown::parse` output changes shape (chunking, links,
/// tags, title rules). A mismatch with `meta.parser_version` forces every
/// file to be re-parsed on the next run even if its hash is unchanged.
pub const PARSER_VERSION: &str = "1";

/// Options for an index run.
#[derive(Debug, Clone, Default)]
pub struct IndexOptions {
    /// Ignore the mtime/size fast path and re-hash every file.
    pub full: bool,
    pub quiet: bool,
    /// Only index roots whose path equals this (after tilde expansion).
    pub only_root: Option<PathBuf>,
}

/// What an index run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexReport {
    pub roots: usize,
    pub roots_missing: usize,
    pub scanned: usize,
    pub unchanged: usize,
    pub added: usize,
    pub updated: usize,
    pub renamed: usize,
    pub removed: usize,
    pub chunks: usize,
    pub links_unresolved: usize,
    pub elapsed_ms: u128,
}

impl std::fmt::Display for IndexReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} roots ({} missing), {} files: {} unchanged, {} added, {} updated, {} renamed, {} removed; {} chunks written, {} links unresolved; {} ms",
            self.roots,
            self.roots_missing,
            self.scanned,
            self.unchanged,
            self.added,
            self.updated,
            self.renamed,
            self.removed,
            self.chunks,
            self.links_unresolved,
            self.elapsed_ms
        )
    }
}

/// Run an incremental index over all configured roots.
///
/// # Errors
/// Returns an error if the database cannot be written or a root cannot be walked.
pub fn run(config: &Config, store: &mut Store, opts: &IndexOptions) -> Result<IndexReport> {
    let started = Instant::now();
    let mut report = IndexReport::default();

    let expanded = roots::expand(&config.roots);
    let selected: Vec<&Root> = expanded
        .iter()
        .filter(|r| opts.only_root.as_ref().is_none_or(|p| &r.path == p))
        .collect();
    report.roots = selected.len();

    let tx = store.conn_mut().transaction()?;
    let stored_parser: Option<String> = tx
        .query_row(
            "SELECT value FROM meta WHERE key = 'parser_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let reparse = stored_parser.as_deref() != Some(PARSER_VERSION);
    let known_roots = known_root_paths(&tx)?;
    report.roots_missing = known_roots
        .iter()
        .filter(|p| !expanded.iter().any(|r| &r.path == *p))
        .count();

    for root in selected {
        let root_id = upsert_root(&tx, root)?;
        index_root(&tx, root_id, root, opts, reparse, &mut report)?;
    }

    report.links_unresolved = resolve_links(&tx)?;
    let now = jiff::Timestamp::now().to_string();
    tx.execute(
        "INSERT INTO meta(key, value) VALUES ('last_index_at', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![now],
    )?;
    tx.execute(
        "INSERT INTO meta(key, value) VALUES ('tokenizer', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![TOKENIZER_ID],
    )?;
    tx.execute(
        "INSERT INTO meta(key, value) VALUES ('parser_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![PARSER_VERSION],
    )?;
    tx.commit()?;

    report.elapsed_ms = started.elapsed().as_millis();
    Ok(report)
}

fn known_root_paths(tx: &Transaction<'_>) -> Result<Vec<PathBuf>> {
    let mut stmt = tx.prepare("SELECT path FROM roots")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows
        .filter_map(std::result::Result::ok)
        .map(PathBuf::from)
        .collect())
}

fn upsert_root(tx: &Transaction<'_>, root: &Root) -> Result<i64> {
    let path = root.path.to_string_lossy();
    tx.execute(
        "INSERT INTO roots(path, kind, weight, project) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(path) DO UPDATE SET kind = excluded.kind, weight = excluded.weight,
                                         project = excluded.project",
        params![path, root.kind.as_str(), root.weight, root.project],
    )?;
    Ok(
        tx.query_row("SELECT id FROM roots WHERE path = ?1", params![path], |r| {
            r.get(0)
        })?,
    )
}

/// Stored state for one document, used for the change-detection fast path.
#[derive(Debug, Clone)]
struct DocRow {
    id: i64,
    blake3: String,
    mtime_ns: i64,
    size: i64,
    active: bool,
}

struct FileStat {
    rel_path: String,
    abs_path: PathBuf,
    mtime_ns: i64,
    size: i64,
    recent: bool,
}

fn index_root(
    tx: &Transaction<'_>,
    root_id: i64,
    root: &Root,
    opts: &IndexOptions,
    reparse: bool,
    report: &mut IndexReport,
) -> Result<()> {
    let existing = load_docs(tx, root_id)?;
    let files = walk(&root.path)?;
    report.scanned += files.len();

    let mut seen: HashSet<String> = HashSet::with_capacity(files.len());
    // (file, hash, text) for files that need parsing, decided after rename detection.
    let mut pending_new: Vec<(FileStat, String, String)> = Vec::new();

    for file in files {
        seen.insert(file.rel_path.clone());
        match existing.get(&file.rel_path) {
            Some(row) if row.active => {
                let fast_ok = !opts.full
                    && !reparse
                    && row.mtime_ns == file.mtime_ns
                    && row.size == file.size
                    && !file.recent;
                if fast_ok {
                    report.unchanged += 1;
                    continue;
                }
                let text = std::fs::read_to_string(&file.abs_path)?;
                let hash = blake3::hash(text.as_bytes()).to_hex().to_string();
                if hash == row.blake3 && !reparse {
                    tx.execute(
                        "UPDATE docs SET mtime_ns = ?1, size = ?2 WHERE id = ?3",
                        params![file.mtime_ns, file.size, row.id],
                    )?;
                    report.unchanged += 1;
                } else {
                    replace_doc(tx, row.id, &file, &hash, &text, report)?;
                    report.updated += 1;
                }
            }
            _ => {
                // New path, or a path that was soft-deleted earlier.
                let text = std::fs::read_to_string(&file.abs_path)?;
                let hash = blake3::hash(text.as_bytes()).to_hex().to_string();
                pending_new.push((file, hash, text));
            }
        }
    }

    // Vanished = active rows whose path was not seen. Candidates for rename.
    let mut vanished: HashMap<String, (String, i64)> = existing
        .iter()
        .filter(|(p, r)| r.active && !seen.contains(*p))
        .map(|(p, r)| (r.blake3.clone(), (p.clone(), r.id)))
        .collect();
    // Soft-deleted rows by hash: a file restored under any path reclaims its id.
    let mut inactive_by_hash: HashMap<String, i64> = existing
        .values()
        .filter(|r| !r.active)
        .map(|r| (r.blake3.clone(), r.id))
        .collect();

    for (file, hash, text) in pending_new {
        if let Some((old_path, id)) = vanished.remove(&hash) {
            // Rename: keep the id, rewrite chunks so the FTS path column is right.
            tx.execute(
                "UPDATE docs SET rel_path = ?1, active = 1 WHERE id = ?2",
                params![file.rel_path, id],
            )?;
            replace_doc(tx, id, &file, &hash, &text, report)?;
            report.renamed += 1;
            debug_assert_ne!(old_path, file.rel_path);
        } else if let Some(row) = existing.get(&file.rel_path) {
            // Previously soft-deleted path came back.
            inactive_by_hash.remove(&row.blake3);
            tx.execute("UPDATE docs SET active = 1 WHERE id = ?1", params![row.id])?;
            replace_doc(tx, row.id, &file, &hash, &text, report)?;
            report.added += 1;
        } else if let Some(id) = inactive_by_hash.remove(&hash) {
            // Same content as a soft-deleted doc, new path: restore onto its id.
            tx.execute(
                "UPDATE docs SET rel_path = ?1, active = 1 WHERE id = ?2",
                params![file.rel_path, id],
            )?;
            replace_doc(tx, id, &file, &hash, &text, report)?;
            report.added += 1;
        } else {
            let id = insert_doc(tx, root_id, &file, &hash)?;
            replace_doc(tx, id, &file, &hash, &text, report)?;
            report.added += 1;
        }
    }

    for (_, (_, id)) in vanished {
        clear_doc_children(tx, id)?;
        // Incoming links lose their target only when the doc is gone; a content
        // update or rename keeps the id, so those links stay resolved.
        tx.execute(
            "UPDATE links SET to_doc_id = NULL WHERE to_doc_id = ?1",
            params![id],
        )?;
        tx.execute("UPDATE docs SET active = 0 WHERE id = ?1", params![id])?;
        report.removed += 1;
    }
    Ok(())
}

fn load_docs(tx: &Transaction<'_>, root_id: i64) -> Result<HashMap<String, DocRow>> {
    let mut stmt = tx.prepare(
        "SELECT rel_path, id, blake3, mtime_ns, size, active FROM docs WHERE root_id = ?1",
    )?;
    let rows = stmt.query_map(params![root_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            DocRow {
                id: r.get(1)?,
                blake3: r.get(2)?,
                mtime_ns: r.get(3)?,
                size: r.get(4)?,
                active: r.get::<_, i64>(5)? != 0,
            },
        ))
    })?;
    Ok(rows.filter_map(std::result::Result::ok).collect())
}

/// Walk a root for `*.md` files. Git ignore files are deliberately *not*
/// honoured: roots are often inside ignored or excluded directories.
/// Hidden entries are skipped; a `.kvaultignore` file (gitignore syntax) is
/// honoured.
fn walk(root: &Path) -> Result<Vec<FileStat>> {
    let now = SystemTime::now();
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .hidden(true)
        .add_custom_ignore_filename(".kvaultignore")
        .follow_links(false)
        .sort_by_file_path(Path::cmp)
        .build();
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("warning: {e}");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let meta = entry
            .metadata()
            .map_err(|e| anyhow::anyhow!("{}: {e}", entry.path().display()))?;
        let modified = meta.modified()?;
        let mtime_ns = modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX));
        let recent = now
            .duration_since(modified)
            .map_or(true, |age| age < RACY_WINDOW);
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        out.push(FileStat {
            rel_path: rel,
            abs_path: path.to_path_buf(),
            mtime_ns,
            size: i64::try_from(meta.len()).unwrap_or(i64::MAX),
            recent,
        });
    }
    Ok(out)
}

fn insert_doc(tx: &Transaction<'_>, root_id: i64, file: &FileStat, hash: &str) -> Result<i64> {
    tx.execute(
        "INSERT INTO docs(root_id, rel_path, title, blake3, mtime_ns, size, indexed_at)
         VALUES (?1, ?2, '', ?3, ?4, ?5, 0)",
        params![root_id, file.rel_path, hash, file.mtime_ns, file.size],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Re-parse `text` and replace every derived row for doc `id`.
fn replace_doc(
    tx: &Transaction<'_>,
    id: i64,
    file: &FileStat,
    hash: &str,
    text: &str,
    report: &mut IndexReport,
) -> Result<()> {
    let stem = Path::new(&file.rel_path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parsed = markdown::parse(text, &stem);
    let (created, updated) = doc_times(&parsed, &file.abs_path);
    let frontmatter_json = frontmatter_json(&parsed);
    let now = jiff::Timestamp::now().as_second();

    clear_doc_children(tx, id)?;
    tx.execute(
        "UPDATE docs SET title = ?1, blake3 = ?2, mtime_ns = ?3, size = ?4, frontmatter = ?5,
                         kind = ?6, created = ?7, updated = ?8, active = 1, indexed_at = ?9
         WHERE id = ?10",
        params![
            parsed.title,
            hash,
            file.mtime_ns,
            file.size,
            frontmatter_json,
            parsed.frontmatter.kind,
            created,
            updated,
            now,
            id
        ],
    )?;

    // FTS title column also carries the frontmatter description so a one-line
    // summary (Claude memory files) is searchable alongside the title.
    let fts_title = match &parsed.frontmatter.description {
        Some(d) => format!("{} — {d}", parsed.title),
        None => parsed.title.clone(),
    };

    let mut ins_chunk = tx.prepare_cached(
        "INSERT INTO chunks(doc_id, ordinal, heading_path, level, start_line, end_line,
                            byte_start, byte_end, tokens, blake3)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    let mut ins_fts = tx.prepare_cached(
        "INSERT INTO chunks_fts(rowid, title, heading_path, body, path) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for c in &parsed.chunks {
        let body = &text[c.byte_start..c.byte_end];
        let chunk_hash = blake3::hash(body.as_bytes()).to_hex().to_string();
        ins_chunk.execute(params![
            id,
            c.ordinal,
            c.heading_path,
            c.level,
            c.start_line,
            c.end_line,
            i64::try_from(c.byte_start).unwrap_or(i64::MAX),
            i64::try_from(c.byte_end).unwrap_or(i64::MAX),
            i64::try_from(c.tokens).unwrap_or(i64::MAX),
            chunk_hash
        ])?;
        let chunk_id = tx.last_insert_rowid();
        ins_fts.execute(params![
            chunk_id,
            fts_title,
            c.heading_path,
            body,
            file.rel_path
        ])?;
        report.chunks += 1;
    }

    let mut ins_link = tx.prepare_cached(
        "INSERT INTO links(from_doc_id, to_doc_id, target, link_type, line) VALUES (?1, NULL, ?2, ?3, ?4)",
    )?;
    for l in &parsed.links {
        ins_link.execute(params![id, l.target, l.link_type.as_db_str(), l.line])?;
    }

    let mut ins_tag =
        tx.prepare_cached("INSERT OR IGNORE INTO tags(doc_id, tag) VALUES (?1, ?2)")?;
    for t in &parsed.tags {
        ins_tag.execute(params![id, t])?;
    }

    let mut ins_obs = tx.prepare_cached(
        "INSERT INTO observations(doc_id, chunk_id, line, category, text)
         VALUES (?1, (SELECT id FROM chunks WHERE doc_id = ?1 AND start_line <= ?2 AND end_line >= ?2 LIMIT 1), ?2, ?3, ?4)",
    )?;
    for o in &parsed.observations {
        ins_obs.execute(params![id, o.line, o.category, o.text])?;
    }
    Ok(())
}

fn clear_doc_children(tx: &Transaction<'_>, id: i64) -> Result<()> {
    tx.execute(
        "DELETE FROM chunks_fts WHERE rowid IN (SELECT id FROM chunks WHERE doc_id = ?1)",
        params![id],
    )?;
    tx.execute("DELETE FROM chunks WHERE doc_id = ?1", params![id])?;
    tx.execute("DELETE FROM links WHERE from_doc_id = ?1", params![id])?;
    tx.execute("DELETE FROM tags WHERE doc_id = ?1", params![id])?;
    tx.execute("DELETE FROM observations WHERE doc_id = ?1", params![id])?;
    Ok(())
}

fn frontmatter_json(parsed: &Parsed) -> Option<String> {
    let fm = &parsed.frontmatter;
    if fm == &markdown::Frontmatter::default() {
        return None;
    }
    let mut map = serde_json::Map::new();
    if let Some(t) = &fm.title {
        map.insert("title".into(), t.clone().into());
    }
    if let Some(d) = &fm.description {
        map.insert("description".into(), d.clone().into());
    }
    if let Some(k) = &fm.kind {
        map.insert("kind".into(), k.clone().into());
    }
    if !fm.tags.is_empty() {
        map.insert("tags".into(), fm.tags.clone().into());
    }
    if fm.pinned {
        map.insert("pinned".into(), true.into());
    }
    for (k, v) in &fm.extra {
        map.insert(k.clone(), v.clone());
    }
    Some(serde_json::Value::Object(map).to_string())
}

/// `(created, updated)` as `RFC 3339` strings: frontmatter first, filesystem second.
fn doc_times(parsed: &Parsed, path: &Path) -> (Option<String>, Option<String>) {
    let fs_time = |t: std::io::Result<SystemTime>| {
        t.ok()
            .and_then(|st| jiff::Timestamp::try_from(st).ok())
            .map(|ts| ts.to_string())
    };
    let meta = std::fs::metadata(path).ok();
    let updated = parsed
        .frontmatter
        .updated
        .clone()
        .or_else(|| meta.as_ref().and_then(|m| fs_time(m.modified())));
    let created = parsed
        .frontmatter
        .created
        .clone()
        .or_else(|| meta.as_ref().and_then(|m| fs_time(m.created())))
        .or_else(|| updated.clone());
    (created, updated)
}

/// Resolve `links.to_doc_id` for every unresolved link. Returns how many
/// remain unresolved. Resolution order: relative path within the same root,
/// file stem within the same root, title within the same root, then the same
/// three across all roots.
fn resolve_links(tx: &Transaction<'_>) -> Result<usize> {
    struct DocKey {
        id: i64,
        root_id: i64,
        rel_path: String,
        stem: String,
        title: String,
    }
    let docs: Vec<DocKey> = {
        let mut stmt =
            tx.prepare("SELECT id, root_id, rel_path, title FROM docs WHERE active = 1")?;
        let rows = stmt.query_map([], |r| {
            let rel_path: String = r.get(2)?;
            let stem = Path::new(&rel_path)
                .file_stem()
                .map(|s| link_key(&s.to_string_lossy()))
                .unwrap_or_default();
            Ok(DocKey {
                id: r.get(0)?,
                root_id: r.get(1)?,
                rel_path,
                stem,
                title: link_key(&r.get::<_, String>(3)?),
            })
        })?;
        rows.filter_map(std::result::Result::ok).collect()
    };
    let by_path: HashMap<(i64, &str), i64> = docs
        .iter()
        .map(|d| ((d.root_id, d.rel_path.as_str()), d.id))
        .collect();
    let mut by_stem: HashMap<&str, Vec<&DocKey>> = HashMap::new();
    let mut by_title: HashMap<&str, Vec<&DocKey>> = HashMap::new();
    for d in &docs {
        by_stem.entry(d.stem.as_str()).or_default().push(d);
        by_title.entry(d.title.as_str()).or_default().push(d);
    }

    let unresolved: Vec<(i64, i64, String, String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT l.rowid, d.root_id, d.rel_path, l.target, l.link_type
             FROM links l JOIN docs d ON d.id = l.from_doc_id
             WHERE l.to_doc_id IS NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        rows.filter_map(std::result::Result::ok).collect()
    };

    let pick = |cands: Option<&Vec<&DocKey>>, root_id: i64| -> Option<i64> {
        let cands = cands?;
        cands
            .iter()
            .find(|d| d.root_id == root_id)
            .or_else(|| cands.first())
            .map(|d| d.id)
    };

    let mut remaining = 0;
    let mut upd = tx.prepare_cached("UPDATE links SET to_doc_id = ?1 WHERE rowid = ?2")?;
    for (rowid, root_id, from_rel, target, link_type) in unresolved {
        let resolved = if link_type == "md" {
            let joined = normalize_relative(&from_rel, &target);
            by_path.get(&(root_id, joined.as_str())).copied()
        } else {
            let raw = target.strip_suffix(".md").unwrap_or(&target);
            let key = link_key(raw);
            let stem_key = Path::new(raw)
                .file_name()
                .map(|s| link_key(&s.to_string_lossy()))
                .unwrap_or_default();
            by_path
                .get(&(root_id, format!("{raw}.md").as_str()))
                .copied()
                .or_else(|| pick(by_stem.get(stem_key.as_str()), root_id))
                .or_else(|| pick(by_title.get(key.as_str()), root_id))
        };
        match resolved {
            Some(id) => {
                upd.execute(params![id, rowid])?;
            }
            None => remaining += 1,
        }
    }
    Ok(remaining)
}

/// Normalise a wikilink target, file stem, or title for matching: lower-case,
/// `_` and spaces folded to `-`, so `[[user-profile]]` finds `user_profile.md`.
fn link_key(s: &str) -> String {
    s.trim().to_lowercase().replace([' ', '_'], "-")
}

/// Resolve `target` relative to the directory of `from_rel`, collapsing `..`.
fn normalize_relative(from_rel: &str, target: &str) -> String {
    let mut parts: Vec<String> = Path::new(from_rel)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s.to_owned()),
        }
    }
    parts.join("/")
}

/// Is a given doc id active? Used by tests and `list`.
///
/// # Errors
/// Returns an error if the query fails.
pub fn doc_active(store: &Store, id: i64) -> Result<Option<bool>> {
    Ok(store
        .conn()
        .query_row("SELECT active FROM docs WHERE id = ?1", params![id], |r| {
            r.get::<_, i64>(0).map(|v| v != 0)
        })
        .optional()?)
}
