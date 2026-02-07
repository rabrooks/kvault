//! Tantivy-based search backend with BM25 ranking.
//!
//! Provides ranked search results using the Tantivy full-text search engine.
//! Supports fuzzy matching for typo-tolerant queries.

use std::path::{Path, PathBuf};

use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::{BooleanQuery, FuzzyTermQuery, Occur, QueryParser, TermQuery};
use tantivy::schema::{FAST, Field, STORED, STRING, Schema, TEXT, Value};
use tantivy::{Index, IndexReader, IndexSettings, IndexWriter, ReloadPolicy, Term};

use crate::corpus::Corpus;
use crate::search::{SearchBackend, SearchOptions, SearchResult};

/// Default index directory name within corpus root.
const INDEX_DIR: &str = ".index";

/// Default heap size for index writer (50MB).
const WRITER_HEAP_SIZE: usize = 50_000_000;

/// Index mode controls whether the backend can write to the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexMode {
    /// Index is writable (local corpus, client builds index).
    ReadWrite,
    /// Index is read-only (synced index from S3, no writes allowed).
    ReadOnly,
}

/// Schema field handles for the Tantivy index.
#[derive(Debug, Clone)]
struct SchemaFields {
    title: Field,
    content: Field,
    category: Field,
    tags: Field,
    path: Field,
}

/// Tantivy-based search backend with BM25 ranking.
///
/// Provides ranked search results using the Tantivy full-text search engine.
/// Supports both read-write mode (for local indexing) and read-only mode
/// (for synced indexes from S3).
pub struct TantivyBackend {
    index: Index,
    reader: IndexReader,
    fields: SchemaFields,
    mode: IndexMode,
    index_path: PathBuf,
}

impl TantivyBackend {
    /// Build the Tantivy schema for knowledge documents.
    ///
    /// Fields:
    /// - `title`: Searchable text, stored for display
    /// - `content`: Searchable text (document body)
    /// - `category`: Exact match filter, stored
    /// - `tags`: Stored for display (space-separated)
    /// - `path`: Stored for result retrieval
    fn build_schema() -> (Schema, SchemaFields) {
        let mut schema_builder = Schema::builder();

        let title = schema_builder.add_text_field("title", TEXT | STORED);
        let content = schema_builder.add_text_field("content", TEXT);
        let category = schema_builder.add_text_field("category", STRING | STORED | FAST);
        let tags = schema_builder.add_text_field("tags", STORED);
        let path = schema_builder.add_text_field("path", STRING | STORED);

        let schema = schema_builder.build();
        let fields = SchemaFields {
            title,
            content,
            category,
            tags,
            path,
        };

        (schema, fields)
    }

    /// Open or create a Tantivy index at the specified path.
    ///
    /// # Arguments
    ///
    /// * `index_path` - Path to the index directory
    /// * `mode` - Whether to open in read-write or read-only mode
    ///
    /// # Errors
    ///
    /// Returns an error if the index cannot be opened or created.
    pub fn open(index_path: &Path, mode: IndexMode) -> anyhow::Result<Self> {
        // Open or create index first, then extract schema from the actual index
        let index = if index_path.exists() {
            // Open existing index - use its stored schema
            let directory = MmapDirectory::open(index_path)?;
            Index::open(directory)?
        } else if mode == IndexMode::ReadWrite {
            // Create new index with our schema
            let (schema, _) = Self::build_schema();
            std::fs::create_dir_all(index_path)?;
            let directory = MmapDirectory::open(index_path)?;
            Index::create(directory, schema, IndexSettings::default())?
        } else {
            anyhow::bail!(
                "Index not found at {} (read-only mode)",
                index_path.display()
            );
        };

        // Get schema from the actual index (handles schema evolution correctly)
        let schema = index.schema();
        let fields = SchemaFields {
            title: schema.get_field("title")?,
            content: schema.get_field("content")?,
            category: schema.get_field("category")?,
            tags: schema.get_field("tags")?,
            path: schema.get_field("path")?,
        };

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;

        Ok(Self {
            index,
            reader,
            fields,
            mode,
            index_path: index_path.to_path_buf(),
        })
    }

    /// Open or create a Tantivy index for a corpus.
    ///
    /// The index is stored in `.index/` within the corpus root.
    ///
    /// # Errors
    ///
    /// Returns an error if the index cannot be opened or created.
    pub fn open_for_corpus(corpus: &Corpus, mode: IndexMode) -> anyhow::Result<Self> {
        let index_path = corpus.root.join(INDEX_DIR);
        Self::open(&index_path, mode)
    }

