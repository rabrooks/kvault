//! End-to-end CLI tests for kvault.
//!
//! These tests exercise the full CLI binary with isolated test environments.
//! Each test creates its own temporary corpus and config to ensure isolation.

use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use tempfile::TempDir;

// =============================================================================
// Test Environment Helper
// =============================================================================

/// Isolated test environment with its own corpus and config.
struct TestEnv {
    _temp_dir: TempDir,
    corpus_path: PathBuf,
    config_path: PathBuf,
}

impl TestEnv {
    /// Create a new empty test environment.
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path();

        let corpus_path = root.join("corpus");
        fs::create_dir_all(&corpus_path).expect("Failed to create corpus dir");

        // Create empty manifest
        let manifest = r#"{"version": "1", "documents": []}"#;
        fs::write(corpus_path.join("manifest.json"), manifest).expect("Failed to write manifest");

        // Create config pointing to corpus
        let config_path = root.join("config.toml");
        let config_content = format!("[corpus]\npaths = [\"{}\"]\n", corpus_path.display());
        fs::write(&config_path, config_content).expect("Failed to write config");

        Self {
            _temp_dir: temp_dir,
            corpus_path,
            config_path,
        }
    }

    /// Create a test environment with sample documents.
    fn with_documents() -> Self {
        let env = Self::new();

        // Create category directories
        fs::create_dir_all(env.corpus_path.join("rust")).expect("Failed to create rust dir");
        fs::create_dir_all(env.corpus_path.join("aws")).expect("Failed to create aws dir");

        // Create test documents
        fs::write(
            env.corpus_path.join("rust/error-handling.md"),
            "# Error Handling in Rust\n\nUse Result and Option types for error handling.\nThe ? operator propagates errors elegantly.",
        ).expect("Failed to write rust doc");

        fs::write(
            env.corpus_path.join("aws/lambda-patterns.md"),
            "# AWS Lambda Patterns\n\nBest practices for AWS Lambda functions.\nUse environment variables for configuration.",
        ).expect("Failed to write aws doc");

        // Update manifest with documents
        let manifest = r#"{
    "version": "1",
    "documents": [
        {"path": "rust/error-handling.md", "title": "Error Handling", "category": "rust", "tags": ["rust", "errors"]},
        {"path": "aws/lambda-patterns.md", "title": "Lambda Patterns", "category": "aws", "tags": ["aws", "lambda"]}
    ]
}"#;
        fs::write(env.corpus_path.join("manifest.json"), manifest)
            .expect("Failed to write manifest");

        env
    }

    /// Get a Command configured for this test environment.
    fn command(&self) -> Command {
        let mut cmd = cargo_bin_cmd!("kvault");
        cmd.env("KVAULT_CONFIG", &self.config_path);
        cmd
    }

    /// Get the corpus path.
    fn corpus(&self) -> &PathBuf {
        &self.corpus_path
    }
}

// =============================================================================
// 1. Help / No Command Tests
// =============================================================================

#[test]
fn tc_1_1_no_subcommand_shows_help() {
    let env = TestEnv::new();

    env.command()
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("search"))
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("add"))
        .stdout(predicate::str::contains("get"));
}

#[test]
fn tc_1_2_help_flag() {
    let env = TestEnv::new();

    env.command()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Searchable knowledge corpus"));
}

#[test]
fn tc_1_3_version_flag() {
    let env = TestEnv::new();

    env.command()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("kvault"));
}

// =============================================================================
// 2. Search Command Tests
// =============================================================================

