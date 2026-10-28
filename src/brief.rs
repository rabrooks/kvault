//! `kvault brief`: the passive context block a `SessionStart` hook injects.
//!
//! Not query-driven. For the project a working directory belongs to it
//! lists pinned notes and rules, recent notes, and notes elsewhere that link
//! to or share tags with the project, each as one line with a chunk id the
//! agent can `get`. The header is byte-stable across runs except the counts
//! at its end, so a cached prompt prefix survives; the whole block fits the
//! token budget and stays under Claude Code's hook output cap.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use rusqlite::params_from_iter;

use crate::error::Result;
use crate::get::{self, CHUNK_REF_COLUMNS, ChunkRef, DocRef, chunk_ref_from_row};
use crate::list::{self, ListFilter};
use crate::pack::{self, ByteHeuristic, TokenCounter};
use crate::store::Store;

/// Claude Code swaps hook output over 10,000 characters for a file path and
/// a preview; stay well under.
pub const MAX_BYTES: usize = 8_000;
/// Entries fetched per section before packing.
const SECTION_LIMIT: usize = 10;
/// Entries in the related-elsewhere section.
const RELATED_LIMIT: usize = 5;
/// Characters of the first line shown per entry.
const LINE_CHARS: usize = 100;

#[derive(Debug, Clone)]
pub struct BriefOptions {
    /// Working directory the brief is for.
    pub cwd: std::path::PathBuf,
    pub budget: usize,
    pub recent_days: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Pinned,
    Recent,
    Related,
}

impl Section {
    fn heading(self, recent_days: u32, project: Option<&str>) -> String {
        match (self, project) {
            (Self::Pinned, _) => "Pinned / rules:".to_owned(),
            (Self::Recent, Some(_)) => format!("Recent ({recent_days}d) in this project:"),
            (Self::Recent, None) => format!("Recent ({recent_days}d):"),
            (Self::Related, _) => "Related elsewhere:".to_owned(),
        }
    }
}

/// A document row plus the `updated` date the recent section prints.
struct Doc {
    r: DocRef,
    updated: Option<String>,
}

