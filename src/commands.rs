//! Command implementations shared by CLI and MCP server.

use std::path::PathBuf;

use crate::config::{Config, expand_tilde};
use crate::corpus::{Corpus, Document};
use crate::search::ripgrep::RipgrepBackend;
use crate::search::{SearchBackend, SearchOptions, SearchResult};
use crate::storage::StorageBackend;
use crate::storage::local::LocalStorageBackend;

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

/// Information about a document with resolved path.
///
/// Used for list and add results. The path is absolute (resolved from corpus root).
#[derive(Debug, Clone)]
pub struct DocumentInfo {
    pub title: String,
    pub category: String,
    pub tags: Vec<String>,
    pub path: PathBuf,
}

/// Add a new document to the knowledge corpus.
///
/// # Errors
///
/// Returns an error if config loading fails, storage operations fail,
/// or no corpus path is configured.
pub fn add(
    title: &str,
    content: &str,
    category: &str,
    tags: Vec<String>,
) -> anyhow::Result<DocumentInfo> {
    let config = Config::load()?;

    let corpus_path = config
        .corpus
        .paths
        .first()
        .ok_or_else(|| anyhow::anyhow!("No corpus path configured"))?;

    let root = expand_tilde(corpus_path);
    let storage = LocalStorageBackend::new(root.clone());

    let mut manifest = storage.read_manifest()?;

    let slug = slugify(title);
    let doc_path = PathBuf::from(category).join(format!("{slug}.md"));

    if storage.exists(&doc_path) {
        anyhow::bail!("Document already exists: {}", doc_path.display());
    }

    storage.write_document(&doc_path, content)?;

    let document = Document {
        path: doc_path.clone(),
        title: title.to_string(),
        category: category.to_string(),
        tags: tags.clone(),
    };

    manifest.documents.push(document);
    storage.write_manifest(&manifest)?;

    Ok(DocumentInfo {
        title: title.to_string(),
        category: category.to_string(),
        tags,
        path: root.join(&doc_path),
    })
}

/// Convert a title to a URL-safe slug.
fn slugify(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}
