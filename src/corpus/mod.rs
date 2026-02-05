use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub path: PathBuf,
    pub title: String,
    pub category: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub documents: Vec<Document>,
}

#[derive(Debug, Clone)]
pub struct Corpus {
    pub root: PathBuf,
    pub manifest: Manifest,
}

impl Corpus {
    pub fn load(_path: &PathBuf) -> anyhow::Result<Self> {
        todo!("Implement corpus loading in Phase 2")
    }
}
