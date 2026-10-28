//! Configuration: `~/.config/kvault/config.toml`, overridable with `KVAULT_CONFIG`.
//!
//! The config declares *roots* (directories of markdown indexed in place),
//! capture settings for session notes, the session-start brief budget, and
//! optional path-prefix context strings.

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};

use directories::BaseDirs;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::roots::RootKind;

/// Environment variable that overrides the config file location.
pub const KVAULT_CONFIG_ENV: &str = "KVAULT_CONFIG";

/// Environment variable that overrides the index database location.
pub const KVAULT_DB_ENV: &str = "KVAULT_DB";

/// Environment variable that overrides the hook log location.
pub const KVAULT_LOG_ENV: &str = "KVAULT_LOG";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub roots: Vec<RootConfig>,
    pub capture: CaptureConfig,
    pub brief: BriefConfig,
    /// Path-prefix descriptions injected onto hits under that prefix.
    pub context: BTreeMap<String, String>,
    /// Per-project settings, keyed by the project's canonical name.
    pub projects: BTreeMap<String, ProjectConfig>,
}

/// Settings for one project, `[projects.<name>]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
    /// Other names roots derive for this project. A root whose
    /// `project_from` yields an alias is indexed under the canonical name,
    /// so two roots that name the same project differently (a notes dir's
    /// parent `mas`, a Claude memory slug `work-mas`) share one project.
    pub aliases: Vec<String>,
}

/// One indexed root. `path` may contain `~` and glob metacharacters
/// (`~/Projects/*/notes`), expanded at index time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RootConfig {
    pub path: String,
    #[serde(default)]
    pub kind: RootKind,
    /// Ranking authority multiplier contribution; 1.0 is neutral.
    #[serde(default = "default_weight")]
    pub weight: f64,
    /// How to derive the project name for files under this root.
    #[serde(default)]
    pub project_from: ProjectFrom,
    /// Globs, relative to the root, whose files rank below their peers:
    /// still searchable, but a close live note wins. Empty disables.
    #[serde(default = "default_demote")]
    pub demote: Vec<String>,
    /// Authority factor for demoted files, in `(0, 1]`. Applied inside the
    /// authority clamp, so it can never move a hit by more than the band.
    #[serde(default = "default_demote_factor")]
    pub demote_factor: f64,
}

fn default_weight() -> f64 {
    1.0
}

/// Finished work conventionally lives under `archive/` at any depth.
fn default_demote() -> Vec<String> {
    vec!["**/archive/**".to_owned()]
}

/// About six RRF ranks at the top of a k=60 list: enough for a live note to
/// beat an archived one of similar relevance, not enough to bury a clearly
/// better archived hit.
pub const DEFAULT_DEMOTE_FACTOR: f64 = 0.9;

fn default_demote_factor() -> f64 {
    DEFAULT_DEMOTE_FACTOR
}

impl RootConfig {
    /// A root with the default weight and demotion.
    #[must_use]
    pub fn new(path: impl Into<String>, kind: RootKind, project_from: ProjectFrom) -> Self {
        Self {
            path: path.into(),
            kind,
            weight: default_weight(),
            project_from,
            demote: default_demote(),
            demote_factor: default_demote_factor(),
        }
    }
}

