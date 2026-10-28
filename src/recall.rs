//! `kvault recall`: search packed into a token budget. This is the command
//! agents call; `search` is for people.
//!
//! The top hits are rendered at three tiers (see [`crate::pack`]) and the
//! packer sheds detail from the bottom of the ranking until the whole
//! output, framing included, fits the budget. The budget applies to the
//! bytes actually printed, in whichever format was asked for. Every shown
//! chunk is complete and verbatim apart from [`pack::trim`], so an agent can
//! quote it; a chunk whose file changed since indexing is never shown in full
//! because its byte range no longer means anything.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde::Serialize;

use crate::cli::RecallFormat;
use crate::error::Result;
use crate::get::{self, ChunkRef, Text};
use crate::pack::{self, ByteHeuristic, Limits, Packed, Tier};
use crate::search::{self, Hit, Query, SearchOptions};
use crate::store::Store;

/// Hits that can be shown with any text. Ranks past the preview band are
/// one-line references, so more than this is rarely worth its lines.
pub const PACK_LIMIT: usize = 20;
/// Hits counted so the omitted total is honest; past this it is reported as
/// a lower bound.
pub const COUNT_LIMIT: usize = 200;
/// Lines of a chunk shown at the preview tier.
const PREVIEW_LINES: usize = 2;

#[derive(Debug, Clone)]
pub struct RecallOptions {
    pub budget: usize,
    pub project: Option<String>,
    pub format: RecallFormat,
}

/// One hit as packed.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub id: String,
    pub path: String,
    pub lines: (u32, u32),
    pub title: String,
    pub heading: String,
    pub tier: Tier,
    pub stale: bool,
    /// Preview lines at `L1`, the trimmed chunk at `L2`, nothing at `L0`.
    pub text: Option<String>,
}

/// The packed result.
#[derive(Debug, Clone, Serialize)]
pub struct Recall {
    pub query: String,
    pub budget: usize,
    /// Tokens of `text`, by the index's token counter.
    pub tokens: usize,
    /// Hits found, capped at [`COUNT_LIMIT`].
    pub total: usize,
    pub entries: Vec<Entry>,
    /// Hits that did not fit, best first, as short ids.
    pub omitted: Vec<String>,
    /// The output as printed.
    pub text: String,
}

/// A hit with its text read from disk, ready to render at any tier.
struct Prepared {
    hit: Hit,
    header: String,
    preview: String,
    full: Option<String>,
    stale: bool,
}

/// File contents by document, read once per `recall`.
#[derive(Default)]
struct Files(HashMap<i64, Option<(String, bool)>>);

impl Files {
    fn read(&mut self, chunk: &ChunkRef) -> Option<Text> {
        let entry = self.0.entry(chunk.doc.doc_id).or_insert_with(|| {
            let path = chunk.doc.abs_path();
            std::fs::read_to_string(&path)
                .ok()
                .map(|file| (file, get::is_stale(&path, chunk.doc.mtime_ns)))
        });
        entry
            .as_ref()
            .map(|(file, stale)| get::slice_chunk(file, chunk, *stale))
    }
}

impl Prepared {
    /// `with_text` is false for ranks that can only ever be references, so
    /// their files are not read.
    fn new(hit: Hit, files: &mut Files, with_text: bool) -> Self {
        let text = if with_text {
            files.read(&hit.chunk)
        } else {
            None
        };
        let stale = text.as_ref().is_some_and(|t| t.stale);
        let trimmed = text.as_ref().map(|t| pack::trim(&t.text));
        let c = &hit.chunk;
        let mut header = format!(
            "#{}  {}:{}-{}  \"{}\"",
            c.id,
            hit.display_path,
            c.start_line,
            c.end_line,
            label(&c.doc.title, &c.heading_path)
        );
        if stale {
            header.push_str("  [stale]");
        } else if with_text && text.is_none() {
            header.push_str("  [file unreadable]");
        }
        let preview = trimmed
            .as_deref()
            .map(|t| {
                pack::preview(t, PREVIEW_LINES)
                    .iter()
                    .fold(String::new(), |mut s, l| {
                        let _ = writeln!(s, "  {l}");
                        s
                    })
            })
            .unwrap_or_default();
        Self {
            hit,
            header,
            preview,
            // A stale byte range is a guess at the text, never quoted whole.
            full: trimmed.filter(|t| !t.is_empty() && !stale),
            stale,
        }
    }

    fn render_md(&self, tier: Tier, out: &mut String) {
        out.push_str(&self.header);
        out.push('\n');
        match tier {
            Tier::L0 => {}
            Tier::L1 => out.push_str(&self.preview),
            Tier::L2 => {
                if let Some(full) = &self.full {
                    out.push_str(full);
                }
                out.push('\n');
            }
        }
    }

