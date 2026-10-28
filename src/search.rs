//! Search: query parsing, retrieval streams, reciprocal rank fusion, explain.
//!
//! The agent writes the query; there is no local expander. Bare words are
//! OR-joined and prefix-matched, `"quoted"` words are a phrase, `-word`
//! excludes, `tag:x` filters, and `lex: ...` passes raw `FTS5` syntax through.
//!
//! Streams run independently and each may return nothing:
//!
//! | stream | source | weight |
//! |---|---|---|
//! | `strict` | `FTS5` bm25, every positive unit required | 2.0 |
//! | `fts` | `FTS5` bm25, any positive unit | 1.0 |
//! | `title` | every positive unit appears in `docs.title` | 1.0 |
//! | `tags` | query words that equal a tag | 1.0 |
//! | `browse` | filter-only queries (`tag:x` alone): newest documents first | 1.0 |
//!
//! Exclusions (`-word`, `-tag:x`, `NOT` in a `lex:` query) apply to every
//! stream: a chunk matching an excluded unit is never a candidate, and an
//! excluded tag removes the document.
//!
//! Fusion is reciprocal rank fusion (`k = 60`), then a bounded authority
//! multiplier from the root weight, `pinned` frontmatter, and the root's
//! demotion globs (`archive/` by default). Results are one
//! chunk per document unless `per_chunk` is set.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{ToSql, params_from_iter};
use serde::Serialize;

use crate::error::{Error, Result};
use crate::get::{CHUNK_REF_COLUMNS, ChunkRef, DocRef, chunk_ref_from_row};
use crate::store::Store;

/// Reciprocal rank fusion constant.
pub const RRF_K: f64 = 60.0;
/// Rows fetched from each stream before fusion.
const CANDIDATES: usize = 200;
/// `bm25()` column weights: title, `heading_path`, body, path.
const BM25_WEIGHTS: &str = "4.0, 2.5, 1.0, 1.5";
/// Authority multiplier bounds. Under RRF with `k = 60`, rank 1 and rank 10
/// differ by about 15%, so a multiplier band wider than that lets a root's
/// weight outvote the text ranking entirely (measured on the bench: a 1.5x
/// band pushed a bm25 #1 hit to position 7). The band is kept to a few ranks.
const AUTHORITY_MIN: f64 = 0.85;
const AUTHORITY_MAX: f64 = 1.15;
/// How much of `roots.weight - 1` reaches the multiplier.
const AUTHORITY_DAMPING: f64 = 0.25;
const PINNED_BONUS: f64 = 1.05;
/// Words at least this long are prefix-matched (`word*`).
const PREFIX_MIN_LEN: usize = 3;
/// Sibling chunk ids listed per document hit.
const MAX_SIBLINGS: usize = 5;
/// Hex digits of the chunk hash printed as its id. Eight digits keep the
/// chance of two chunks sharing an id negligible for corpora of 100k chunks.
const SHORT_ID_LEN: usize = 8;

/// Common English words dropped from bare queries. Kept when quoted, when
/// part of a phrase, or when nothing else remains.
const STOPWORDS: &[&str] = &[
    "a", "about", "an", "and", "are", "as", "at", "be", "but", "by", "can", "did", "do", "does",
    "for", "from", "had", "has", "have", "he", "her", "his", "how", "i", "if", "in", "into", "is",
    "it", "its", "just", "me", "my", "no", "nor", "not", "of", "on", "or", "our", "she", "so",
    "some", "than", "that", "the", "their", "them", "then", "there", "these", "they", "this", "to",
    "too", "up", "us", "was", "we", "were", "what", "when", "where", "which", "who", "why", "will",
    "with", "would", "you", "your",
];

/// Filters applied to every stream.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub limit: usize,
    pub project: Option<String>,
    pub root: Option<PathBuf>,
    /// Root kind or frontmatter kind.
    pub kind: Option<String>,
    /// Every listed tag must be present on the document.
    pub tags: Vec<String>,
    /// Glob over the relative path or the absolute path.
    pub path: Option<String>,
    /// `30d`, `2w`, `12h`, or a date.
    pub since: Option<String>,
    /// Return every matching chunk instead of the best chunk per document.
    pub per_chunk: bool,
}

/// One positive or negative search unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Unit {
    /// A single word; `prefix` means `word*`.
    Term { word: String, prefix: bool },
    /// Consecutive words, from quotes or a compound token like `src/lib.rs`.
    Phrase(Vec<String>),
}

impl Unit {
    fn fts(&self) -> String {
        match self {
            Self::Term { word, prefix: true } => format!("\"{word}\"*"),
            Self::Term {
                word,
                prefix: false,
            } => format!("\"{word}\""),
            Self::Phrase(words) => format!("\"{}\"", words.join(" ")),
        }
    }

    /// The unit as a case-insensitive substring to look for in text.
    fn needle(&self) -> String {
        match self {
            Self::Term { word, .. } => word.clone(),
            Self::Phrase(words) => words.join(" "),
        }
    }
}

/// A parsed query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Query {
    pub raw: String,
    pub positive: Vec<Unit>,
    pub negative: Vec<Unit>,
    /// Inline `tag:x` filters.
    pub tags: Vec<String>,
    /// Inline `-tag:x` filters.
    pub excluded_tags: Vec<String>,
    /// Raw `FTS5` expression from a `lex:` query, passed through untouched.
    pub lexical: Option<String>,
}

