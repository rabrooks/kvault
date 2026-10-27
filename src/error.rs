//! Crate-wide error type.

use std::path::PathBuf;

/// Roadmap milestone named in "not implemented" errors; the README lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Milestone {
    M1Index,
    M2Search,
    M3Recall,
    M4Hooks,
    M5Graph,
    M6Mcp,
}

impl std::fmt::Display for Milestone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::M1Index => "M1 (index)",
            Self::M2Search => "M2 (search)",
            Self::M3Recall => "M3 (recall/brief)",
            Self::M4Hooks => "M4 (hooks/capture)",
            Self::M5Graph => "M5 (links)",
            Self::M6Mcp => "M6 (mcp)",
        };
        f.write_str(s)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("`{command}` is not implemented yet (planned for milestone {milestone})")]
    NotImplemented {
        command: &'static str,
        milestone: Milestone,
    },

    #[error("config file {path}: {source}")]
    Config {
        path: PathBuf,
        #[source]
        source: anyhow::Error,
    },

    #[error("database {path}: {source}")]
    Store {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
