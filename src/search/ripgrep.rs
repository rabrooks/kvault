//! Ripgrep-based search backend.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

use crate::corpus::{Corpus, Document};
use crate::search::{SearchBackend, SearchOptions, SearchResult};

/// Maximum allowed query length to prevent abuse.
const MAX_QUERY_LENGTH: usize = 1000;

/// Search backend using ripgrep for fast text search.
///
/// Uses `--fixed-strings` mode to treat queries as literal text rather than
/// regex patterns, preventing regex denial-of-service attacks and unexpected behavior.
#[derive(Default)]
pub struct RipgrepBackend;

impl RipgrepBackend {
    /// Create a new ripgrep search backend.
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

impl SearchBackend for RipgrepBackend {
    fn search(
        &self,
        query: &str,
        corpus: &Corpus,
        options: &SearchOptions,
    ) -> anyhow::Result<Vec<SearchResult>> {
        Self::check_available()?;

        // Validate query to prevent abuse
        if query.is_empty() {
            return Ok(vec![]);
        }

        if query.len() > MAX_QUERY_LENGTH {
            anyhow::bail!(
                "Query too long: {} chars (max {})",
                query.len(),
                MAX_QUERY_LENGTH
            );
        }

        // Reject queries with null bytes (could cause issues with C-based tools)
        if query.contains('\0') {
            anyhow::bail!("Query contains invalid characters");
        }

        let mut cmd = Command::new("rg");
        cmd.arg("--json")
            // Use fixed-strings to treat query as literal text, not regex.
            // This prevents ReDoS attacks and unexpected regex behavior.
            .arg("--fixed-strings")
            // Exclude manifest.json from search results
            .arg("--glob")
            .arg("!manifest.json")
            .arg("--max-count")
            .arg(options.limit.unwrap_or(100).to_string());

        // Case-insensitive by default, unless --case-sensitive is specified
        if !options.case_sensitive {
            cmd.arg("--ignore-case");
        }

        let output = cmd
            .arg("--") // End of options, query follows
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

/// Parsed match from ripgrep JSON output.
struct RgMatch {
    path: PathBuf,
    matched_line: String,
    line_number: usize,
}

#[derive(Debug, Deserialize)]
struct RgMessage {
    #[serde(rename = "type")]
    msg_type: String,
    data: Option<RgMatchData>,
}

#[derive(Debug, Deserialize)]
struct RgMatchData {
    path: Option<RgText>,
    lines: Option<RgText>,
    line_number: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct RgText {
    text: String,
}

/// Parse a single line of ripgrep JSON output into a match.
fn parse_rg_line(line: &str) -> Option<RgMatch> {
    let msg: RgMessage = serde_json::from_str(line).ok()?;

    if msg.msg_type != "match" {
        return None;
    }

    let data = msg.data?;
    Some(RgMatch {
        path: PathBuf::from(&data.path?.text),
        matched_line: data.lines?.text.trim().to_string(),
        line_number: data.line_number?,
    })
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

    let mut results: Vec<SearchResult> = output
        .lines()
        .filter_map(parse_rg_line)
        .filter_map(|m| {
            let (title, category) = doc_map.get(&m.path).map_or_else(
                || {
                    let title = m.path.file_stem().map_or_else(
                        || "Unknown".to_string(),
                        |s| s.to_string_lossy().to_string(),
                    );
                    (title, "unknown".to_string())
                },
                |doc| (doc.title.clone(), doc.category.clone()),
            );

            if let Some(ref cat) = options.category
                && &category != cat
            {
                return None;
            }

            Some(SearchResult {
                path: m.path,
                title,
                matched_line: m.matched_line,
                line_number: m.line_number,
                score: None,
            })
        })
        .collect();

    if let Some(limit) = options.limit {
        results.truncate(limit);
    }

    results
}