    fn entry(&self, tier: Tier) -> Entry {
        let c = &self.hit.chunk;
        Entry {
            id: c.id.clone(),
            path: self.hit.display_path.clone(),
            lines: (c.start_line, c.end_line),
            title: c.doc.title.clone(),
            heading: c.heading_path.clone(),
            tier,
            stale: self.stale,
            text: match tier {
                Tier::L0 => None,
                Tier::L1 => Some(self.preview.clone()),
                Tier::L2 => self.full.clone(),
            },
        }
    }
}

/// Run a search and pack it.
///
/// # Errors
/// Returns the search's error for an empty query or bad syntax.
pub fn run(store: &Store, query: &Query, opts: &RecallOptions) -> Result<Recall> {
    let result = search::run(
        store,
        query,
        &SearchOptions {
            limit: COUNT_LIMIT,
            project: opts.project.clone(),
            ..SearchOptions::default()
        },
    )?;
    let total = result.hits.len();
    let mut hits = result.hits.into_iter();
    let mut files = Files::default();
    let prepared: Vec<Prepared> = hits
        .by_ref()
        .take(PACK_LIMIT)
        .enumerate()
        .map(|(i, h)| Prepared::new(h, &mut files, i < pack::FULL_RANKS + pack::PREVIEW_RANKS))
        .collect();
    let uncounted: Vec<String> = hits.map(|h| h.chunk.id).collect();

    let render = |tiers: &[Option<Tier>]| match opts.format {
        RecallFormat::Md => render_md(&query.raw, &prepared, tiers, total),
        RecallFormat::Json => render_json(&query.raw, &prepared, tiers, total, opts),
    };
    let packed: Packed = pack::pack(
        prepared.len(),
        |i| prepared[i].full.is_some(),
        Limits {
            tokens: opts.budget,
            bytes: None,
        },
        &ByteHeuristic,
        render,
    );
    let mut entries = Vec::new();
    let mut omitted = Vec::new();
    for (p, tier) in prepared.iter().zip(&packed.tiers) {
        match tier {
            Some(tier) => entries.push(p.entry(*tier)),
            None => omitted.push(p.hit.chunk.id.clone()),
        }
    }
    omitted.extend(uncounted);
    Ok(Recall {
        query: query.raw.clone(),
        budget: opts.budget,
        tokens: packed.tokens,
        total,
        entries,
        omitted,
        text: packed.text,
    })
}

/// `title › heading path`, except that a heading path already starting with
/// the document title (the usual case, since the H1 is the title) is shown
/// alone.
fn label(title: &str, heading_path: &str) -> String {
    if heading_path.is_empty() {
        title.to_owned()
    } else if heading_path == title || heading_path.starts_with(&format!("{title} / ")) {
        heading_path.to_owned()
    } else {
        format!("{title} › {heading_path}")
    }
}

/// `"12"` or `"180+"` when the count hit [`COUNT_LIMIT`].
fn omitted_label(total: usize, shown: usize) -> String {
    let n = total - shown;
    if total >= COUNT_LIMIT {
        format!("{n}+")
    } else {
        n.to_string()
    }
}

fn render_md(raw: &str, prepared: &[Prepared], tiers: &[Option<Tier>], total: usize) -> String {
    let shown = tiers.iter().flatten().count();
    let mut out = String::new();
    if total == 0 {
        let _ = writeln!(out, "No results for \"{raw}\".");
        return out;
    }
    let total_label = if total >= COUNT_LIMIT {
        format!("{total}+")
    } else {
        total.to_string()
    };
    let _ = writeln!(out, "## recall \"{raw}\" ({shown} of {total_label} hits)\n");
    for (p, tier) in prepared.iter().zip(tiers) {
        if let Some(tier) = tier {
            p.render_md(*tier, &mut out);
        }
    }
    if total > shown {
        let _ = writeln!(out, "\n{} more hits omitted.", omitted_label(total, shown));
    }
    out.push_str("Fetch more: `kvault get <id>`\n");
    out
}

#[derive(Serialize)]
struct JsonOut<'a> {
    query: &'a str,
    budget: usize,
    total: usize,
    entries: Vec<Entry>,
    omitted: usize,
}

fn render_json(
    raw: &str,
    prepared: &[Prepared],
    tiers: &[Option<Tier>],
    total: usize,
    opts: &RecallOptions,
) -> String {
    let entries: Vec<Entry> = prepared
        .iter()
        .zip(tiers)
        .filter_map(|(p, t)| t.map(|t| p.entry(t)))
        .collect();
    let out = JsonOut {
        query: raw,
        budget: opts.budget,
        total,
        omitted: total - entries.len(),
        entries,
    };
    serde_json::to_string_pretty(&out).map_or_else(|_| String::new(), |s| s + "\n")
}
