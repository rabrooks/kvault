use std::path::PathBuf;

use crate::corpus::Corpus;

#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub limit: Option<usize>,
    pub category: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub path: PathBuf,
    pub title: String,
    pub matched_line: String,
    pub line_number: usize,
    pub score: Option<f32>,
}

pub trait SearchBackend: Send + Sync {
    fn search(
        &self,
        query: &str,
        corpus: &Corpus,
        options: &SearchOptions,
    ) -> anyhow::Result<Vec<SearchResult>>;

    fn index(&self, corpus: &Corpus) -> anyhow::Result<()>;

    fn needs_indexing(&self) -> bool;
}
