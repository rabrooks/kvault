pub mod cli;
pub mod commands;
pub mod config;
pub mod corpus;
pub mod search;
pub mod storage;

#[cfg(feature = "mcp")]
pub mod mcp;