#[test]
fn tc_2_1_search_with_matches() {
    let env = TestEnv::with_documents();

    env.command()
        .args(["search", "error"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Error Handling"))
        .stdout(predicate::str::contains("result(s) found"));
}

#[test]
fn tc_2_2_search_with_no_matches() {
    let env = TestEnv::with_documents();

    env.command()
        .args(["search", "xyznonexistent123"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "No matches found for 'xyznonexistent123'",
        ));
}

#[test]
fn tc_2_3_search_with_limit() {
    let env = TestEnv::with_documents();

    // Search for a term that appears in both documents
    env.command()
        .args(["search", "for", "--limit", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 result(s) found"));
}

#[test]
fn tc_2_4_search_with_category_filter_matching() {
    let env = TestEnv::with_documents();

    env.command()
        .args(["search", "Lambda", "--category", "aws"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Lambda Patterns"));
}

#[test]
fn tc_2_5_search_with_category_filter_non_matching() {
    let env = TestEnv::with_documents();

    env.command()
        .args(["search", "Lambda", "--category", "rust"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No matches found"));
}

#[test]
fn tc_2_6_search_empty_query() {
    let env = TestEnv::with_documents();

    env.command()
        .args(["search", ""])
        .assert()
        .success()
        .stdout(predicate::str::contains("No matches found"));
}

#[test]
fn tc_2_7_search_query_too_long() {
    let env = TestEnv::with_documents();
    let long_query = "a".repeat(1001);

    env.command()
        .args(["search", &long_query])
        .assert()
        .failure()
        .stderr(predicate::str::contains("too long"));
}

#[test]
fn tc_2_9_search_nonexistent_corpus_path() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("config.toml");

    // Config points to non-existent path
    fs::write(&config_path, "[corpus]\npaths = [\"/nonexistent/path\"]").unwrap();

    cargo_bin_cmd!("kvault")
        .env("KVAULT_CONFIG", &config_path)
        .args(["search", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No matches found"));
}

#[test]
fn tc_2_10_search_invalid_manifest() {
    let env = TestEnv::new();

    // Corrupt the manifest
    fs::write(env.corpus().join("manifest.json"), "not valid json").unwrap();

    env.command()
        .args(["search", "test"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Search failed"));
}

#[test]
fn tc_2_11_search_missing_manifest() {
    let env = TestEnv::new();

    // Remove the manifest
    fs::remove_file(env.corpus().join("manifest.json")).unwrap();

    env.command()
        .args(["search", "test"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Search failed"));
}

#[test]
fn tc_2_12_search_case_insensitive_by_default() {
    let env = TestEnv::with_documents();

    // Search with lowercase should find "Lambda" (uppercase in document)
    env.command()
        .args(["search", "lambda"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Lambda Patterns"));
}

#[test]
fn tc_2_13_search_case_sensitive_flag() {
    let env = TestEnv::with_documents();

    // With --case-sensitive, lowercase "lambda" should NOT find "Lambda"
    env.command()
        .args(["search", "lambda", "--case-sensitive"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No matches found"));

    // But exact case should still work
    env.command()
        .args(["search", "Lambda", "--case-sensitive"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Lambda Patterns"));
}

// =============================================================================
// 3. List Command Tests
// =============================================================================

#[test]
fn tc_3_1_list_all_documents() {
    let env = TestEnv::with_documents();

    env.command()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("rust: Error Handling"))
        .stdout(predicate::str::contains("aws: Lambda Patterns"))
        .stdout(predicate::str::contains("[rust, errors]"))
        .stdout(predicate::str::contains("[aws, lambda]"));
}

#[test]
fn tc_3_2_list_no_documents() {
    let env = TestEnv::new();

    env.command()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("No documents found"));
}

#[test]
fn tc_3_3_list_with_category_filter_matching() {
    let env = TestEnv::with_documents();

    env.command()
        .args(["list", "--category", "rust"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Error Handling"))
        .stdout(predicate::str::contains("Lambda Patterns").not());
}

#[test]
fn tc_3_4_list_with_category_filter_non_matching() {
    let env = TestEnv::with_documents();

    env.command()
        .args(["list", "--category", "nonexistent"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No documents found"));
}

#[test]
fn tc_3_5_list_document_without_tags() {
    let env = TestEnv::new();

    // Create document without tags
    fs::create_dir_all(env.corpus().join("test")).unwrap();
    fs::write(env.corpus().join("test/doc.md"), "# Test\n\nContent").unwrap();

    let manifest = r#"{
    "version": "1",
    "documents": [
        {"path": "test/doc.md", "title": "Test Doc", "category": "test", "tags": []}
    ]
}"#;
    fs::write(env.corpus().join("manifest.json"), manifest).unwrap();

    env.command()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("test: Test Doc"))
        // Should not have empty brackets
        .stdout(predicate::str::contains("[]").not());
}

// =============================================================================
// 4. Add Command Tests
// =============================================================================

#[test]
fn tc_4_1_add_document_from_stdin() {
    let env = TestEnv::new();

    env.command()
        .args(["add", "--title", "Test Doc", "--category", "test"])
        .write_stdin("# Test Document\n\nThis is test content.")
        .assert()
        .success()
        .stdout(predicate::str::contains("Added: Test Doc"))
        .stdout(predicate::str::contains("Category: test"));

    // Verify file was created
    assert!(env.corpus().join("test/test-doc.md").exists());
}

#[test]
fn tc_4_2_add_document_from_file() {
    let env = TestEnv::new();
    let input_file = env.corpus().join("input.md");
    fs::write(&input_file, "# From File\n\nContent from file.").unwrap();

    env.command()
        .args([
            "add",
            "--title",
            "From File",
            "--category",
            "test",
            "--file",
        ])
        .arg(&input_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("Added: From File"));
}

#[test]
fn tc_4_3_add_document_with_tags() {
    let env = TestEnv::new();

    env.command()
        .args([
            "add",
            "--title",
            "Tagged",
            "--category",
            "test",
            "--tags",
            "tag1, tag2, tag3",
        ])
        .write_stdin("# Tagged\n\nContent")
        .assert()
        .success();

    // Verify tags in manifest
    let manifest = fs::read_to_string(env.corpus().join("manifest.json")).unwrap();
    assert!(manifest.contains("tag1"));
    assert!(manifest.contains("tag2"));
    assert!(manifest.contains("tag3"));
}

#[test]
fn tc_4_4_add_empty_title() {
    let env = TestEnv::new();

    env.command()
        .args(["add", "--title", "", "--category", "test"])
        .write_stdin("content")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Title cannot be empty"));
}

#[test]
fn tc_4_5_add_title_too_long() {
    let env = TestEnv::new();
    let long_title = "a".repeat(201);

    env.command()
        .args(["add", "--title", &long_title, "--category", "test"])
        .write_stdin("content")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Title too long"));
}

#[test]
fn tc_4_6_add_invalid_category_slash() {
    let env = TestEnv::new();

    env.command()
        .args(["add", "--title", "Test", "--category", "my/category"])
        .write_stdin("content")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid character"));
}

#[test]
fn tc_4_7_add_category_starting_with_hyphen() {
    let env = TestEnv::new();

    // Use --category=-invalid syntax to prevent clap from treating -invalid as a flag
    env.command()
        .args(["add", "--title", "Test", "--category=-invalid"])
        .write_stdin("content")
        .assert()
        .failure()
        .stderr(predicate::str::contains("must start with"));
}

#[test]
fn tc_4_8_add_category_with_spaces() {
    let env = TestEnv::new();

    env.command()
        .args(["add", "--title", "Test", "--category", "my category"])
        .write_stdin("content")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid character"));
}

#[test]
fn tc_4_9_add_invalid_tag() {
    let env = TestEnv::new();

    env.command()
        .args([
            "add",
            "--title",
            "Test",
            "--category",
            "test",
            "--tags",
            "valid, in/valid",
        ])
        .write_stdin("content")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Tag").and(predicate::str::contains("invalid character")));
}

#[test]
fn tc_4_10_add_empty_content_stdin() {
    let env = TestEnv::new();

    env.command()
        .args(["add", "--title", "Empty", "--category", "test"])
        .write_stdin("")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Content cannot be empty"));
}

#[test]
fn tc_4_11_add_whitespace_only_content() {
    let env = TestEnv::new();

    env.command()
        .args(["add", "--title", "Whitespace", "--category", "test"])
        .write_stdin("   \n\t\n   ")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Content cannot be empty"));
}

#[test]
fn tc_4_12_add_duplicate_document() {
    let env = TestEnv::new();

    // First add should succeed
    env.command()
        .args(["add", "--title", "Duplicate", "--category", "test"])
        .write_stdin("content")
        .assert()
        .success();

    // Second add with same title/category should fail
    env.command()
        .args(["add", "--title", "Duplicate", "--category", "test"])
        .write_stdin("content")
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn tc_4_13_add_file_not_found() {
    let env = TestEnv::new();

    env.command()
        .args([
            "add",
            "--title",
            "Test",
            "--category",
            "test",
            "--file",
            "/nonexistent/path.md",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Failed to read file"));
}

#[test]
fn tc_4_14_add_slugify_special_chars() {
    let env = TestEnv::new();

    env.command()
        .args([
            "add",
            "--title",
            "AWS Lambda: Best Practices!",
            "--category",
            "aws",
        ])
        .write_stdin("content")
        .assert()
        .success();

    // Verify slug was created correctly
    assert!(
        env.corpus()
            .join("aws/aws-lambda-best-practices.md")
            .exists()
    );
}

#[test]
fn tc_4_15_add_tags_with_whitespace() {
    let env = TestEnv::new();

    env.command()
        .args([
            "add",
            "--title",
            "Test",
            "--category",
            "test",
            "--tags",
            "  tag1  ,  tag2  ",
        ])
        .write_stdin("content")
        .assert()
        .success();

    let manifest = fs::read_to_string(env.corpus().join("manifest.json")).unwrap();
    assert!(manifest.contains("\"tag1\""));
    assert!(manifest.contains("\"tag2\""));
    // Should not have extra whitespace
    assert!(!manifest.contains("\"  tag1  \""));
}

#[test]
fn tc_4_16_add_tags_empty_entries_filtered() {
    let env = TestEnv::new();

    env.command()
        .args([
            "add",
            "--title",
            "Test",
            "--category",
            "test",
            "--tags",
            "tag1,,tag2,",
        ])
        .write_stdin("content")
        .assert()
        .success();

    let manifest = fs::read_to_string(env.corpus().join("manifest.json")).unwrap();
    assert!(manifest.contains("\"tag1\""));
    assert!(manifest.contains("\"tag2\""));
}

#[test]
fn tc_4_17_add_category_path_traversal() {
    let env = TestEnv::new();

    env.command()
        .args(["add", "--title", "Test", "--category", ".."])
        .write_stdin("content")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("invalid character")
                .or(predicate::str::contains("must start with")),
        );
}

// =============================================================================
// 5. Get Command Tests
// =============================================================================

#[test]
fn tc_5_1_get_existing_document() {
    let env = TestEnv::with_documents();

    env.command()
        .args(["get", "rust/error-handling.md"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Error Handling in Rust"))
        .stdout(predicate::str::contains("Result and Option"));
}

#[test]
fn tc_5_2_get_document_not_found() {
    let env = TestEnv::with_documents();

    env.command()
        .args(["get", "nonexistent/doc.md"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Document not found"));
}

#[test]
fn tc_5_3_get_path_traversal_attempt() {
    let env = TestEnv::with_documents();

    env.command()
        .args(["get", "../../../etc/passwd"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("Invalid document path")
                .or(predicate::str::contains("contains '..'")),
        );
}

#[test]
fn tc_5_4_get_file_not_in_manifest() {
    let env = TestEnv::with_documents();

    // Create orphan file not in manifest
    fs::create_dir_all(env.corpus().join("orphan")).unwrap();
    fs::write(env.corpus().join("orphan/file.md"), "# Orphan").unwrap();

    env.command()
        .args(["get", "orphan/file.md"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Document not found"));
}

// =============================================================================
// 6. Edge Cases and Config Tests
// =============================================================================

#[test]
fn tc_6_1_no_corpus_paths_configured() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    fs::write(&config_path, "[corpus]\npaths = []\n").unwrap();

    cargo_bin_cmd!("kvault")
        .env("KVAULT_CONFIG", &config_path)
        .args(["search", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No matches found"));
}

#[test]
fn tc_6_2_invalid_config_toml() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    fs::write(&config_path, "this is not valid toml {{{{").unwrap();

    cargo_bin_cmd!("kvault")
        .env("KVAULT_CONFIG", &config_path)
        .args(["search", "test"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Error"));
}

#[test]
fn tc_6_3_multiple_corpora() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create two corpora
    let corpus1 = root.join("corpus1");
    let corpus2 = root.join("corpus2");
    fs::create_dir_all(corpus1.join("cat1")).unwrap();
    fs::create_dir_all(corpus2.join("cat2")).unwrap();

    fs::write(
        corpus1.join("cat1/doc1.md"),
        "# Doc1\n\nUnique content alpha",
    )
    .unwrap();
    fs::write(
        corpus2.join("cat2/doc2.md"),
        "# Doc2\n\nUnique content beta",
    )
    .unwrap();

    fs::write(corpus1.join("manifest.json"), r#"{"version":"1","documents":[{"path":"cat1/doc1.md","title":"Doc1","category":"cat1","tags":[]}]}"#).unwrap();
    fs::write(corpus2.join("manifest.json"), r#"{"version":"1","documents":[{"path":"cat2/doc2.md","title":"Doc2","category":"cat2","tags":[]}]}"#).unwrap();

    let config_path = root.join("config.toml");
    fs::write(
        &config_path,
        format!(
            "[corpus]\npaths = [\"{}\", \"{}\"]\n",
            corpus1.display(),
            corpus2.display()
        ),
    )
    .unwrap();

    // Search should find results from both corpora
    cargo_bin_cmd!("kvault")
        .env("KVAULT_CONFIG", &config_path)
        .args(["search", "Unique"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Doc1"))
        .stdout(predicate::str::contains("Doc2"));
}

#[test]
fn tc_6_4_document_in_manifest_but_file_missing() {
    let env = TestEnv::new();

    // Add document to manifest but don't create file
    let manifest = r#"{
    "version": "1",
    "documents": [
        {"path": "missing/file.md", "title": "Missing", "category": "missing", "tags": []}
    ]
}"#;
    fs::write(env.corpus().join("manifest.json"), manifest).unwrap();

    env.command()
        .args(["get", "missing/file.md"])
        .assert()
        .failure();
}

#[test]
fn tc_6_5_kvault_config_env_overrides_default() {
    let env = TestEnv::with_documents();

    // This test verifies KVAULT_CONFIG works (which all other tests depend on)
    env.command()
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Error Handling"));
}

#[test]
fn tc_6_6_config_not_found_uses_defaults() {
    let temp_dir = TempDir::new().unwrap();
    let nonexistent_config = temp_dir.path().join("nonexistent/config.toml");

    // When config doesn't exist, should use defaults (which point to ~/.kvault)
    // This will likely show "No documents" or similar since default path probably doesn't exist
    cargo_bin_cmd!("kvault")
        .env("KVAULT_CONFIG", &nonexistent_config)
        .args(["list"])
        .assert()
        .success();
}
