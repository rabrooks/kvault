//! Search backend trait and types.

pub mod ripgrep;

#[cfg(feature = "ranked")]
pub mod tantivy;

use std::path::PathBuf;

use crate::corpus::Corpus;

/// Options for filtering and limiting search results.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// Maximum number of results to return.
    pub limit: Option<usize>,
    /// Filter results to this category only.
    pub category: Option<String>,
    /// Use case-sensitive matching (default is case-insensitive).
    pub case_sensitive: bool,
    /// Fuzzy search edit distance (0-2). None means exact matching.
    /// Only used by backends that support fuzzy search (e.g., Tantivy).
    pub fuzzy: Option<u8>,
}

/// A single search result with match context.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Absolute path to the matched file.
    pub path: PathBuf,
    /// Document title from manifest, or filename if not in manifest.
    pub title: String,
    /// The line containing the match (trimmed).
    pub matched_line: String,
    /// Line number where the match occurred (1-indexed).
    pub line_number: usize,
    /// Relevance score (populated by ranking backends like Tantivy).
    pub score: Option<f32>,
}

/// Trait for search backends (ripgrep, tantivy, etc.).
pub trait SearchBackend: Send + Sync {
    /// Search the corpus for documents matching the query.
    ///
    /// # Errors
    ///
    /// Returns an error if the search operation fails.
    fn search(
        &self,
        query: &str,
        corpus: &Corpus,
        options: &SearchOptions,
    ) -> anyhow::Result<Vec<SearchResult>>;

    /// Build or update the search index for the corpus.
    ///
    /// # Errors
    ///
    /// Returns an error if indexing fails.
    fn index(&self, corpus: &Corpus) -> anyhow::Result<()>;

    /// Returns true if this backend requires indexing before search.
    fn needs_indexing(&self) -> bool;
}