/// Build the brief.
///
/// # Errors
/// Returns an error if the index cannot be read.
pub fn run(store: &Store, opts: &BriefOptions) -> Result<String> {
    let project = project_for(store, &opts.cwd)?;
    let header = header(store, project.as_deref())?;
    let footer = "Search: `kvault recall \"<q>\" --budget 1500` · Read: `kvault get <id>` · Save: `kvault add`\n";

    // Sections in the order they print, which is also the reverse of the
    // order they are shed: related first, then recent, then pinned.
    let mut sections: Vec<(Section, Vec<Doc>)> = vec![
        (Section::Pinned, pinned(store, project.as_deref())?),
        (
            Section::Recent,
            recent(store, project.as_deref(), opts.recent_days)?,
        ),
    ];
    if let Some(p) = &project {
        sections.push((Section::Related, related(store, p)?));
    }
    let mut seen: Vec<i64> = Vec::new();
    let mut lines: Vec<(Section, String)> = Vec::new();
    for (section, docs) in &sections {
        let docs: Vec<&Doc> = docs
            .iter()
            .filter(|d| !seen.contains(&d.r.doc_id))
            .collect();
        let firsts = first_chunks(store, &docs)?;
        for d in docs {
            if let Some(chunk) = firsts.get(&d.r.doc_id) {
                seen.push(d.r.doc_id);
                lines.push((*section, line(*section, d, chunk)));
            }
        }
    }

    let render = |lines: &[(Section, String)]| {
        let mut out = header.clone();
        let mut current: Option<Section> = None;
        for (section, line) in lines {
            if current != Some(*section) {
                out.push_str(&section.heading(opts.recent_days, project.as_deref()));
                out.push('\n');
                current = Some(*section);
            }
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(footer);
        out
    };
    // Every entry is one line, so fitting is just dropping from the tail.
    let mut text = render(&lines);
    while ByteHeuristic.count(&text) > opts.budget || text.len() > MAX_BYTES {
        if lines.pop().is_none() {
            return Ok(String::new());
        }
        text = render(&lines);
    }
    Ok(text)
}

/// `# kvault: <project> (N notes, M memory facts, indexed 3m ago)`. Counts
/// go last so the stable part of the line is a common prefix across runs.
fn header(store: &Store, project: Option<&str>) -> Result<String> {
    let count = |kind: &str| -> Result<i64> {
        let sql = match project {
            Some(_) => {
                "SELECT COUNT(*) FROM docs d JOIN roots r ON r.id = d.root_id
                 WHERE d.active = 1 AND r.kind = ?1 AND r.project = ?2"
            }
            None => {
                "SELECT COUNT(*) FROM docs d JOIN roots r ON r.id = d.root_id
                 WHERE d.active = 1 AND r.kind = ?1 AND ?2 IS NULL"
            }
        };
        Ok(store
            .conn()
            .query_row(sql, rusqlite::params![kind, project], |r| r.get(0))?)
    };
    let notes = count("notes")?;
    let memory = count("memory")?;
    let age = store
        .meta("last_index_at")?
        .and_then(|t| t.parse::<jiff::Timestamp>().ok())
        .map_or_else(|| "never".to_owned(), ago);
    Ok(format!(
        "# kvault: {} ({notes} notes, {memory} memory facts, indexed {age})\n",
        project.unwrap_or("all projects")
    ))
}

/// Coarse on purpose: a brief that changes every second would never be a
/// stable prompt prefix.
fn ago(t: jiff::Timestamp) -> String {
    let secs = jiff::Timestamp::now().duration_since(t).as_secs().max(0);
    match secs {
        s if s < 60 => "just now".to_owned(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// The project a directory belongs to: the nearest ancestor (the directory
/// itself first) whose name is a project some root was configured with, or
/// an alias of one. Returns the canonical name.
///
/// # Errors
/// Returns an error if the roots cannot be read.
pub fn project_for(store: &Store, cwd: &Path) -> Result<Option<String>> {
    let known: Vec<String> = store
        .conn()
        .prepare_cached("SELECT DISTINCT project FROM roots WHERE project IS NOT NULL")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<_, _>>()?;
    let aliases = store.project_aliases()?;
    let dir = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    Ok(dir
        .ancestors()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .map(|name| aliases.get(&name).cloned().unwrap_or(name))
        .find(|name| known.contains(name)))
}

const DOC_COLUMNS: &str = "d.id, r.path, r.project, d.rel_path, d.title, d.mtime_ns, d.updated";

fn doc_rows(store: &Store, sql: &str, params: &[&dyn rusqlite::ToSql]) -> Result<Vec<Doc>> {
    let mut stmt = store.conn().prepare(sql)?;
    let rows = stmt.query_map(params, |r| {
        Ok(Doc {
            r: DocRef {
                doc_id: r.get(0)?,
                root_path: r.get(1)?,
                project: r.get(2)?,
                rel_path: r.get(3)?,
                title: r.get(4)?,
                mtime_ns: r.get(5)?,
            },
            updated: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Documents with `pinned: true` or under a `_rules/` directory, newest
/// first: those of the project plus global ones (roots with no project), or
/// only global ones when there is no project.
fn pinned(store: &Store, project: Option<&str>) -> Result<Vec<Doc>> {
    let scope = match project {
        Some(_) => "(r.project = ?1 OR r.project IS NULL)",
        None => "(r.project IS NULL AND ?1 IS NULL)",
    };
    let sql = format!(
        "SELECT {DOC_COLUMNS} FROM docs d JOIN roots r ON r.id = d.root_id
         WHERE d.active = 1 AND {scope}
           AND (json_extract(d.frontmatter, '$.pinned') = 1
                OR d.rel_path LIKE '\\_rules/%' ESCAPE '\\'
                OR d.rel_path LIKE '%/\\_rules/%' ESCAPE '\\')
         ORDER BY d.updated DESC, d.rel_path
         LIMIT {SECTION_LIMIT}"
    );
    doc_rows(store, &sql, &[&project])
}

fn recent(store: &Store, project: Option<&str>, days: u32) -> Result<Vec<Doc>> {
    let docs = list::run(
        store,
        &ListFilter {
            project: project.map(str::to_owned),
            recent_days: Some(days),
            limit: Some(SECTION_LIMIT),
            ..ListFilter::default()
        },
    )?;
    Ok(docs
        .into_iter()
        .map(|d| Doc {
            r: DocRef {
                doc_id: d.id,
                root_path: d.root_path,
                project: d.project,
                rel_path: d.rel_path,
                title: d.title,
                mtime_ns: 0,
            },
            updated: d.updated,
        })
        .collect())
}

/// Documents outside the project that link to or from it, or share a tag
/// with it, by number of such connections.
fn related(store: &Store, project: &str) -> Result<Vec<Doc>> {
    let sql = format!(
        "WITH here AS (SELECT d.id FROM docs d JOIN roots r ON r.id = d.root_id
                       WHERE d.active = 1 AND r.project = ?1),
              hits AS (SELECT l.from_doc_id AS id FROM links l WHERE l.to_doc_id IN here
                       UNION ALL
                       SELECT l.to_doc_id FROM links l
                       WHERE l.from_doc_id IN here AND l.to_doc_id IS NOT NULL
                       UNION ALL
                       SELECT t.doc_id FROM tags t
                       WHERE t.tag IN (SELECT tag FROM tags WHERE doc_id IN here))
         SELECT {DOC_COLUMNS} FROM hits h
         JOIN docs d ON d.id = h.id JOIN roots r ON r.id = d.root_id
         WHERE d.active = 1 AND d.id NOT IN here
         GROUP BY d.id
         ORDER BY COUNT(*) DESC, d.updated DESC, d.rel_path
         LIMIT {RELATED_LIMIT}"
    );
    doc_rows(store, &sql, &[&project])
}

/// First chunk of each document, in one query.
fn first_chunks(store: &Store, docs: &[&Doc]) -> Result<HashMap<i64, ChunkRef>> {
    if docs.is_empty() {
        return Ok(HashMap::new());
    }
    let ids: Vec<i64> = docs.iter().map(|d| d.r.doc_id).collect();
    let placeholders = vec!["?"; ids.len()].join(", ");
    let sql = format!(
        "SELECT {CHUNK_REF_COLUMNS}
         FROM chunks c JOIN docs d ON d.id = c.doc_id JOIN roots r ON r.id = d.root_id
         WHERE c.doc_id IN ({placeholders})
         ORDER BY c.doc_id, c.ordinal"
    );
    let mut out = HashMap::new();
    let mut stmt = store.conn().prepare(&sql)?;
    for row in stmt.query_map(params_from_iter(ids.iter()), chunk_ref_from_row)? {
        let chunk = row?;
        out.entry(chunk.doc.doc_id).or_insert(chunk);
    }
    Ok(out)
}

/// `  #id  [date  ]path[ "title"] — first line`.
fn line(section: Section, doc: &Doc, chunk: &ChunkRef) -> String {
    let first = get::read_chunk(chunk)
        .ok()
        .map(|t| pack::trim(&t.text))
        .and_then(|t| {
            pack::preview(&t, 1)
                .first()
                .map(|l| pack::truncate(l, LINE_CHARS))
        })
        .unwrap_or_default();
    let mut line = format!("  #{}  ", chunk.id);
    if section == Section::Recent
        && let Some(date) = &doc.updated
    {
        let _ = write!(line, "{}  ", date.chars().take(10).collect::<String>());
    }
    let _ = write!(line, "{}", doc.r.display_path());
    if section == Section::Pinned {
        let _ = write!(line, " \"{}\"", doc.r.title);
    }
    if !first.is_empty() {
        let _ = write!(line, " — {first}");
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_buckets() {
        let now = jiff::Timestamp::now();
        let at = |secs: i64| {
            now.checked_sub(jiff::SignedDuration::from_secs(secs))
                .unwrap_or(now)
        };
        assert_eq!(ago(at(5)), "just now");
        assert_eq!(ago(at(120)), "2m ago");
        assert_eq!(ago(at(7200)), "2h ago");
        assert_eq!(ago(at(3 * 86_400)), "3d ago");
    }
}
