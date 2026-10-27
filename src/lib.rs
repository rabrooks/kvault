//! kvault: a local-first knowledge index for coding agents.
//!
//! Markdown files on disk are the only source of truth. kvault derives a
//! disposable `SQLite` index (`FTS5` over heading-level chunks, links, tags) and
//! exposes token-budgeted retrieval to agents through a CLI, Claude Code
//! hooks, and an optional MCP adapter.
//!
//! # Module map
//!
//! | Module | Role |
//! |---|---|
//! | [`config`] | `config.toml` loading, root declarations |
//! | [`roots`] | root kinds, glob expansion, project detection |
//! | [`store`] | `SQLite` schema, migrations, connection |
//! | [`markdown`] | frontmatter, heading chunker, link extraction |
//! | [`index`] | walk roots, hash, upsert docs/chunks |
//! | [`list`] | document listing with filters |
//! | [`search`] | query parser, streams, rank fusion, explain |
//! | [`pack`] | token-budget packer for `recall` and `brief` |
//! | [`capture`] | session-note writer used by Stop/`PreCompact` hooks |
//! | [`hooks`] | Claude Code hook entry points |
//! | [`cli`] | clap command definitions |

pub mod capture;
pub mod cli;
pub mod config;
pub mod error;
pub mod hooks;
pub mod index;
pub mod list;
pub mod markdown;
pub mod pack;
pub mod roots;
pub mod search;
pub mod store;

pub use error::{Error, Result};
