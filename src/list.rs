//! `kvault list`: documents in the index with filters.

use rusqlite::{ToSql, params_from_iter};

use crate::error::Result;
use crate::store::Store;

#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub root: Option<String>,
    pub kind: Option<String>,
    pub tag: Option<String>,
    pub project: Option<String>,
    /// Only documents updated within the last N days.
    pub recent_days: Option<u32>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocSummary {
    pub id: i64,
    pub root_kind: String,
    pub root_path: String,
    pub project: Option<String>,
    pub rel_path: String,
    pub title: String,
    pub kind: Option<String>,
    pub updated: Option<String>,
    pub chunks: i64,
}

/// List active documents, newest first.
///
/// # Errors
/// Returns an error if the query fails.
pub fn run(store: &Store, filter: &ListFilter) -> Result<Vec<DocSummary>> {
    let mut sql = String::from(
        "SELECT d.id, r.kind, r.path, r.project, d.rel_path, d.title, d.kind, d.updated,
                (SELECT COUNT(*) FROM chunks c WHERE c.doc_id = d.id)
         FROM docs d JOIN roots r ON r.id = d.root_id
         WHERE d.active = 1",
    );
    let mut args: Vec<Box<dyn ToSql>> = Vec::new();
    if let Some(root) = &filter.root {
        sql.push_str(" AND r.path = ?");
        args.push(Box::new(
            crate::config::expand_tilde(root)
                .to_string_lossy()
                .into_owned(),
        ));
    }
    if let Some(kind) = &filter.kind {
        sql.push_str(" AND (r.kind = ? OR d.kind = ?)");
        args.push(Box::new(kind.clone()));
        args.push(Box::new(kind.clone()));
    }
    if let Some(project) = &filter.project {
        sql.push_str(" AND r.project = ?");
        args.push(Box::new(store.canonical_project(project)?));
    }
    if let Some(tag) = &filter.tag {
        sql.push_str(" AND d.id IN (SELECT doc_id FROM tags WHERE tag = ?)");
        args.push(Box::new(tag.to_lowercase()));
    }
    if let Some(days) = filter.recent_days {
        let cutoff = jiff::Timestamp::now()
            .checked_sub(jiff::SignedDuration::from_hours(i64::from(days) * 24))
            .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
            .to_string();
        sql.push_str(" AND d.updated >= ?");
        args.push(Box::new(cutoff));
    }
    sql.push_str(" ORDER BY d.updated DESC, d.rel_path");
    if let Some(limit) = filter.limit {
        sql.push_str(" LIMIT ?");
        args.push(Box::new(i64::try_from(limit).unwrap_or(i64::MAX)));
    }

    let mut stmt = store.conn().prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args.iter().map(AsRef::as_ref)), |r| {
        Ok(DocSummary {
            id: r.get(0)?,
            root_kind: r.get(1)?,
            root_path: r.get(2)?,
            project: r.get(3)?,
            rel_path: r.get(4)?,
            title: r.get(5)?,
            kind: r.get(6)?,
            updated: r.get(7)?,
            chunks: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
