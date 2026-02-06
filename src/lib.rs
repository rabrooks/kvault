//! kvault - A searchable knowledge corpus.
//!
//! This library provides tools for managing and searching a personal knowledge base.
//! It supports multiple storage backends (local filesystem, with S3 planned) and
//! search algorithms (ripgrep for fast text search, Tantivy for BM25 ranking).
//!
//! # Modules
//!
//! - [`commands`] - High-level operations (search, list, add, get)
//! - [`corpus`] - Document and manifest types
//! - [`search`] - Search backend trait and implementations
//! - [`storage`] - Storage backend trait and implementations
//! - [`config`] - Configuration loading
//! - [`cli`] - Command-line interface definitions

pub mod cli;
pub mod commands;
pub mod config;
pub mod corpus;
pub mod search;
pub mod storage;

#[cfg(feature = "mcp")]
pub mod mcp;
