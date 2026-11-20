//! `kvault report`: what the query log says about a period of use.
//!
//! The default output is a summary of counts and rates with no query text,
//! file paths or project names in it, so it can be handed to someone else
//! as it is. `--full` prints the log lines themselves for the same period,
//! which do contain those; it is for the person whose log it is to read
//! before deciding to share.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use serde::Serialize;

use crate::error::Result;
use crate::querylog::{self, Event, How, Source};

/// Counts and rates over the events of a period.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Summary {
    /// First and last event, as recorded.
    pub from: Option<String>,
    pub to: Option<String>,
    /// kvault versions that wrote the lines.
    pub versions: Vec<String>,
    pub sessions: Sessions,
    pub queries: Queries,
    pub briefs: Briefs,
    /// Listed notes that were then used, by what listed them and how.
    pub uses: BTreeMap<String, usize>,
    /// Size of the index at the last brief of the period.
    pub corpus: Option<Corpus>,
    /// Lines in the hook log for the period: each is a hook that failed.
    pub hook_errors: usize,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Sessions {
    /// Sessions that were given a brief at start.
    pub briefed: usize,
    /// Sessions that completed at least one turn.
    pub completed: usize,
    /// Sessions that ran `search` or `recall` at least once.
    pub searched: usize,
    /// Sessions that used a note a query listed.
    pub used_a_result: usize,
    /// Sessions that used a note the brief listed.
    pub followed_the_brief: usize,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Queries {
    pub total: usize,
    pub recall: usize,
    pub search: usize,
    /// Run from a shell that was not a Claude Code session.
    pub outside_a_session: usize,
    pub no_hits: usize,
    /// `recall` replies that were a weak match.
    pub weak: usize,
    /// In-session queries one of whose results was then used.
    pub with_a_used_result: usize,
    /// In-session queries, the denominator for the line above.
    pub in_a_session: usize,
    /// Median tokens a `recall` printed.
    pub recall_tokens_p50: Option<usize>,
    pub ms_p50: Option<u64>,
    pub ms_p95: Option<u64>,
    /// `recall` queries by the share of the query the top hits contained:
    /// under 45%, 45 to 69%, 70% and over.
    pub coverage_bands: [usize; 3],
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Briefs {
    pub injected: usize,
    pub tokens_p50: Option<usize>,
    /// Median notes listed.
    pub items_p50: Option<usize>,
    /// Briefs that listed nothing.
    pub empty: usize,
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct Corpus {
    pub docs: i64,
    pub chunks: i64,
    pub roots: i64,
}

fn percentile<T: Copy + Ord>(values: &mut [T], p: usize) -> Option<T> {
    values.sort_unstable();
    let last = values.len().checked_sub(1)?;
    values.get((last * p + 50) / 100).copied()
}

/// Events at or after `since` (an RFC 3339 instant), all of them if `None`.
/// A line whose time cannot be read is kept.
#[must_use]
pub fn in_period(events: Vec<Event>, since: Option<&str>) -> Vec<Event> {
    let Some(cutoff) = since.and_then(|s| s.parse::<jiff::Timestamp>().ok()) else {
        return events;
    };
    events
        .into_iter()
        .filter(|e| {
            e.at().parse::<jiff::Timestamp>().is_ok_and(|t| t >= cutoff) || e.at().is_empty()
        })
        .collect()
}

/// Summarise events. `hook_errors` is counted by the caller, from the hook
/// log.
#[must_use]
pub fn summarize(events: &[Event], hook_errors: usize) -> Summary {
    let mut s = Summary {
        hook_errors,
        ..Summary::default()
    };
    let mut times: Vec<&str> = events.iter().map(Event::at).collect();
    times.sort_unstable();
    s.from = times.first().map(|t| (*t).to_owned());
    s.to = times.last().map(|t| (*t).to_owned());

    let mut versions = BTreeSet::new();
    let (mut briefed, mut completed, mut searched) =
        (HashSet::new(), HashSet::new(), HashSet::new());
    let (mut used, mut followed) = (HashSet::new(), HashSet::new());
    let mut used_queries: HashSet<(&str, &str)> = HashSet::new();
    let (mut recall_tokens, mut millis) = (Vec::new(), Vec::new());
    let (mut brief_tokens, mut brief_items) = (Vec::new(), Vec::new());
    for e in events {
        match e {
            Event::Use(u) => {
                *s.uses.entry(use_key(u)).or_default() += 1;
                match u.from {
                    Source::Query => {
                        used.insert(u.session.as_str());
                        used_queries.insert((u.session.as_str(), u.query_at.as_str()));
                    }
                    Source::Brief => {
                        followed.insert(u.session.as_str());
                    }
                }
            }
            Event::Brief(b) => {
                versions.extend(b.version.clone());
                briefed.insert(b.session.as_str());
                s.briefs.injected += 1;
                s.briefs.empty += usize::from(b.hits.is_empty());
                brief_tokens.push(b.tokens);
                brief_items.push(b.hits.len());
                s.corpus = Some(Corpus {
                    docs: b.docs,
                    chunks: b.chunks,
                    roots: b.roots,
                });
            }
            Event::Session(x) => {
                versions.extend(x.version.clone());
                completed.insert(x.session.as_str());
            }
            Event::Query(q) => {
                versions.extend(q.version.clone());
                let is_recall = q.command == "recall";
                s.queries.total += 1;
                s.queries.recall += usize::from(is_recall);
                s.queries.search += usize::from(!is_recall);
                s.queries.no_hits += usize::from(q.total == 0);
                s.queries.weak += usize::from(q.weak);
                match &q.session {
                    Some(id) => {
                        searched.insert(id.as_str());
                        s.queries.in_a_session += 1;
                    }
                    None => s.queries.outside_a_session += 1,
                }
                if is_recall {
                    recall_tokens.extend(q.tokens);
                    if let Some(c) = q.coverage_pct {
                        s.queries.coverage_bands[match c {
                            0..45 => 0,
                            45..70 => 1,
                            _ => 2,
                        }] += 1;
                    }
                }
                millis.extend(q.ms);
            }
        }
    }
    s.queries.with_a_used_result = events
        .iter()
        .filter(|e| match e {
            Event::Query(q) => q
                .session
                .as_deref()
                .is_some_and(|id| used_queries.contains(&(id, q.at.as_str()))),
            _ => false,
        })
        .count();
    s.versions = versions.into_iter().collect();
    s.sessions = Sessions {
        briefed: briefed.len(),
        completed: completed.len(),
        searched: searched.len(),
        used_a_result: used.len(),
        followed_the_brief: followed.len(),
    };
    s.queries.recall_tokens_p50 = percentile(&mut recall_tokens, 50);
    s.queries.ms_p50 = percentile(&mut millis, 50);
    s.queries.ms_p95 = percentile(&mut millis, 95);
    s.briefs.tokens_p50 = percentile(&mut brief_tokens, 50);
    s.briefs.items_p50 = percentile(&mut brief_items, 50);
    s
}

/// `query:get`, `brief:file`, …: what listed the note and how it was used.
fn use_key(u: &querylog::UseEvent) -> String {
    let from = match u.from {
        Source::Query => "query",
        Source::Brief => "brief",
    };
    let how = match u.how {
        How::Get => "get",
        How::File => "file",
        How::Shell => "shell",
        How::Cited => "cited",
    };
    format!("{from}:{how}")
}

/// Lines of the hook log at or after `since`; each line starts with its time.
#[must_use]
pub fn hook_errors(log: &Path, since: Option<&str>) -> usize {
    let Ok(text) = std::fs::read_to_string(log) else {
        return 0;
    };
    let cutoff = since.and_then(|s| s.parse::<jiff::Timestamp>().ok());
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| match cutoff {
            None => true,
            Some(c) => l
                .split_whitespace()
                .next()
                .and_then(|t| t.parse::<jiff::Timestamp>().ok())
                .is_none_or(|t| t >= c),
        })
        .count()
}

fn share(part: usize, whole: usize) -> String {
    if whole == 0 {
        "-".to_owned()
    } else {
        #[allow(clippy::cast_precision_loss)]
        let pct = part as f64 * 100.0 / whole as f64;
        format!("{pct:.0}%")
    }
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or("-".to_owned(), |v| v.to_string())
}

impl Summary {
    /// The summary as text.
    #[must_use]
    pub fn render(&self) -> String {
        let day = |t: &Option<String>| {
            t.as_deref()
                .map_or("-".to_owned(), |t| t.chars().take(10).collect())
        };
        let (se, q, b) = (&self.sessions, &self.queries, &self.briefs);
        let mut out = String::new();
        let _ = writeln!(
            out,
            "kvault report  {} to {}  version {}",
            day(&self.from),
            day(&self.to),
            if self.versions.is_empty() {
                "-".to_owned()
            } else {
                self.versions.join(", ")
            }
        );
        if let Some(c) = self.corpus {
            let _ = writeln!(
                out,
                "corpus    {} docs, {} chunks, {} roots",
                c.docs, c.chunks, c.roots
            );
        }
        let _ = writeln!(
            out,
            "sessions  {} given a brief, {} completed a turn",
            se.briefed, se.completed
        );
        let base = se.completed.max(se.briefed);
        let _ = writeln!(
            out,
            "          {} searched ({}), {} used a result ({}), {} used a note from the brief ({})",
            se.searched,
            share(se.searched, base),
            se.used_a_result,
            share(se.used_a_result, base),
            se.followed_the_brief,
            share(se.followed_the_brief, base)
        );
        let _ = writeln!(
            out,
            "queries   {} ({} recall, {} search; {} outside a session)",
            q.total, q.recall, q.search, q.outside_a_session
        );
        let _ = writeln!(
            out,
            "          {} with no hits, {} weak matches ({} of recalls), {} of {} in-session queries had a result used ({})",
            q.no_hits,
            q.weak,
            share(q.weak, q.recall),
            q.with_a_used_result,
            q.in_a_session,
            share(q.with_a_used_result, q.in_a_session)
        );
        let _ = writeln!(
            out,
            "          share of the query found: {} under 45%, {} at 45-69%, {} at 70%+",
            q.coverage_bands[0], q.coverage_bands[1], q.coverage_bands[2]
        );
        let _ = writeln!(
            out,
            "          recall tokens p50 {}, time p50 {} ms, p95 {} ms",
            opt(q.recall_tokens_p50),
            opt(q.ms_p50),
            opt(q.ms_p95)
        );
        let _ = writeln!(
            out,
            "briefs    {} injected, {} listed nothing, p50 {} notes, {} tokens",
            b.injected,
            b.empty,
            opt(b.items_p50),
            opt(b.tokens_p50)
        );
        let uses = if self.uses.is_empty() {
            "none".to_owned()
        } else {
            self.uses
                .iter()
                .map(|(k, n)| format!("{k} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let _ = writeln!(out, "uses      {uses}");
        let _ = writeln!(out, "hooks     {} errors logged", self.hook_errors);
        out
    }
}

/// The log lines of a period, as written.
///
/// # Errors
/// Returns an error if an event cannot be serialised.
pub fn full(events: &[Event]) -> Result<String> {
    let mut out = String::new();
    for e in events {
        out.push_str(&serde_json::to_string(e).map_err(|e| crate::Error::Other(e.into()))?);
        out.push('\n');
    }
    Ok(out)
}

/// Read the log and keep the period.
///
/// # Errors
/// Returns an error if the log exists but cannot be read.
pub fn load(log: &Path, since: Option<&str>) -> Result<Vec<Event>> {
    Ok(in_period(querylog::read(log)?, since))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn events() -> Vec<Event> {
        let text = r#"
{"event":"brief","at":"2026-11-20T10:00:00Z","version":"0.2.0","session":"s1","source":"startup","project":"secret-project","cwd":"/work/secret","tokens":300,"docs":100,"chunks":400,"roots":5,"hits":[{"id":"cccc3333","path":"p/n.md","file":"/work/p/n.md"}]}
{"event":"brief","at":"2026-11-20T11:00:00Z","version":"0.2.0","session":"s2","tokens":40,"docs":101,"chunks":404,"roots":5,"hits":[]}
{"event":"query","at":"2026-11-20T10:01:00Z","version":"0.2.0","command":"recall","query":"confidential words","session":"s1","total":7,"coverage_pct":82,"tokens":1200,"ms":4,"hits":[{"id":"aaaa1111","path":"p/a.md","file":"/work/p/a.md"}]}
{"event":"query","at":"2026-11-20T10:02:00Z","version":"0.2.0","command":"recall","query":"nothing like it","session":"s1","total":40,"weak":true,"coverage_pct":30,"tokens":310,"ms":6,"hits":[]}
{"event":"query","at":"2026-11-20T10:03:00Z","command":"search","query":"by hand","total":0,"ms":2,"hits":[]}
{"event":"session","at":"2026-11-20T10:04:00Z","session":"s1"}
{"event":"session","at":"2026-11-20T11:04:00Z","session":"s2"}
{"event":"use","at":"2026-11-20T10:04:00Z","session":"s1","query_at":"2026-11-20T10:01:00Z","from":"query","id":"aaaa1111","how":"get"}
{"event":"use","at":"2026-11-20T10:04:00Z","session":"s1","query_at":"2026-11-20T10:01:00Z","from":"query","id":"aaaa1111","how":"cited"}
{"event":"use","at":"2026-11-20T10:04:00Z","session":"s1","query_at":"2026-11-20T10:00:00Z","from":"brief","id":"cccc3333","how":"file"}
"#;
        text.lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn summary_counts_sessions_queries_and_uses() {
        let s = summarize(&events(), 2);
        assert_eq!(
            s.sessions,
            Sessions {
                briefed: 2,
                completed: 2,
                searched: 1,
                used_a_result: 1,
                followed_the_brief: 1,
            }
        );
        let q = &s.queries;
        assert_eq!((q.total, q.recall, q.search), (3, 2, 1));
        assert_eq!((q.outside_a_session, q.no_hits, q.weak), (1, 1, 1));
        assert_eq!((q.with_a_used_result, q.in_a_session), (1, 2));
        assert_eq!(q.coverage_bands, [1, 0, 1]);
        assert_eq!(
            (q.recall_tokens_p50, q.ms_p50, q.ms_p95),
            (Some(1200), Some(4), Some(6))
        );
        assert_eq!((s.briefs.injected, s.briefs.empty), (2, 1));
        assert_eq!(s.uses["query:get"], 1);
        assert_eq!(s.uses["query:cited"], 1);
        assert_eq!(s.uses["brief:file"], 1);
        assert_eq!(s.corpus.unwrap().docs, 101);
        assert_eq!(s.versions, vec!["0.2.0".to_owned()]);
        assert_eq!(s.hook_errors, 2);
    }

    #[test]
    fn summary_carries_no_query_text_paths_or_project_names() {
        let s = summarize(&events(), 0);
        let both = s.render() + &serde_json::to_string(&s).unwrap();
        for private in ["confidential", "secret", "/work", "n.md", "by hand"] {
            assert!(!both.contains(private), "{private} in:\n{both}");
        }
        assert!(both.contains("1 searched (50%)"), "{both}");
        // The full export is the log itself.
        assert!(full(&events()).unwrap().contains("confidential words"));
    }

    #[test]
    fn period_filter_and_empty_log() {
        let later = in_period(events(), Some("2026-11-20T10:30:00Z"));
        assert_eq!(later.len(), 2);
        let s = summarize(&[], 0);
        assert_eq!(s, Summary::default());
        assert!(s.render().contains("queries   0 ("));
        assert_eq!(percentile(&mut [5, 1, 3], 50), Some(3));
        assert_eq!(percentile::<u64>(&mut [], 50), None);
    }

    #[test]
    fn hook_errors_respect_the_period() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("hooks.log");
        std::fs::write(
            &log,
            "2026-11-19T10:00:00Z stop boom\n2026-11-21T10:00:00Z stop boom\n",
        )
        .unwrap();
        assert_eq!(hook_errors(&log, None), 2);
        assert_eq!(hook_errors(&log, Some("2026-11-20T00:00:00Z")), 1);
        assert_eq!(hook_errors(&tmp.path().join("absent"), None), 0);
    }
}
