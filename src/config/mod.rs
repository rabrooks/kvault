//! Configuration loading for kvault.

use std::path::PathBuf;

use directories::{BaseDirs, ProjectDirs};
use serde::Deserialize;

/// Top-level configuration loaded from config.toml.
#[derive(Debug, Default, Deserialize)]
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
    vec!["~/.kvault".to_string()]
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
    ///
    /// # Errors
    ///
    /// Returns an error if the config file exists but cannot be read or parsed.
    pub fn load() -> anyhow::Result<Self> {
        if let Some(path) = Self::config_path()
            && path.exists()
        {
            let contents = std::fs::read_to_string(&path)?;
            let config: Config = toml::from_str(&contents)?;
            return Ok(config);
        }

        Ok(Config::default())
    }

    #[must_use]
    pub fn config_path() -> Option<PathBuf> {
        ProjectDirs::from("", "", "kvault").map(|dirs| dirs.config_dir().join("config.toml"))
    }
}

/// Expand ~ to the user's home directory.
#[must_use]
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/")
        && let Some(base_dirs) = BaseDirs::new()
    {
        return base_dirs.home_dir().join(stripped);
    }
    PathBuf::from(path)
}
