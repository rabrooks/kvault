//! `kvault add`: write a frontmatter-bearing note into a root and index it.
//!
//! The target is `--root`, else the notes root (then the memory root) of
//! `--project` or of the project the working directory belongs to. The file
//! name is a slug of the title. An existing file is only replaced when
//! `--expected-hash` matches the BLAKE3 of its current bytes, so two agents
//! cannot silently overwrite each other. Notes in a Claude Code memory root
//! use that loader's `name`/`description`/`metadata.type` frontmatter and get
//! a pointer line in `MEMORY.md`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::brief;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::index;
use crate::markdown;
use crate::roots::{self, Root, RootKind};
use crate::store::Store;

/// Longest file stem produced from a title.
const SLUG_CHARS: usize = 80;
/// Longest memory `description` derived from the body.
const DESCRIPTION_CHARS: usize = 120;
/// Shortest `--expected-hash` prefix accepted.
const HASH_PREFIX_MIN: usize = 8;

#[derive(Debug, Clone)]
pub struct AddOptions {
    pub title: String,
    pub kind: Option<String>,
    pub project: Option<String>,
    pub tags: Vec<String>,
    pub root: Option<PathBuf>,
    pub expected_hash: Option<String>,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    pub path: PathBuf,
    /// BLAKE3 of the bytes written, for a later `--expected-hash`.
    pub hash: String,
    pub replaced: bool,
    /// False when the file is outside every configured root.
    pub indexed: bool,
}

/// Write the note and index its root.
///
/// # Errors
/// Returns an error if the body is empty, no target root can be determined,
/// the file exists and the expected hash is missing or wrong, or the write
/// or index fails.
pub fn run(config: &Config, store: &mut Store, opts: &AddOptions, body: &str) -> Result<Added> {
    let body = body.trim();
    if body.is_empty() {
        return Err(other(
            "nothing to add: pass --file or pipe the note body on stdin",
        ));
    }
    let slug = slug(&opts.title);
    if slug.is_empty() {
        return Err(other(
            "the title has no letters or digits to make a file name from",
        ));
    }
    let (dir, root) = target(config, store, opts)?;
    let path = dir.join(format!("{slug}.md"));

    let existing = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let mut created = None;
    if let Some(bytes) = &existing {
        let current = blake3::hash(bytes).to_hex().to_string();
        match opts.expected_hash.as_deref().map(str::to_ascii_lowercase) {
            Some(h) if h.len() >= HASH_PREFIX_MIN && current.starts_with(&h) => {}
            Some(_) => {
                return Err(other(format!(
                    "{} changed since it was read (blake3 is now {current}); re-read it and retry",
                    path.display()
                )));
            }
            None => {
                return Err(other(format!(
                    "{} exists; pass --expected-hash {current} to replace it",
                    path.display()
                )));
            }
        }
        let (fm, _) = markdown::split_frontmatter(&String::from_utf8_lossy(bytes));
        created = fm.created;
    }

    let today = jiff::Zoned::now().date().to_string();
    let created = created.unwrap_or_else(|| today.clone());
    let is_memory = root.as_ref().is_some_and(|r| r.kind == RootKind::Memory);
    let project = opts
        .project
        .clone()
        .or_else(|| root.as_ref().and_then(|r| r.project.clone()));
    let text = if is_memory {
        memory_note(opts, &slug, body)
    } else {
        note(opts, project.as_deref(), &created, &today, body)
    };

    std::fs::create_dir_all(&dir)?;
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, &text)?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::Io(e)
    })?;
    if is_memory {
        add_memory_pointer(&dir, &slug, &opts.title, &description(body))?;
    }
    let indexed = index::reindex_root_containing(config, store, &path)?;
    Ok(Added {
        path,
        hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
        replaced: existing.is_some(),
        indexed,
    })
}

fn other(msg: impl Into<String>) -> Error {
    Error::Other(anyhow::anyhow!(msg.into()))
}

/// Pick the directory to write into and the configured root it is under.
fn target(config: &Config, store: &Store, opts: &AddOptions) -> Result<(PathBuf, Option<Root>)> {
    let expanded = roots::expand(&config.roots);
    if let Some(dir) = &opts.root {
        if !dir.is_dir() {
            return Err(other(format!(
                "--root {} is not a directory",
                dir.display()
            )));
        }
        let canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone());
        let root = expanded.into_iter().find(|r| {
            let rp = std::fs::canonicalize(&r.path).unwrap_or_else(|_| r.path.clone());
            canon.starts_with(rp)
        });
        return Ok((dir.clone(), root));
    }
    let project = match &opts.project {
        Some(p) => Some(p.clone()),
        None => brief::project_for(store, &opts.cwd)?,
    };
    let Some(project) = project else {
        return Err(other(format!(
            "cannot tell which project {} belongs to; pass --project or --root",
            opts.cwd.display()
        )));
    };
    let mut candidates: Vec<Root> = expanded
        .into_iter()
        .filter(|r| r.project.as_deref() == Some(project.as_str()))
        .collect();
    candidates.sort_by_key(|r| match r.kind {
        RootKind::Notes => 0,
        RootKind::Memory => 1,
        RootKind::Other => 2,
        RootKind::Reference => 3,
    });
    let Some(root) = candidates.into_iter().next() else {
        return Err(other(format!(
            "no configured root belongs to project {project}; pass --root"
        )));
    };
    Ok((root.path.clone(), Some(root)))
}

