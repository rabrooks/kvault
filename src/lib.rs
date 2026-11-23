//! kvault: shared memory for coding agents, built from markdown notes.
//!
//! Markdown files on disk are the only source of truth. kvault derives a
//! disposable `SQLite` index (`FTS5` over heading-level chunks, links, tags) and
//! exposes token-budgeted retrieval to agents through a CLI and Claude Code
//! hooks; an MCP adapter is planned.
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
//! | [`render`] | output formats for `search` |
//! | [`get`] | chunk and document text by id, path, or line window |
//! | [`bench`] | retrieval quality and latency over a query file |
//! | [`pack`] | token-budget packer: tiers, measure-and-drop, text trimming |
//! | [`recall`] | `search` packed into a budget, what agents call |
//! | [`brief`] | session-start context block for a working directory |
//! | [`add`] | write a note into a root and index it |
//! | [`capture`] | session-note writer used by Stop/`PreCompact` hooks |
//! | [`hooks`] | Claude Code hook entry points |
//! | [`report`] | summary of the query log for a period |
//! | [`querylog`] | local record of searches and which results were used |
//! | [`update`] | plugin/binary mismatch and new-release notices |
//! | [`cli`] | clap command definitions |

pub mod add;
pub mod bench;
pub mod brief;
pub mod capture;
pub mod cli;
pub mod config;
pub mod error;
pub mod get;
pub mod hooks;
pub mod index;
pub mod list;
pub mod markdown;
pub mod pack;
pub mod querylog;
pub mod recall;
pub mod render;
pub mod report;
pub mod roots;
pub mod search;
pub mod store;
pub mod update;

pub use error::{Error, Result};
