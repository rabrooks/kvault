//! Markdown parsing: frontmatter, heading-level chunks, links, tags, observations.
//!
//! Everything here is pure: `parse` takes the file text and returns byte
//! ranges into it. Nothing is stored except what the indexer chooses to.
//! Frontmatter is optional; a plain `.md` with no metadata parses fully.

use std::collections::BTreeMap;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::pack::{ByteHeuristic, TokenCounter};

/// Sections larger than this (in tokens) are split at scored break points.
pub const MAX_CHUNK_TOKENS: usize = 1_200;
/// Split target when a section is oversize.
pub const TARGET_CHUNK_TOKENS: usize = 900;
/// Sections smaller than this are merged forward into the next section.
pub const MIN_CHUNK_TOKENS: usize = 40;

/// Parsed `YAML` frontmatter. Well-known keys are lifted into fields; every
/// other key is kept as `JSON` in `extra` so it stays filterable.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frontmatter {
    pub title: Option<String>,
    pub description: Option<String>,
    pub kind: Option<String>,
    pub tags: Vec<String>,
    pub created: Option<String>,
    pub updated: Option<String>,
    pub pinned: bool,
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// One heading section of a document, addressed by line and byte range.
/// Byte offsets are into the full file text (frontmatter included).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub ordinal: u32,
    /// Ancestor headings joined by " / ", own heading last. Empty for the preamble.
    pub heading_path: String,
    /// Heading level 1..=6; 0 for the preamble before the first heading.
    pub level: u8,
    pub start_line: u32,
    pub end_line: u32,
    pub byte_start: usize,
    pub byte_end: usize,
    pub tokens: usize,
}

/// A link found in a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub target: String,
    pub link_type: LinkType,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkType {
    /// `[[target]]` or `[[target|alias]]`
    Wiki,
    /// `[text](relative.md)`
    Markdown,
    /// `- verb [[Target]]` (basic-memory convention); the verb is stored.
    Relation(String),
}

impl LinkType {
    #[must_use]
    pub fn as_db_str(&self) -> String {
        match self {
            Self::Wiki => "wiki".to_owned(),
            Self::Markdown => "md".to_owned(),
            Self::Relation(v) => format!("rel:{v}"),
        }
    }
}

/// `- [category] text #tag` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub line: u32,
    pub category: String,
    pub text: String,
    pub tags: Vec<String>,
}

/// Everything the indexer needs from one file.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub frontmatter: Frontmatter,
    /// Byte offset where the markdown body begins (after frontmatter).
    pub body_start: usize,
    pub title: String,
    pub chunks: Vec<Chunk>,
    pub links: Vec<Link>,
    /// Lower-cased, de-duplicated: frontmatter tags plus observation tags.
    pub tags: Vec<String>,
    pub observations: Vec<Observation>,
}

/// Parse a markdown file. `fallback_title` is used when neither frontmatter
/// nor a level-1 heading supplies one (the indexer passes the file stem).
#[must_use]
pub fn parse(text: &str, fallback_title: &str) -> Parsed {
    let counter = ByteHeuristic;
    let (frontmatter, body_start) = split_frontmatter(text);
    let body = &text[body_start..];
    let line_index = LineIndex::new(text);

    let headings = collect_headings(body, body_start);
    let title = frontmatter
        .title
        .clone()
        .or_else(|| {
            headings
                .iter()
                .find(|h| h.level == 1)
                .map(|h| h.text.clone())
        })
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| fallback_title.to_owned());

    let mut chunks = build_chunks(text, body_start, &headings, &line_index, &counter);
    chunks = split_oversize(text, chunks, &line_index, &counter);
    chunks = merge_tiny(chunks);
    for (i, c) in chunks.iter_mut().enumerate() {
        c.ordinal = u32::try_from(i).unwrap_or(u32::MAX);
    }

    let links = collect_links(body, body_start, &line_index);
    let observations = collect_observations(body, body_start, &line_index);

    let mut tags: Vec<String> = frontmatter
        .tags
        .iter()
        .chain(observations.iter().flat_map(|o| o.tags.iter()))
        .map(|t| t.to_lowercase())
        .collect();
    tags.sort();
    tags.dedup();

    Parsed {
        frontmatter,
        body_start,
        title,
        chunks,
        links,
        tags,
        observations,
    }
}

// ---------------------------------------------------------------- frontmatter

