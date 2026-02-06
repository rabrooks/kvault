//! Storage backend trait and implementations.
//!
//! This module provides an abstraction for storage operations, allowing
//! kvault to work with different storage backends (local filesystem, S3, etc.).

pub mod local;

use std::path::Path;

use crate::corpus::Manifest;

/// Errors that can occur during storage operations.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("Path not found: {0}")]
    NotFound(String),

    #[error("Failed to read: {0}")]
    ReadError(String),

    #[error("Failed to write: {0}")]
    WriteError(String),

    #[error("Failed to parse manifest: {0}")]
    ParseError(String),

    #[error("Failed to serialize: {0}")]
    SerializeError(String),
}

/// Trait for storage backends (local filesystem, S3, database, etc.).
pub trait StorageBackend: Send + Sync {
    /// Read the manifest from the storage root.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` if the manifest cannot be read or parsed.
    fn read_manifest(&self) -> Result<Manifest, StorageError>;

    /// Write the manifest to the storage root.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` if the manifest cannot be written.
    fn write_manifest(&self, manifest: &Manifest) -> Result<(), StorageError>;

    /// Read a document's content.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` if the document cannot be read.
    fn read_document(&self, path: &Path) -> Result<String, StorageError>;

    /// Write a document's content.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` if the document cannot be written.
    fn write_document(&self, path: &Path, content: &str) -> Result<(), StorageError>;

    /// Check if a path exists in storage.
    fn exists(&self, path: &Path) -> bool;

    /// Get the root path/identifier for this storage backend.
    fn root(&self) -> &Path;
}
