//! Command implementations shared by CLI and MCP server.

use crate::config::{Config, expand_tilde};
use crate::corpus::Corpus;
use crate::search::ripgrep::RipgrepBackend;
use crate::search::{SearchBackend, SearchOptions, SearchResult};

/// Search across all configured corpora.
///
/// # Errors
///
/// Returns an error if config loading fails or search backend fails.
pub fn search(
    query: &str,
    limit: usize,
    category: Option<String>,
) -> anyhow::Result<Vec<SearchResult>> {
    let config = Config::load()?;
    let backend = RipgrepBackend::new();

    let options = SearchOptions {
        limit: Some(limit),
        category,
    };

    let mut all_results = Vec::new();

    for path_str in &config.corpus.paths {
        let path = expand_tilde(path_str);

        if !path.exists() {
            continue;
        }

        if let Ok(corpus) = Corpus::load(&path)
            && let Ok(results) = backend.search(query, &corpus, &options)
        {
            all_results.extend(results);
        }
    }

    all_results.truncate(limit);
    Ok(all_results)
}

/// List documents from all configured corpora.
///
/// # Errors
///
/// Returns an error if config loading fails.
pub fn list(category: Option<&str>) -> anyhow::Result<Vec<DocumentInfo>> {
    let config = Config::load()?;
    let mut documents = Vec::new();

    for path_str in &config.corpus.paths {
        let path = expand_tilde(path_str);

        if !path.exists() {
            continue;
        }

        if let Ok(corpus) = Corpus::load(&path) {
            for doc in corpus.documents() {
                if let Some(cat) = category
                    && doc.category != cat
                {
                    continue;
                }

                documents.push(DocumentInfo {
                    title: doc.title.clone(),
                    category: doc.category.clone(),
                    tags: doc.tags.clone(),
                    path: corpus.resolve_document_path(doc),
                });
            }
        }
    }

    Ok(documents)
}

/// Get the contents of a document by its path.
///
/// # Errors
///
/// Returns an error if the document is not found or cannot be read.
pub fn get(doc_path: &str) -> anyhow::Result<String> {
    let config = Config::load()?;

    for path_str in &config.corpus.paths {
        let corpus_path = expand_tilde(path_str);

        if !corpus_path.exists() {
            continue;
        }

        if let Ok(corpus) = Corpus::load(&corpus_path) {
            for doc in corpus.documents() {
                if doc.path.to_string_lossy() == doc_path {
                    let full_path = corpus.resolve_document_path(doc);
                    return std::fs::read_to_string(&full_path).map_err(Into::into);
                }
            }
        }
    }

    anyhow::bail!("Document not found: {doc_path}")
}

/// Information about a document for listing.
#[derive(Debug, Clone)]
pub struct DocumentInfo {
    pub title: String,
    pub category: String,
    pub tags: Vec<String>,
    pub path: std::path::PathBuf,
}
