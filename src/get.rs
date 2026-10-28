//! `kvault get`: print a chunk by id, a document by path, or a line window.
//!
//! Chunk text is not stored in the index; it is read from the file by byte
//! range. If the file changed since indexing the byte range is no longer
//! trustworthy, so the chunk's line range is used instead and the output is
//! marked stale.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::search::{display_path, short_id};
use crate::store::Store;

/// What the user asked `get` for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A short (or full) hex chunk id.
    Id(String),
    /// A path, optionally with a 1-based line window `path:from[:count]`.
    Path {
        path: String,
        from: Option<u32>,
        count: Option<u32>,
    },
}

/// Lines printed for `path:from` when no count is given.
pub const DEFAULT_WINDOW: u32 = 40;

impl Target {
    /// Hex of 6 to 64 digits is an id; anything else is a path with an
    /// optional `:from[:count]` suffix.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        let is_hex = (6..=64).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit());
        if is_hex && !s.contains('.') {
            return Self::Id(s.to_ascii_lowercase());
        }
        let mut parts = s.rsplitn(3, ':');
        let last = parts.next().unwrap_or_default();
        let mid = parts.next();
        let head = parts.next();
        match (head, mid, last.parse::<u32>()) {
            // path:from:count
            (Some(path), Some(from), Ok(count)) => match from.parse::<u32>() {
                Ok(from) => Self::Path {
                    path: path.to_owned(),
                    from: Some(from),
                    count: Some(count),
                },
                Err(_) => Self::Path {
                    path: format!("{path}:{from}"),
                    from: Some(count),
                    count: None,
                },
            },
            // path:from
            (None, Some(path), Ok(from)) => Self::Path {
                path: path.to_owned(),
                from: Some(from),
                count: None,
            },
            _ => Self::Path {
                path: s.to_owned(),
                from: None,
                count: None,
            },
        }
    }
}

/// A document row with enough to locate the file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DocRef {
    pub doc_id: i64,
    pub root_path: String,
    pub project: Option<String>,
    pub rel_path: String,
    pub title: String,
    pub mtime_ns: i64,
}

impl DocRef {
    #[must_use]
    pub fn abs_path(&self) -> PathBuf {
        Path::new(&self.root_path).join(&self.rel_path)
    }

    #[must_use]
    pub fn display_path(&self) -> String {
        display_path(&self.root_path, self.project.as_deref(), &self.rel_path)
    }
}

/// A chunk row plus its document.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChunkRef {
    pub chunk_id: i64,
    /// Short id: a prefix of the chunk `BLAKE3`, what `search` prints.
    pub id: String,
    pub heading_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub byte_start: usize,
    pub byte_end: usize,
    #[serde(flatten)]
    pub doc: DocRef,
}

/// Columns, in order, that [`chunk_ref_from_row`] reads: use as the start
/// of a select over `chunks c JOIN docs d JOIN roots r`.
pub(crate) const CHUNK_REF_COLUMNS: &str =
    "c.id, c.blake3, c.heading_path, c.start_line, c.end_line, c.byte_start, c.byte_end,
     d.id, r.path, r.project, d.rel_path, d.title, d.mtime_ns";

/// Build a [`ChunkRef`] from a row that starts with [`CHUNK_REF_COLUMNS`].
pub(crate) fn chunk_ref_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChunkRef> {
    Ok(ChunkRef {
        chunk_id: r.get(0)?,
        id: short_id(&r.get::<_, String>(1)?),
        heading_path: r.get(2)?,
        start_line: r.get(3)?,
        end_line: r.get(4)?,
        byte_start: usize::try_from(r.get::<_, i64>(5)?).unwrap_or(0),
        byte_end: usize::try_from(r.get::<_, i64>(6)?).unwrap_or(0),
        doc: DocRef {
            doc_id: r.get(7)?,
            root_path: r.get(8)?,
            project: r.get(9)?,
            rel_path: r.get(10)?,
            title: r.get(11)?,
            mtime_ns: r.get(12)?,
        },
    })
}

/// Text read from disk for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    pub text: String,
    /// First line number of `text`, 1-based.
    pub first_line: u32,
    /// The file's mtime differs from the indexed one.
    pub stale: bool,
}

/// Whether the file on disk still matches what was indexed.
#[must_use]
pub fn is_stale(path: &Path, indexed_mtime_ns: i64) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .is_none_or(|t| crate::index::system_time_ns(t) != indexed_mtime_ns)
}

