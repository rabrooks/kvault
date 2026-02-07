# kvault CLI Manual E2E Test Plan

This document covers all CLI commands and their branches for manual end-to-end testing.
These tests will eventually be automated.

## Prerequisites

1. Build kvault: `cargo build`
2. Ensure ripgrep is installed: `rg --version`
3. Create a test corpus directory for isolated testing

## Test Setup

```bash
# Create isolated test environment
export KVAULT_TEST_DIR=$(mktemp -d)
export KVAULT_CORPUS="$KVAULT_TEST_DIR/corpus"
mkdir -p "$KVAULT_CORPUS"

# Create manifest.json
cat > "$KVAULT_CORPUS/manifest.json" << 'EOF'
{
  "version": "1",
  "documents": []
}
EOF

# Create config pointing to test corpus
cat > "$KVAULT_TEST_DIR/config.toml" << EOF
[corpus]
paths = ["$KVAULT_CORPUS"]
EOF

# Point kvault to test config via environment variable
export KVAULT_CONFIG="$KVAULT_TEST_DIR/config.toml"

# Alias for testing (adjust path as needed)
alias kvault="./target/debug/kvault"
```

> **Note:** The `KVAULT_CONFIG` environment variable overrides the default config location
> (`~/.config/kvault/config.toml`). This enables isolated testing without affecting your
> real configuration.

---

## 1. Help / No Command

### TC-1.1: No subcommand shows help
**Command:** `kvault`
**Expected:** Shows help text with available commands (search, list, add, get)
**Status:** [ ]

### TC-1.2: --help flag
**Command:** `kvault --help`
**Expected:** Shows detailed help with all options
**Status:** [ ]

### TC-1.3: --version flag
**Command:** `kvault --version`
**Expected:** Shows version number
**Status:** [ ]

---

## 2. Search Command

### Setup for Search Tests
```bash
# Add test documents first
mkdir -p "$KVAULT_CORPUS/rust" "$KVAULT_CORPUS/aws"

cat > "$KVAULT_CORPUS/rust/error-handling.md" << 'EOF'
# Error Handling in Rust

Use Result and Option types for error handling.
The ? operator propagates errors elegantly.
EOF

cat > "$KVAULT_CORPUS/aws/lambda-patterns.md" << 'EOF'
# AWS Lambda Patterns

Best practices for AWS Lambda functions.
Use environment variables for configuration.
EOF

# Update manifest
cat > "$KVAULT_CORPUS/manifest.json" << 'EOF'
{
  "version": "1",
  "documents": [
    {"path": "rust/error-handling.md", "title": "Error Handling", "category": "rust", "tags": ["rust", "errors"]},
    {"path": "aws/lambda-patterns.md", "title": "Lambda Patterns", "category": "aws", "tags": ["aws", "lambda"]}
  ]
}
EOF
```

### TC-2.1: Search with matches
**Command:** `kvault search "error"`
**Expected:**
- Shows "Error Handling" result with file path and line number
- Shows matched line content
- Shows result count
**Status:** [ ]

### TC-2.2: Search with no matches
**Command:** `kvault search "xyznonexistent123"`
**Expected:** `No matches found for 'xyznonexistent123'`
**Status:** [ ]

### TC-2.3: Search with --limit
**Command:** `kvault search "the" --limit 1`
**Expected:** Returns at most 1 result
**Status:** [ ]

### TC-2.4: Search with --category filter (matching)
**Command:** `kvault search "patterns" --category aws`
**Expected:** Shows Lambda Patterns result (aws category)
**Status:** [ ]

### TC-2.5: Search with --category filter (non-matching)
**Command:** `kvault search "Lambda" --category rust`
**Expected:** No matches (Lambda is in aws category, not rust)
**Status:** [ ]

### TC-2.6: Search with empty query
**Command:** `kvault search ""`
**Expected:** No matches found (empty query returns empty results)
**Status:** [ ]

### TC-2.7: Search with very long query (>1000 chars)
**Command:** `kvault search "$(printf 'a%.0s' {1..1001})"`
**Expected:** Error: "Query too long: 1001 chars (max 1000)"
**Status:** [ ]

### TC-2.8: Search when ripgrep not installed
**Setup:** Temporarily rename/hide rg binary
**Command:** `kvault search "test"`
**Expected:** Error with ripgrep installation instructions
**Status:** [ ]

### TC-2.9: Search with non-existent corpus path
**Setup:** Configure a path that doesn't exist in config.toml
**Command:** `kvault search "test"`
**Expected:** Silently skips non-existent paths, searches remaining corpora
**Status:** [ ]

### TC-2.10: Search with invalid manifest JSON
**Setup:** Write invalid JSON to manifest.json
**Command:** `kvault search "test"`
**Expected:** Error about failed manifest parsing
**Status:** [ ]

### TC-2.11: Search with missing manifest
**Setup:** Delete manifest.json from corpus
**Command:** `kvault search "test"`
**Expected:** Error about manifest not found
**Status:** [ ]

