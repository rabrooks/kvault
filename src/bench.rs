//! `kvault bench`: retrieval quality and latency over a query file.
//!
//! The file is JSON lines: `{"query": "...", "expected": ["path#heading", ...],
//! "note": "..."}`. A hit matches an expected entry when its path (as
//! printed by `search`, relative to its root, or absolute) equals or ends
//! with the path part, and, if a `#heading` is given, its heading path
//! contains the heading. Document-level metrics ignore headings; `sec@5`
//! counts only cases that name a heading.
//!
//! The baseline is what an agent would do without kvault: case-insensitive
//! grep over every indexed file, ranking files by how many query words they
//! contain and then by total occurrences.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::search::{self, Query, SearchOptions, display_path};
use crate::store::Store;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Case {
    pub query: String,
    pub expected: Vec<String>,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BenchOptions {
    /// Results considered per query.
    pub limit: usize,
    /// Also run the grep baseline.
    pub baseline: bool,
}

impl Default for BenchOptions {
    fn default() -> Self {
        Self {
            limit: 10,
            baseline: false,
        }
    }
}

/// One query's outcome under one system.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseResult {
    pub query: String,
    /// 1-based rank of the first hit matching an expected path.
    pub doc_rank: Option<usize>,
    /// 1-based rank of the first hit matching path and heading; `None` when
    /// the case names no heading.
    pub sec_rank: Option<Option<usize>>,
    /// Top three paths returned, for diagnosing misses.
    pub top: Vec<String>,
    pub millis: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Metrics {
    pub n: usize,
    pub hit1: f64,
    pub hit3: f64,
    pub hit5: f64,
    /// Section-level hit@5 over cases with a heading, with that count.
    pub sec5: Option<(f64, usize)>,
    pub mrr: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct System {
    pub name: &'static str,
    pub metrics: Metrics,
    pub cases: Vec<CaseResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub kvault: System,
    pub baseline: Option<System>,
}

/// Parse a JSON-lines query file. Blank lines and `#` comments are skipped.
///
/// # Errors
/// Returns an error if the file cannot be read or a line is not a case.
pub fn load(path: &Path) -> Result<Vec<Case>> {
    let text = std::fs::read_to_string(path)?;
    let mut cases = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let case: Case = serde_json::from_str(line)
            .map_err(|e| Error::Other(anyhow::anyhow!("{}:{}: {e}", path.display(), i + 1)))?;
        if case.expected.is_empty() {
            return Err(Error::Other(anyhow::anyhow!(
                "{}:{}: expected list is empty",
                path.display(),
                i + 1
            )));
        }
        cases.push(case);
    }
    if cases.is_empty() {
        return Err(Error::Other(anyhow::anyhow!(
            "{}: no cases",
            path.display()
        )));
    }
    Ok(cases)
}

/// An `expected` entry split into its parts.
struct Expected<'a> {
    path: &'a str,
    heading: Option<String>,
}

impl<'a> Expected<'a> {
    fn parse(s: &'a str) -> Self {
        match s.split_once('#') {
            Some((p, h)) if !h.is_empty() => Self {
                path: p.trim_end_matches('/'),
                heading: Some(h.to_lowercase()),
            },
            _ => Self {
                path: s.trim_end_matches('/'),
                heading: None,
            },
        }
    }

    fn path_matches(&self, candidates: &[String]) -> bool {
        let want = self.path.to_lowercase();
        let suffix = format!("/{want}");
        candidates.iter().any(|c| {
            let c = c.to_lowercase();
            c == want || c.ends_with(&suffix)
        })
    }
}

/// Paths a hit can be referred to by.
fn hit_paths(root_path: &str, project: Option<&str>, rel_path: &str) -> Vec<String> {
    vec![
        display_path(root_path, project, rel_path),
        rel_path.to_owned(),
        Path::new(root_path)
            .join(rel_path)
            .to_string_lossy()
            .into_owned(),
    ]
}

/// A returned result in the shape both systems produce.
struct Returned {
    paths: Vec<String>,
    heading_path: String,
}

fn score_case(query: &str, expected: &[String], returned: &[Returned], millis: f64) -> CaseResult {
    let expected: Vec<Expected<'_>> = expected.iter().map(|s| Expected::parse(s)).collect();
    let doc_rank = returned
        .iter()
        .position(|r| expected.iter().any(|e| e.path_matches(&r.paths)))
        .map(|i| i + 1);
    let has_heading = expected.iter().any(|e| e.heading.is_some());
    let sec_rank = has_heading.then(|| {
        returned
            .iter()
            .position(|r| {
                expected.iter().any(|e| {
                    e.path_matches(&r.paths)
                        && e.heading
                            .as_ref()
                            .is_none_or(|h| r.heading_path.to_lowercase().contains(h.as_str()))
                })
            })
            .map(|i| i + 1)
    });
    CaseResult {
        query: query.to_owned(),
        doc_rank,
        sec_rank,
        top: returned
            .iter()
            .take(3)
            .filter_map(|r| r.paths.first().cloned())
            .collect(),
        millis,
    }
}