/// Read a chunk's text. Uses the byte range when the file is unchanged,
/// else the line range (which is close enough for a human to orient).
///
/// # Errors
/// Returns an error if the file cannot be read.
pub fn read_chunk(chunk: &ChunkRef) -> Result<Text> {
    let path = chunk.doc.abs_path();
    let stale = is_stale(&path, chunk.doc.mtime_ns);
    let file = std::fs::read_to_string(&path)?;
    Ok(slice_chunk(&file, chunk, stale))
}

/// [`read_chunk`] over file text the caller already read, so one read can
/// serve every chunk of a document.
#[must_use]
pub fn slice_chunk(file: &str, chunk: &ChunkRef, stale: bool) -> Text {
    if !stale
        && chunk.byte_end <= file.len()
        && file.is_char_boundary(chunk.byte_start)
        && file.is_char_boundary(chunk.byte_end)
    {
        return Text {
            text: file[chunk.byte_start..chunk.byte_end].to_owned(),
            first_line: chunk.start_line,
            stale: false,
        };
    }
    let count = chunk.end_line.saturating_sub(chunk.start_line) + 1;
    Text {
        text: window(file, chunk.start_line, count),
        first_line: chunk.start_line,
        stale: true,
    }
}

/// Lines `from..from+count` (1-based, inclusive start) of `text`.
#[must_use]
pub fn window(text: &str, from: u32, count: u32) -> String {
    let skip = usize::try_from(from.saturating_sub(1)).unwrap_or(usize::MAX);
    let take = usize::try_from(count).unwrap_or(usize::MAX);
    let mut out = String::new();
    for line in text.lines().skip(skip).take(take) {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Find the chunk whose `BLAKE3` starts with `prefix`.
///
/// # Errors
/// Returns an error if no chunk matches or the prefix is ambiguous across
/// documents (identical text in two files is reported as both).
pub fn chunk_by_id(store: &Store, prefix: &str) -> Result<ChunkRef> {
    let upper = format!("{prefix}g");
    let sql = format!(
        "SELECT {CHUNK_REF_COLUMNS}
         FROM chunks c JOIN docs d ON d.id = c.doc_id JOIN roots r ON r.id = d.root_id
         WHERE c.blake3 >= ?1 AND c.blake3 < ?2 AND d.active = 1
         ORDER BY d.id, c.ordinal"
    );
    let mut stmt = store.conn().prepare_cached(&sql)?;
    let rows = stmt
        .query_map([prefix, upper.as_str()], chunk_ref_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut docs: Vec<&ChunkRef> = Vec::new();
    for c in &rows {
        if !docs.iter().any(|d| d.doc.doc_id == c.doc.doc_id) {
            docs.push(c);
        }
    }
    match docs.len() {
        0 => Err(Error::Other(anyhow::anyhow!(
            "no chunk with id {prefix} (index out of date? run `kvault index`)"
        ))),
        1 => Ok(docs[0].clone()),
        _ => {
            let list: Vec<String> = docs
                .iter()
                .map(|c| format!("  {}:{}", c.doc.display_path(), c.start_line))
                .collect();
            Err(Error::Other(anyhow::anyhow!(
                "id {prefix} matches chunks in {} documents; use a path:\n{}",
                docs.len(),
                list.join("\n")
            )))
        }
    }
}

fn all_docs(store: &Store) -> Result<Vec<DocRef>> {
    let mut stmt = store.conn().prepare_cached(
        "SELECT d.id, r.path, r.project, d.rel_path, d.title, d.mtime_ns
         FROM docs d JOIN roots r ON r.id = d.root_id WHERE d.active = 1",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(DocRef {
            doc_id: r.get(0)?,
            root_path: r.get(1)?,
            project: r.get(2)?,
            rel_path: r.get(3)?,
            title: r.get(4)?,
            mtime_ns: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Resolve a path the way a user would type it: absolute, as printed by
/// `search` (`project/notes/x.md`), relative to a root, or any unique
/// suffix of those.
///
/// # Errors
/// Returns an error if nothing matches or several documents do.
pub fn doc_by_path(store: &Store, path: &str) -> Result<DocRef> {
    let given = path.trim().trim_end_matches('/');
    let expanded = crate::config::expand_tilde(given);
    let abs = std::fs::canonicalize(&expanded).unwrap_or(expanded);
    let abs = abs.to_string_lossy();
    let docs = all_docs(store)?;

    let exact: Vec<&DocRef> = docs
        .iter()
        .filter(|d| {
            d.abs_path().to_string_lossy() == abs
                || d.display_path() == given
                || d.rel_path == given
        })
        .collect();
    let candidates = if exact.is_empty() {
        let suffix = format!("/{}", given.trim_start_matches('/'));
        docs.iter()
            .filter(|d| {
                d.abs_path().to_string_lossy().ends_with(&suffix)
                    || format!("/{}", d.display_path()).ends_with(&suffix)
            })
            .collect()
    } else {
        exact
    };
    match candidates.len() {
        0 => Err(Error::Other(anyhow::anyhow!(
            "no indexed document matches {given:?}"
        ))),
        1 => Ok(candidates[0].clone()),
        n => {
            let list: Vec<String> = candidates
                .iter()
                .take(10)
                .map(|d| format!("  {}", d.display_path()))
                .collect();
            Err(Error::Other(anyhow::anyhow!(
                "{given:?} matches {n} documents:\n{}",
                list.join("\n")
            )))
        }
    }
}

/// Render `get` output: a header line, then numbered lines.
///
/// # Errors
/// Returns an error if the target cannot be resolved or the file read.
pub fn run(store: &Store, target: &Target, whole_doc: bool) -> Result<String> {
    match target {
        Target::Id(prefix) => {
            let chunk = chunk_by_id(store, prefix)?;
            if whole_doc {
                return render_doc(&chunk.doc, None, None);
            }
            let text = read_chunk(&chunk)?;
            let mut out = format!(
                "# {}  {}:{}-{}  \"{}\"",
                chunk.id,
                chunk.doc.display_path(),
                chunk.start_line,
                chunk.end_line,
                chunk.doc.title
            );
            if !chunk.heading_path.is_empty() {
                let _ = write!(out, "  › {}", chunk.heading_path);
            }
            if text.stale {
                out.push_str("  [stale: file changed since indexing; run `kvault index`]");
            }
            out.push('\n');
            out.push_str(&numbered(&text.text, text.first_line));
            Ok(out)
        }
        Target::Path { path, from, count } => {
            let doc = doc_by_path(store, path)?;
            render_doc(&doc, *from, *count)
        }
    }
}

fn render_doc(doc: &DocRef, from: Option<u32>, count: Option<u32>) -> Result<String> {
    let file = std::fs::read_to_string(doc.abs_path())?;
    let stale = is_stale(&doc.abs_path(), doc.mtime_ns);
    let total = u32::try_from(file.lines().count()).unwrap_or(u32::MAX);
    let (from, count) = match (from, count) {
        (Some(f), Some(c)) => (f.max(1), c),
        (Some(f), None) => (f.max(1), DEFAULT_WINDOW),
        (None, _) => (1, total),
    };
    let mut out = format!("# {}:{}  \"{}\"", doc.display_path(), from, doc.title);
    if stale {
        out.push_str("  [stale: file changed since indexing; run `kvault index`]");
    }
    out.push('\n');
    out.push_str(&numbered(&window(&file, from, count), from));
    Ok(out)
}

/// Prefix each line with its 1-based number.
#[must_use]
pub fn numbered(text: &str, first_line: u32) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for (n, line) in (first_line..).zip(text.lines()) {
        let _ = writeln!(out, "{n:>5}  {line}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_parsing() {
        assert_eq!(Target::parse("a1f3c9"), Target::Id("a1f3c9".into()));
        assert_eq!(Target::parse("A1F3C9DE"), Target::Id("a1f3c9de".into()));
        assert_eq!(
            Target::parse("abcde"),
            Target::Path {
                path: "abcde".into(),
                from: None,
                count: None
            },
            "five hex digits is too short for an id"
        );
        assert_eq!(
            Target::parse("notes/x.md"),
            Target::Path {
                path: "notes/x.md".into(),
                from: None,
                count: None
            }
        );
        assert_eq!(
            Target::parse("notes/x.md:12"),
            Target::Path {
                path: "notes/x.md".into(),
                from: Some(12),
                count: None
            }
        );
        assert_eq!(
            Target::parse("notes/x.md:12:5"),
            Target::Path {
                path: "notes/x.md".into(),
                from: Some(12),
                count: Some(5)
            }
        );
        assert_eq!(
            Target::parse("c:/odd/x.md:3"),
            Target::Path {
                path: "c:/odd/x.md".into(),
                from: Some(3),
                count: None
            }
        );
    }

    #[test]
    fn window_and_numbering() {
        let t = "a\nb\nc\nd\n";
        assert_eq!(window(t, 2, 2), "b\nc\n");
        assert_eq!(window(t, 1, 99), t);
        assert_eq!(window(t, 9, 2), "");
        assert_eq!(numbered("x\ny", 3), "    3  x\n    4  y\n");
    }
}
