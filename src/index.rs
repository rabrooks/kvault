//! Indexer: walk roots, detect changes by mtime/size then blake3, chunk, upsert.
//!
//! Implemented in M1.

use crate::error::{Error, Milestone, Result};

/// Options for an index run.
#[derive(Debug, Clone, Copy, Default)]
pub struct IndexOptions {
    /// Ignore the mtime/size fast path and re-hash every file.
    pub full: bool,
    pub quiet: bool,
}

/// Run an incremental index over all configured roots.
///
/// # Errors
/// Not implemented until M1.
pub fn run(_opts: IndexOptions) -> Result<()> {
    Err(Error::NotImplemented {
        command: "index",
        milestone: Milestone::M1Index,
    })
}