#[allow(clippy::cast_precision_loss)]
fn metrics(cases: &[CaseResult]) -> Metrics {
    let n = cases.len().max(1) as f64;
    let hit_at = |k: usize| {
        cases
            .iter()
            .filter(|c| c.doc_rank.is_some_and(|r| r <= k))
            .count() as f64
            / n
    };
    let mrr = cases
        .iter()
        .filter_map(|c| c.doc_rank)
        .map(|r| 1.0 / r as f64)
        .sum::<f64>()
        / n;
    let with_heading: Vec<&CaseResult> = cases.iter().filter(|c| c.sec_rank.is_some()).collect();
    let sec5 = (!with_heading.is_empty()).then(|| {
        let hits = with_heading
            .iter()
            .filter(|c| c.sec_rank.flatten().is_some_and(|r| r <= 5))
            .count();
        (hits as f64 / with_heading.len() as f64, with_heading.len())
    });
    let mut times: Vec<f64> = cases.iter().map(|c| c.millis).collect();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f64| -> f64 {
        if times.is_empty() {
            return 0.0;
        }
        // nearest-rank percentile; p is in [0, 1] so the cast is exact enough
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let idx = ((times.len() - 1) as f64 * p).round() as usize;
        times[idx.min(times.len() - 1)]
    };
    Metrics {
        n: cases.len(),
        hit1: hit_at(1),
        hit3: hit_at(3),
        hit5: hit_at(5),
        sec5,
        mrr,
        p50_ms: pct(0.5),
        p95_ms: pct(0.95),
    }
}

/// Run every case through `search` and, optionally, the grep baseline.
///
/// # Errors
/// Returns an error if a search fails.
pub fn run(store: &Store, cases: &[Case], opts: &BenchOptions) -> Result<Report> {
    let search_opts = SearchOptions {
        limit: opts.limit,
        ..SearchOptions::default()
    };
    let mut results = Vec::with_capacity(cases.len());
    for case in cases {
        let query = Query::parse(&case.query);
        let start = Instant::now();
        let res = search::run(store, &query, &search_opts)?;
        let millis = start.elapsed().as_secs_f64() * 1000.0;
        let returned: Vec<Returned> = res
            .hits
            .iter()
            .map(|h| Returned {
                paths: hit_paths(
                    &h.doc().root_path,
                    h.doc().project.as_deref(),
                    &h.doc().rel_path,
                ),
                heading_path: h.chunk.heading_path.clone(),
            })
            .collect();
        results.push(score_case(&case.query, &case.expected, &returned, millis));
    }
    let kvault = System {
        name: "kvault",
        metrics: metrics(&results),
        cases: results,
    };
    let baseline = if opts.baseline {
        Some(grep_baseline(store, cases, opts.limit)?)
    } else {
        None
    };
    Ok(Report { kvault, baseline })
}

struct GrepFile {
    abs_path: std::path::PathBuf,
    paths: Vec<String>,
}

