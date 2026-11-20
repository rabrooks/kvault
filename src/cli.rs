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
        /// A notes directory, or a quoted glob of them such as
        /// '~/code/*/notes'. Repeat for several. Added alongside the
        /// usual locations that exist
        #[arg(long, value_name = "PATH")]
        notes: Vec<String>,
    },
    /// Change which directories are indexed
    Root {
        #[command(subcommand)]
        command: RootCommand,
    },
    /// Incrementally (re)index all configured roots
    Index {
        /// Re-hash every file, ignoring the mtime/size fast path
        #[arg(long)]
        full: bool,
        /// Only this root path
        #[arg(long)]
        root: Option<String>,
        /// Drop the index rows of roots that no longer exist (their
        /// documents are already hidden from results)
        #[arg(long)]
        prune: bool,
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
    /// List indexed documents, newest first
    List {
        #[arg(long)]
        root: Option<String>,
        /// Root kind (memory, notes, reference) or frontmatter kind
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        project: Option<String>,
        /// Only documents updated in the last N days
        #[arg(long)]
        recent: Option<u32>,
        #[arg(long, short = 'n')]
        limit: Option<usize>,
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
    /// What the query log says about a period of use: counts and rates
    /// only, safe to hand to someone else
    Report {
        /// Only events since then, e.g. `7d`, `2026-11-20`
        #[arg(long)]
        since: Option<String>,
        /// The summary as `JSON`, for pooling several people's reports
        #[arg(long, conflicts_with = "full")]
        json: bool,
        /// The log lines themselves, which include query text and file
        /// paths: read before sharing
        #[arg(long)]
        full: bool,
    },
    /// Run a retrieval benchmark file (JSON lines: query, expected, note)
    Bench {
        file: String,
        /// Also score a case-insensitive grep over the same files
        #[arg(long)]
        baseline: bool,
        /// Fail (exit 1) if hit@5 falls below this
        #[arg(long)]
        min_hit5: Option<f64>,
        /// Results considered per query
        #[arg(long, short = 'n', default_value_t = 10)]
        limit: usize,
        /// List every miss with the top paths returned
        #[arg(long, short)]
        verbose: bool,
    },
    /// Claude Code hook entry points (stdin `JSON` → stdout `JSON`, always exit 0)
    Hook {
        #[arg(value_enum)]
        event: HookEvent,
    },
    /// Start the MCP stdio server
    #[cfg(feature = "mcp")]
    Serve,
}

#[derive(Debug, Subcommand)]
pub enum RootCommand {
    /// Add a directory (or a quoted glob of directories) to the config and
    /// index it
    Add {
        path: String,
        #[arg(long, value_enum, default_value_t = KindArg::Notes)]
        kind: KindArg,
        /// How files under it get their project name; worked out from the
        /// path when omitted
        #[arg(long, value_enum)]
        project_from: Option<ProjectFromArg>,
        /// Ranking weight; 1.0 is neutral
        #[arg(long)]
        weight: Option<f64>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum KindArg {
    Notes,
    Memory,
    Reference,
    Other,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum ProjectFromArg {
    /// The directory above the root: `<project>/notes`
    Parent,
    /// The root's own name
    Dir,
    /// A Claude Code memory directory
    ClaudeSlug,
    /// No project
    None,
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
