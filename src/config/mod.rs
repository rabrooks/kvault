//! Configuration loading for kvault.

use std::path::PathBuf;

use directories::{BaseDirs, ProjectDirs};
use serde::Deserialize;

/// Top-level configuration loaded from config.toml.
#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub corpus: CorpusConfig,
}

/// Configuration for knowledge corpus locations.
#[derive(Debug, Deserialize)]
pub struct CorpusConfig {
    #[serde(default = "default_corpus_paths")]
    pub paths: Vec<String>,
}

fn default_corpus_paths() -> Vec<String> {
    vec![
        "~/.claude/knowledge".to_string(),
        "./.claude/knowledge".to_string(),
    ]
}

impl Default for Config {
    fn default() -> Self {
        Self {
            corpus: CorpusConfig::default(),
        }
    }
}

impl Default for CorpusConfig {
    fn default() -> Self {
        Self {
            paths: default_corpus_paths(),
        }
    }
}

impl Config {
    /// Load config from ~/.config/kvault/config.toml, or return defaults.
    pub fn load() -> anyhow::Result<Self> {
        let config_path = Self::config_path();

        if let Some(path) = config_path {
            if path.exists() {
                let contents = std::fs::read_to_string(&path)?;
                let config: Config = toml::from_str(&contents)?;
                return Ok(config);
            }
        }

        Ok(Config::default())
    }

    pub fn config_path() -> Option<PathBuf> {
        ProjectDirs::from("", "", "kvault")
            .map(|dirs| dirs.config_dir().join("config.toml"))
    }
}

/// Expand ~ to the user's home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    if path.starts_with("~/") {
        if let Some(base_dirs) = BaseDirs::new() {
            return base_dirs.home_dir().join(&path[2..]);
        }
    }
    PathBuf::from(path)
}