/// Split `---` frontmatter off the top of the file. Returns the parsed
/// frontmatter (default if absent or malformed) and the body's byte offset.
/// A malformed block is treated as body text so nothing is lost.
#[must_use]
pub fn split_frontmatter(text: &str) -> (Frontmatter, usize) {
    let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let t = &text[bom..];
    let Some(first_nl) = t.find('\n') else {
        return (Frontmatter::default(), bom);
    };
    if t[..first_nl].trim_end_matches('\r') != "---" {
        return (Frontmatter::default(), bom);
    }
    let mut pos = first_nl + 1;
    while pos <= t.len() {
        let end = t[pos..].find('\n').map_or(t.len(), |i| pos + i);
        let line = t[pos..end].trim_end_matches('\r');
        if line == "---" || line == "..." {
            let yaml = &t[first_nl + 1..pos];
            let body_start = bom + (end + 1).min(t.len());
            return match parse_yaml(yaml) {
                Some(fm) => (fm, body_start),
                None => (Frontmatter::default(), bom),
            };
        }
        if end == t.len() {
            break;
        }
        pos = end + 1;
    }
    (Frontmatter::default(), bom)
}

fn parse_yaml(yaml: &str) -> Option<Frontmatter> {
    if yaml.trim().is_empty() {
        return Some(Frontmatter::default());
    }
    let value: serde_json::Value = serde_norway::from_str(yaml).ok()?;
    let serde_json::Value::Object(map) = value else {
        return None;
    };
    let mut fm = Frontmatter::default();
    for (k, v) in map {
        match k.as_str() {
            "title" => fm.title = scalar_string(&v),
            "description" => fm.description = scalar_string(&v),
            "kind" | "type" => fm.kind = scalar_string(&v),
            "tags" => fm.tags = string_list(&v),
            "created" | "date" => fm.created = scalar_string(&v),
            "updated" | "modified" => fm.updated = scalar_string(&v),
            "pinned" => fm.pinned = v.as_bool().unwrap_or(false),
            _ => {
                fm.extra.insert(k, v);
            }
        }
    }
    Some(fm)
}

fn scalar_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.trim().to_owned()).filter(|s| !s.is_empty()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn string_list(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::Array(items) => items.iter().filter_map(scalar_string).collect(),
        serde_json::Value::String(s) => s
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------- lines

/// Byte offset → 1-based line number, via binary search over line starts.
pub struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    #[must_use]
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { starts }
    }

    #[must_use]
    pub fn line_of(&self, offset: usize) -> u32 {
        let idx = self.starts.partition_point(|&s| s <= offset);
        u32::try_from(idx.max(1)).unwrap_or(u32::MAX)
    }

    /// Byte offset of the start of the line containing `offset`.
    #[must_use]
    pub fn line_start(&self, offset: usize) -> usize {
        let idx = self.starts.partition_point(|&s| s <= offset);
        self.starts[idx.saturating_sub(1)]
    }
}

// ---------------------------------------------------------------- headings

