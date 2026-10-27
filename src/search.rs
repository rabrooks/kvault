//! Search: query parsing, retrieval streams, reciprocal rank fusion, explain.
//!
//! Implemented in M2.

use crate::error::{Error, Milestone, Result};

/// A ranked hit at chunk granularity.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub chunk_id: i64,
    pub doc_id: i64,
    pub score: f64,
}

/// Run a search.
///
/// # Errors
/// Not implemented until M2.
pub fn run(_query: &str) -> Result<Vec<Hit>> {
    Err(Error::NotImplemented {
        command: "search",
        milestone: Milestone::M2Search,
    })
}
