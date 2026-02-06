//! Local filesystem storage backend.

use std::fs;
use std::path::{Path, PathBuf};

use crate::corpus::Manifest;
use crate::storage::{StorageBackend, StorageError};

/// Storage backend for local filesystem operations.
pub struct LocalStorageBackend {
    root: PathBuf,
}

impl LocalStorageBackend {
    /// Create a new local storage backend rooted at the given path.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn manifest_path(&self) -> PathBuf {
        self.root.join("manifest.json")
    }
}

impl StorageBackend for LocalStorageBackend {
    fn read_manifest(&self) -> Result<Manifest, StorageError> {
        let path = self.manifest_path();

        if !path.exists() {
            return Ok(Manifest::empty());
        }

        let contents = fs::read_to_string(&path)
            .map_err(|e| StorageError::ReadError(format!("{}: {e}", path.display())))?;

        serde_json::from_str(&contents)
            .map_err(|e| StorageError::ParseError(format!("{}: {e}", path.display())))
    }

    fn write_manifest(&self, manifest: &Manifest) -> Result<(), StorageError> {
        let path = self.manifest_path();

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                StorageError::WriteError(format!("create dir {}: {e}", parent.display()))
            })?;
        }

        let contents = serde_json::to_string_pretty(manifest)
            .map_err(|e| StorageError::SerializeError(e.to_string()))?;

        fs::write(&path, contents)
            .map_err(|e| StorageError::WriteError(format!("{}: {e}", path.display())))
    }

    fn read_document(&self, path: &Path) -> Result<String, StorageError> {
        let full_path = self.root.join(path);

        if !full_path.exists() {
            return Err(StorageError::NotFound(full_path.display().to_string()));
        }

        fs::read_to_string(&full_path)
            .map_err(|e| StorageError::ReadError(format!("{}: {e}", full_path.display())))
    }

    fn write_document(&self, path: &Path, content: &str) -> Result<(), StorageError> {
        let full_path = self.root.join(path);

        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                StorageError::WriteError(format!("create dir {}: {e}", parent.display()))
            })?;
        }

        fs::write(&full_path, content)
            .map_err(|e| StorageError::WriteError(format!("{}: {e}", full_path.display())))
    }

    fn exists(&self, path: &Path) -> bool {
        self.root.join(path).exists()
    }

    fn root(&self) -> &Path {
        &self.root
    }
}
