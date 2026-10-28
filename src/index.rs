//! Indexer: walk roots, detect changes by mtime/size then `BLAKE3`, chunk, upsert.
//!
//! Rules:
//! - a file whose `(mtime, size)` match the stored row is skipped without
//!   reading, unless its mtime is within 2 s of when the stored hash was
//!   taken (git's racily-clean rule);
//! - a changed file is re-hashed; an unchanged hash only refreshes mtime;
//! - a new path whose hash matches a vanished path in the same root is a
//!   rename and keeps its `docs.id`; a soft-deleted doc with the same hash
//!   is restored onto its old id;
//! - vanished files are soft-deleted (`active = 0`) and their chunks removed,
//!   but only when the root was walked without errors;
//! - a root that is missing on disk is skipped, never treated as mass delete;
//! - an unreadable file is reported and skipped, never deleted;
//! - the whole run is one `IMMEDIATE` transaction, so concurrent runs queue
//!   on `busy_timeout` instead of failing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::markdown::{self, Parsed};
use crate::roots::{self, Root};
use crate::store::{PROJECT_ALIASES_KEY, Store};

/// A file modified within this window of the stored hash time is re-read
/// even if mtime and size match, because mtime granularity can hide an edit.
const RACY_WINDOW_NS: i64 = 2_000_000_000;

/// Identifier of the token counter used for `chunks.tokens`, stored in `meta`.
pub const TOKENIZER_ID: &str = "bytes36";

/// Bump whenever `markdown::parse` output changes shape (chunking, links,
/// tags, title rules) or the chunk id derivation changes. Each root records
/// the version it was parsed with; a mismatch forces every file under it to
/// be re-parsed on the next run.
pub const PARSER_VERSION: &str = "2";

#[derive(Debug, Clone, Default)]
pub struct IndexOptions {
    /// Ignore the mtime/size fast path and re-hash every file.
    pub full: bool,
    pub quiet: bool,
    /// Only index the root at this exact (tilde-expanded) path.
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
    /// Files or directories that could not be read; paths go to stderr.
    pub errors: usize,
    pub chunks: usize,
    pub links_unresolved: usize,
    pub elapsed_ms: u128,
}

impl std::fmt::Display for IndexReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} roots ({} missing), {} files: {} unchanged, {} added, {} updated, {} renamed, {} removed, {} errors; {} chunks written, {} links unresolved; {} ms",
            self.roots,
            self.roots_missing,
            self.scanned,
            self.unchanged,
            self.added,
            self.updated,
            self.renamed,
            self.removed,
            self.errors,
            self.chunks,
            self.links_unresolved,
            self.elapsed_ms
        )
    }
}

/// Re-index only the configured root that contains `path` (what `add` and
/// the capture hooks do after writing one file). Returns `false`, having
/// done nothing, when the file is outside every root.
///
/// # Errors
/// Returns an error if the database cannot be written.
pub fn reindex_root_containing(config: &Config, store: &mut Store, path: &Path) -> Result<bool> {
    let file = canonical_file(path);
    let root = roots::expand(&config.roots, &config.project_aliases())
        .into_iter()
        .find(|r| file.starts_with(canonical_dir(&r.path)));
    let Some(root) = root else {
        return Ok(false);
    };
    run(
        config,
        store,
        &IndexOptions {
            full: false,
            quiet: true,
            only_root: Some(root.path),
        },
    )?;
    Ok(true)
}

fn canonical_dir(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// Canonicalize the directory part only, so a file that does not exist yet
/// still compares equal to a root reached through a symlink.
fn canonical_file(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) => canonical_dir(dir).join(name),
        _ => path.to_path_buf(),
    }
}