/// Strategy for naming the project a root's files belong to.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectFrom {
    /// No project; files are global.
    #[default]
    None,
    /// Parent directory of the matched root (`~/Projects/<name>/notes` → `name`).
    Parent,
    /// Claude Code memory slug (`-Users-x-Projects-<name>` → `name`).
    ClaudeSlug,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureConfig {
    pub enabled: bool,
    /// Relative to the project directory.
    pub session_dir: String,
    /// Used when the project has no notes dir.
    pub fallback_dir: String,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            session_dir: "notes/sessions".to_owned(),
            fallback_dir: "~/.local/share/kvault/sessions".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct BriefConfig {
    /// Token budget for the `SessionStart` brief.
    pub budget: usize,
    pub recent_days: u32,
}

impl Default for BriefConfig {
    fn default() -> Self {
        Self {
            budget: 2000,
            recent_days: 14,
        }
    }
}

impl Config {
    /// Load from `KVAULT_CONFIG` or the platform config dir; defaults if absent.
    ///
    /// # Errors
    /// Returns [`Error::Config`] if the file exists but cannot be read or parsed.
    pub fn load() -> Result<Self> {
        match Self::config_path() {
            Some(path) if path.exists() => Self::load_from(&path),
            _ => Ok(Self::default()),
        }
    }

    /// Load from an explicit path.
    ///
    /// # Errors
    /// Returns [`Error::Config`] on read or parse failure.
    pub fn load_from(path: &Path) -> Result<Self> {
        let wrap = |e: anyhow::Error| Error::Config {
            path: path.to_path_buf(),
            source: e,
        };
        let text = std::fs::read_to_string(path).map_err(|e| wrap(e.into()))?;
        let config: Self = toml::from_str(&text).map_err(|e| wrap(e.into()))?;
        config.validate().map_err(wrap)?;
        Ok(config)
    }

    /// Reject settings that parse but cannot mean anything coherent.
    ///
    /// # Errors
    /// Describes the first problem found.
    pub fn validate(&self) -> anyhow::Result<()> {
        for root in &self.roots {
            if !(root.demote_factor > 0.0 && root.demote_factor <= 1.0) {
                anyhow::bail!(
                    "root {}: demote_factor must be in (0, 1], got {}",
                    root.path,
                    root.demote_factor
                );
            }
            for pattern in &root.demote {
                crate::roots::demote_glob(pattern)
                    .map_err(|e| anyhow::anyhow!("root {}: demote {pattern:?}: {e}", root.path))?;
            }
        }
        let mut owner: BTreeMap<&str, &str> = BTreeMap::new();
        for (name, project) in &self.projects {
            for alias in &project.aliases {
                if alias == name {
                    continue;
                }
                // One level only: an alias that is itself canonical would make
                // the project a name resolves to depend on lookup order.
                if self.projects.contains_key(alias) {
                    anyhow::bail!(
                        "project alias {alias:?} (of {name:?}) is also a [projects] name"
                    );
                }
                if let Some(prev) = owner.insert(alias, name) {
                    anyhow::bail!(
                        "project alias {alias:?} is claimed by both {prev:?} and {name:?}"
                    );
                }
            }
        }
        Ok(())
    }

    /// Alias → canonical project name, for every configured alias.
    #[must_use]
    pub fn project_aliases(&self) -> BTreeMap<String, String> {
        self.projects
            .iter()
            .flat_map(|(name, p)| p.aliases.iter().map(move |a| (a.clone(), name.clone())))
            .filter(|(alias, name)| alias != name)
            .collect()
    }

    /// The config file path: `KVAULT_CONFIG`, else `$XDG_CONFIG_HOME/kvault/config.toml`,
    /// else `~/.config/kvault/config.toml`. XDG layout is used on every platform
    /// because that is where CLI developer tools are expected to live.
    #[must_use]
    pub fn config_path() -> Option<PathBuf> {
        if let Ok(p) = env::var(KVAULT_CONFIG_ENV) {
            return Some(PathBuf::from(p));
        }
        xdg_dir("XDG_CONFIG_HOME", ".config").map(|d| d.join("kvault").join("config.toml"))
    }

    /// The index database path: `KVAULT_DB`, else `$XDG_DATA_HOME/kvault/index.db`,
    /// else `~/.local/share/kvault/index.db`.
    #[must_use]
    pub fn db_path() -> Option<PathBuf> {
        if let Ok(p) = env::var(KVAULT_DB_ENV) {
            return Some(PathBuf::from(p));
        }
        xdg_dir("XDG_DATA_HOME", ".local/share").map(|d| d.join("kvault").join("index.db"))
    }

    /// Where hooks append failures: `KVAULT_LOG`, else
    /// `$XDG_STATE_HOME/kvault/hooks.log`, else `~/.local/state/kvault/hooks.log`.
    #[must_use]
    pub fn hook_log_path() -> Option<PathBuf> {
        if let Ok(p) = env::var(KVAULT_LOG_ENV) {
            return Some(PathBuf::from(p));
        }
        xdg_dir("XDG_STATE_HOME", ".local/state").map(|d| d.join("kvault").join("hooks.log"))
    }

    /// The roots `kvault init` writes when none are configured.
    #[must_use]
    pub fn suggested_roots() -> Vec<RootConfig> {
        vec![
            RootConfig {
                weight: 1.2,
                ..RootConfig::new(
                    "~/.claude/projects/*/memory",
                    RootKind::Memory,
                    ProjectFrom::ClaudeSlug,
                )
            },
            RootConfig::new("~/Projects/*/notes", RootKind::Notes, ProjectFrom::Parent),
            RootConfig {
                weight: 0.8,
                ..RootConfig::new("~/Projects/notes", RootKind::Reference, ProjectFrom::None)
            },
        ]
    }

    /// Serialize to `TOML`.
    ///
    /// # Errors
    /// Returns an error if serialization fails (should not happen for valid config).
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| Error::Other(e.into()))
    }
}

fn xdg_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    if let Ok(p) = env::var(var)
        && !p.is_empty()
    {
        return Some(PathBuf::from(p));
    }
    BaseDirs::new().map(|b| b.home_dir().join(fallback))
}

