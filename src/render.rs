//! Output formats for `kvault search`.

use std::fmt::Write as _;
use std::time::Duration;

use serde::Serialize;

use crate::cli::SearchFormat;
use crate::error::Result;
use crate::get;
use crate::pack::truncate;
use crate::search::{Hit, SearchResult};

/// Longest snippet line printed, in characters.
const SNIPPET_CHARS: usize = 160;

/// The line of a hit's chunk that best shows why it matched: the first
/// line containing a query term, else the first non-empty body line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Snippet {
    pub line: u32,
    pub text: String,
    pub stale: bool,
}

/// Pick a snippet for a hit. A missing or unreadable file yields `None`.
#[must_use]
pub fn snippet(h: &Hit, needles: &[String]) -> Option<Snippet> {
    let text = get::read_chunk(&h.chunk).ok()?;
    let mut fallback: Option<(u32, &str)> = None;
    for (i, line) in text.text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let n = text.first_line + u32::try_from(i).unwrap_or(u32::MAX);
        let is_heading = trimmed.starts_with('#');
        if fallback.is_none() && !is_heading {
            fallback = Some((n, trimmed));
        }
        let lower = trimmed.to_lowercase();
        if !is_heading && needles.iter().any(|t| lower.contains(t.as_str())) {
            return Some(Snippet {
                line: n,
                text: truncate(trimmed, SNIPPET_CHARS),
                stale: text.stale,
            });
        }
    }
    fallback.map(|(line, t)| Snippet {
        line,
        text: truncate(t, SNIPPET_CHARS),
        stale: text.stale,
    })
}

/// Render a search result in the requested format.
///
/// # Errors
/// Returns an error only if JSON serialisation fails.
pub fn render(result: &SearchResult, format: SearchFormat, explain: bool) -> Result<String> {
    let needles = result.query.needles();
    let out = match format {
        SearchFormat::Cli => cli(result, &needles, explain),
        SearchFormat::Md => md(result, &needles),
        SearchFormat::Json => json(result, &needles)?,
        SearchFormat::Paths => {
            let mut seen = Vec::new();
            for h in &result.hits {
                if !seen.contains(&h.display_path) {
                    seen.push(h.display_path.clone());
                }
            }
            lines(seen.iter().map(String::as_str))
        }
        SearchFormat::Ids => lines(result.hits.iter().map(|h| h.chunk.id.as_str())),
    };
    Ok(out)
}

/// One item per line; nothing at all for no items, so a pipe into `xargs`
/// sees no arguments rather than an empty one.
fn lines<'a>(items: impl Iterator<Item = &'a str>) -> String {
    items.fold(String::new(), |mut out, item| {
        out.push_str(item);
        out.push('\n');
        out
    })
}

fn cli(result: &SearchResult, needles: &[String], explain: bool) -> String {
    let mut out = String::new();
    if result.hits.is_empty() {
        out.push_str("no results\n");
    }
    for h in &result.hits {
        let c = &h.chunk;
        let _ = writeln!(
            out,
            "#{}  {:<9} {}:{}-{}  \"{}\"",
            c.id, h.root_kind, h.display_path, c.start_line, c.end_line, c.doc.title
        );
        if !c.heading_path.is_empty() {
            let _ = writeln!(out, "         {}", c.heading_path);
        }
        match snippet(h, needles) {
            Some(s) => {
                let stale = if s.stale { "  [stale]" } else { "" };
                let _ = writeln!(out, "         L{}: {}{stale}", s.line, s.text);
            }
            None => out.push_str("         [file unreadable; run `kvault index`]\n"),
        }
        if !h.siblings.is_empty() {
            let _ = writeln!(
                out,
                "         also: {}",
                h.siblings
                    .iter()
                    .map(|s| format!("#{s}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        if explain {
            let _ = writeln!(out, "         {}", explain_line(h));
        }
    }
    if explain {
        let _ = writeln!(
            out,
            "query: {}\nstreams: {}\ncandidates: {}",
            result
                .query
                .fts_loose()
                .unwrap_or_else(|| result.query.raw.clone()),
            result
                .timings
                .iter()
                .map(|(n, d)| format!("{n} {}", ms(*d)))
                .collect::<Vec<_>>()
                .join(", "),
            result.candidates
        );
    }
    out
}

fn explain_line(h: &Hit) -> String {
    let parts: Vec<String> = h
        .explain
        .streams
        .iter()
        .map(|s| format!("{}#{} ({:.2})", s.stream, s.rank, s.raw))
        .collect();
    let demoted = h
        .explain
        .demoted
        .map(|f| format!(" (demoted ×{f:.2})"))
        .unwrap_or_default();
    format!(
        "{}  rrf {:.4} × auth {:.2}{demoted} = {:.4}",
        parts.join("  "),
        h.explain.rrf,
        h.explain.authority,
        h.score
    )
}

fn ms(d: Duration) -> String {
    format!("{:.1}ms", d.as_secs_f64() * 1000.0)
}

fn md(result: &SearchResult, needles: &[String]) -> String {
    let mut out = String::new();
    if result.hits.is_empty() {
        out.push_str("No results.\n");
    }
    for h in &result.hits {
        let c = &h.chunk;
        let heading = if c.heading_path.is_empty() {
            String::new()
        } else {
            format!(" › {}", c.heading_path)
        };
        let _ = writeln!(
            out,
            "- #{} `{}:{}-{}` **{}**{heading}",
            c.id, h.display_path, c.start_line, c.end_line, c.doc.title
        );
        if let Some(s) = snippet(h, needles) {
            let _ = writeln!(out, "  {}", s.text);
        }
    }
    out
}

#[derive(Serialize)]
struct JsonHit<'a> {
    #[serde(flatten)]
    hit: &'a Hit,
    snippet: Option<Snippet>,
}

#[derive(Serialize)]
struct JsonOut<'a> {
    query: &'a crate::search::Query,
    hits: Vec<JsonHit<'a>>,
    candidates: usize,
    timings_ms: Vec<(String, f64)>,
}

fn json(result: &SearchResult, needles: &[String]) -> Result<String> {
    let out = JsonOut {
        query: &result.query,
        hits: result
            .hits
            .iter()
            .map(|h| JsonHit {
                hit: h,
                snippet: snippet(h, needles),
            })
            .collect(),
        candidates: result.candidates,
        timings_ms: result
            .timings
            .iter()
            .map(|(n, d)| (n.clone(), d.as_secs_f64() * 1000.0))
            .collect(),
    };
    Ok(serde_json::to_string_pretty(&out).map_err(|e| anyhow::anyhow!(e))? + "\n")
}