struct Heading {
    /// Absolute byte offset of the heading's first byte.
    start: usize,
    level: u8,
    text: String,
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn collect_headings(body: &str, body_start: usize) -> Vec<Heading> {
    let mut out = Vec::new();
    let mut current: Option<(usize, u8, String)> = None;
    for (event, range) in Parser::new_ext(body, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some((
                    body_start + range.start,
                    heading_level(level),
                    String::new(),
                ));
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some((_, _, buf)) = current.as_mut() {
                    buf.push_str(&t);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((start, level, buf)) = current.take() {
                    out.push(Heading {
                        start,
                        level,
                        text: buf.split_whitespace().collect::<Vec<_>>().join(" "),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------- chunks

fn build_chunks(
    text: &str,
    body_start: usize,
    headings: &[Heading],
    lines: &LineIndex,
    counter: &impl TokenCounter,
) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let first_heading = headings.first().map_or(text.len(), |h| h.start);

    if text[body_start..first_heading].trim().is_empty() {
        // No preamble worth keeping.
    } else {
        chunks.push(make_chunk(
            text,
            body_start,
            first_heading,
            String::new(),
            0,
            lines,
            counter,
        ));
    }

    let mut stack: Vec<(u8, String)> = Vec::new();
    for (i, h) in headings.iter().enumerate() {
        while stack.last().is_some_and(|(lvl, _)| *lvl >= h.level) {
            stack.pop();
        }
        stack.push((h.level, h.text.clone()));
        let path = stack
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        let end = headings.get(i + 1).map_or(text.len(), |n| n.start);
        chunks.push(make_chunk(
            text, h.start, end, path, h.level, lines, counter,
        ));
    }
    chunks
}

fn make_chunk(
    text: &str,
    byte_start: usize,
    byte_end: usize,
    heading_path: String,
    level: u8,
    lines: &LineIndex,
    counter: &impl TokenCounter,
) -> Chunk {
    let body = &text[byte_start..byte_end];
    let trimmed_end = byte_start + body.trim_end().len();
    Chunk {
        ordinal: 0,
        heading_path,
        level,
        start_line: lines.line_of(byte_start),
        end_line: lines.line_of(trimmed_end.saturating_sub(1).max(byte_start)),
        byte_start,
        byte_end,
        tokens: counter.count(body),
    }
}

/// Split sections over [`MAX_CHUNK_TOKENS`] at the best-scoring line break
/// near [`TARGET_CHUNK_TOKENS`], never inside a fenced code block.
fn split_oversize(
    text: &str,
    chunks: Vec<Chunk>,
    lines: &LineIndex,
    counter: &impl TokenCounter,
) -> Vec<Chunk> {
    let mut out = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        if chunk.tokens <= MAX_CHUNK_TOKENS {
            out.push(chunk);
            continue;
        }
        let mut start = chunk.byte_start;
        loop {
            let remaining = &text[start..chunk.byte_end];
            if counter.count(remaining) <= MAX_CHUNK_TOKENS {
                out.push(make_chunk(
                    text,
                    start,
                    chunk.byte_end,
                    chunk.heading_path.clone(),
                    chunk.level,
                    lines,
                    counter,
                ));
                break;
            }
            let Some(cut) = best_cut(remaining, counter) else {
                out.push(make_chunk(
                    text,
                    start,
                    chunk.byte_end,
                    chunk.heading_path.clone(),
                    chunk.level,
                    lines,
                    counter,
                ));
                break;
            };
            out.push(make_chunk(
                text,
                start,
                start + cut,
                chunk.heading_path.clone(),
                chunk.level,
                lines,
                counter,
            ));
            start += cut;
        }
    }
    out
}

/// Offset (relative to `s`) of the best line start to cut at, or `None` if
/// no legal cut exists.
fn best_cut(s: &str, counter: &impl TokenCounter) -> Option<usize> {
    // Target in bytes derived from the counter's own ratio so the two agree.
    let total_tokens = counter.count(s).max(1);
    let target = s.len() * TARGET_CHUNK_TOKENS / total_tokens;
    let lo = target * 6 / 10;
    let hi = (target * 12 / 10).min(s.len());

    let mut in_fence = false;
    let mut best: Option<(usize, u32, usize)> = None; // (offset, score, distance)
    let mut fallback_after: Option<usize> = None;
    let mut offset = 0;
    let mut prev_blank = true;
    for line in s.split_inclusive('\n') {
        let trimmed = line.trim();
        let fence_line = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if offset > 0 && !in_fence {
            let score: u32 = if is_hr(trimmed) {
                60
            } else if prev_blank {
                20
            } else if trimmed.starts_with("- ")
                || trimmed.starts_with("* ")
                || trimmed.starts_with("+ ")
            {
                5
            } else {
                1
            };
            if offset >= lo && offset <= hi {
                let dist = offset.abs_diff(target);
                let better = match best {
                    None => true,
                    Some((_, bs, bd)) => score > bs || (score == bs && dist < bd),
                };
                if better {
                    best = Some((offset, score, dist));
                }
            } else if offset > hi && fallback_after.is_none() {
                fallback_after = Some(offset);
            }
        }
        if fence_line {
            in_fence = !in_fence;
        }
        prev_blank = trimmed.is_empty();
        offset += line.len();
    }
    best.map(|(o, _, _)| o)
        .or(fallback_after)
        .filter(|&o| o > 0 && o < s.len())
}

fn is_hr(trimmed: &str) -> bool {
    trimmed.len() >= 3
        && (trimmed.chars().all(|c| c == '-' || c == ' ')
            || trimmed.chars().all(|c| c == '*' || c == ' ')
            || trimmed.chars().all(|c| c == '_' || c == ' '))
        && trimmed.chars().filter(|c| !c.is_whitespace()).count() >= 3
}

/// Merge sections under [`MIN_CHUNK_TOKENS`] forward into the next section
/// so no hit is a bare heading. The next section keeps its own heading path.
fn merge_tiny(chunks: Vec<Chunk>) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::with_capacity(chunks.len());
    let mut pending: Option<Chunk> = None;
    for mut c in chunks {
        if let Some(p) = pending.take() {
            c.byte_start = p.byte_start;
            c.start_line = p.start_line;
            c.tokens += p.tokens;
        }
        if c.tokens < MIN_CHUNK_TOKENS {
            pending = Some(c);
        } else {
            out.push(c);
        }
    }
    if let Some(p) = pending {
        // Trailing tiny section: attach to the previous chunk if any, else keep.
        if let Some(last) = out.last_mut() {
            last.byte_end = p.byte_end;
            last.end_line = p.end_line;
            last.tokens += p.tokens;
        } else {
            out.push(p);
        }
    }
    out
}

// ---------------------------------------------------------------- links

fn collect_links(body: &str, body_start: usize, lines: &LineIndex) -> Vec<Link> {
    let mut out = Vec::new();
    // Byte ranges (relative to body) that are code: fenced/indented blocks and spans.
    // Wikilinks inside them are not links. pulldown-cmark splits text at every
    // bracket, so wikilinks are scanned on the raw body instead of on events.
    let mut code_ranges: Vec<(usize, usize)> = Vec::new();
    for (event, range) in Parser::new_ext(body, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) | Event::Code(_) => {
                code_ranges.push((range.start, range.end));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let dest = dest_url.as_ref();
                if is_relative_md(dest) {
                    let target = dest.split('#').next().unwrap_or(dest).to_owned();
                    out.push(Link {
                        target,
                        link_type: LinkType::Markdown,
                        line: lines.line_of(body_start + range.start),
                    });
                }
            }
            _ => {}
        }
    }
    let in_code = |pos: usize| code_ranges.iter().any(|&(s, e)| pos >= s && pos < e);

    let mut pos = 0;
    while let Some(open) = body[pos..].find("[[") {
        let open_abs = pos + open;
        let Some(close) = body[open_abs + 2..].find("]]") else {
            break;
        };
        let inner = &body[open_abs + 2..open_abs + 2 + close];
        pos = open_abs + 2 + close + 2;
        if in_code(open_abs) || inner.contains('\n') || inner.contains("[[") {
            continue;
        }
        let target = inner.split('|').next().unwrap_or(inner);
        let target = target.split('#').next().unwrap_or(target).trim();
        if target.is_empty() {
            continue;
        }
        let abs = body_start + open_abs;
        let line_start = lines.line_start(abs);
        let line_text = body[line_start - body_start..]
            .split('\n')
            .next()
            .unwrap_or("");
        let link_type =
            relation_verb(line_text, abs - line_start).map_or(LinkType::Wiki, LinkType::Relation);
        out.push(Link {
            target: target.to_owned(),
            link_type,
            line: lines.line_of(abs),
        });
    }
    out.sort_by_key(|l| l.line);
    out
}

fn is_relative_md(dest: &str) -> bool {
    !dest.contains("://")
        && !dest.starts_with('/')
        && !dest.starts_with('#')
        && dest.split('#').next().is_some_and(|p| {
            std::path::Path::new(p)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        })
}

/// If `line` is `- verb [[...` with the wikilink at `col`, return the verb.
fn relation_verb(line: &str, col: usize) -> Option<String> {
    let before = line.get(..col)?;
    let t = before.trim_start();
    let t = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .or_else(|| t.strip_prefix("+ "))?;
    let verb = t.trim();
    let verb = verb
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(verb);
    if verb.is_empty() || verb.len() > 40 {
        return None;
    }
    if verb
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == ' ')
    {
        Some(verb.to_lowercase().replace(' ', "_"))
    } else {
        None
    }
}

// ---------------------------------------------------------------- observations

fn collect_observations(body: &str, body_start: usize, lines: &LineIndex) -> Vec<Observation> {
    let mut out = Vec::new();
    let mut in_fence = false;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        } else if !in_fence
            && !trimmed.starts_with('>')
            && let Some(obs) = parse_observation(trimmed, lines.line_of(body_start + offset))
        {
            out.push(obs);
        }
        offset += line.len();
    }
    out
}