/// Run an incremental index over all configured roots.
///
/// # Errors
/// Returns an error if `only_root` matches no configured root, or if the
/// database cannot be written.
pub fn run(config: &Config, store: &mut Store, opts: &IndexOptions) -> Result<IndexReport> {
    let started = Instant::now();
    let mut report = IndexReport::default();

    let aliases = config.project_aliases();
    let expanded = roots::expand(&config.roots, &aliases);
    let selected: Vec<&Root> = expanded
        .iter()
        .filter(|r| opts.only_root.as_ref().is_none_or(|p| &r.path == p))
        .collect();
    if selected.is_empty()
        && let Some(p) = &opts.only_root
    {
        return Err(Error::Other(anyhow::anyhow!(
            "{} is not a configured root; configured: {}",
            p.display(),
            expanded
                .iter()
                .map(|r| r.path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    report.roots = selected.len();

    let tx = store
        .conn_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let known_roots = known_root_paths(&tx)?;
    report.roots_missing = known_roots
        .iter()
        .filter(|p| !expanded.iter().any(|r| &r.path == *p))
        .count();

    for root in selected {
        let (root_id, reparse) = upsert_root(&tx, root)?;
        let clean = index_root(&tx, root_id, root, opts, reparse, &mut report)?;
        if clean {
            tx.execute(
                "UPDATE roots SET parser_version = ?1 WHERE id = ?2",
                params![PARSER_VERSION, root_id],
            )?;
        }
    }

    report.links_unresolved = resolve_links(&tx)?;
    let now = jiff::Timestamp::now().to_string();
    let aliases = serde_json::to_string(&aliases).map_err(|e| Error::Other(e.into()))?;
    for (key, value) in [
        ("last_index_at", now.as_str()),
        ("tokenizer", TOKENIZER_ID),
        (PROJECT_ALIASES_KEY, aliases.as_str()),
    ] {
        tx.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
    }
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

/// Returns the root id and whether its files must be re-parsed.
fn upsert_root(tx: &Transaction<'_>, root: &Root) -> Result<(i64, bool)> {
    let path = root.path.to_string_lossy();
    let demote = if root.demote.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&root.demote).map_err(|e| Error::Other(e.into()))?)
    };
    tx.execute(
        "INSERT INTO roots(path, kind, weight, project, demote, demote_factor)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(path) DO UPDATE SET kind = excluded.kind, weight = excluded.weight,
                                         project = excluded.project, demote = excluded.demote,
                                         demote_factor = excluded.demote_factor",
        params![
            path,
            root.kind.as_str(),
            root.weight,
            root.project,
            demote,
            root.demote_factor
        ],
    )?;
    let (id, parser_version): (i64, Option<String>) = tx.query_row(
        "SELECT id, parser_version FROM roots WHERE path = ?1",
        params![path],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok((id, parser_version.as_deref() != Some(PARSER_VERSION)))
}

#[derive(Debug, Clone)]
struct DocRow {
    id: i64,
    blake3: String,
    mtime_ns: i64,
    size: i64,
    indexed_at: i64,
    active: bool,
}

struct FileStat {
    rel_path: String,
    abs_path: PathBuf,
    mtime_ns: i64,
    size: i64,
}

/// Index one root. Returns `true` if the walk completed without errors, in
/// which case vanished files were soft-deleted.
fn index_root(
    tx: &Transaction<'_>,
    root_id: i64,
    root: &Root,
    opts: &IndexOptions,
    reparse: bool,
    report: &mut IndexReport,
) -> Result<bool> {
    let existing = load_docs(tx, root_id)?;
    let (files, walk_errors) = walk(&root.path);
    report.scanned += files.len();
    report.errors += walk_errors;

    let mut seen: HashSet<String> = HashSet::with_capacity(files.len());
    let mut pending_new: Vec<(FileStat, String, String)> = Vec::new();

    for file in files {
        seen.insert(file.rel_path.clone());
        let row = existing.get(&file.rel_path).filter(|r| r.active);
        if let Some(row) = row {
            let racy = file.mtime_ns + RACY_WINDOW_NS >= row.indexed_at;
            let fast_ok = !opts.full
                && !reparse
                && !racy
                && row.mtime_ns == file.mtime_ns
                && row.size == file.size;
            if fast_ok {
                report.unchanged += 1;
                continue;
            }
        }
        let Some(text) = read_file(&file.abs_path, report) else {
            continue;
        };
        let hash = blake3::hash(text.as_bytes()).to_hex().to_string();
        match row {
            Some(row) if hash == row.blake3 && !reparse => {
                tx.execute(
                    "UPDATE docs SET mtime_ns = ?1, size = ?2, indexed_at = ?3 WHERE id = ?4",
                    params![file.mtime_ns, file.size, now_ns(), row.id],
                )?;
                report.unchanged += 1;
            }
            Some(row) => {
                replace_doc(tx, row.id, &file, &hash, &text, report)?;
                report.updated += 1;
            }
            None => pending_new.push((file, hash, text)),
        }
    }

    // Active rows whose path was not seen: rename candidates first, then
    // deletions. Keyed by hash with a list, since files may share content.
    let mut vanished: HashMap<String, Vec<(String, i64)>> = HashMap::new();
    for (path, row) in &existing {
        if row.active && !seen.contains(path) {
            vanished
                .entry(row.blake3.clone())
                .or_default()
                .push((path.clone(), row.id));
        }
    }
    let mut inactive_by_hash: HashMap<String, Vec<i64>> = HashMap::new();
    for row in existing.values().filter(|r| !r.active) {
        inactive_by_hash
            .entry(row.blake3.clone())
            .or_default()
            .push(row.id);
    }

    for (file, hash, text) in pending_new {
        if let Some((_, id)) = vanished.get_mut(&hash).and_then(Vec::pop) {
            move_doc_to_path(tx, id, &file.rel_path, &existing)?;
            replace_doc(tx, id, &file, &hash, &text, report)?;
            report.renamed += 1;
        } else if let Some(row) = existing.get(&file.rel_path) {
            if let Some(ids) = inactive_by_hash.get_mut(&row.blake3) {
                ids.retain(|&id| id != row.id);
            }
            tx.execute("UPDATE docs SET active = 1 WHERE id = ?1", params![row.id])?;
            replace_doc(tx, row.id, &file, &hash, &text, report)?;
            report.added += 1;
        } else if let Some(id) = inactive_by_hash.get_mut(&hash).and_then(Vec::pop) {
            move_doc_to_path(tx, id, &file.rel_path, &existing)?;
            replace_doc(tx, id, &file, &hash, &text, report)?;
            report.added += 1;
        } else {
            let id = insert_doc(tx, root_id, &file, &hash)?;
            replace_doc(tx, id, &file, &hash, &text, report)?;
            report.added += 1;
        }
    }

    if walk_errors > 0 {
        eprintln!(
            "warning: {} had {walk_errors} unreadable entries; skipping deletions for this root",
            root.path.display()
        );
        return Ok(false);
    }
    for (_, id) in vanished.into_values().flatten() {
        soft_delete(tx, id)?;
        report.removed += 1;
    }
    Ok(true)
}

fn soft_delete(tx: &Transaction<'_>, id: i64) -> Result<()> {
    clear_doc_children(tx, id)?;
    tx.execute(
        "UPDATE links SET to_doc_id = NULL WHERE to_doc_id = ?1",
        params![id],
    )?;
    tx.execute("UPDATE docs SET active = 0 WHERE id = ?1", params![id])?;
    Ok(())
}

/// Point doc `id` at `rel_path`, first removing any soft-deleted row that
/// already holds that path so `UNIQUE(root_id, rel_path)` is satisfied.
fn move_doc_to_path(
    tx: &Transaction<'_>,
    id: i64,
    rel_path: &str,
    existing: &HashMap<String, DocRow>,
) -> Result<()> {
    if let Some(stale) = existing.get(rel_path).filter(|r| !r.active && r.id != id) {
        clear_doc_children(tx, stale.id)?;
        tx.execute(
            "UPDATE links SET to_doc_id = NULL WHERE to_doc_id = ?1",
            params![stale.id],
        )?;
        tx.execute("DELETE FROM docs WHERE id = ?1", params![stale.id])?;
    }
    tx.execute(
        "UPDATE docs SET rel_path = ?1, active = 1 WHERE id = ?2",
        params![rel_path, id],
    )?;
    Ok(())
}

/// Read a file as text, lossily decoding invalid UTF-8. An I/O failure is
/// reported with its path and counted; the stored row is left as it was.
fn read_file(path: &Path, report: &mut IndexReport) -> Option<String> {
    match std::fs::read(path) {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(e) => {
            eprintln!("warning: cannot read {}: {e}", path.display());
            report.errors += 1;
            None
        }
    }
}

fn now_ns() -> i64 {
    system_time_ns(SystemTime::now())
}

pub(crate) fn system_time_ns(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

fn load_docs(tx: &Transaction<'_>, root_id: i64) -> Result<HashMap<String, DocRow>> {
    let mut stmt = tx.prepare(
        "SELECT rel_path, id, blake3, mtime_ns, size, indexed_at, active FROM docs WHERE root_id = ?1",
    )?;
    let rows = stmt.query_map(params![root_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            DocRow {
                id: r.get(1)?,
                blake3: r.get(2)?,
                mtime_ns: r.get(3)?,
                size: r.get(4)?,
                indexed_at: r.get(5)?,
                active: r.get::<_, i64>(6)? != 0,
            },
        ))
    })?;
    Ok(rows.filter_map(std::result::Result::ok).collect())
}

/// Walk a root for `*.md` files. Returns the files and the number of entries
/// that could not be read. Git ignore files are deliberately not honoured
/// (roots often live inside ignored directories); hidden entries are skipped
/// and a `.kvaultignore` file in gitignore syntax is honoured.
fn walk(root: &Path) -> (Vec<FileStat>, usize) {
    let mut out = Vec::new();
    let mut errors = 0;
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
                errors += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            continue;
        }
        let stat = entry.metadata().map_err(|e| e.to_string()).and_then(|m| {
            m.modified()
                .map(|t| (system_time_ns(t), m.len()))
                .map_err(|e| e.to_string())
        });
        let (mtime_ns, len) = match stat {
            Ok(v) => v,
            Err(e) => {
                eprintln!("warning: cannot stat {}: {e}", path.display());
                errors += 1;
                continue;
            }
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        out.push(FileStat {
            rel_path: rel,
            abs_path: path.to_path_buf(),
            mtime_ns,
            size: i64::try_from(len).unwrap_or(i64::MAX),
        });
    }
    (out, errors)
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
            now_ns(),
            id
        ],
    )?;

    // A frontmatter description is a one-line summary; indexing it with the
    // title makes single-fact files findable by their summary.
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
        let chunk_hash = chunk_hash(&file.abs_path.to_string_lossy(), &c.heading_path, body);
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

/// Chunk identity: the file's absolute path and heading path go into the
/// hash so identical sections in different notes (templates, boilerplate)
/// get distinct ids. `search` prints a prefix of this; `get` resolves it.
fn chunk_hash(abs_path: &str, heading_path: &str, body: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(abs_path.as_bytes());
    h.update(b"\0");
    h.update(heading_path.as_bytes());
    h.update(b"\0");
    h.update(body.as_bytes());
    h.finalize().to_hex().to_string()
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

/// `(created, updated)` as `RFC 3339` UTC strings so they sort and compare
/// correctly. Frontmatter dates are normalised; unparseable ones fall back
/// to filesystem times.
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
        .as_deref()
        .and_then(normalize_date)
        .or_else(|| meta.as_ref().and_then(|m| fs_time(m.modified())));
    let created = parsed
        .frontmatter
        .created
        .as_deref()
        .and_then(normalize_date)
        .or_else(|| meta.as_ref().and_then(|m| fs_time(m.created())))
        .or_else(|| updated.clone());
    (created, updated)
}

/// Accepts `RFC 3339`, a zoned datetime, a civil datetime, or a bare date
/// (taken as midnight UTC). Returns an `RFC 3339` UTC string.
pub(crate) fn normalize_date(s: &str) -> Option<String> {
    let s = s.trim();
    if let Ok(ts) = s.parse::<jiff::Timestamp>() {
        return Some(ts.to_string());
    }
    if let Ok(z) = s.parse::<jiff::Zoned>() {
        return Some(z.timestamp().to_string());
    }
    if let Ok(dt) = s.parse::<jiff::civil::DateTime>() {
        return dt
            .to_zoned(jiff::tz::TimeZone::UTC)
            .ok()
            .map(|z| z.timestamp().to_string());
    }
    if let Ok(d) = s.parse::<jiff::civil::Date>() {
        return d
            .to_zoned(jiff::tz::TimeZone::UTC)
            .ok()
            .map(|z| z.timestamp().to_string());
    }
    None
}

/// Resolve every link's `to_doc_id` from scratch so incremental runs produce
/// the same graph as a full rebuild. Returns how many remain unresolved.
/// Order: relative path in the same root, file stem in the same root, title in
/// the same root, then the same three across all roots.
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

    let links: Vec<(i64, i64, String, String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT l.rowid, d.root_id, d.rel_path, l.target, l.link_type
             FROM links l JOIN docs d ON d.id = l.from_doc_id",
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
    for (rowid, root_id, from_rel, target, link_type) in links {
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
        upd.execute(params![resolved, rowid])?;
        if resolved.is_none() {
            remaining += 1;
        }
    }
    Ok(remaining)
}

/// Matching key for wikilink targets, file stems, and titles: lower-case with
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

/// Whether doc `id` is active, or `None` if it does not exist.
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn dates_normalise_to_rfc3339_utc() {
        assert_eq!(
            normalize_date("2026-10-27").unwrap(),
            "2026-10-27T00:00:00Z"
        );
        assert_eq!(
            normalize_date("2026-10-27T15:27:00-05:00").unwrap(),
            "2026-10-27T20:27:00Z"
        );
        assert_eq!(
            normalize_date("2026-10-27 09:30").unwrap(),
            "2026-10-27T09:30:00Z"
        );
        assert_eq!(normalize_date("yesterday"), None);
    }

    #[test]
    fn relative_paths_collapse() {
        assert_eq!(normalize_relative("a/b/c.md", "../x.md"), "a/x.md");
        assert_eq!(normalize_relative("c.md", "./d.md"), "d.md");
        assert_eq!(normalize_relative("a/c.md", "../../../d.md"), "d.md");
    }
}
