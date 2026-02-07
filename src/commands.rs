//! Command implementations shared by CLI and MCP server.

use std::path::{Path, PathBuf};

use crate::cli::Backend;
use crate::config::{Config, expand_tilde};
use crate::corpus::{Corpus, Document};
use crate::search::ripgrep::RipgrepBackend;
use crate::search::{SearchBackend, SearchOptions, SearchResult};
use crate::storage::StorageBackend;
use crate::storage::local::LocalStorageBackend;

#[cfg(feature = "ranked")]
use crate::search::tantivy::{IndexMode, TantivyBackend};

/// Maximum length for user-provided strings (title, category, etc.).
const MAX_INPUT_LENGTH: usize = 200;

/// Validate that a path is safely contained within a root directory.
///
/// Returns the full path if valid, or an error if the path would escape
/// the root directory (e.g., via `..` components or symlink tricks).
///
/// # Security
///
/// This function validates paths for new files that may not exist yet.
/// It walks up the path hierarchy to find an existing ancestor and
/// verifies that ancestor is within the root directory.
fn validate_path_within_root(root: &Path, relative_path: &Path) -> anyhow::Result<PathBuf> {
    // Reject paths with parent directory references
    for component in relative_path.components() {
        if let std::path::Component::ParentDir = component {
            anyhow::bail!("Invalid path: contains '..' component");
        }
    }

    // Reject absolute paths
    if relative_path.is_absolute() {
        anyhow::bail!("Invalid path: must be relative");
    }

    // Reject empty paths
    if relative_path.as_os_str().is_empty() {
        anyhow::bail!("Invalid path: cannot be empty");
    }

    let full_path = root.join(relative_path);

    // Canonicalize the root to get the real path
    let canonical_root = root
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("Cannot access corpus root {}: {}", root.display(), e))?;

    // Walk up the path hierarchy to find an existing ancestor
    // This handles the case where we're creating new directories
    let mut check_path = full_path.as_path();
    loop {
        if check_path.exists() {
            let canonical_check = check_path.canonicalize()?;
            if !canonical_check.starts_with(&canonical_root) {
                anyhow::bail!("Path escapes corpus root: {}", relative_path.display());
            }
            break;
        }

        // Move up to parent
        match check_path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                check_path = parent;
            }
            _ => {
                // Reached filesystem root without finding existing ancestor
                // This means the root path itself doesn't exist
                anyhow::bail!(
                    "Cannot validate path: no existing ancestor found for {}",
                    relative_path.display()
                );
            }
        }
    }

    Ok(full_path)
}

/// Validate a user-provided identifier (category, title slug component).
///
/// Only allows alphanumeric characters, hyphens, and underscores.
fn validate_identifier(value: &str, field_name: &str) -> anyhow::Result<()> {
    if value.is_empty() {
        anyhow::bail!("{field_name} cannot be empty");
    }

    if value.len() > MAX_INPUT_LENGTH {
        anyhow::bail!(
            "{field_name} too long: {} chars (max {MAX_INPUT_LENGTH})",
            value.len()
        );
    }

    // Must start with alphanumeric
    if !value.chars().next().is_some_and(char::is_alphanumeric) {
        anyhow::bail!("{field_name} must start with a letter or number");
    }

    // Only allow safe characters
    for c in value.chars() {
        if !c.is_alphanumeric() && c != '-' && c != '_' {
            anyhow::bail!(
                "{field_name} contains invalid character: '{c}' \
                (only letters, numbers, hyphens, and underscores allowed)"
            );
        }
    }

    Ok(())
}