    /// Check if the index exists for a corpus.
    #[must_use]
    pub fn index_exists(corpus: &Corpus) -> bool {
        corpus.root.join(INDEX_DIR).exists()
    }

    /// Get the index path.
    #[must_use]
    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    /// Build a fuzzy query that searches both title and content fields.
    ///
    /// Creates `FuzzyTermQuery` for each word in the query string, allowing
    /// typo-tolerant matching up to the specified edit distance.
    fn build_fuzzy_query(&self, query_str: &str, distance: u8) -> Box<dyn tantivy::query::Query> {
        let clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = query_str
            .split_whitespace()
            .flat_map(|word| {
                let title_term = Term::from_field_text(self.fields.title, word);
                let content_term = Term::from_field_text(self.fields.content, word);

                // Third parameter enables prefix matching (e.g., "lamb" matches "lambda")
                let title_fuzzy = FuzzyTermQuery::new(title_term, distance, true);
                let content_fuzzy = FuzzyTermQuery::new(content_term, distance, true);

                vec![
                    (
                        Occur::Should,
                        Box::new(title_fuzzy) as Box<dyn tantivy::query::Query>,
                    ),
                    (
                        Occur::Should,
                        Box::new(content_fuzzy) as Box<dyn tantivy::query::Query>,
                    ),
                ]
            })
            .collect();

        Box::new(BooleanQuery::new(clauses))
    }

    /// Build a search query from the user's query string.
    ///
    /// If `fuzzy_distance` is set, uses fuzzy term matching for typo tolerance.
    fn build_query(
        &self,
        query_str: &str,
        fuzzy_distance: Option<u8>,
        category_filter: Option<&str>,
    ) -> anyhow::Result<Box<dyn tantivy::query::Query>> {
        let content_query: Box<dyn tantivy::query::Query> = if let Some(distance) = fuzzy_distance {
            self.build_fuzzy_query(query_str, distance)
        } else {
            let query_parser =
                QueryParser::for_index(&self.index, vec![self.fields.title, self.fields.content]);
            query_parser.parse_query(query_str)?
        };

        // Add category filter if specified
        if let Some(category) = category_filter {
            let category_term = Term::from_field_text(self.fields.category, category);
            let category_query =
                TermQuery::new(category_term, tantivy::schema::IndexRecordOption::Basic);

            let combined = BooleanQuery::new(vec![
                (Occur::Must, content_query),
                (Occur::Must, Box::new(category_query)),
            ]);

            Ok(Box::new(combined))
        } else {
            Ok(content_query)
        }
    }

    /// Index all documents from a corpus.
    ///
    /// This clears the existing index and rebuilds it from scratch.
    ///
    /// # Errors
    ///
    /// Returns an error if indexing fails or if in read-only mode.
    pub fn index_corpus(&self, corpus: &Corpus) -> anyhow::Result<()> {
        if self.mode == IndexMode::ReadOnly {
            anyhow::bail!("Cannot index in read-only mode");
        }

        let mut writer: IndexWriter = self.index.writer(WRITER_HEAP_SIZE)?;

        // Clear existing documents
        writer.delete_all_documents()?;

        // Index each document
        for doc in corpus.documents() {
            let full_path = corpus.resolve_document_path(doc);

            // Read document content
            let content = match std::fs::read_to_string(&full_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Warning: Could not read {}: {e}", full_path.display());
                    continue;
                }
            };

            // Create Tantivy document
            let mut tantivy_doc = tantivy::TantivyDocument::new();
            tantivy_doc.add_text(self.fields.title, &doc.title);
            tantivy_doc.add_text(self.fields.content, &content);
            tantivy_doc.add_text(self.fields.category, &doc.category);
            tantivy_doc.add_text(self.fields.tags, doc.tags.join(" "));
            tantivy_doc.add_text(self.fields.path, doc.path.to_string_lossy());

            writer.add_document(tantivy_doc)?;
        }

        writer.commit()?;

        Ok(())
    }
    /// Convert a Tantivy document to a `SearchResult`.
    ///
    /// Note: `matched_line` currently uses the title as a placeholder.
    /// TODO: Extract actual content snippet for better search result display.
    fn doc_to_search_result(
        &self,
        doc: &tantivy::TantivyDocument,
        score: f32,
        corpus: &Corpus,
    ) -> SearchResult {
        let title = doc
            .get_first(self.fields.title)
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string();

        let path_str = doc
            .get_first(self.fields.path)
            .and_then(|v| v.as_str())
            .unwrap_or("");

        SearchResult {
            path: corpus.root.join(path_str),
            matched_line: title.clone(),
            title,
            line_number: 1,
            score: Some(score),
        }
    }
}

