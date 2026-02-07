//! Integration tests for kvault commands.
//!
//! These tests focus on validation logic that doesn't depend on config files.
//! Tests that require a full corpus setup are marked with #[ignore] and can be
//! run manually with `cargo test -- --ignored` in an appropriate environment.

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

/// Test helper to create a temporary corpus directory.
struct TestCorpus {
    _temp_dir: TempDir,
    pub root: PathBuf,
}

impl TestCorpus {
    /// Create a new empty test corpus with manifest.
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path().to_path_buf();

        // Create manifest.json
        let manifest = r#"{"version": "1", "documents": []}"#;
        fs::write(root.join("manifest.json"), manifest).expect("Failed to write manifest");

        Self {
            _temp_dir: temp_dir,
            root,
        }
    }

    /// Create a corpus with some test documents.
    fn with_documents() -> Self {
        let corpus = Self::new();

        // Create category directories
        fs::create_dir_all(corpus.root.join("rust")).expect("Failed to create rust dir");
        fs::create_dir_all(corpus.root.join("aws")).expect("Failed to create aws dir");

        // Create test documents
        fs::write(
            corpus.root.join("rust/error-handling.md"),
            "# Error Handling in Rust\n\nUse Result and Option types.",
        )
        .expect("Failed to write doc");

        fs::write(
            corpus.root.join("aws/lambda-patterns.md"),
            "# Lambda Patterns\n\nBest practices for AWS Lambda.",
        )
        .expect("Failed to write doc");

        // Update manifest
        let manifest = r#"{
    "version": "1",
    "documents": [
        {"path": "rust/error-handling.md", "title": "Error Handling", "category": "rust", "tags": ["rust", "errors"]},
        {"path": "aws/lambda-patterns.md", "title": "Lambda Patterns", "category": "aws", "tags": ["aws", "lambda"]}
    ]
}"#;
        fs::write(corpus.root.join("manifest.json"), manifest).expect("Failed to write manifest");

        corpus
    }
}

// =============================================================================
// Validation Tests (don't require config)
// =============================================================================

mod input_validation_tests {

    #[test]
    fn validate_identifier_accepts_valid_names() {
        // These would be valid categories/tags
        let valid_names = ["rust", "aws-lambda", "my_category", "Category123", "a"];

        for name in valid_names {
            // We can't call validate_identifier directly (it's private),
            // but we can test through parse_tags which uses similar logic
            let result = kvault::commands::parse_tags(Some(name.to_string()));
            assert_eq!(result, vec![name], "Expected {name} to be valid");
        }
    }

    #[test]
    fn parse_tags_handles_whitespace() {
        let tags = kvault::commands::parse_tags(Some("  tag1  ,  tag2  ,  tag3  ".to_string()));
        assert_eq!(tags, vec!["tag1", "tag2", "tag3"]);
    }

    #[test]
    fn parse_tags_filters_empty_strings() {
        let tags = kvault::commands::parse_tags(Some("tag1,,tag2,".to_string()));
        assert_eq!(tags, vec!["tag1", "tag2"]);
    }

    #[test]
    fn parse_tags_none_returns_empty_vec() {
        let tags = kvault::commands::parse_tags(None);
        assert!(tags.is_empty());
    }

    #[test]
    fn parse_tags_empty_string_returns_empty_vec() {
        let tags = kvault::commands::parse_tags(Some(String::new()));
        assert!(tags.is_empty());
    }

    #[test]
    fn parse_tags_preserves_order() {
        let tags = kvault::commands::parse_tags(Some("z, a, m".to_string()));
        assert_eq!(tags, vec!["z", "a", "m"]);
    }
}

// =============================================================================
// Corpus Loading Tests (use temp directories directly)
// =============================================================================

mod corpus_tests {
    use super::*;

    #[test]
    fn corpus_load_valid_manifest() {
        let corpus = TestCorpus::with_documents();
        let loaded = kvault::corpus::Corpus::load(&corpus.root);

        assert!(
            loaded.is_ok(),
            "Expected corpus to load, got: {:?}",
            loaded.err()
        );
        let loaded = loaded.unwrap();
        assert_eq!(loaded.documents().len(), 2);
    }

    #[test]
    fn corpus_load_empty_manifest() {
        let corpus = TestCorpus::new();
        let loaded = kvault::corpus::Corpus::load(&corpus.root);

        assert!(loaded.is_ok());
        assert!(loaded.unwrap().documents().is_empty());
    }

    #[test]
    fn corpus_load_missing_manifest() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let loaded = kvault::corpus::Corpus::load(temp_dir.path());

        assert!(loaded.is_err());
    }

    #[test]
    fn corpus_load_invalid_json() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        fs::write(temp_dir.path().join("manifest.json"), "not valid json")
            .expect("Failed to write");

        let loaded = kvault::corpus::Corpus::load(temp_dir.path());
        assert!(loaded.is_err());
    }

    #[test]
    fn corpus_resolve_document_path() {
        let corpus = TestCorpus::with_documents();
        let loaded = kvault::corpus::Corpus::load(&corpus.root).unwrap();

        let doc = &loaded.documents()[0];
        let resolved = loaded.resolve_document_path(doc);

        assert!(resolved.starts_with(&corpus.root));
        assert!(resolved.exists());
    }
}

// =============================================================================
// Storage Backend Tests
// =============================================================================

mod storage_tests {
    use super::*;
    use kvault::storage::StorageBackend;
    use kvault::storage::local::LocalStorageBackend;

    #[test]
    fn local_storage_read_manifest() {
        let corpus = TestCorpus::with_documents();
        let storage = LocalStorageBackend::new(corpus.root.clone());

        let manifest = storage.read_manifest();
        assert!(manifest.is_ok());
        assert_eq!(manifest.unwrap().documents.len(), 2);
    }

