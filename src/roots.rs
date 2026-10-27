//! Roots: the directories kvault indexes in place.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{ProjectFrom, RootConfig, expand_tilde};

/// What kind of knowledge a root holds. Drives ranking authority and defaults.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    /// Claude Code auto-memory dirs: one fact per file, frontmatter, wikilinks.
    Memory,
    /// Project `notes/` dirs: long heading-structured research and plans.
    #[default]
    Notes,
    /// Reference material, ranked slightly below the others.
    Reference,
    Other,
}

impl RootKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Notes => "notes",
            Self::Reference => "reference",
            Self::Other => "other",
        }
    }
}

/// A concrete root directory after glob expansion.
#[derive(Debug, Clone, PartialEq)]
pub struct Root {
    pub path: PathBuf,
    pub kind: RootKind,
    pub weight: f64,
    pub project: Option<String>,
}

/// Expand every configured root pattern into existing directories.
///
/// Patterns may start with `~/` and may contain glob metacharacters. Missing
/// directories are skipped silently: a temporarily absent root is never an
/// error and never a mass delete.
#[must_use]
pub fn expand(configs: &[RootConfig]) -> Vec<Root> {
    let mut out = Vec::new();
    for cfg in configs {
        let pattern = expand_tilde(&cfg.path);
        for dir in glob_dirs(&pattern) {
            let project = derive_project(&dir, cfg.project_from);
            out.push(Root {
                path: dir,
                kind: cfg.kind,
                weight: cfg.weight,
                project,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

fn glob_dirs(pattern: &Path) -> Vec<PathBuf> {
    let Some(s) = pattern.to_str() else {
        return Vec::new();
    };
    let has_meta = s.contains(['*', '?', '[']);
    if !has_meta {
        return if pattern.is_dir() {
            vec![pattern.to_path_buf()]
        } else {
            Vec::new()
        };
    }
    glob::glob(s)
        .map(|paths| {
            paths
                .filter_map(Result::ok)
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

/// Derive a project name for a root directory.
#[must_use]
pub fn derive_project(dir: &Path, from: ProjectFrom) -> Option<String> {
    match from {
        ProjectFrom::None => None,
        ProjectFrom::Parent => dir
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned()),
        ProjectFrom::ClaudeSlug => dir
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .and_then(|slug| claude_slug_project(&slug)),
    }
}

/// `-Users-abrooks-Projects-work-mas` → `work-mas`.
///
/// Claude Code's slug is the absolute path with `/` replaced by `-`, so the
/// project name is whatever follows the last path segment we can recognise
/// as a projects container. Without that knowledge we take the tail after
/// `-Projects-` when present, else the whole slug minus its leading dash.
#[must_use]
pub fn claude_slug_project(slug: &str) -> Option<String> {
    let s = slug.strip_prefix('-').unwrap_or(slug);
    let tail = s.rsplit_once("-Projects-").map_or(s, |(_, tail)| tail);
    if tail.is_empty() {
        None
    } else {
        Some(tail.to_owned())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::config::RootConfig;

    #[test]
    fn claude_slug_extracts_project_tail() {
        assert_eq!(
            claude_slug_project("-Users-abrooks-Projects-work-mas").as_deref(),
            Some("work-mas")
        );
        assert_eq!(
            claude_slug_project("-Users-x-foo").as_deref(),
            Some("Users-x-foo")
        );
        assert_eq!(claude_slug_project("-"), None);
    }

    #[test]
    fn expand_globs_existing_dirs_only() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        std::fs::create_dir_all(base.join("a/notes")).unwrap();
        std::fs::create_dir_all(base.join("b/notes")).unwrap();
        std::fs::write(base.join("c"), "file, not dir").unwrap();
        let cfg = RootConfig {
            path: format!("{}/*/notes", base.display()),
            kind: RootKind::Notes,
            weight: 1.0,
            project_from: ProjectFrom::Parent,
        };
        let roots = expand(&[cfg]);
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].project.as_deref(), Some("a"));
        assert_eq!(roots[1].project.as_deref(), Some("b"));
    }

    #[test]
    fn expand_skips_missing_plain_path() {
        let cfg = RootConfig {
            path: "/definitely/not/here".to_owned(),
            kind: RootKind::Other,
            weight: 1.0,
            project_from: ProjectFrom::None,
        };
        assert!(expand(&[cfg]).is_empty());
    }
}