/// Parse comma-separated tags into a vector.
///
/// Splits the input on commas, trims whitespace, and filters out empty strings.
/// Does not validate tag format - callers should validate if needed.
#[must_use]
pub fn parse_tags(tags: Option<String>) -> Vec<String> {
    tags.map(|t| {
        t.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

/// Search across all configured corpora.
///
/// # Arguments
///
/// * `query` - The search query string
/// * `limit` - Maximum number of results to return
/// * `category` - Optional category filter
/// * `case_sensitive` - Use case-sensitive matching (default is case-insensitive)
/// * `backend` - Search backend to use (ripgrep, ranked, or auto)
/// * `fuzzy` - Optional fuzzy search edit distance (only for ranked backend)
///
/// # Returns
///
/// A vector of search results from all configured corpora, sorted by relevance.
///
/// # Errors
///
/// Returns an error if config loading fails or all search operations fail.
/// Individual corpus failures are logged but don't fail the entire search.
pub fn search(
    query: &str,
    limit: usize,
    category: Option<String>,
    case_sensitive: bool,
    backend: Backend,
    fuzzy: Option<u8>,
) -> anyhow::Result<Vec<SearchResult>> {
    let config = Config::load()?;

    let options = SearchOptions {
        limit: Some(limit),
        category,
        case_sensitive,
        fuzzy,
    };

    let mut all_results = Vec::new();
    let mut errors = Vec::new();

    for path_str in &config.corpus.paths {
        let path = expand_tilde(path_str);

        if !path.exists() {
            continue;
        }

        match Corpus::load(&path) {
            Ok(corpus) => {
                let results = search_corpus(query, &corpus, &options, backend);
                match results {
                    Ok(results) => all_results.extend(results),
                    Err(e) => errors.push(format!("Search in {}: {e}", path.display())),
                }
            }
            Err(e) => errors.push(format!("Load {}: {e}", path.display())),
        }
    }

    // If we got no results and had errors, report them
    if all_results.is_empty() && !errors.is_empty() {
        anyhow::bail!("Search failed:\n  {}", errors.join("\n  "));
    }

    // Sort by score if available (ranked backend), otherwise keep order
    all_results.sort_by(|a, b| match (b.score, a.score) {
        (Some(b_score), Some(a_score)) => b_score
            .partial_cmp(&a_score)
            .unwrap_or(std::cmp::Ordering::Equal),
        _ => std::cmp::Ordering::Equal,
    });

    all_results.truncate(limit);
    Ok(all_results)
}

/// Search a single corpus using the specified backend.
fn search_corpus(
    query: &str,
    corpus: &Corpus,
    options: &SearchOptions,
    backend: Backend,
) -> anyhow::Result<Vec<SearchResult>> {
    match backend {
        Backend::Ripgrep => {
            let rg = RipgrepBackend::new();
            rg.search(query, corpus, options)
        }
        #[cfg(feature = "ranked")]
        Backend::Ranked => {
            if !TantivyBackend::index_exists(corpus) {
                anyhow::bail!(
                    "No index found for corpus at {}. Run `kvault index` first.",
                    corpus.root.display()
                );
            }
            let tantivy = TantivyBackend::open_for_corpus(corpus, IndexMode::ReadOnly)?;
            tantivy.search(query, corpus, options)
        }
        Backend::Auto => {
            // Auto-select: use Tantivy if index exists, otherwise ripgrep
            #[cfg(feature = "ranked")]
            if TantivyBackend::index_exists(corpus) {
                let tantivy = TantivyBackend::open_for_corpus(corpus, IndexMode::ReadOnly)?;
                return tantivy.search(query, corpus, options);
            }

            let rg = RipgrepBackend::new();
            rg.search(query, corpus, options)
        }
    }
}

/// Build or rebuild the search index for all configured corpora.
///
/// # Returns
///
/// The number of corpora successfully indexed.
///
/// # Errors
///
/// Returns an error if config loading fails or all index operations fail.
#[cfg(feature = "ranked")]
pub fn index_all() -> anyhow::Result<usize> {
    let config = Config::load()?;
    let mut indexed_count = 0;
    let mut errors = Vec::new();

    for path_str in &config.corpus.paths {
        let path = expand_tilde(path_str);

        if !path.exists() {
            continue;
        }

        match Corpus::load(&path) {
            Ok(corpus) => match TantivyBackend::open_for_corpus(&corpus, IndexMode::ReadWrite) {
                Ok(backend) => match backend.index(&corpus) {
                    Ok(()) => {
                        println!("Indexed: {}", path.display());
                        indexed_count += 1;
                    }
                    Err(e) => errors.push(format!("Index {}: {e}", path.display())),
                },
                Err(e) => errors.push(format!("Open index {}: {e}", path.display())),
            },
            Err(e) => errors.push(format!("Load {}: {e}", path.display())),
        }
    }

    if indexed_count == 0 && !errors.is_empty() {
        anyhow::bail!("Indexing failed:\n  {}", errors.join("\n  "));
    }

    if !errors.is_empty() {
        eprintln!("Warnings:\n  {}", errors.join("\n  "));
    }

    Ok(indexed_count)
}

/// List documents from all configured corpora.
///
/// # Arguments
///
/// * `category` - Optional category filter
///
/// # Returns
///
/// A vector of document info from all configured corpora.
///
/// # Errors
///
/// Returns an error if config loading fails or all corpora fail to load.
/// Individual corpus failures are logged but don't fail the entire list.
pub fn list(category: Option<&str>) -> anyhow::Result<Vec<DocumentInfo>> {
    let config = Config::load()?;
    let mut documents = Vec::new();
    let mut errors = Vec::new();

    for path_str in &config.corpus.paths {
        let path = expand_tilde(path_str);

        if !path.exists() {
            continue;
        }

        match Corpus::load(&path) {
            Ok(corpus) => {
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
            Err(e) => errors.push(format!("Load {}: {e}", path.display())),
        }
    }

    // If we got no documents and had errors, report them
    if documents.is_empty() && !errors.is_empty() {
        anyhow::bail!("List failed:\n  {}", errors.join("\n  "));
    }

    Ok(documents)
}

/// Get the contents of a document by its path.
///
/// # Arguments
///
/// * `doc_path` - Relative path to the document (e.g., "aws/lambda-patterns.md")
///
/// # Returns
///
/// The document content as a string.
///
/// # Errors
///
/// Returns an error if:
/// - The document is not found in any corpus
/// - The path is invalid or attempts path traversal
/// - The document cannot be read
pub fn get(doc_path: &str) -> anyhow::Result<String> {
    let config = Config::load()?;

    // Early validation of the requested path
    let requested_path = PathBuf::from(doc_path);
    if requested_path.to_string_lossy().contains("..") {
        anyhow::bail!("Invalid document path: contains '..' component");
    }

    for path_str in &config.corpus.paths {
        let corpus_path = expand_tilde(path_str);

        if !corpus_path.exists() {
            continue;
        }

        if let Ok(corpus) = Corpus::load(&corpus_path) {
            for doc in corpus.documents() {
                if doc.path.to_string_lossy() == doc_path {
                    // Validate the resolved path stays within corpus root
                    let full_path = validate_path_within_root(&corpus.root, &doc.path)?;
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
    /// Human-readable document title.
    pub title: String,
    /// Category for grouping (e.g., "aws", "rust").
    pub category: String,
    /// Tags for additional classification.
    pub tags: Vec<String>,
    /// Absolute path to the document file.
    pub path: PathBuf,
}

/// Add a new document to the knowledge corpus.
///
/// # Arguments
///
/// * `title` - Human-readable document title
/// * `content` - Document content (markdown)
/// * `category` - Category for grouping (e.g., "aws", "rust")
/// * `tags` - Optional tags for classification
///
/// # Returns
///
/// Information about the created document including its path.
///
/// # Errors
///
/// Returns an error if:
/// - No corpus path is configured
/// - Title or category contain invalid characters
/// - Document already exists
/// - Storage operations fail
pub fn add(
    title: &str,
    content: &str,
    category: &str,
    tags: Vec<String>,
) -> anyhow::Result<DocumentInfo> {
    // Validate inputs before any file operations
    if title.is_empty() {
        anyhow::bail!("Title cannot be empty");
    }
    if title.len() > MAX_INPUT_LENGTH {
        anyhow::bail!(
            "Title too long: {} chars (max {MAX_INPUT_LENGTH})",
            title.len()
        );
    }

    validate_identifier(category, "Category")?;

    // Validate tags
    for tag in &tags {
        if !tag.is_empty() {
            validate_identifier(tag, "Tag")?;
        }
    }

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

    // Validate the constructed path is safe
    validate_path_within_root(&root, &doc_path)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    mod slugify_tests {
        use super::*;

        #[test]
        fn simple_title() {
            assert_eq!(slugify("Hello World"), "hello-world");
        }

        #[test]
        fn title_with_special_chars() {
            assert_eq!(
                slugify("AWS Lambda: Best Practices!"),
                "aws-lambda-best-practices"
            );
        }

        #[test]
        fn title_with_numbers() {
            assert_eq!(slugify("Top 10 Rust Tips"), "top-10-rust-tips");
        }

        #[test]
        fn title_with_multiple_spaces() {
            assert_eq!(slugify("Hello    World"), "hello-world");
        }

        #[test]
        fn empty_title() {
            assert_eq!(slugify(""), "");
        }

        #[test]
        fn unicode_title() {
            // Unicode alphanumeric chars are preserved
            assert_eq!(slugify("Café"), "café");
        }
    }

    mod validate_identifier_tests {
        use super::*;

        #[test]
        fn valid_identifier() {
            assert!(validate_identifier("aws", "Category").is_ok());
            assert!(validate_identifier("rust-tips", "Category").is_ok());
            assert!(validate_identifier("my_category", "Category").is_ok());
            assert!(validate_identifier("Category123", "Category").is_ok());
        }

        #[test]
        fn empty_identifier() {
            let result = validate_identifier("", "Category");
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("cannot be empty"));
        }

        #[test]
        fn identifier_with_invalid_chars() {
            let result = validate_identifier("aws/lambda", "Category");
            assert!(result.is_err());
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("invalid character")
            );
        }

        #[test]
        fn identifier_starting_with_hyphen() {
            let result = validate_identifier("-invalid", "Category");
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("must start with"));
        }

        #[test]
        fn identifier_with_spaces() {
            let result = validate_identifier("my category", "Category");
            assert!(result.is_err());
        }

        #[test]
        fn identifier_too_long() {
            let long_value = "a".repeat(MAX_INPUT_LENGTH + 1);
            let result = validate_identifier(&long_value, "Category");
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("too long"));
        }

        #[test]
        fn identifier_with_path_traversal() {
            let result = validate_identifier("../etc", "Category");
            assert!(result.is_err());
        }
    }

    mod parse_tags_tests {
        use super::*;

        #[test]
        fn parse_single_tag() {
            assert_eq!(parse_tags(Some("rust".to_string())), vec!["rust"]);
        }

        #[test]
        fn parse_multiple_tags() {
            assert_eq!(
                parse_tags(Some("rust, aws, lambda".to_string())),
                vec!["rust", "aws", "lambda"]
            );
        }

        #[test]
        fn parse_tags_with_whitespace() {
            assert_eq!(
                parse_tags(Some("  rust  ,  aws  ".to_string())),
                vec!["rust", "aws"]
            );
        }

        #[test]
        fn parse_empty_tags() {
            let empty: Vec<String> = vec![];
            assert_eq!(parse_tags(None), empty);
            assert_eq!(parse_tags(Some(String::new())), empty);
        }

        #[test]
        fn parse_tags_filters_empty() {
            assert_eq!(
                parse_tags(Some("rust,,aws,".to_string())),
                vec!["rust", "aws"]
            );
        }
    }
}
