//! Upgrade notices: a Claude Code plugin newer than the binary, and a newer
//! kvault release on crates.io.
//!
//! Both run at `SessionStart` only, and neither makes the hook wait on the
//! network. The plugin check reads one small file. The release check reads a
//! cached answer; when that answer is a day old the hook marks it checked,
//! starts a detached `kvault update-check --quiet` and returns. That child is
//! the only code that touches the network: one `curl` of the crate's static
//! sparse-index file, with a short timeout and nothing about the user in it.
//! Marking the cache before the spawn holds an offline machine to one
//! attempt a day.

use std::cmp::Ordering;
use std::fmt;
use std::path::Path;
use std::process::{Command, Stdio};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};

/// Set (to anything but `0`) to turn the release check off.
pub const NO_UPDATE_CHECK_ENV: &str = "KVAULT_NO_UPDATE_CHECK";

/// Overrides where the release index is fetched from.
pub const UPDATE_URL_ENV: &str = "KVAULT_UPDATE_URL";

/// The crates.io sparse-index file for `kvault`: one JSON line per
/// published version.
pub const INDEX_URL: &str = "https://index.crates.io/kv/au/kvault";

/// How old a cached answer may get before the hook asks again.
const MAX_AGE_SECS: i64 = 24 * 60 * 60;

/// Seconds `curl` may spend on the whole fetch.
const FETCH_TIMEOUT_SECS: &str = "3";

/// The command users run to upgrade.
pub const UPGRADE_COMMAND: &str = "cargo install kvault";

/// The running binary's version.
#[must_use]
pub const fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// A semantic version, enough of one to order releases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Option<String>,
}