impl Query {
    /// Parse the agent-authored query string.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let mut q = Self {
            raw: raw.to_owned(),
            ..Self::default()
        };
        let trimmed = raw.trim();
        if let Some(rest) = strip_prefix_ci(trimmed, "lex:") {
            q.lexical = Some(rest.trim().to_owned());
            // Units for highlighting, the title/tag streams, and exclusion.
            (q.positive, q.negative) = lex_units(rest);
            return q;
        }

        let mut dropped: Vec<Unit> = Vec::new();
        for tok in scan(trimmed) {
            if !tok.quoted
                && let Some(tag) = strip_prefix_ci(&tok.text, "tag:")
            {
                let tag = tag.trim().to_lowercase();
                if !tag.is_empty() {
                    if tok.negated {
                        q.excluded_tags.push(tag);
                    } else {
                        q.tags.push(tag);
                    }
                }
                continue;
            }
            let words = alnum_runs(&tok.text);
            let unit = match words.len() {
                0 => continue,
                1 => {
                    let word = words.into_iter().next().unwrap_or_default();
                    if !tok.quoted && !tok.negated && is_stopword(&word) {
                        dropped.push(Unit::Term {
                            word,
                            prefix: false,
                        });
                        continue;
                    }
                    Unit::Term {
                        prefix: !tok.quoted && word.chars().count() >= PREFIX_MIN_LEN,
                        word,
                    }
                }
                _ => Unit::Phrase(words),
            };
            if tok.negated {
                q.negative.push(unit);
            } else {
                q.positive.push(unit);
            }
        }
        if q.positive.is_empty() {
            q.positive = dropped;
        }
        q
    }

    /// True when nothing can be searched for or filtered on.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.filter_only() && self.tags.is_empty() && self.excluded_tags.is_empty()
    }

    /// True when the query has filters but no text to rank by.
    #[must_use]
    pub fn filter_only(&self) -> bool {
        self.lexical.as_deref().is_none_or(str::is_empty) && self.positive.is_empty()
    }

    /// Lowercase words of every positive unit, for highlighting.
    #[must_use]
    pub fn words(&self) -> Vec<String> {
        let mut out = Vec::new();
        for u in &self.positive {
            match u {
                Unit::Term { word, .. } => out.push(word.clone()),
                Unit::Phrase(ws) => out.extend(ws.iter().cloned()),
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Substrings to look for when picking a snippet line.
    #[must_use]
    pub fn needles(&self) -> Vec<String> {
        let mut seen: HashSet<String> = HashSet::new();
        self.positive
            .iter()
            .map(Unit::needle)
            .chain(self.words())
            .filter(|n| !n.is_empty() && seen.insert(n.clone()))
            .collect()
    }

    /// `FTS5` expression matching any negative unit, for excluding chunks
    /// from the streams that do not run through `FTS5` themselves.
    #[must_use]
    pub fn fts_exclude(&self) -> Option<String> {
        if self.negative.is_empty() {
            return None;
        }
        let neg: Vec<String> = self.negative.iter().map(Unit::fts).collect();
        Some(format!("({})", neg.join(" OR ")))
    }

    /// `FTS5` expression matching any positive unit.
    #[must_use]
    pub fn fts_loose(&self) -> Option<String> {
        if let Some(lex) = &self.lexical {
            return Some(lex.clone());
        }
        self.join_fts(" OR ")
    }

    /// `FTS5` expression requiring every positive unit; `None` unless there
    /// are at least two, since it would equal the loose query.
    #[must_use]
    pub fn fts_strict(&self) -> Option<String> {
        if self.lexical.is_some() || self.positive.len() < 2 {
            return None;
        }
        self.join_fts(" AND ")
    }

    fn join_fts(&self, op: &str) -> Option<String> {
        if self.positive.is_empty() {
            return None;
        }
        let pos: Vec<String> = self.positive.iter().map(Unit::fts).collect();
        let mut expr = format!("({})", pos.join(op));
        if !self.negative.is_empty() {
            let neg: Vec<String> = self.negative.iter().map(Unit::fts).collect();
            expr = format!("{expr} NOT ({})", neg.join(" OR "));
        }
        Some(expr)
    }
}

struct RawToken {
    text: String,
    quoted: bool,
    negated: bool,
}

/// Split on whitespace, honouring double quotes and a leading `-`.
fn scan(s: &str) -> Vec<RawToken> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut negated = false;
        if c == '-' {
            chars.next();
            match chars.peek() {
                Some(n) if !n.is_whitespace() => negated = true,
                _ => continue,
            }
        }
        let mut quoted = false;
        let mut text = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            quoted = true;
            for n in chars.by_ref() {
                if n == '"' {
                    break;
                }
                text.push(n);
            }
        } else {
            while let Some(&n) = chars.peek() {
                if n.is_whitespace() {
                    break;
                }
                text.push(n);
                chars.next();
            }
        }
        out.push(RawToken {
            text,
            quoted,
            negated,
        });
    }
    out
}