/// Lower-case ASCII letters and digits, runs of anything else collapsed to
/// one `-`, trimmed and cut at [`SLUG_CHARS`].
#[must_use]
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else {
            dash = true;
        }
    }
    out.chars()
        .take(SLUG_CHARS)
        .collect::<String>()
        .trim_end_matches('-')
        .to_owned()
}

#[derive(Serialize)]
struct NoteFrontmatter<'a> {
    title: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<&'a str>,
    created: &'a str,
    updated: &'a str,
}

fn note(
    opts: &AddOptions,
    project: Option<&str>,
    created: &str,
    updated: &str,
    body: &str,
) -> String {
    let fm = NoteFrontmatter {
        title: &opts.title,
        kind: opts.kind.as_deref(),
        project,
        tags: opts.tags.iter().map(String::as_str).collect(),
        created,
        updated,
    };
    let mut out = frontmatter(&fm);
    let _ = write!(out, "# {}\n\n{body}\n", opts.title);
    out
}

#[derive(Serialize)]
struct MemoryFrontmatter<'a> {
    name: &'a str,
    description: String,
    title: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<&'a str>,
    metadata: MemoryMetadata<'a>,
}

#[derive(Serialize)]
struct MemoryMetadata<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
}

/// Claude Code's memory format: one fact per file, `name`, `description`,
/// `metadata.type`, no heading.
fn memory_note(opts: &AddOptions, slug: &str, body: &str) -> String {
    let fm = MemoryFrontmatter {
        name: slug,
        description: description(body),
        title: &opts.title,
        tags: opts.tags.iter().map(String::as_str).collect(),
        metadata: MemoryMetadata {
            kind: opts.kind.as_deref().unwrap_or("project"),
        },
    };
    let mut out = frontmatter(&fm);
    let _ = write!(out, "\n{body}\n");
    out
}

fn frontmatter<T: Serialize>(fm: &T) -> String {
    // Structs of strings and string lists serialize without error.
    format!(
        "---\n{}---\n",
        serde_norway::to_string(fm).unwrap_or_default()
    )
}

/// First non-empty body line, heading and list markers stripped.
fn description(body: &str) -> String {
    let line = body
        .lines()
        .map(|l| l.trim().trim_start_matches(['#', '-', '*', ' ']))
        .find(|l| !l.is_empty())
        .unwrap_or("");
    crate::pack::truncate(
        &line.split_whitespace().collect::<Vec<_>>().join(" "),
        DESCRIPTION_CHARS,
    )
}

/// Append `- [title](slug.md) — description` to the root's `MEMORY.md` unless
/// a line already points at that file.
fn add_memory_pointer(dir: &Path, slug: &str, title: &str, description: &str) -> Result<()> {
    let index = dir.join("MEMORY.md");
    let mut text = std::fs::read_to_string(&index).unwrap_or_default();
    if text.contains(&format!("]({slug}.md)")) {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    let _ = writeln!(text, "- [{title}]({slug}.md) — {description}");
    std::fs::write(index, text)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn opts(title: &str, kind: Option<&str>, tags: &[&str]) -> AddOptions {
        AddOptions {
            title: title.to_owned(),
            kind: kind.map(str::to_owned),
            project: None,
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            root: None,
            expected_hash: None,
            cwd: PathBuf::new(),
        }
    }

    #[test]
    fn slug_normalizes() {
        assert_eq!(
            slug("Lambda cold start: what we chose"),
            "lambda-cold-start-what-we-chose"
        );
        assert_eq!(slug("  --Hello__World!! "), "hello-world");
        assert_eq!(slug("日本語"), "");
        assert_eq!(slug(&"a".repeat(100)).len(), SLUG_CHARS);
    }

    #[test]
    fn description_takes_first_real_line() {
        assert_eq!(description("\n\n## Heading  here\nbody"), "Heading here");
        assert_eq!(description("- a list item"), "a list item");
        assert_eq!(description(""), "");
    }

    #[test]
    fn note_frontmatter_quotes_and_omits() {
        let out = note(
            &opts("Decision: sqlite", Some("decision"), &[]),
            Some("p"),
            "2026-10-01",
            "2026-10-28",
            "Body.",
        );
        assert_eq!(
            out,
            "---\ntitle: 'Decision: sqlite'\nkind: decision\nproject: p\ncreated: 2026-10-01\nupdated: 2026-10-28\n---\n# Decision: sqlite\n\nBody.\n"
        );
        let (fm, _) = markdown::split_frontmatter(&out);
        assert_eq!(fm.title.as_deref(), Some("Decision: sqlite"));
        assert_eq!(fm.created.as_deref(), Some("2026-10-01"));
    }

    #[test]
    fn memory_note_matches_claude_format() {
        let out = memory_note(
            &opts("Prefer sqlite", None, &["db"]),
            "prefer-sqlite",
            "We chose sqlite.\n\n**Why:** simpler.",
        );
        assert_eq!(
            out,
            "---\nname: prefer-sqlite\ndescription: We chose sqlite.\ntitle: Prefer sqlite\ntags:\n- db\nmetadata:\n  type: project\n---\n\nWe chose sqlite.\n\n**Why:** simpler.\n"
        );
    }

    #[test]
    fn memory_pointer_is_appended_once() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("MEMORY.md"), "- [Old](old.md) — x").unwrap();
        add_memory_pointer(tmp.path(), "new", "New", "desc").unwrap();
        add_memory_pointer(tmp.path(), "new", "New", "desc").unwrap();
        let text = std::fs::read_to_string(tmp.path().join("MEMORY.md")).unwrap();
        assert_eq!(text, "- [Old](old.md) — x\n- [New](new.md) — desc\n");
    }
}