fn grep_corpus(store: &Store) -> Result<Vec<GrepFile>> {
    let mut stmt = store.conn().prepare(
        "SELECT r.path, r.project, d.rel_path FROM docs d JOIN roots r ON r.id = d.root_id
         WHERE d.active = 1",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut files = Vec::new();
    for row in rows {
        let (root_path, project, rel_path) = row?;
        files.push(GrepFile {
            abs_path: Path::new(&root_path).join(&rel_path),
            paths: hit_paths(&root_path, project.as_deref(), &rel_path),
        });
    }
    Ok(files)
}

/// Rank files by distinct query words present, then total occurrences:
/// what `rg -i` plus a sort by match count gives. Files are read inside the
/// timed region, as `rg` would read them, so the latency is comparable to
/// kvault's, which includes its index reads.
fn grep_baseline(store: &Store, cases: &[Case], limit: usize) -> Result<System> {
    let corpus = grep_corpus(store)?;
    let mut results = Vec::with_capacity(cases.len());
    for case in cases {
        let words = Query::parse(&case.query).words();
        let start = Instant::now();
        let mut scored: Vec<(usize, usize, &GrepFile)> = corpus
            .iter()
            .filter_map(|f| {
                let lower = std::fs::read_to_string(&f.abs_path).ok()?.to_lowercase();
                let mut distinct = 0;
                let mut total = 0;
                for w in &words {
                    let n = lower.matches(w.as_str()).count();
                    if n > 0 {
                        distinct += 1;
                        total += n;
                    }
                }
                (distinct > 0).then_some((distinct, total, f))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(b.1.cmp(&a.1))
                .then(a.2.paths.cmp(&b.2.paths))
        });
        let millis = start.elapsed().as_secs_f64() * 1000.0;
        let returned: Vec<Returned> = scored
            .iter()
            .take(limit)
            .map(|(_, _, f)| Returned {
                paths: f.paths.clone(),
                heading_path: String::new(),
            })
            .collect();
        results.push(score_case(&case.query, &case.expected, &returned, millis));
    }
    Ok(System {
        name: "grep",
        metrics: metrics(&results),
        cases: results,
    })
}

impl Report {
    /// The summary table, plus per-case misses when `verbose`.
    #[must_use]
    pub fn render(&self, verbose: bool) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{:<8} {:>4} {:>6} {:>6} {:>6} {:>12} {:>6} {:>8} {:>8}",
            "system", "n", "hit@1", "hit@3", "hit@5", "sec@5", "MRR", "p50", "p95"
        );
        for sys in std::iter::once(&self.kvault).chain(self.baseline.as_ref()) {
            let m = &sys.metrics;
            let sec5 = m
                .sec5
                .map_or("-".to_owned(), |(v, n)| format!("{v:.2} (n={n})"));
            let _ = writeln!(
                out,
                "{:<8} {:>4} {:>6.2} {:>6.2} {:>6.2} {:>12} {:>6.2} {:>7.1}ms {:>7.1}ms",
                sys.name, m.n, m.hit1, m.hit3, m.hit5, sec5, m.mrr, m.p50_ms, m.p95_ms
            );
        }
        if verbose {
            for sys in std::iter::once(&self.kvault).chain(self.baseline.as_ref()) {
                for c in &sys.cases {
                    let doc_ok = c.doc_rank.is_some_and(|r| r <= 5);
                    let sec_ok = c.sec_rank.is_none_or(|s| s.is_some_and(|r| r <= 5));
                    if doc_ok && sec_ok {
                        continue;
                    }
                    let what = if doc_ok { "SEC-MISS" } else { "MISS" };
                    let _ = writeln!(
                        out,
                        "{what:<9} {:<7} {:?}  rank {}  top: {}",
                        sys.name,
                        c.query,
                        c.doc_rank.map_or("-".to_owned(), |r| r.to_string()),
                        c.top.join(", ")
                    );
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ret(path: &str, heading: &str) -> Returned {
        Returned {
            paths: vec![path.to_owned()],
            heading_path: heading.to_owned(),
        }
    }

    #[test]
    fn expected_matches_suffixes_case_insensitively() {
        let e = Expected::parse("Notes/Plan.md#phase one");
        assert!(e.path_matches(&["proj/notes/plan.md".into()]));
        assert!(!e.path_matches(&["proj/notes/myplan.md".into()]));
        assert_eq!(e.heading.as_deref(), Some("phase one"));
        assert!(Expected::parse("plan.md#").heading.is_none());
    }

    #[test]
    fn ranks_and_metrics() {
        let returned = vec![ret("a/x.md", "Intro"), ret("a/plan.md", "Plan / Phase two")];
        let c = score_case("q", &["plan.md#phase two".into()], &returned, 1.0);
        assert_eq!(c.doc_rank, Some(2));
        assert_eq!(c.sec_rank, Some(Some(2)));
        let miss = score_case("q", &["plan.md#phase nine".into()], &returned, 3.0);
        assert_eq!(miss.doc_rank, Some(2));
        assert_eq!(miss.sec_rank, Some(None));
        let none = score_case("q", &["zzz.md".into()], &returned, 2.0);
        assert_eq!(none.doc_rank, None);
        assert_eq!(none.sec_rank, None);

        let m = metrics(&[c, miss, none]);
        assert_eq!(m.n, 3);
        assert!((m.hit1 - 0.0).abs() < f64::EPSILON);
        assert!((m.hit3 - 2.0 / 3.0).abs() < 1e-9);
        assert!((m.mrr - (0.5 + 0.5) / 3.0).abs() < 1e-9);
        assert_eq!(m.sec5, Some((0.5, 2)));
        assert!((m.p50_ms - 2.0).abs() < f64::EPSILON);
        assert!((m.p95_ms - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn load_rejects_bad_lines() {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let p = tmp.path().join("q.jsonl");
        std::fs::write(
            &p,
            "# comment\n\n{\"query\":\"a\",\"expected\":[\"x.md\"]}\n",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(load(&p).map(|c| c.len()).ok(), Some(1));
        std::fs::write(&p, "{\"query\":\"a\",\"expected\":[]}\n").unwrap_or_else(|e| panic!("{e}"));
        assert!(load(&p).is_err());
        std::fs::write(&p, "not json\n").unwrap_or_else(|e| panic!("{e}"));
        assert!(load(&p).is_err());
    }
}