    #[test]
    fn local_storage_write_document() {
        let corpus = TestCorpus::new();
        fs::create_dir_all(corpus.root.join("test")).expect("Failed to create dir");

        let storage = LocalStorageBackend::new(corpus.root.clone());
        let doc_path = PathBuf::from("test/new-doc.md");

        let result = storage.write_document(&doc_path, "# New Document\n\nContent here.");
        assert!(result.is_ok());
        assert!(corpus.root.join(&doc_path).exists());
    }

    #[test]
    fn local_storage_read_document() {
        let corpus = TestCorpus::with_documents();
        let storage = LocalStorageBackend::new(corpus.root.clone());
        let doc_path = PathBuf::from("rust/error-handling.md");

        let content = storage.read_document(&doc_path);
        assert!(content.is_ok());
        assert!(content.unwrap().contains("Error Handling"));
    }

    #[test]
    fn local_storage_exists() {
        let corpus = TestCorpus::with_documents();
        let storage = LocalStorageBackend::new(corpus.root.clone());

        assert!(storage.exists(&PathBuf::from("rust/error-handling.md")));
        assert!(!storage.exists(&PathBuf::from("nonexistent/doc.md")));
    }
}

// =============================================================================
// Search Backend Tests
// =============================================================================

mod search_tests {
    use super::*;
    use kvault::search::ripgrep::RipgrepBackend;
    use kvault::search::{SearchBackend, SearchOptions};

    #[test]
    fn ripgrep_search_finds_content() {
        // Skip if ripgrep is not installed
        if RipgrepBackend::check_available().is_err() {
            eprintln!("Skipping test: ripgrep not installed");
            return;
        }

        let corpus = TestCorpus::with_documents();
        let loaded = kvault::corpus::Corpus::load(&corpus.root).unwrap();
        let backend = RipgrepBackend::new();

        let results = backend.search(
            "Lambda",
            &loaded,
            &SearchOptions {
                limit: Some(10),
                category: None,
                case_sensitive: false,
                fuzzy: None,
            },
        );

        assert!(results.is_ok());
        let results = results.unwrap();
        assert!(
            !results.is_empty(),
            "Expected to find 'Lambda' in documents"
        );
    }

    #[test]
    fn ripgrep_search_empty_query() {
        if RipgrepBackend::check_available().is_err() {
            return;
        }

        let corpus = TestCorpus::with_documents();
        let loaded = kvault::corpus::Corpus::load(&corpus.root).unwrap();
        let backend = RipgrepBackend::new();

        let results = backend.search("", &loaded, &SearchOptions::default());

        assert!(results.is_ok());
        assert!(results.unwrap().is_empty());
    }

    #[test]
    fn ripgrep_search_no_match() {
        if RipgrepBackend::check_available().is_err() {
            return;
        }

        let corpus = TestCorpus::with_documents();
        let loaded = kvault::corpus::Corpus::load(&corpus.root).unwrap();
        let backend = RipgrepBackend::new();

        let results = backend.search("xyznonexistent123", &loaded, &SearchOptions::default());

        assert!(results.is_ok());
        assert!(results.unwrap().is_empty());
    }

    #[test]
    fn ripgrep_search_with_category_filter() {
        if RipgrepBackend::check_available().is_err() {
            return;
        }

        let corpus = TestCorpus::with_documents();
        let loaded = kvault::corpus::Corpus::load(&corpus.root).unwrap();
        let backend = RipgrepBackend::new();

        // Search for "patterns" but filter to rust category
        let results = backend.search(
            "Patterns",
            &loaded,
            &SearchOptions {
                limit: Some(10),
                category: Some("rust".to_string()),
                case_sensitive: false,
                fuzzy: None,
            },
        );

        assert!(results.is_ok());
        // Lambda Patterns is in aws category, so should not appear
        let results = results.unwrap();
        assert!(
            !results.iter().any(|r| r.title.contains("Lambda")),
            "Should not find Lambda in rust category"
        );
    }

    #[test]
    fn ripgrep_rejects_long_query() {
        if RipgrepBackend::check_available().is_err() {
            return;
        }

        let corpus = TestCorpus::with_documents();
        let loaded = kvault::corpus::Corpus::load(&corpus.root).unwrap();
        let backend = RipgrepBackend::new();

        let long_query = "a".repeat(1001);
        let results = backend.search(&long_query, &loaded, &SearchOptions::default());

        assert!(results.is_err());
        assert!(results.unwrap_err().to_string().contains("too long"));
    }

    #[test]
    fn ripgrep_rejects_null_byte() {
        if RipgrepBackend::check_available().is_err() {
            return;
        }

        let corpus = TestCorpus::with_documents();
        let loaded = kvault::corpus::Corpus::load(&corpus.root).unwrap();
        let backend = RipgrepBackend::new();

        let results = backend.search("test\0query", &loaded, &SearchOptions::default());

        assert!(results.is_err());
        assert!(results.unwrap_err().to_string().contains("invalid"));
    }
}

// =============================================================================
// Config Tests
// =============================================================================

mod config_tests {
    use kvault::config::expand_tilde;
    use std::path::PathBuf;

    #[test]
    fn expand_tilde_with_home_prefix() {
        let result = expand_tilde("~/.kvault");
        assert!(!result.to_string_lossy().starts_with('~'));
        assert!(result.to_string_lossy().ends_with(".kvault"));
    }

    #[test]
    fn expand_tilde_absolute_path_unchanged() {
        let result = expand_tilde("/absolute/path");
        assert_eq!(result, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn expand_tilde_relative_path_unchanged() {
        let result = expand_tilde("relative/path");
        assert_eq!(result, PathBuf::from("relative/path"));
    }
}