/// Lowercase alphanumeric runs, the same units the `unicode61` tokenizer
/// produces, so `DEC-0054` and `src/lib.rs` become phrases of their parts.
fn alnum_runs(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|p| !p.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Positive and negative units of a raw `FTS5` expression: operators, column
/// filters and `NEAR` distances are syntax, not words, and the unit after a
/// `NOT` is an exclusion. Approximate on purpose; the expression itself is
/// what `FTS5` evaluates.
fn lex_units(expr: &str) -> (Vec<Unit>, Vec<Unit>) {
    let mut positive = Vec::new();
    let mut negative = Vec::new();
    let mut negate_next = false;
    let mut in_column_set = false;
    for tok in scan(expr) {
        if !tok.quoted {
            match tok.text.trim_matches(|c| c == '(' || c == ')') {
                "AND" | "OR" => continue,
                "NOT" => {
                    negate_next = true;
                    continue;
                }
                _ => {}
            }
        }
        let text = if tok.quoted {
            tok.text.as_str()
        } else {
            // `col:word`, `{col col}:word`, `NEAR(a b, 3)`, `^first`.
            let t = tok.text.as_str();
            if t.starts_with('{') {
                in_column_set = true;
            }
            let t = if in_column_set {
                let Some((_, w)) = t.split_once("}:") else {
                    continue;
                };
                in_column_set = false;
                w
            } else {
                t.rsplit_once(':').map_or(t, |(_, w)| w)
            };
            t.strip_prefix("NEAR(").unwrap_or(t)
        };
        let prefix = !tok.quoted && text.ends_with('*');
        let words = alnum_runs(text);
        let unit = match words.len() {
            0 => continue,
            1 if words[0].chars().all(|c| c.is_ascii_digit()) => continue,
            1 => Unit::Term {
                word: words.into_iter().next().unwrap_or_default(),
                prefix,
            },
            _ => Unit::Phrase(words),
        };
        if std::mem::take(&mut negate_next) {
            negative.push(unit);
        } else {
            positive.push(unit);
        }
    }
    (positive, negative)
}

fn is_stopword(w: &str) -> bool {
    STOPWORDS.contains(&w)
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Where a hit ranked in one stream.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StreamRank {
    pub stream: &'static str,
    /// 1-based rank within the stream.
    pub rank: usize,
    /// Stream-native score (`bm25` for the FTS streams, match count otherwise).
    pub raw: f64,
    /// Contribution to the fused score: `weight / (k + rank)`.
    pub contribution: f64,
}

/// Why a hit scored what it did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Explain {
    pub streams: Vec<StreamRank>,
    pub rrf: f64,
    pub authority: f64,
    /// The demotion factor folded into `authority`, when a root's `demote`
    /// glob matched this document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub demoted: Option<f64>,
}

/// A ranked hit at chunk granularity, with enough document context to print.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hit {
    #[serde(flatten)]
    pub chunk: ChunkRef,
    pub root_kind: String,
    /// `[project/]<root dir name>/<rel_path>`, what the CLI prints.
    pub display_path: String,
    pub tokens: i64,
    pub updated: Option<String>,
    pub score: f64,
    /// Other chunks of the same document that also matched, best first.
    pub siblings: Vec<String>,
    pub explain: Explain,
}

impl Hit {
    /// The document this chunk belongs to.
    #[must_use]
    pub fn doc(&self) -> &DocRef {
        &self.chunk.doc
    }
}

/// Everything a search produced.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub query: Query,
    pub hits: Vec<Hit>,
    /// Per-stream wall time, in the order the streams ran.
    pub timings: Vec<(String, Duration)>,
    /// Distinct chunks seen across all streams before cutting to `limit`.
    pub candidates: usize,
}

