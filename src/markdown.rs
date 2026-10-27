//! Markdown parsing: frontmatter, heading-level chunks, links, tags.
//!
//! Implemented in M1. Types are declared now so the store and index modules
//! can be written against them.

use std::collections::BTreeMap;

/// Parsed `YAML` frontmatter. Every value is kept as `JSON` so unknown keys are
/// preserved and filterable; well-known keys are lifted into fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frontmatter {
    pub title: Option<String>,
    pub kind: Option<String>,
    pub tags: Vec<String>,
    pub created: Option<String>,
    pub updated: Option<String>,
    pub pinned: bool,
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// One heading section of a document, addressed by line and byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub ordinal: u32,
    pub heading_path: String,
    pub level: u8,
    pub start_line: u32,
    pub end_line: u32,
    pub byte_start: usize,
    pub byte_end: usize,
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
    /// `- verb [[Target]]` (basic-memory convention)
    Relation(String),
}
