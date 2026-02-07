//! CLI interface for kvault.
//!
//! Provides command-line argument parsing using clap.

use clap::{Parser, Subcommand};

/// Default number of search results to return.
pub const DEFAULT_SEARCH_LIMIT: usize = 10;

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

    /// Start the MCP server for AI editor integration.
    #[cfg(feature = "mcp")]
    Serve,
}