/// `[project/]<root dir name>/<rel_path>`.
#[must_use]
pub fn display_path(root_path: &str, project: Option<&str>, rel_path: &str) -> String {
    let base = Path::new(root_path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match project {
        Some(p) => format!("{p}/{base}/{rel_path}"),
        None => format!("{base}/{rel_path}"),
    }
}

/// Short chunk id: a prefix of its `BLAKE3`.
#[must_use]
pub fn short_id(blake3: &str) -> String {
    blake3.get(..SHORT_ID_LEN).unwrap_or(blake3).to_owned()
}

/// SQL fragment and parameters restricting `docs d` / `roots r` rows.
struct Filter {
    sql: String,
    params: Vec<Box<dyn ToSql>>,
}

impl Filter {
    fn build(store: &Store, opts: &SearchOptions, query: &Query) -> Result<Self> {
        let mut sql = String::new();
        let mut params: Vec<Box<dyn ToSql>> = Vec::new();
        if let Some(p) = &opts.project {
            sql.push_str(" AND r.project = ?");
            params.push(Box::new(store.canonical_project(p)?));
        }
        if let Some(root) = &opts.root {
            // Roots are stored without a trailing separator.
            let normalized: PathBuf = root.components().collect();
            sql.push_str(" AND r.path = ?");
            params.push(Box::new(normalized.to_string_lossy().into_owned()));
        }
        if let Some(k) = &opts.kind {
            sql.push_str(" AND (r.kind = ? OR d.kind = ?)");
            params.push(Box::new(k.clone()));
            params.push(Box::new(k.clone()));
        }
        for tag in opts.tags.iter().chain(&query.tags) {
            sql.push_str(" AND d.id IN (SELECT doc_id FROM tags WHERE tag = ?)");
            params.push(Box::new(tag.to_lowercase()));
        }
        for tag in &query.excluded_tags {
            sql.push_str(" AND d.id NOT IN (SELECT doc_id FROM tags WHERE tag = ?)");
            params.push(Box::new(tag.clone()));
        }
        if let Some(glob) = &opts.path {
            sql.push_str(" AND (d.rel_path GLOB ? OR (r.path || '/' || d.rel_path) GLOB ?)");
            params.push(Box::new(glob.clone()));
            params.push(Box::new(glob.clone()));
        }
        if let Some(since) = &opts.since {
            let cutoff = parse_since(since)
                .ok_or_else(|| Error::Other(anyhow::anyhow!("cannot parse --since {since:?}")))?;
            sql.push_str(" AND d.updated >= ?");
            params.push(Box::new(cutoff));
        }
        Ok(Self { sql, params })
    }
}

/// `30d`, `2w`, `12h`, `3m` (months of 30 days), or any date form the
/// indexer accepts. Returns an `RFC 3339` UTC cutoff.
#[must_use]
pub fn parse_since(s: &str) -> Option<String> {
    let s = s.trim();
    if let Some((num, unit)) = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| s.split_at(i))
        && !num.is_empty()
    {
        let n: i64 = num.parse().ok()?;
        let per_unit = match unit {
            "h" => 1,
            "d" => 24,
            "w" => 24 * 7,
            "m" => 24 * 30,
            _ => return crate::index::normalize_date(s),
        };
        let hours = n.checked_mul(per_unit)?;
        return jiff::Timestamp::now()
            .checked_sub(jiff::SignedDuration::try_from_hours(hours)?)
            .ok()
            .map(|t| t.to_string());
    }
    crate::index::normalize_date(s)
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    chunk_id: i64,
    doc_id: i64,
    raw: f64,
}

struct Stream {
    name: &'static str,
    weight: f64,
    ranked: Vec<Candidate>,
    elapsed: Duration,
}

/// Run a search.
///
/// # Errors
/// Returns an error for an empty query, an unparseable `--since`, invalid
/// `lex:` syntax, or a database failure.
pub fn run(store: &Store, query: &Query, opts: &SearchOptions) -> Result<SearchResult> {
    if query.is_empty() {
        return Err(Error::Other(anyhow::anyhow!("empty query")));
    }
    let filter = Filter::build(store, opts, query)?;
    let excluded = excluded_chunks(store, query)?;
    let mut streams = Vec::new();

    if query.filter_only() {
        streams.push(timed("browse", 1.0, || {
            browse_stream(store, &filter, &excluded)
        })?);
    }
    // The stream closest to the literal query carries the 2.0 weight: the
    // all-terms form when there is one, else the only FTS stream.
    let strict = query.fts_strict();
    let fts_weight = if strict.is_some() { 1.0 } else { 2.0 };
    if let Some(expr) = strict {
        streams.push(timed("strict", 2.0, || fts_stream(store, &expr, &filter))?);
    }
    if let Some(expr) = query.fts_loose() {
        streams.push(timed("fts", fts_weight, || {
            fts_stream(store, &expr, &filter)
        })?);
    }
    // Document-level streams attach to the document's best FTS chunk so the
    // fused list stays at chunk granularity.
    let best_chunk: HashMap<i64, i64> =
        streams
            .iter()
            .flat_map(|s| s.ranked.iter())
            .fold(HashMap::new(), |mut m, c| {
                m.entry(c.doc_id).or_insert(c.chunk_id);
                m
            });
    streams.push(timed("title", 1.0, || {
        title_stream(store, query, &filter, &best_chunk, &excluded)
    })?);
    streams.push(timed("tags", 1.0, || {
        tags_stream(store, query, &filter, &best_chunk, &excluded)
    })?);

    let mut fused = fuse(&streams);
    let candidates = fused.len();
    fused.sort_by(|a, b| {
        b.rrf
            .partial_cmp(&a.rrf)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });
    let limit = opts.limit.max(1);
    if !opts.per_chunk {
        fused = collapse(fused);
    }
    // Authority can move a candidate by at most the band ratio, so anything
    // below the limit-th score scaled by that ratio cannot reach the top.
    if let Some(cut) = fused
        .get(limit - 1)
        .map(|f| f.rrf * AUTHORITY_MIN / AUTHORITY_MAX)
    {
        fused.retain(|f| f.rrf >= cut);
    }
    let mut hits = load_hits(store, &fused)?;
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.chunk.chunk_id.cmp(&b.chunk.chunk_id))
    });
    hits.truncate(limit);

    Ok(SearchResult {
        query: query.clone(),
        hits,
        timings: streams
            .iter()
            .map(|s| (s.name.to_owned(), s.elapsed))
            .collect(),
        candidates,
    })
}

fn timed(
    name: &'static str,
    weight: f64,
    f: impl FnOnce() -> Result<Vec<Candidate>>,
) -> Result<Stream> {
    let start = Instant::now();
    let ranked = f()?;
    Ok(Stream {
        name,
        weight,
        ranked,
        elapsed: start.elapsed(),
    })
}

