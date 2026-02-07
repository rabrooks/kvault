//! Configuration loading for kvault.

use std::env;
use std::path::PathBuf;

use directories::{BaseDirs, ProjectDirs};
use serde::Deserialize;

/// Environment variable to override config file location.
pub const KVAULT_CONFIG_ENV: &str = "KVAULT_CONFIG";

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

    /// Returns the config file path.
    ///
    /// Checks in order:
    /// 1. `KVAULT_CONFIG` environment variable (if set)
    /// 2. Default location: `~/.config/kvault/config.toml` (or platform equivalent)
    #[must_use]
    pub fn config_path() -> Option<PathBuf> {
        // Check environment variable first
        if let Ok(path) = env::var(KVAULT_CONFIG_ENV) {
            return Some(PathBuf::from(path));
        }

        // Fall back to default platform-specific location
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_with_home() {
        let result = expand_tilde("~/.kvault");
        // Should expand to something like /Users/name/.kvault
        assert!(!result.to_string_lossy().starts_with('~'));
        assert!(result.to_string_lossy().ends_with(".kvault"));
    }

    #[test]
    fn expand_tilde_absolute_path() {
        let result = expand_tilde("/absolute/path");
        assert_eq!(result, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn expand_tilde_relative_path() {
        let result = expand_tilde("relative/path");
        assert_eq!(result, PathBuf::from("relative/path"));
    }

    #[test]
    fn expand_tilde_just_tilde() {
        // "~" alone without "/" should not be expanded
        let result = expand_tilde("~");
        assert_eq!(result, PathBuf::from("~"));
    }

    #[test]
    fn default_corpus_paths_returns_kvault() {
        let paths = default_corpus_paths();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], "~/.kvault");
    }

    #[test]
    fn config_default_has_corpus_paths() {
        let config = Config::default();
        assert!(!config.corpus.paths.is_empty());
    }

    #[test]
    fn config_path_respects_env_var() {
        let test_path = "/custom/config/path.toml";

        // SAFETY: Test is single-threaded and we restore the env var after
        unsafe {
            std::env::set_var(KVAULT_CONFIG_ENV, test_path);
        }

        let result = Config::config_path();
        assert_eq!(result, Some(PathBuf::from(test_path)));

        // Clean up
        unsafe {
            std::env::remove_var(KVAULT_CONFIG_ENV);
        }
    }

    #[test]
    fn config_path_falls_back_to_default() {
        // Ensure env var is not set
        unsafe {
            std::env::remove_var(KVAULT_CONFIG_ENV);
        }

        let result = Config::config_path();

        // Should return Some path ending in config.toml
        assert!(result.is_some());
        let path = result.unwrap();
        assert!(path.to_string_lossy().ends_with("config.toml"));
    }
}