impl SearchBackend for TantivyBackend {
    fn search(
        &self,
        query: &str,
        corpus: &Corpus,
        options: &SearchOptions,
    ) -> anyhow::Result<Vec<SearchResult>> {
        if query.trim().is_empty() {
            return Ok(vec![]);
        }

        let searcher = self.reader.searcher();
        let limit = options.limit.unwrap_or(10);
        let tantivy_query = self.build_query(query, options.fuzzy, options.category.as_deref())?;
        let top_docs = searcher.search(&tantivy_query, &TopDocs::with_limit(limit))?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (score, doc_address) in top_docs {
            let doc: tantivy::TantivyDocument = searcher.doc(doc_address)?;
            results.push(self.doc_to_search_result(&doc, score, corpus));
        }

        Ok(results)
    }

    fn index(&self, corpus: &Corpus) -> anyhow::Result<()> {
        self.index_corpus(corpus)
    }

    fn needs_indexing(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{Document, Manifest};
    use tempfile::TempDir;

    fn create_test_corpus(temp_dir: &TempDir) -> Corpus {
        let root = temp_dir.path().to_path_buf();

        // Create test document
        let doc_dir = root.join("test");
        std::fs::create_dir_all(&doc_dir).unwrap();
        std::fs::write(
            doc_dir.join("example.md"),
            "# Example Document\n\nThis is about AWS Lambda and serverless patterns.",
        )
        .unwrap();

        // Create manifest
        let manifest = Manifest {
            version: "1".to_string(),
            documents: vec![Document {
                path: PathBuf::from("test/example.md"),
                title: "Example Document".to_string(),
                category: "test".to_string(),
                tags: vec!["lambda".to_string(), "serverless".to_string()],
            }],
        };

        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        Corpus { root, manifest }
    }

    #[test]
    fn test_schema_creation() {
        let (schema, _fields) = TantivyBackend::build_schema();

        assert!(schema.get_field("title").is_ok());
        assert!(schema.get_field("content").is_ok());
        assert!(schema.get_field("category").is_ok());
        assert!(schema.get_field("tags").is_ok());
        assert!(schema.get_field("path").is_ok());
    }

    #[test]
    fn test_open_creates_index() {
        let temp_dir = TempDir::new().unwrap();
        let index_path = temp_dir.path().join(".index");

        let backend = TantivyBackend::open(&index_path, IndexMode::ReadWrite).unwrap();

        assert!(index_path.exists());
        assert_eq!(backend.mode, IndexMode::ReadWrite);
    }

    #[test]
    fn test_read_only_mode_fails_without_index() {
        let temp_dir = TempDir::new().unwrap();
        let index_path = temp_dir.path().join(".index");

        let result = TantivyBackend::open(&index_path, IndexMode::ReadOnly);

        assert!(result.is_err());
    }

    #[test]
    fn test_index_and_search() {
        let temp_dir = TempDir::new().unwrap();
        let corpus = create_test_corpus(&temp_dir);

        let backend = TantivyBackend::open_for_corpus(&corpus, IndexMode::ReadWrite).unwrap();
        backend.index_corpus(&corpus).unwrap();

        // Need to reload reader after indexing
        let backend = TantivyBackend::open_for_corpus(&corpus, IndexMode::ReadWrite).unwrap();

        let options = SearchOptions::default();
        let results = backend.search("lambda", &corpus, &options).unwrap();

        assert!(!results.is_empty());
        assert!(results[0].score.is_some());
    }

    #[test]
    fn test_category_filter() {
        let temp_dir = TempDir::new().unwrap();
        let corpus = create_test_corpus(&temp_dir);

        let backend = TantivyBackend::open_for_corpus(&corpus, IndexMode::ReadWrite).unwrap();
        backend.index_corpus(&corpus).unwrap();

        let backend = TantivyBackend::open_for_corpus(&corpus, IndexMode::ReadWrite).unwrap();

        // Search with matching category
        let options = SearchOptions {
            category: Some("test".to_string()),
            ..Default::default()
        };
        let results = backend.search("lambda", &corpus, &options).unwrap();
        assert!(!results.is_empty());

        // Search with non-matching category
        let options = SearchOptions {
            category: Some("nonexistent".to_string()),
            ..Default::default()
        };
        let results = backend.search("lambda", &corpus, &options).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_empty_query_returns_empty() {
        let temp_dir = TempDir::new().unwrap();
        let corpus = create_test_corpus(&temp_dir);

        let backend = TantivyBackend::open_for_corpus(&corpus, IndexMode::ReadWrite).unwrap();

        let options = SearchOptions::default();
        let results = backend.search("", &corpus, &options).unwrap();

        assert!(results.is_empty());
    }
}