---

## 3. List Command

### TC-3.1: List all documents
**Command:** `kvault list`
**Expected:**
- Shows all documents with category, title, tags, and path
- Format: `category: title [tags]` followed by path
**Status:** [ ]

### TC-3.2: List with no documents
**Setup:** Empty documents array in manifest.json
**Command:** `kvault list`
**Expected:** `No documents found.`
**Status:** [ ]

### TC-3.3: List with --category filter (matching)
**Command:** `kvault list --category rust`
**Expected:** Shows only rust category documents
**Status:** [ ]

### TC-3.4: List with --category filter (non-matching)
**Command:** `kvault list --category nonexistent`
**Expected:** `No documents found.`
**Status:** [ ]

### TC-3.5: List document without tags
**Setup:** Add document with empty tags array
**Command:** `kvault list`
**Expected:** Document shown without tag brackets
**Status:** [ ]

---

## 4. Add Command

### TC-4.1: Add document from stdin
**Command:**
```bash
echo "# Test Document\n\nThis is test content." | kvault add --title "Test Doc" --category test
```
**Expected:**
- Success message with title, category, path
- Document created at `$KVAULT_CORPUS/test/test-doc.md`
- Manifest updated with new document entry
**Status:** [ ]

### TC-4.2: Add document from file
**Setup:** Create a temp file with content
```bash
echo "# From File\n\nContent from file." > /tmp/test-doc.md
```
**Command:** `kvault add --title "From File" --category test --file /tmp/test-doc.md`
**Expected:** Document created successfully
**Status:** [ ]

### TC-4.3: Add document with tags
**Command:**
```bash
echo "# Tagged Doc\n\nContent." | kvault add --title "Tagged Doc" --category test --tags "tag1, tag2, tag3"
```
**Expected:** Document added with tags in manifest
**Status:** [ ]

### TC-4.4: Add document - empty title
**Command:**
```bash
echo "content" | kvault add --title "" --category test
```
**Expected:** Error: "Title cannot be empty"
**Status:** [ ]

### TC-4.5: Add document - title too long (>200 chars)
**Command:**
```bash
echo "content" | kvault add --title "$(printf 'a%.0s' {1..201})" --category test
```
**Expected:** Error: "Title too long: 201 chars (max 200)"
**Status:** [ ]

### TC-4.6: Add document - invalid category characters
**Command:**
```bash
echo "content" | kvault add --title "Test" --category "my/category"
```
**Expected:** Error: "Category contains invalid character: '/'"
**Status:** [ ]

### TC-4.7: Add document - category starting with hyphen
**Command:**
```bash
echo "content" | kvault add --title "Test" --category "-invalid"
```
**Expected:** Error: "Category must start with a letter or number"
**Status:** [ ]

### TC-4.8: Add document - category with spaces
**Command:**
```bash
echo "content" | kvault add --title "Test" --category "my category"
```
**Expected:** Error: "Category contains invalid character: ' '"
**Status:** [ ]

### TC-4.9: Add document - invalid tag characters
**Command:**
```bash
echo "content" | kvault add --title "Test" --category test --tags "valid, in/valid"
```
**Expected:** Error: "Tag contains invalid character: '/'"
**Status:** [ ]

### TC-4.10: Add document - empty content from stdin
**Command:**
```bash
echo "" | kvault add --title "Empty" --category test
```
**Expected:** Error: "Content cannot be empty"
**Status:** [ ]

### TC-4.11: Add document - whitespace-only content
**Command:**
```bash
echo "   " | kvault add --title "Whitespace" --category test
```
**Expected:** Error: "Content cannot be empty"
**Status:** [ ]

### TC-4.12: Add document - already exists
**Setup:** Add a document first, then try to add with same title/category
**Command:**
```bash
echo "content" | kvault add --title "Duplicate" --category test
echo "content" | kvault add --title "Duplicate" --category test
```
**Expected:** Second add fails: "Document already exists: test/duplicate.md"
**Status:** [ ]

### TC-4.13: Add document - file not found
**Command:** `kvault add --title "Test" --category test --file /nonexistent/path.md`
**Expected:** Error: "Failed to read file /nonexistent/path.md: ..."
**Status:** [ ]

### TC-4.14: Add document - slugify special characters
**Command:**
```bash
echo "content" | kvault add --title "AWS Lambda: Best Practices!" --category aws
```
**Expected:** Document created at `aws/aws-lambda-best-practices.md`
**Status:** [ ]

### TC-4.15: Add document - tags with extra whitespace
**Command:**
```bash
echo "content" | kvault add --title "Test" --category test --tags "  tag1  ,  tag2  "
```
**Expected:** Tags trimmed to ["tag1", "tag2"]
**Status:** [ ]

### TC-4.16: Add document - tags with empty entries filtered
**Command:**
```bash
echo "content" | kvault add --title "Test" --category test --tags "tag1,,tag2,"
```
**Expected:** Tags filtered to ["tag1", "tag2"]
**Status:** [ ]