impl Version {
    /// Parse `1.2.3`, `1.2.3-beta.2`; build metadata (`+...`) is ignored.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s.split_once('+').map_or(s, |(v, _)| v);
        let (core, pre) = match s.split_once('-') {
            Some((core, pre)) if !pre.is_empty() => (core, Some(pre.to_owned())),
            Some(_) => return None,
            None => (s, None),
        };
        let mut parts = core.split('.').map(str::parse::<u64>);
        let (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    #[must_use]
    pub const fn is_prerelease(&self) -> bool {
        self.pre.is_some()
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                // A release ranks above its own pre-releases.
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => cmp_prerelease(a, b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Semver pre-release order: dot-separated fields, numeric ones compared as
/// numbers and below alphanumeric ones, a shorter list first on a tie.
fn cmp_prerelease(left: &str, right: &str) -> Ordering {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(l), Some(r)) => {
                let field = match (l.parse::<u64>(), r.parse::<u64>()) {
                    (Ok(ln), Ok(rn)) => ln.cmp(&rn),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => l.cmp(r),
                };
                if field != Ordering::Equal {
                    return field;
                }
            }
        }
    }
}

/// The notice for a plugin newer than this binary, read from the plugin
/// directory Claude Code names in `CLAUDE_PLUGIN_ROOT`. `None` when the
/// versions agree, the binary is newer, or the manifest cannot be read.
#[must_use]
pub fn plugin_notice(plugin_root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(plugin_root.join(".claude-plugin/plugin.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&text).ok()?;
    let plugin = Version::parse(manifest.get("version")?.as_str()?)?;
    let binary = Version::parse(current())?;
    (plugin > binary).then(|| {
        format!(
            "kvault: the plugin is {plugin} but the kvault binary is {binary}, so its hooks \
             may fail. Upgrade the binary: {UPGRADE_COMMAND}"
        )
    })
}

/// Whether the release check is on: `[update_check] enabled` and no
/// `KVAULT_NO_UPDATE_CHECK`.
#[must_use]
pub fn enabled(config: &Config) -> bool {
    let env_off = std::env::var(NO_UPDATE_CHECK_ENV).is_ok_and(|v| !v.is_empty() && v != "0");
    config.update_check.enabled && !env_off
}

/// What is known about the latest release, kept between sessions.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Cache {
    /// When crates.io was last asked (or an attempt was started).
    pub checked_at: Option<String>,
    /// The newest release found.
    pub latest: Option<String>,
    /// The release the user was last told about, so each is told once.
    pub notified: Option<String>,
}

impl Cache {
    /// Read the cache; a missing or unreadable file is an empty one.
    #[must_use]
    pub fn read(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Write through a temporary file, so a reader never sees half a file.
    ///
    /// # Errors
    /// Returns an error if the directory or file cannot be written.
    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        let text = serde_json::to_string(self)
            .map_err(|e| Error::Other(anyhow::anyhow!("update cache: {e}")))?;
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// The latest release, when it is newer than this binary.
    #[must_use]
    pub fn newer_release(&self) -> Option<Version> {
        let latest = Version::parse(self.latest.as_deref()?)?;
        (latest > Version::parse(current())?).then_some(latest)
    }

    fn is_due(&self, now: Timestamp) -> bool {
        self.checked_at
            .as_deref()
            .and_then(|t| t.parse::<Timestamp>().ok())
            // A clock set back past the stamp counts as stale too.
            .is_none_or(|t| (now.as_second() - t.as_second()).abs() >= MAX_AGE_SECS)
    }
}

/// Where the cache lives: beside the hook log.
#[must_use]
pub fn cache_path() -> Option<std::path::PathBuf> {
    Config::hook_log_path().map(|p| p.with_file_name("latest-release.json"))
}

/// The outcome of the hook's look at the cache.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SessionCheck {
    /// A release notice to show this session.
    pub notice: Option<String>,
    /// Whether to start a background refresh.
    pub refresh: bool,
}

/// The `SessionStart` side of the release check: no network. Shows a newer
/// release once (when `show`), and asks for a refresh when the cached answer
/// is a day old. The refresh is asked for only if the cache could be marked,
/// so a cache that cannot be written never means a spawn every session.
#[must_use]
pub fn session_check(path: &Path, now: Timestamp, show: bool) -> SessionCheck {
    let mut cache = Cache::read(path);
    let mut changed = false;
    let mut notice = None;
    if show
        && let Some(latest) = cache.newer_release()
        && cache.notified.as_deref() != Some(latest.to_string().as_str())
    {
        notice = Some(release_notice(&latest));
        cache.notified = Some(latest.to_string());
        changed = true;
    }
    let due = cache.is_due(now);
    if due {
        cache.checked_at = Some(now.to_string());
        changed = true;
    }
    let written = !changed || cache.write(path).is_ok();
    SessionCheck {
        notice,
        refresh: due && written,
    }
}

fn release_notice(latest: &Version) -> String {
    format!(
        "kvault {latest} is available (you have {}). Upgrade: {UPGRADE_COMMAND}. \
         Turn this notice off with {NO_UPDATE_CHECK_ENV}=1 or [update_check] enabled = false.",
        current()
    )
}

/// Start `kvault update-check --quiet` detached and return at once. Errors
/// are ignored: a missed check only means no notice.
pub fn spawn_refresh() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.args(["update-check", "--quiet"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Its own process group, so ending the hook's group does not end it.
        cmd.process_group(0);
    }
    let _ = cmd.spawn();
}

/// Fetch the release index now and record the newest release in the cache.
///
/// # Errors
/// Returns an error if `curl` cannot run or fails, or the cache cannot be
/// written.
pub fn refresh(path: &Path) -> Result<Option<Version>> {
    let url = std::env::var(UPDATE_URL_ENV).unwrap_or_else(|_| INDEX_URL.to_owned());
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", FETCH_TIMEOUT_SECS])
        .args(["-A", "kvault-update-check", &url])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| Error::Other(anyhow::anyhow!("update check: cannot run curl: {e}")))?;
    if !out.status.success() {
        return Err(Error::Other(anyhow::anyhow!(
            "update check: fetching {url} failed ({})",
            out.status
        )));
    }
    let latest = latest_from_index(&String::from_utf8_lossy(&out.stdout));
    let mut cache = Cache::read(path);
    cache.checked_at = Some(Timestamp::now().to_string());
    if let Some(v) = &latest {
        cache.latest = Some(v.to_string());
    }
    cache.write(path)?;
    Ok(latest)
}

/// The newest release in a sparse-index file, ignoring yanked versions and
/// pre-releases.
#[must_use]
pub fn latest_from_index(text: &str) -> Option<Version> {
    #[derive(Deserialize)]
    struct Line {
        vers: String,
        #[serde(default)]
        yanked: bool,
    }
    text.lines()
        .filter_map(|l| serde_json::from_str::<Line>(l).ok())
        .filter(|l| !l.yanked)
        .filter_map(|l| Version::parse(&l.vers))
        .filter(|v| !v.is_prerelease())
        .max()
}

/// One line for `kvault status`, from the cache only.
#[must_use]
pub fn status_line(config: &Config) -> String {
    let now = current();
    if !enabled(config) {
        return format!("{now} (release check off)");
    }
    let Some(path) = cache_path() else {
        return now.to_owned();
    };
    let cache = Cache::read(&path);
    match (cache.newer_release(), cache.checked_at.as_deref()) {
        (Some(latest), _) => format!("{now} ({latest} is available: {UPGRADE_COMMAND})"),
        (None, Some(at)) if cache.latest.is_some() => {
            format!(
                "{now} (latest release, checked {})",
                at.get(..10).unwrap_or(at)
            )
        }
        _ => format!("{now} (not checked yet; `kvault update-check` asks now)"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn versions_order_like_semver() {
        assert!(v("0.2.0-beta.2") < v("0.2.0-beta.10"));
        assert!(v("0.2.0-beta.10") < v("0.2.0"));
        assert!(v("0.2.0-alpha") < v("0.2.0-beta"));
        assert!(v("0.2.0-beta") < v("0.2.0-beta.1"));
        assert!(v("0.2.9") < v("0.10.0"));
        assert_eq!(v("1.2.3+build.5"), v("1.2.3"));
        assert_eq!(v("0.2.0-beta.2").to_string(), "0.2.0-beta.2");
        for bad in ["", "1.2", "1.2.3.4", "1.x.3", "1.2.3-"] {
            assert!(Version::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn index_latest_skips_yanked_and_prereleases() {
        let text = [
            r#"{"name":"kvault","vers":"0.2.0","yanked":false}"#,
            r#"{"name":"kvault","vers":"0.4.0","yanked":true}"#,
            r#"{"name":"kvault","vers":"0.3.0","yanked":false}"#,
            r#"{"name":"kvault","vers":"0.5.0-beta.1","yanked":false}"#,
            "not json",
        ]
        .join("\n");
        assert_eq!(latest_from_index(&text), Some(v("0.3.0")));
        assert_eq!(latest_from_index(""), None);
    }

    fn cache_at(dir: &Path, cache: &Cache) -> std::path::PathBuf {
        let path = dir.join("latest-release.json");
        cache.write(&path).unwrap();
        path
    }

    #[test]
    fn a_missing_cache_is_marked_and_refreshed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/latest-release.json");
        let now = Timestamp::now();
        let check = session_check(&path, now, true);
        assert_eq!(
            check,
            SessionCheck {
                notice: None,
                refresh: true
            }
        );
        assert_eq!(Cache::read(&path).checked_at, Some(now.to_string()));
        // Marked, so the next session within the day does not spawn again.
        assert!(!session_check(&path, now, true).refresh);
    }

    #[test]
    fn a_day_old_answer_is_refreshed() {
        let dir = tempfile::tempdir().unwrap();
        let now = Timestamp::now();
        let old = now - jiff::SignedDuration::from_hours(25);
        let path = cache_at(
            dir.path(),
            &Cache {
                checked_at: Some(old.to_string()),
                ..Cache::default()
            },
        );
        assert!(session_check(&path, now, true).refresh);
    }

    #[test]
    fn a_newer_release_is_told_once() {
        let dir = tempfile::tempdir().unwrap();
        let now = Timestamp::now();
        let path = cache_at(
            dir.path(),
            &Cache {
                checked_at: Some(now.to_string()),
                latest: Some("99.0.0".to_owned()),
                notified: None,
            },
        );
        let first = session_check(&path, now, true);
        assert!(first.notice.unwrap().contains("kvault 99.0.0 is available"));
        assert!(!first.refresh);
        assert_eq!(session_check(&path, now, true).notice, None);
    }

    #[test]
    fn a_withheld_notice_is_still_owed() {
        let dir = tempfile::tempdir().unwrap();
        let now = Timestamp::now();
        let path = cache_at(
            dir.path(),
            &Cache {
                checked_at: Some(now.to_string()),
                latest: Some("99.0.0".to_owned()),
                notified: None,
            },
        );
        assert_eq!(session_check(&path, now, false).notice, None);
        assert!(session_check(&path, now, true).notice.is_some());
    }

    #[test]
    fn the_current_or_an_older_release_is_not_news() {
        let dir = tempfile::tempdir().unwrap();
        let now = Timestamp::now();
        for latest in [current(), "0.0.1"] {
            let path = cache_at(
                dir.path(),
                &Cache {
                    checked_at: Some(now.to_string()),
                    latest: Some(latest.to_owned()),
                    notified: None,
                },
            );
            assert_eq!(
                session_check(&path, now, true),
                SessionCheck::default(),
                "{latest}"
            );
        }
    }

    #[test]
    fn an_unwritable_cache_never_spawns() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should be: reads fail, writes fail.
        let path = dir.path().join("latest-release.json");
        std::fs::create_dir(&path).unwrap();
        assert!(!session_check(&path, Timestamp::now(), true).refresh);
    }

    #[test]
    fn plugin_notice_only_when_the_plugin_is_newer() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join(".claude-plugin/plugin.json");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        assert_eq!(plugin_notice(dir.path()), None, "no manifest");
        for (version, expect) in [("99.0.0", true), (current(), false), ("0.0.1", false)] {
            std::fs::write(
                &manifest,
                format!(r#"{{"name":"kvault","version":"{version}"}}"#),
            )
            .unwrap();
            assert_eq!(plugin_notice(dir.path()).is_some(), expect, "{version}");
        }
    }
}
