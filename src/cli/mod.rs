use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "kvault")]
#[command(author, version, about = "Searchable knowledge corpus with BM25 ranking", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Search the knowledge corpus
    Search {
        query: String,

        #[arg(short, long, default_value = "10")]
        limit: usize,

        #[arg(short, long)]
        category: Option<String>,

        /// all, global, or project
        #[arg(short, long, default_value = "all")]
        scope: String,
    },

    /// List documents in the corpus
    List {
        #[arg(short, long)]
        category: Option<String>,

        /// all, global, or project
        #[arg(short, long, default_value = "all")]
        scope: String,
    },

    /// Add a document to the corpus
    Add {
        #[arg(short, long)]
        title: String,

        #[arg(short = 'C', long)]
        category: String,

        /// Comma-separated
        #[arg(short = 'T', long)]
        tags: Option<String>,

        /// global or project
        #[arg(short, long, default_value = "global")]
        scope: String,

        /// Read content from file instead of stdin
        #[arg(short, long)]
        file: Option<String>,
    },

    /// Get a document by path
    Get { path: String },

    /// Start the MCP server
    #[cfg(feature = "mcp")]
    Serve,
}
