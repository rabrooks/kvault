//! Knowledge corpus management and manifest parsing.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur when loading a corpus.
#[derive(Debug, Error)]
pub enum CorpusError {
    #[error("Manifest not found at {0}")]
    ManifestNotFound(PathBuf),

    #[error("Failed to read manifest: {0}")]
    ReadError(#[from] std::io::Error),

    #[error("Failed to parse manifest: {0}")]
    ParseError(#[from] serde_json::Error),
}

/// A knowledge document with metadata.
///
/// Stored in manifest.json. The path is relative to the corpus root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// Path relative to corpus root (e.g., "aws/lambda-patterns.md").
    pub path: PathBuf,
    /// Human-readable document title.
    pub title: String,
    /// Category for grouping (e.g., "aws", "rust", "devops").
    pub category: String,
    /// Optional tags for additional classification.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// The manifest.json structure listing all documents in a corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub documents: Vec<Document>,
}

/// A loaded knowledge corpus with its root path and manifest.
#[derive(Debug, Clone)]
pub struct Corpus {
    pub root: PathBuf,
    pub manifest: Manifest,
}

impl Corpus {
    /// Load a corpus from a directory containing manifest.json.
    ///
    /// # Errors
    ///
    /// Returns `CorpusError::ManifestNotFound` if manifest.json doesn't exist.
    /// Returns `CorpusError::ReadError` if the file cannot be read.
    /// Returns `CorpusError::ParseError` if the JSON is invalid.
    pub fn load(root: &Path) -> Result<Self, CorpusError> {
        let manifest_path = root.join("manifest.json");

        if !manifest_path.exists() {
            return Err(CorpusError::ManifestNotFound(manifest_path));
        }

        let contents = fs::read_to_string(&manifest_path)?;
        let manifest: Manifest = serde_json::from_str(&contents)?;

        Ok(Self {
            root: root.to_path_buf(),
            manifest,
        })
    }

    #[must_use]
    pub fn resolve_document_path(&self, doc: &Document) -> PathBuf {
        self.root.join(&doc.path)
    }

    #[must_use]
    pub fn documents(&self) -> &[Document] {
        &self.manifest.documents
    }
}

impl Manifest {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            version: "1".to_string(),
            documents: vec![],
        }
    }
}
