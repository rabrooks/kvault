//! Command-line surface. The CLI *is* the API; hooks and MCP wrap it.

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "kvault",
    version,
    about = "Local-first knowledge index for coding agents"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Write a config with suggested roots, then index
    Init {
        /// Overwrite an existing config
        #[arg(long)]
        force: bool,
    },
    /// Incrementally (re)index all configured roots
    Index {
        /// Re-hash every file, ignoring the mtime/size fast path
        #[arg(long)]
        full: bool,
        /// Only this root path
        #[arg(long)]
        root: Option<String>,
        #[arg(long, short)]
        quiet: bool,
    },
    /// Ranked search over chunks
    Search(SearchArgs),
    /// Search and pack results into a token budget (what agents call)
    Recall {
        query: String,
        #[arg(long, default_value_t = 1500)]
        budget: usize,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, value_enum, default_value_t = RecallFormat::Md)]
        format: RecallFormat,
    },
    /// Print a chunk by id, a file by path, or a line window `path:from:count`
    Get {
        target: String,
        /// Print the whole document instead of the chunk
        #[arg(long)]
        doc: bool,
    },
    /// What the `SessionStart` hook injects for a working directory
    Brief {
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        budget: Option<usize>,
    },
    /// Write a note into a root and index it
    Add {
        #[arg(long)]
        title: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        tags: Option<String>,
        #[arg(long)]
        root: Option<String>,
        /// Read content from a file instead of stdin
        #[arg(long)]
        file: Option<String>,
        /// Refuse to overwrite unless the existing file's blake3 matches
        #[arg(long)]
        expected_hash: Option<String>,
    },
    /// List indexed documents
    List {
        #[arg(long)]
        root: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        recent: Option<u32>,
    },
    /// Show link neighbours of a document
    Links {
        target: String,
        #[arg(long, conflicts_with = "out")]
        r#in: bool,
        #[arg(long)]
        out: bool,
    },
    /// Roots, counts, index age, database size
    Status,
    /// Run a retrieval benchmark file
    Bench { file: String },
    /// Claude Code hook entry points (stdin `JSON` → stdout `JSON`, always exit 0)
    Hook {
        #[arg(value_enum)]
        event: HookEvent,
    },
    /// Start the MCP stdio server
    #[cfg(feature = "mcp")]
    Serve,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    pub query: String,
    #[arg(long, short = 'n', default_value_t = 10)]
    pub limit: usize,
    #[arg(long)]
    pub project: Option<String>,
    #[arg(long)]
    pub root: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub tag: Vec<String>,
    #[arg(long)]
    pub path: Option<String>,
    /// e.g. `30d`, `2026-10-01`
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long, value_enum, default_value_t = SearchFormat::Cli)]
    pub format: SearchFormat,
    /// Show per-stream ranks and timings for each hit
    #[arg(long)]
    pub explain: bool,
    /// Return every matching chunk instead of the best chunk per document
    #[arg(long)]
    pub per_chunk: bool,
    /// Disable the link-neighbour stream
    #[arg(long)]
    pub no_graph: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum SearchFormat {
    Cli,
    Json,
    Md,
    Paths,
    Ids,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum RecallFormat {
    Md,
    Json,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum HookEvent {
    SessionStart,
    Stop,
    PreCompact,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
