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
//!
//! When none of the top hits contains much of the query, the note the agent
//! wants probably does not exist, and whole chunks about something else
//! would only spend its context. Such a weak match is shown as a few
//! one-line references and a notice instead. Nothing is hidden: a correct
//! answer worded differently from the query is still listed and one `get`
//! away.

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
/// Top hits whose share of the query decides whether the match is weak.
pub const COVERAGE_RANKS: usize = 3;
/// Below this share of the query (see [`search::coverage`]) in every one of
/// the top hits, the results are probably about something else. Chosen on
/// a real corpus: queries with no relevant note peaked under it, and nearly
/// every correct answer, paraphrased ones included, sat above it.
pub const WEAK_COVERAGE: f64 = 0.45;
/// References shown for a weak match.
pub const WEAK_SHOWN: usize = 5;

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
    /// Where the file is, for callers; not part of the printed output.
    #[serde(skip)]
    pub abs_path: std::path::PathBuf,
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
    /// The largest share of the query any top hit contains, when that is
    /// meaningful; see [`search::coverage`].
    pub coverage: Option<f64>,
    /// The match was weak, so only references are shown.
    pub weak: bool,
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
            get::read_as_indexed(&path)
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
            abs_path: c.doc.abs_path(),
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

    let top: Vec<i64> = prepared
        .iter()
        .take(COVERAGE_RANKS)
        .map(|p| p.hit.chunk.chunk_id)
        .collect();
    let coverage = search::coverage(store, query, &top)?.map(|c| c.into_iter().fold(0.0, f64::max));
    let weak = coverage.is_some_and(|c| c < WEAK_COVERAGE);
    let frame = Frame {
        raw: &query.raw,
        total,
        budget: opts.budget,
        weak: weak.then_some(coverage.unwrap_or(0.0)),
    };
    let render = |tiers: &[Option<Tier>]| match opts.format {
        RecallFormat::Md => render_md(&frame, &prepared, tiers),
        RecallFormat::Json => render_json(&frame, &prepared, tiers),
    };
    let limits = Limits {
        tokens: opts.budget,
        bytes: None,
    };
    let packed: Packed = if weak {
        let tiers = (0..prepared.len())
            .map(|i| (i < WEAK_SHOWN).then_some(Tier::L0))
            .collect();
        pack::pack_from(tiers, limits, &ByteHeuristic, render)
    } else {
        pack::pack(
            prepared.len(),
            |i| prepared[i].full.is_some(),
            limits,
            &ByteHeuristic,
            render,
        )
    };
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
        coverage,
        weak,
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

/// What every rendering of one recall shares.
struct Frame<'a> {
    raw: &'a str,
    total: usize,
    budget: usize,
    /// The best share of the query found, when the match is weak.
    weak: Option<f64>,
}

fn render_md(frame: &Frame<'_>, prepared: &[Prepared], tiers: &[Option<Tier>]) -> String {
    let (raw, total) = (frame.raw, frame.total);
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
    if let Some(share) = frame.weak {
        let _ = writeln!(
            out,
            "Weak match: the closest result has about {:.0}% of the query, so there may be no \
             note on this. References only; rephrase, or `kvault get <id>` to check one.\n",
            share * 100.0
        );
    }
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
    /// Present on a weak match: the best share of the query found.
    #[serde(skip_serializing_if = "Option::is_none")]
    weak_match: Option<f64>,
    entries: Vec<Entry>,
    omitted: usize,
}

fn render_json(frame: &Frame<'_>, prepared: &[Prepared], tiers: &[Option<Tier>]) -> String {
    let total = frame.total;
    let entries: Vec<Entry> = prepared
        .iter()
        .zip(tiers)
        .filter_map(|(p, t)| t.map(|t| p.entry(t)))
        .collect();
    let out = JsonOut {
        query: frame.raw,
        budget: frame.budget,
        total,
        weak_match: frame.weak.map(|w| (w * 100.0).round() / 100.0),
        omitted: total - entries.len(),
        entries,
    };
    serde_json::to_string_pretty(&out).map_or_else(|_| String::new(), |s| s + "\n")
}
