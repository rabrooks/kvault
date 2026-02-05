//! Search backend trait and types.

use std::path::PathBuf;

use crate::corpus::Corpus;

/// Options for filtering and limiting search results.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub limit: Option<usize>,
    pub category: Option<String>,
}

/// A single search result with match context.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub path: PathBuf,
    pub title: String,
    pub matched_line: String,
    pub line_number: usize,
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
