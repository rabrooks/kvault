//! kvault: a local-first knowledge index for coding agents.
//!
//! Markdown files on disk are the only source of truth. kvault derives a
//! disposable `SQLite` index (`FTS5` over heading-level chunks, links, tags) and
//! exposes token-budgeted retrieval to agents through a CLI, Claude Code
//! hooks, and an optional MCP adapter.
//!
//! # Module map
//!
//! | Module | Role | Milestone |
//! |---|---|---|
//! | [`config`] | `config.toml` loading, root declarations | M0 |
//! | [`roots`] | root kinds, glob expansion, project detection | M0/M1 |
//! | [`store`] | `SQLite` schema, migrations, connection | M0/M1 |
//! | [`markdown`] | frontmatter, heading chunker, link extraction | M1 |
//! | [`index`] | walk roots, hash, upsert docs/chunks | M1 |
//! | [`list`] | document listing with filters | M1 |
//! | [`search`] | query parser, streams, rank fusion, explain | M2 |
//! | [`pack`] | token-budget packer for `recall` and `brief` | M3 |
//! | [`capture`] | session-note writer used by Stop/`PreCompact` hooks | M4 |
//! | [`hooks`] | Claude Code hook entry points | M4 |
//! | [`cli`] | clap command definitions | M0 |

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