fn parse_observation(trimmed: &str, line: u32) -> Option<Observation> {
    let rest = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))?;
    let rest = rest.strip_prefix('[')?;
    let close = rest.find(']')?;
    let category = &rest[..close];
    let after = &rest[close + 1..];
    // Reject checkboxes, empty, link syntax, wikilinks, timecodes.
    if category.is_empty()
        || category.trim().is_empty()
        || matches!(category, "x" | "X" | "-" | " ")
        || category.starts_with('[')
        || after.starts_with('(')
        || after.starts_with(']')
        || category.chars().all(|c| c.is_ascii_digit() || c == ':')
        || category.contains(['[', ']'])
    {
        return None;
    }
    let text = after.trim();
    if text.is_empty() {
        return None;
    }
    let tags = text
        .split_whitespace()
        .filter_map(|w| w.strip_prefix('#'))
        .filter(|t| t.chars().next().is_some_and(char::is_alphanumeric))
        .map(|t| {
            t.trim_end_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|t| !t.is_empty())
        .collect();
    Some(Observation {
        line,
        category: category.trim().to_owned(),
        text: text.to_owned(),
        tags,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_absent() {
        let (fm, start) = split_frontmatter("# Title\nbody\n");
        assert_eq!(fm, Frontmatter::default());
        assert_eq!(start, 0);
    }

    #[test]
    fn frontmatter_lifts_known_keys_and_keeps_extra() {
        let text = "---\ntitle: Hello\ntype: decision\ntags: [a, B]\ncreated: 2026-10-27\npinned: true\ncustom: 7\ndescription: one-liner\n---\n# H\n";
        let (fm, start) = split_frontmatter(text);
        assert_eq!(fm.title.as_deref(), Some("Hello"));
        assert_eq!(fm.kind.as_deref(), Some("decision"));
        assert_eq!(fm.tags, vec!["a", "B"]);
        assert_eq!(fm.created.as_deref(), Some("2026-10-27"));
        assert!(fm.pinned);
        assert_eq!(fm.description.as_deref(), Some("one-liner"));
        assert_eq!(fm.extra.get("custom"), Some(&serde_json::json!(7)));
        assert_eq!(&text[start..], "# H\n");
    }

    #[test]
    fn frontmatter_comma_tags_and_nested_metadata() {
        let text =
            "---\nname: x\ndescription: d\nmetadata:\n  type: user\ntags: one, two\n---\nbody";
        let (fm, _) = split_frontmatter(text);
        assert_eq!(fm.tags, vec!["one", "two"]);
        assert_eq!(fm.extra.get("name"), Some(&serde_json::json!("x")));
        assert_eq!(fm.extra["metadata"]["type"], "user");
    }

    #[test]
    fn malformed_frontmatter_is_body() {
        let text = "---\n: : not yaml [\n---\n# H\n";
        let (fm, start) = split_frontmatter(text);
        assert_eq!(fm, Frontmatter::default());
        assert_eq!(start, 0);
        let text = "---\nnever closed\n# H\n";
        let (_, start) = split_frontmatter(text);
        assert_eq!(start, 0);
    }

    #[test]
    fn bom_is_skipped() {
        let text = "\u{feff}---\ntitle: T\n---\nbody";
        let (fm, start) = split_frontmatter(text);
        assert_eq!(fm.title.as_deref(), Some("T"));
        assert_eq!(&text[start..], "body");
    }

    #[test]
    fn title_fallback_chain() {
        assert_eq!(parse("---\ntitle: FM\n---\n# H1\n", "stem").title, "FM");
        assert_eq!(parse("intro\n# H1 here\n## H2\n", "stem").title, "H1 here");
        assert_eq!(parse("## only h2\ntext\n", "stem").title, "stem");
        assert_eq!(parse("", "stem").title, "stem");
    }

    #[test]
    fn chunks_follow_headings_with_paths_and_lines() {
        let text =
            "pre\n\n# A\na body\n\n## B\nb body line\nmore\n\n### C\nc body\n\n## D\nd body\n";
        let p = parse(text, "f");
        // Tiny-merge collapses most of these; check the structural pass directly.
        let lines = LineIndex::new(text);
        let heads = collect_headings(text, 0);
        let chunks = build_chunks(text, 0, &heads, &lines, &ByteHeuristic);
        let paths: Vec<_> = chunks.iter().map(|c| c.heading_path.as_str()).collect();
        assert_eq!(paths, vec!["", "A", "A / B", "A / B / C", "A / D"]);
        assert_eq!(chunks[0].level, 0);
        assert_eq!(chunks[1].start_line, 3);
        assert_eq!(chunks[2].start_line, 6);
        assert_eq!(chunks[2].end_line, 8);
        assert_eq!(
            &text[chunks[3].byte_start..chunks[3].byte_end],
            "### C\nc body\n\n"
        );
        assert_eq!(chunks[4].end_line, 14);
        // Full parse keeps every byte covered exactly once, in order.
        let mut covered = 0;
        for c in &p.chunks {
            assert_eq!(c.byte_start, covered, "gap before {:?}", c.heading_path);
            covered = c.byte_end;
        }
        assert_eq!(covered, text.len());
    }

    #[test]
    fn hash_inside_code_fence_is_not_a_heading() {
        let text = "# Real\n\n```sh\n# not a heading\necho hi\n```\n\nSetext\n======\n\ntext\n";
        let heads = collect_headings(text, 0);
        let names: Vec<_> = heads.iter().map(|h| (h.level, h.text.as_str())).collect();
        assert_eq!(names, vec![(1, "Real"), (1, "Setext")]);
    }

    #[test]
    fn oversize_section_splits_outside_fences() {
        let mut text = String::from("# Big\n\n");
        for i in 0..60 {
            use std::fmt::Write as _;
            let _ = write!(
                text,
                "Paragraph {i} with enough words to count as tokens here.\n\n"
            );
            if i == 30 {
                text.push_str("```\n");
                for _ in 0..40 {
                    text.push_str("code line inside fence that must not be cut\n");
                }
                text.push_str("```\n\n");
            }
        }
        let p = parse(&text, "f");
        assert!(
            p.chunks.len() >= 2,
            "expected a split, got {}",
            p.chunks.len()
        );
        for c in &p.chunks {
            assert!(c.tokens <= MAX_CHUNK_TOKENS, "chunk too big: {}", c.tokens);
            assert_eq!(c.heading_path, "Big");
            let body = &text[c.byte_start..c.byte_end];
            let fences = body.matches("```").count();
            assert_eq!(fences % 2, 0, "cut inside a fence");
        }
        let mut covered = 0;
        for c in &p.chunks {
            assert_eq!(c.byte_start, covered);
            covered = c.byte_end;
        }
        assert_eq!(covered, text.len());
    }

    #[test]
    fn tiny_sections_merge_forward() {
        let long = "word ".repeat(80);
        let text = format!("# A\n\n## B\n{long}\n\n## C\n{long}\n");
        let p = parse(&text, "f");
        assert_eq!(p.chunks.len(), 2);
        assert_eq!(p.chunks[0].heading_path, "A / B");
        assert_eq!(
            p.chunks[0].byte_start, 0,
            "A's heading bytes belong to the merged chunk"
        );
        assert_eq!(p.chunks[0].start_line, 1);
        assert_eq!(p.chunks[1].heading_path, "A / C");
    }

    #[test]
    fn links_wiki_md_and_relations() {
        let text = "See [[foo-bar|alias]] and [[baz#sec]].\n- fixes [[Other Note]]\n- \"relates to\" [[Third]]\n[doc](../x/y.md#frag) [web](https://e.com/a.md) [abs](/abs.md)\n`[[in code]]`\n```\n[[in fence]]\n```\n";
        let p = parse(text, "f");
        let got: Vec<_> = p
            .links
            .iter()
            .map(|l| (l.target.as_str(), l.link_type.as_db_str(), l.line))
            .collect();
        assert_eq!(
            got,
            vec![
                ("foo-bar", "wiki".to_owned(), 1),
                ("baz", "wiki".to_owned(), 1),
                ("Other Note", "rel:fixes".to_owned(), 2),
                ("Third", "rel:relates_to".to_owned(), 3),
                ("../x/y.md", "md".to_owned(), 4),
            ]
        );
    }

    #[test]
    fn observations_and_tags() {
        let text = "---\ntags: [Alpha]\n---\n- [decision] use sqlite #storage #Design\n- [x] done task\n- [ ] todo\n- [00:01:23] transcript\n- [link](http://x)\n- [[wiki]]\n> - [quoted] no\n```\n- [code] no\n```\n- [note]   \n";
        let p = parse(text, "f");
        assert_eq!(p.observations.len(), 1);
        let o = &p.observations[0];
        assert_eq!(o.category, "decision");
        assert_eq!(o.text, "use sqlite #storage #Design");
        assert_eq!(o.tags, vec!["storage", "design"]);
        assert_eq!(o.line, 4);
        assert_eq!(p.tags, vec!["alpha", "design", "storage"]);
    }

    #[test]
    fn line_index_maps_offsets() {
        let li = LineIndex::new("ab\ncd\n\nef");
        assert_eq!(li.line_of(0), 1);
        assert_eq!(li.line_of(2), 1);
        assert_eq!(li.line_of(3), 2);
        assert_eq!(li.line_of(6), 3);
        assert_eq!(li.line_of(7), 4);
        assert_eq!(li.line_start(4), 3);
    }
}
