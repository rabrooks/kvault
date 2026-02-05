//! Ripgrep-based search backend.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

use crate::corpus::{Corpus, Document};
use crate::search::{SearchBackend, SearchOptions, SearchResult};

/// Search backend using ripgrep for fast text search.
pub struct RipgrepBackend;

impl RipgrepBackend {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Check if ripgrep is available in PATH.
    ///
    /// # Errors
    ///
    /// Returns an error with install instructions if ripgrep is not found.
    pub fn check_available() -> anyhow::Result<()> {
        match Command::new("rg").arg("--version").output() {
            Ok(output) if output.status.success() => Ok(()),
            _ => anyhow::bail!(
                "ripgrep not found\n\n\
                Install ripgrep:\n  \
                brew install ripgrep    # macOS\n  \
                cargo install ripgrep   # any platform\n  \
                apt install ripgrep     # Debian/Ubuntu"
            ),
        }
    }
}

impl Default for RipgrepBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchBackend for RipgrepBackend {
    fn search(
        &self,
        query: &str,
        corpus: &Corpus,
        options: &SearchOptions,
    ) -> anyhow::Result<Vec<SearchResult>> {
        Self::check_available()?;

        if query.is_empty() {
            return Ok(vec![]);
        }

        let output = Command::new("rg")
            .arg("--json")
            .arg("--max-count")
            .arg(options.limit.unwrap_or(100).to_string())
            .arg(query)
            .arg(&corpus.root)
            .output()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let results = parse_ripgrep_output(&stdout, corpus, options);

        Ok(results)
    }

    fn index(&self, _corpus: &Corpus) -> anyhow::Result<()> {
        // Ripgrep doesn't need indexing
        Ok(())
    }

    fn needs_indexing(&self) -> bool {
        false
    }
}

#[derive(Debug, Deserialize)]
struct RgMessage {
    #[serde(rename = "type")]
    msg_type: String,
    data: Option<RgMatchData>,
}

#[derive(Debug, Deserialize)]
struct RgMatchData {
    path: Option<RgPath>,
    lines: Option<RgLines>,
    line_number: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct RgPath {
    text: String,
}

#[derive(Debug, Deserialize)]
struct RgLines {
    text: String,
}

fn parse_ripgrep_output(
    output: &str,
    corpus: &Corpus,
    options: &SearchOptions,
) -> Vec<SearchResult> {
    let doc_map: HashMap<PathBuf, &Document> = corpus
        .documents()
        .iter()
        .map(|d| (corpus.resolve_document_path(d), d))
        .collect();

    let mut results = Vec::new();

    for line in output.lines() {
        if line.is_empty() {
            continue;
        }

        let msg: RgMessage = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(_) => continue,
        };

        if msg.msg_type != "match" {
            continue;
        }

        let Some(data) = msg.data else { continue };
        let Some(path_data) = data.path else { continue };
        let Some(lines_data) = data.lines else {
            continue;
        };
        let Some(line_number) = data.line_number else {
            continue;
        };

        let path = PathBuf::from(&path_data.text);

        let (title, category) = if let Some(doc) = doc_map.get(&path) {
            (doc.title.clone(), doc.category.clone())
        } else {
            let title = path.file_stem().map_or_else(
                || "Unknown".to_string(),
                |s| s.to_string_lossy().to_string(),
            );
            (title, "unknown".to_string())
        };

        if let Some(ref cat) = options.category
            && &category != cat
        {
            continue;
        }

        results.push(SearchResult {
            path,
            title,
            matched_line: lines_data.text.trim().to_string(),
            line_number,
            score: None,
        });
    }

    if let Some(limit) = options.limit {
        results.truncate(limit);
    }

    results
}