### TC-4.17: Add document - category path traversal attempt
**Command:**
```bash
echo "content" | kvault add --title "Test" --category "../etc"
```
**Expected:** Error about invalid character (. is not alphanumeric)
**Status:** [ ]

---

## 5. Get Command

### TC-5.1: Get existing document
**Command:** `kvault get "rust/error-handling.md"`
**Expected:** Outputs full document content to stdout
**Status:** [ ]

### TC-5.2: Get document - not found
**Command:** `kvault get "nonexistent/doc.md"`
**Expected:** Error: "Document not found: nonexistent/doc.md"
**Status:** [ ]

### TC-5.3: Get document - path traversal attempt
**Command:** `kvault get "../../../etc/passwd"`
**Expected:** Error: "Invalid document path: contains '..' component"
**Status:** [ ]

### TC-5.4: Get document - path not in manifest but file exists
**Setup:** Create a file that's not in manifest.json
**Command:** `kvault get "orphan/file.md"`
**Expected:** Error: "Document not found" (must be in manifest)
**Status:** [ ]

---

## 6. Edge Cases and Error Conditions

### TC-6.1: No corpus paths configured
**Setup:** Empty paths array in config.toml
**Command:** `kvault search "test"`
**Expected:** No results (no corpora to search)
**Status:** [ ]

### TC-6.2: Config file with invalid TOML
**Setup:** Write invalid TOML to config.toml
**Command:** `kvault search "test"`
**Expected:** Error about config parsing
**Status:** [ ]

### TC-6.3: Multiple corpora configured
**Setup:** Configure multiple corpus paths in config.toml
**Command:** `kvault search "test"`
**Expected:** Searches all corpora, combines results
**Status:** [ ]

### TC-6.4: Corpus with documents but missing files
**Setup:** Add document to manifest but don't create the file
**Command:** `kvault get "missing/file.md"`
**Expected:** Error about file not found
**Status:** [ ]

### TC-6.5: KVAULT_CONFIG env var overrides default
**Setup:** Create a separate config file with different corpus path
**Command:**
```bash
export KVAULT_CONFIG="/tmp/alt-config.toml"
kvault list
```
**Expected:** Uses the config from KVAULT_CONFIG, not default location
**Status:** [ ]

### TC-6.6: KVAULT_CONFIG not set uses default
**Setup:** Unset KVAULT_CONFIG, ensure ~/.config/kvault/config.toml exists
**Command:**
```bash
unset KVAULT_CONFIG
kvault list
```
**Expected:** Uses default config location
**Status:** [ ]

---

## 7. Output Format Verification

### TC-7.1: Search output format
**Command:** `kvault search "error"`
**Expected Format:**
```
Error Handling: /path/to/corpus/rust/error-handling.md (line N)
  <matched line content>

N result(s) found
```
**Status:** [ ]

### TC-7.2: List output format
**Command:** `kvault list`
**Expected Format:**
```
rust: Error Handling [rust, errors]
  /path/to/corpus/rust/error-handling.md
aws: Lambda Patterns [aws, lambda]
  /path/to/corpus/aws/lambda-patterns.md
```
**Status:** [ ]

### TC-7.3: Add output format
**Command:** `echo "content" | kvault add --title "Test" --category test`
**Expected Format:**
```
Added: Test
  Category: test
  Path: /path/to/corpus/test/test.md
```
**Status:** [ ]

---

## Cleanup

```bash
# Remove test environment
rm -rf "$KVAULT_TEST_DIR"
unset KVAULT_TEST_DIR KVAULT_CORPUS KVAULT_CONFIG
unalias kvault
```

---

## Test Summary

| Section | Total | Passed | Failed | Skipped |
|---------|-------|--------|--------|---------|
| Help/No Command | 3 | | | |
| Search | 11 | | | |
| List | 5 | | | |
| Add | 17 | | | |
| Get | 4 | | | |
| Edge Cases | 6 | | | |
| Output Format | 3 | | | |
| **TOTAL** | **49** | | | |

---

## Notes for Automation

When converting to automated tests:

1. **Test isolation**: Each test should create its own temp directory
2. **Config override**: Use `KVAULT_CONFIG` environment variable (already implemented)
3. **Cleanup**: Ensure temp directories are cleaned up even on test failure
4. **Assertions**:
   - Check exit codes (0 for success, non-zero for errors)
   - Check stdout/stderr content
   - Check filesystem state (files created, manifest updated)
5. **Fixtures**: Create reusable corpus fixtures for common test scenarios
6. **Recommended crates**:
   - `assert_cmd` - CLI testing with command builder
   - `predicates` - Fluent assertions for stdout/stderr
   - `tempfile` - Already in dev-dependencies

Example automated test:
```rust
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn search_with_no_matches() {
    let env = TestEnv::new();  // Creates temp corpus + config

    Command::cargo_bin("kvault")
        .unwrap()
        .env("KVAULT_CONFIG", env.config_path())
        .args(["search", "xyznonexistent"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No matches found"));
}
```
