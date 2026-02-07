//! CLI interface for kvault.
//!
//! Provides command-line argument parsing using clap.

use clap::{Parser, Subcommand, ValueEnum};

/// Default number of search results to return.
pub const DEFAULT_SEARCH_LIMIT: usize = 10;

/// Search backend selection.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum Backend {
    /// Use ripgrep for fast text search (default).
    #[default]
    Ripgrep,
    /// Use Tantivy for BM25 ranked search (requires `ranked` feature).
    #[cfg(feature = "ranked")]
    Ranked,
    /// Automatically select based on corpus size and index availability.
    Auto,
}

/// Command-line interface for kvault.
#[derive(Parser)]
#[command(name = "kvault")]
#[command(author, version, about = "Searchable knowledge corpus", long_about = None)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Available CLI commands.
#[derive(Subcommand)]
pub enum Commands {
    /// Search the knowledge corpus for documents matching a query.
    Search {
        /// The search query string.
        query: String,

        /// Maximum number of results to return.
        #[arg(short, long, default_value_t = DEFAULT_SEARCH_LIMIT)]
        limit: usize,

        /// Filter results to this category only.
        #[arg(short, long)]
        category: Option<String>,

        /// Use case-sensitive matching (default is case-insensitive).
        #[arg(short = 's', long)]
        case_sensitive: bool,

        /// Search backend to use.
        #[arg(short, long, default_value = "ripgrep")]
        backend: Backend,

        /// Enable fuzzy search with specified edit distance (1-2).
        /// Only available with the `ranked` backend.
        #[arg(short, long)]
        fuzzy: Option<u8>,
    },

    /// List all documents in the corpus.
    List {
        /// Filter results to this category only.
        #[arg(short, long)]
        category: Option<String>,
    },

    /// Add a new document to the corpus.
    Add {
        /// Human-readable document title.
        #[arg(short, long)]
        title: String,

        /// Category for grouping (e.g., "aws", "rust").
        #[arg(short = 'C', long)]
        category: String,

        /// Comma-separated tags for additional classification.
        #[arg(short = 'T', long)]
        tags: Option<String>,

        /// Read content from file instead of stdin.
        #[arg(short, long)]
        file: Option<String>,
    },

    /// Get the full contents of a document by its path.
    Get {
        /// Document path (e.g., "aws/lambda-patterns.md").
        path: String,
    },

    /// Build or rebuild the search index for all corpora.
    /// Requires the `ranked` feature.
    #[cfg(feature = "ranked")]
    Index,

    /// Start the MCP server for AI editor integration.
    #[cfg(feature = "mcp")]
    Serve,
}