/// Expand a leading `~/` to the home directory. Other paths are returned as-is.
#[must_use]
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(base) = BaseDirs::new()
    {
        return base.home_dir().join(rest);
    }
    PathBuf::from(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_empty_roots() {
        let c = Config::default();
        assert!(c.roots.is_empty());
        assert_eq!(c.brief.budget, 2000);
        assert!(c.capture.enabled);
    }

    #[test]
    fn parses_design_doc_example() {
        let text = r#"
[[roots]]
path = "~/.claude/projects/*/memory"
kind = "memory"
weight = 1.2
project_from = "claude_slug"

[[roots]]
path = "~/Projects/*/notes"
kind = "notes"
project_from = "parent"

[capture]
enabled = true
session_dir = "notes/sessions"

[brief]
budget = 1500

[context]
"~/Projects/work" = "Work platform"
"#;
        let c: Config = toml::from_str(text).unwrap();
        assert_eq!(c.roots.len(), 2);
        assert_eq!(c.roots[0].kind, RootKind::Memory);
        assert!((c.roots[0].weight - 1.2).abs() < f64::EPSILON);
        assert_eq!(c.roots[1].project_from, ProjectFrom::Parent);
        assert!((c.roots[1].weight - 1.0).abs() < f64::EPSILON);
        assert_eq!(c.brief.budget, 1500);
        assert_eq!(c.brief.recent_days, 14);
        assert_eq!(c.context.get("~/Projects/work").unwrap(), "Work platform");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = toml::from_str::<Config>("[brief]\nbudgett = 3\n").unwrap_err();
        assert!(err.to_string().contains("budgett"));
    }

    #[test]
    fn demote_defaults_to_archive_dirs() {
        let c: Config = toml::from_str("[[roots]]\npath = \"/x\"\n").unwrap();
        assert_eq!(c.roots[0].demote, vec!["**/archive/**".to_owned()]);
        assert!((c.roots[0].demote_factor - DEFAULT_DEMOTE_FACTOR).abs() < f64::EPSILON);
        let c: Config =
            toml::from_str("[[roots]]\npath = \"/x\"\ndemote = []\ndemote_factor = 0.95\n")
                .unwrap();
        assert!(c.roots[0].demote.is_empty());
        c.validate().unwrap();
    }

    #[test]
    fn invalid_demote_settings_are_rejected() {
        let bad_factor: Config =
            toml::from_str("[[roots]]\npath = \"/x\"\ndemote_factor = 1.5\n").unwrap();
        assert!(
            bad_factor
                .validate()
                .unwrap_err()
                .to_string()
                .contains("demote_factor")
        );
        let bad_glob: Config =
            toml::from_str("[[roots]]\npath = \"/x\"\ndemote = [\"a/[\"]\n").unwrap();
        assert!(bad_glob.validate().is_err());
    }

    #[test]
    fn project_aliases_map_to_canonical_names() {
        let c: Config =
            toml::from_str("[projects.mas]\naliases = [\"work-mas\", \"mas\"]\n[projects.other]\n")
                .unwrap();
        c.validate().unwrap();
        let aliases = c.project_aliases();
        assert_eq!(aliases.len(), 1, "the canonical name is not its own alias");
        assert_eq!(aliases.get("work-mas").map(String::as_str), Some("mas"));
    }

    #[test]
    fn ambiguous_project_aliases_are_rejected() {
        let twice: Config =
            toml::from_str("[projects.a]\naliases = [\"x\"]\n[projects.b]\naliases = [\"x\"]\n")
                .unwrap();
        assert!(twice.validate().unwrap_err().to_string().contains("both"));
        let chained: Config =
            toml::from_str("[projects.a]\naliases = [\"b\"]\n[projects.b]\n").unwrap();
        assert!(chained.validate().is_err());
        assert!(toml::from_str::<Config>("[projects.a]\nalias = [\"b\"]\n").is_err());
    }

    #[test]
    fn suggested_roots_round_trip_through_toml() {
        let c = Config {
            roots: Config::suggested_roots(),
            ..Config::default()
        };
        let text = c.to_toml().unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn default_paths_are_xdg_style() {
        // Do not clear KVAULT_* here: other tests may run in parallel. Check the fallback shape only.
        let cfg = xdg_dir("KVAULT_TEST_UNSET_VAR", ".config").unwrap();
        assert!(cfg.ends_with(".config"));
        let data = xdg_dir("KVAULT_TEST_UNSET_VAR", ".local/share").unwrap();
        assert!(data.ends_with(".local/share"));
    }

    #[test]
    fn expand_tilde_cases() {
        assert!(!expand_tilde("~/x").to_string_lossy().starts_with('~'));
        assert_eq!(expand_tilde("/abs"), PathBuf::from("/abs"));
        assert_eq!(expand_tilde("~"), PathBuf::from("~"));
    }
}