/// Map an `FTS5` failure to a readable error; `FTS5` reports bad syntax when
/// the statement is stepped, so this wraps the row collection too.
fn fts_error(e: rusqlite::Error) -> Error {
    match e {
        rusqlite::Error::SqliteFailure(_, Some(msg)) if msg.contains("fts5") => {
            Error::Other(anyhow::anyhow!("bad query syntax: {msg}"))
        }
        other => Error::Sqlite(other),
    }
}

/// Chunks matching a negative unit; the FTS streams exclude them through the
/// expression itself, the other streams through this set.
fn excluded_chunks(store: &Store, query: &Query) -> Result<HashSet<i64>> {
    let Some(expr) = query.fts_exclude() else {
        return Ok(HashSet::new());
    };
    let mut stmt = store
        .conn()
        .prepare_cached("SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ?1")?;
    let rows = stmt
        .query_map([expr], |r| r.get::<_, i64>(0))
        .map_err(fts_error)?;
    rows.collect::<std::result::Result<HashSet<_>, _>>()
        .map_err(fts_error)
}

fn fts_stream(store: &Store, expr: &str, filter: &Filter) -> Result<Vec<Candidate>> {
    let sql = format!(
        "SELECT chunks_fts.rowid, c.doc_id, bm25(chunks_fts, {BM25_WEIGHTS}) AS s
         FROM chunks_fts
         JOIN chunks c ON c.id = chunks_fts.rowid
         JOIN docs d ON d.id = c.doc_id
         JOIN roots r ON r.id = d.root_id
         WHERE chunks_fts MATCH ? AND d.active = 1{}
         ORDER BY s, chunks_fts.rowid
         LIMIT {CANDIDATES}",
        filter.sql
    );
    let mut stmt = store.conn().prepare_cached(&sql)?;
    let params: Vec<&dyn ToSql> = std::iter::once(&expr as &dyn ToSql)
        .chain(filter.params.iter().map(AsRef::as_ref))
        .collect();
    let rows = stmt
        .query_map(params_from_iter(params), |r| {
            Ok(Candidate {
                chunk_id: r.get(0)?,
                doc_id: r.get(1)?,
                raw: r.get(2)?,
            })
        })
        .map_err(fts_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(fts_error)
}

/// Lowercase with every non-alphanumeric run collapsed to one space, so
/// `kvault-rebuild-decision` and `Kvault: rebuild decision` compare equal.
fn normalize_title(s: &str) -> String {
    alnum_runs(s).join(" ")
}

/// The title stream is a precision stream: every positive unit (ignoring
/// words shorter than `PREFIX_MIN_LEN`, which match too much) must appear
/// in the normalized title. Shorter titles rank first because they are
/// more specific. Titles are compared in Rust rather than SQL so the
/// normalisation matches the query's, and 5k titles is a trivial scan.
fn title_stream(
    store: &Store,
    query: &Query,
    filter: &Filter,
    best_chunk: &HashMap<i64, i64>,
    excluded: &HashSet<i64>,
) -> Result<Vec<Candidate>> {
    let needles: Vec<String> = query
        .positive
        .iter()
        .map(Unit::needle)
        .filter(|n| n.chars().count() >= PREFIX_MIN_LEN)
        .collect();
    if needles.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT d.id, d.title FROM docs d JOIN roots r ON r.id = d.root_id
         WHERE d.active = 1{}",
        filter.sql
    );
    let titles = store
        .conn()
        .prepare_cached(&sql)?
        .query_map(
            params_from_iter(filter.params.iter().map(AsRef::as_ref)),
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut matched: Vec<(i64, usize)> = titles
        .into_iter()
        .filter_map(|(id, title)| {
            let norm = normalize_title(&title);
            needles
                .iter()
                .all(|n| norm.contains(n.as_str()))
                .then_some((id, norm.len()))
        })
        .collect();
    matched.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    matched.truncate(CANDIDATES);
    let n = i64::try_from(needles.len()).unwrap_or(i64::MAX);
    let docs: Vec<(i64, i64)> = matched.into_iter().map(|(id, _)| (id, n)).collect();
    attach_chunks(store, docs, best_chunk, excluded)
}

fn tags_stream(
    store: &Store,
    query: &Query,
    filter: &Filter,
    best_chunk: &HashMap<i64, i64>,
    excluded: &HashSet<i64>,
) -> Result<Vec<Candidate>> {
    let words = query.words();
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; words.len()].join(", ");
    let sql = format!(
        "SELECT d.id, COUNT(*) AS n
         FROM tags t JOIN docs d ON d.id = t.doc_id JOIN roots r ON r.id = d.root_id
         WHERE t.tag IN ({placeholders}) AND d.active = 1{}
         GROUP BY d.id
         ORDER BY n DESC, d.id
         LIMIT {CANDIDATES}",
        filter.sql
    );
    let params: Vec<&dyn ToSql> = words
        .iter()
        .map(|w| w as &dyn ToSql)
        .chain(filter.params.iter().map(AsRef::as_ref))
        .collect();
    let docs = store
        .conn()
        .prepare_cached(&sql)?
        .query_map(params_from_iter(params), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    attach_chunks(store, docs, best_chunk, excluded)
}

/// For a query with filters but no words (`tag:lambda` on its own): the
/// documents the filters select, newest first.
fn browse_stream(
    store: &Store,
    filter: &Filter,
    excluded: &HashSet<i64>,
) -> Result<Vec<Candidate>> {
    let sql = format!(
        "SELECT d.id FROM docs d JOIN roots r ON r.id = d.root_id
         WHERE d.active = 1{}
         ORDER BY d.updated DESC, d.id
         LIMIT {CANDIDATES}",
        filter.sql
    );
    let docs = store
        .conn()
        .prepare_cached(&sql)?
        .query_map(
            params_from_iter(filter.params.iter().map(AsRef::as_ref)),
            |r| Ok((r.get::<_, i64>(0)?, 1)),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    attach_chunks(store, docs, &HashMap::new(), excluded)
}

/// Map document-level results onto a chunk: the document's best FTS chunk
/// if it had one, else its first chunk that is not excluded. Order is kept.
#[allow(clippy::cast_precision_loss)]
fn attach_chunks(
    store: &Store,
    docs: Vec<(i64, i64)>,
    best_chunk: &HashMap<i64, i64>,
    excluded: &HashSet<i64>,
) -> Result<Vec<Candidate>> {
    let need: Vec<i64> = docs
        .iter()
        .map(|(d, _)| *d)
        .filter(|d| !best_chunk.contains_key(d))
        .collect();
    let mut first: HashMap<i64, i64> = HashMap::new();
    if !need.is_empty() {
        let placeholders = vec!["?"; need.len()].join(", ");
        let sql = format!(
            "SELECT doc_id, id FROM chunks WHERE doc_id IN ({placeholders}) ORDER BY ordinal"
        );
        let rows = store
            .conn()
            .prepare(&sql)?
            .query_map(params_from_iter(need.iter()), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (doc_id, chunk_id) in rows {
            if !excluded.contains(&chunk_id) {
                first.entry(doc_id).or_insert(chunk_id);
            }
        }
    }
    Ok(docs
        .into_iter()
        .filter_map(|(doc_id, n)| {
            let chunk_id = best_chunk
                .get(&doc_id)
                .or_else(|| first.get(&doc_id))
                .copied()?;
            Some(Candidate {
                chunk_id,
                doc_id,
                raw: n as f64,
            })
        })
        .collect())
}

struct Fused {
    chunk_id: i64,
    doc_id: i64,
    rrf: f64,
    streams: Vec<StreamRank>,
    /// Lower-ranked chunks of the same document, best first.
    siblings: Vec<i64>,
}

#[allow(clippy::cast_precision_loss)]
fn fuse(streams: &[Stream]) -> Vec<Fused> {
    let mut by_chunk: HashMap<i64, Fused> = HashMap::new();
    for s in streams {
        for (i, c) in s.ranked.iter().enumerate() {
            let rank = i + 1;
            let contribution = s.weight / (RRF_K + rank as f64);
            let entry = by_chunk.entry(c.chunk_id).or_insert_with(|| Fused {
                chunk_id: c.chunk_id,
                doc_id: c.doc_id,
                rrf: 0.0,
                streams: Vec::new(),
                siblings: Vec::new(),
            });
            entry.rrf += contribution;
            entry.streams.push(StreamRank {
                stream: s.name,
                rank,
                raw: c.raw,
                contribution,
            });
        }
    }
    by_chunk.into_values().collect()
}

/// Keep the best chunk per document; the rest become its siblings. Done
/// before authority is applied, which is safe because authority is a
/// per-document factor and cannot reorder chunks within one document.
fn collapse(fused: Vec<Fused>) -> Vec<Fused> {
    let mut out: Vec<Fused> = Vec::new();
    let mut index: HashMap<i64, usize> = HashMap::new();
    for f in fused {
        if let Some(&i) = index.get(&f.doc_id) {
            if out[i].siblings.len() < MAX_SIBLINGS {
                out[i].siblings.push(f.chunk_id);
            }
        } else {
            index.insert(f.doc_id, out.len());
            out.push(f);
        }
    }
    out
}

/// Authority multiplier from the root weight, `pinned` frontmatter and
/// demotion: `1 + (weight - 1) * 0.25`, times the pinned bonus, times the
/// demotion factor (1.0 when not demoted), clamped to `[0.85, 1.15]`. A
/// memory root at weight 1.2 scores 1.05; reference at 0.8 scores 0.95; an
/// archived note in a weight-1.0 root at the default factor scores 0.90.
fn authority(root_weight: f64, frontmatter: Option<&str>, demote: f64) -> f64 {
    let pinned = frontmatter
        .and_then(|f| serde_json::from_str::<serde_json::Value>(f).ok())
        .is_some_and(|v| v.get("pinned").and_then(serde_json::Value::as_bool) == Some(true));
    let mut m = 1.0 + (root_weight - 1.0) * AUTHORITY_DAMPING;
    if pinned {
        m *= PINNED_BONUS;
    }
    (m * demote).clamp(AUTHORITY_MIN, AUTHORITY_MAX)
}

/// Compiled demotion globs per root path, built lazily for the roots that
/// appear among the hits being loaded.
#[derive(Default)]
struct Demotions(HashMap<String, (globset::GlobSet, f64)>);

impl Demotions {
    /// The factor for `rel_path` under a root, if one of its globs matches.
    fn factor(
        &mut self,
        root_path: &str,
        globs: Option<&str>,
        factor: f64,
        rel_path: &str,
    ) -> Option<f64> {
        let globs = globs?;
        let (set, factor) = self.0.entry(root_path.to_owned()).or_insert_with(|| {
            let patterns: Vec<String> = serde_json::from_str(globs).unwrap_or_default();
            (crate::roots::demote_set(&patterns), factor)
        });
        set.is_match(rel_path).then_some(*factor)
    }
}

/// Load rows for the fused candidates and their siblings in one query.
fn load_hits(store: &Store, fused: &[Fused]) -> Result<Vec<Hit>> {
    if fused.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = fused
        .iter()
        .flat_map(|f| std::iter::once(f.chunk_id).chain(f.siblings.iter().copied()))
        .collect();
    let placeholders = vec!["?"; ids.len()].join(", ");
    let sql = format!(
        "SELECT {CHUNK_REF_COLUMNS}, c.tokens, d.updated, d.frontmatter, r.kind, r.weight,
                r.demote, r.demote_factor
         FROM chunks c JOIN docs d ON d.id = c.doc_id JOIN roots r ON r.id = d.root_id
         WHERE c.id IN ({placeholders})"
    );
    let mut stmt = store.conn().prepare(&sql)?;
    let mut demotions = Demotions::default();
    let rows = stmt.query_map(params_from_iter(ids.iter()), |r| {
        let chunk = chunk_ref_from_row(r)?;
        let frontmatter: Option<String> = r.get(15)?;
        let weight: f64 = r.get(17)?;
        let globs: Option<String> = r.get(18)?;
        let demoted = demotions.factor(
            &chunk.doc.root_path,
            globs.as_deref(),
            r.get(19)?,
            &chunk.doc.rel_path,
        );
        Ok(Hit {
            display_path: chunk.doc.display_path(),
            tokens: r.get(13)?,
            updated: r.get(14)?,
            root_kind: r.get(16)?,
            score: authority(weight, frontmatter.as_deref(), demoted.unwrap_or(1.0)),
            siblings: Vec::new(),
            explain: Explain {
                demoted,
                ..Explain::default()
            },
            chunk,
        })
    })?;
    let mut by_chunk: HashMap<i64, Hit> = rows
        .map(|r| r.map(|h| (h.chunk.chunk_id, h)))
        .collect::<std::result::Result<_, _>>()?;
    let mut out = Vec::with_capacity(fused.len());
    for f in fused {
        let Some(mut hit) = by_chunk.remove(&f.chunk_id) else {
            continue;
        };
        let auth = hit.score;
        hit.score = f.rrf * auth;
        hit.explain = Explain {
            streams: f.streams.clone(),
            rrf: f.rrf,
            authority: auth,
            demoted: hit.explain.demoted,
        };
        hit.siblings = f
            .siblings
            .iter()
            .filter_map(|id| by_chunk.get(id).map(|h| h.chunk.id.clone()))
            .collect();
        out.push(hit);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(w: &str, prefix: bool) -> Unit {
        Unit::Term {
            word: w.into(),
            prefix,
        }
    }

    #[test]
    fn bare_words_are_prefix_terms_without_stopwords() {
        let q = Query::parse("the lambda cold start of it");
        assert_eq!(
            q.positive,
            vec![
                term("lambda", true),
                term("cold", true),
                term("start", true)
            ]
        );
        assert_eq!(
            q.fts_loose().as_deref(),
            Some(r#"("lambda"* OR "cold"* OR "start"*)"#)
        );
        assert_eq!(
            q.fts_strict().as_deref(),
            Some(r#"("lambda"* AND "cold"* AND "start"*)"#)
        );
    }

    #[test]
    fn quotes_make_phrases_and_dash_negates() {
        let q = Query::parse(r#""cold start" -timeout -"warm pool""#);
        assert_eq!(
            q.positive,
            vec![Unit::Phrase(vec!["cold".into(), "start".into()])]
        );
        assert_eq!(
            q.negative,
            vec![
                term("timeout", true),
                Unit::Phrase(vec!["warm".into(), "pool".into()])
            ]
        );
        assert_eq!(
            q.fts_loose().as_deref(),
            Some(r#"("cold start") NOT ("timeout"* OR "warm pool")"#)
        );
        assert!(
            q.fts_strict().is_none(),
            "one positive unit: no strict form"
        );
    }

    #[test]
    fn compound_tokens_become_phrases_of_parts() {
        let q = Query::parse("DEC-0054 src/lib/x.rs col:thing");
        assert_eq!(
            q.positive,
            vec![
                Unit::Phrase(vec!["dec".into(), "0054".into()]),
                Unit::Phrase(vec!["src".into(), "lib".into(), "x".into(), "rs".into()]),
                Unit::Phrase(vec!["col".into(), "thing".into()]),
            ]
        );
    }

    #[test]
    fn short_words_are_exact_and_quoted_stopwords_survive() {
        let q = Query::parse(r#"go "the""#);
        assert_eq!(q.positive, vec![term("go", false), term("the", false)]);
    }

    #[test]
    fn all_stopwords_falls_back_to_exact_terms() {
        let q = Query::parse("what is it");
        assert_eq!(
            q.positive,
            vec![term("what", false), term("is", false), term("it", false)]
        );
    }

    #[test]
    fn tag_prefix_becomes_a_filter() {
        let q = Query::parse("tag:DevOps lambda");
        assert_eq!(q.tags, vec!["devops"]);
        assert_eq!(q.positive, vec![term("lambda", true)]);
    }

    #[test]
    fn lex_passes_through() {
        let q = Query::parse("lex: title:lambda NEAR(cold start)");
        assert_eq!(
            q.fts_loose().as_deref(),
            Some("title:lambda NEAR(cold start)")
        );
        assert!(q.fts_strict().is_none());
        assert_eq!(
            q.words(),
            vec!["cold", "lambda", "start"],
            "column filters and operators are syntax, not words"
        );
    }

    #[test]
    fn lex_not_excludes_and_syntax_is_skipped() {
        let q = Query::parse(
            r#"lex: (lambda OR "cold start") NOT python* AND {title body}:x NEAR(a b, 3)"#,
        );
        assert_eq!(
            q.positive,
            vec![
                term("lambda", false),
                Unit::Phrase(vec!["cold".into(), "start".into()]),
                term("x", false),
                term("a", false),
                term("b", false),
            ]
        );
        assert_eq!(q.negative, vec![term("python", true)]);
        assert_eq!(q.fts_exclude().as_deref(), Some(r#"("python"*)"#));
        assert!(!q.is_empty());
    }

    #[test]
    fn tag_only_queries_are_filters_not_empty() {
        let q = Query::parse("tag:lambda -tag:draft");
        assert!(!q.is_empty());
        assert!(q.filter_only());
        assert_eq!(q.tags, vec!["lambda"]);
        assert_eq!(q.excluded_tags, vec!["draft"]);
        assert!(q.fts_loose().is_none());
    }

    #[test]
    fn since_rejects_overflow_instead_of_panicking() {
        assert!(parse_since("9999999999999999d").is_none());
        assert!(parse_since("99999999999999999999h").is_none());
        assert!(parse_since("30d").is_some());
    }

    #[test]
    fn empty_and_dash_only_queries_are_empty() {
        assert!(Query::parse("").is_empty());
        assert!(Query::parse("   ").is_empty());
        assert!(Query::parse("-").is_empty());
        assert!(Query::parse("-foo").is_empty(), "nothing positive to match");
    }

    #[test]
    fn since_parses_durations_and_dates() {
        assert!(parse_since("30d").is_some());
        assert!(parse_since("2w").is_some());
        assert!(parse_since("12h").is_some());
        assert_eq!(
            parse_since("2026-10-01").as_deref(),
            Some("2026-10-01T00:00:00Z")
        );
        assert!(parse_since("soon").is_none());
    }

    #[test]
    fn authority_is_bounded() {
        assert!((authority(1.0, None, 1.0) - 1.0).abs() < f64::EPSILON);
        assert!((authority(1.2, None, 1.0) - 1.05).abs() < 1e-12);
        assert!((authority(0.8, None, 1.0) - 0.95).abs() < 1e-12);
        assert!((authority(1.2, Some(r#"{"pinned":true}"#), 1.0) - 1.1025).abs() < 1e-12);
        assert!((authority(0.0, None, 1.0) - AUTHORITY_MIN).abs() < f64::EPSILON);
        assert!((authority(9.0, None, 1.0) - AUTHORITY_MAX).abs() < f64::EPSILON);
        assert!((authority(1.0, None, 0.9) - 0.9).abs() < 1e-12);
        assert!(
            (authority(0.8, None, 0.5) - AUTHORITY_MIN).abs() < f64::EPSILON,
            "demotion stays inside the band however strong"
        );
    }

    #[test]
    fn display_path_uses_project_and_root_dir() {
        assert_eq!(
            display_path("/h/.claude/projects/-h-x/memory", Some("x"), "a.md"),
            "x/memory/a.md"
        );
        assert_eq!(
            display_path("/h/Projects/notes", None, "sub/b.md"),
            "notes/sub/b.md"
        );
    }

    #[test]
    fn titles_normalise_like_queries() {
        assert_eq!(
            normalize_title("Kvault: rebuild-decision (v0)"),
            "kvault rebuild decision v0"
        );
    }

    #[test]
    fn fusion_sums_weighted_reciprocal_ranks() {
        let streams = vec![
            Stream {
                name: "a",
                weight: 2.0,
                ranked: vec![
                    Candidate {
                        chunk_id: 1,
                        doc_id: 1,
                        raw: 0.0,
                    },
                    Candidate {
                        chunk_id: 2,
                        doc_id: 2,
                        raw: 0.0,
                    },
                ],
                elapsed: Duration::ZERO,
            },
            Stream {
                name: "b",
                weight: 1.0,
                ranked: vec![Candidate {
                    chunk_id: 2,
                    doc_id: 2,
                    raw: 0.0,
                }],
                elapsed: Duration::ZERO,
            },
        ];
        let mut fused = fuse(&streams);
        fused.sort_by_key(|f| f.chunk_id);
        assert!((fused[0].rrf - 2.0 / 61.0).abs() < 1e-12);
        assert!((fused[1].rrf - (2.0 / 62.0 + 1.0 / 61.0)).abs() < 1e-12);
        assert_eq!(fused[1].streams.len(), 2);
    }
}
