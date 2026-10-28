//! Budget packer: fit ranked items into a token budget using tiered detail
//! levels and a measure-and-drop loop. An item is shown whole at its tier or
//! not at all; text is never cut mid-chunk.
//!
//! Tiers per item: `L0` is a one-line reference, `L1` adds a short preview,
//! `L2` is the whole chunk. The packer starts generous (ranks 1–3 at `L2`,
//! the next seven at `L1`, the rest at `L0`) and sheds in this order until the
//! rendered output fits: drop `L0` lines from the tail, downgrade `L1` to
//! `L0` from the tail, downgrade `L2` to `L1` from the tail, downgrade those
//! to `L0` too, and only then drop the lowest-ranked item outright, so a
//! one-line reference is always preferred to nothing. The caller renders, so
//! framing text counts.

use crate::markdown::split_frontmatter;

/// Token counting is behind a trait so the byte heuristic can be swapped
/// for a BPE-backed counter without touching the packer.
pub trait TokenCounter {
    fn count(&self, text: &str) -> usize;
}

/// Cheap fallback: ~3.6 bytes per token for English markdown.
#[derive(Debug, Default, Clone, Copy)]
pub struct ByteHeuristic;

impl TokenCounter for ByteHeuristic {
    fn count(&self, text: &str) -> usize {
        // integer math: (len * 10) / 36, rounded up
        text.len().saturating_mul(10).div_ceil(36)
    }
}

/// Ranks shown as the whole chunk when the budget allows.
pub const FULL_RANKS: usize = 3;
/// Ranks after [`FULL_RANKS`] shown with a preview.
pub const PREVIEW_RANKS: usize = 7;

/// Detail level of a packed item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// One-line reference.
    L0,
    /// Reference plus a preview.
    L1,
    /// The whole chunk.
    L2,
}

/// Caps on the rendered output.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub tokens: usize,
    /// Byte cap, for consumers that truncate long output (Claude Code hooks).
    pub bytes: Option<usize>,
}

/// What the packer decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packed {
    /// Tier per item, in rank order; `None` for a dropped item.
    pub tiers: Vec<Option<Tier>>,
    pub text: String,
    pub tokens: usize,
}

impl Packed {
    #[must_use]
    pub fn shown(&self) -> usize {
        self.tiers.iter().filter(|t| t.is_some()).count()
    }

    #[must_use]
    pub fn omitted(&self) -> usize {
        self.tiers.len() - self.shown()
    }
}

/// Fit `n` ranked items into `limits`. `has_full` says whether an item can be
/// shown at `L2` at all; `render` turns a tier assignment into the complete
/// output, framing included, which is what gets measured.
pub fn pack(
    n: usize,
    has_full: impl Fn(usize) -> bool,
    limits: Limits,
    counter: &impl TokenCounter,
    render: impl Fn(&[Option<Tier>]) -> String,
) -> Packed {
    let mut tiers: Vec<Option<Tier>> = (0..n)
        .map(|i| {
            Some(if i < FULL_RANKS && has_full(i) {
                Tier::L2
            } else if i < FULL_RANKS + PREVIEW_RANKS {
                Tier::L1
            } else {
                Tier::L0
            })
        })
        .collect();
    let fits = |text: &str, tokens: usize| {
        tokens <= limits.tokens && limits.bytes.is_none_or(|b| text.len() <= b)
    };
    let steps: [(Tier, Option<Tier>); 5] = [
        (Tier::L0, None),
        (Tier::L1, Some(Tier::L0)),
        (Tier::L2, Some(Tier::L1)),
        (Tier::L1, Some(Tier::L0)),
        (Tier::L0, None),
    ];
    let mut text = render(&tiers);
    let mut tokens = counter.count(&text);
    'shed: for (phase, (from, to)) in steps.iter().enumerate() {
        loop {
            if fits(&text, tokens) {
                break 'shed;
            }
            // The final phase drops whatever remains, lowest rank first.
            let last_phase = phase == steps.len() - 1;
            let Some(i) = tiers
                .iter()
                .rposition(|t| t.is_some_and(|t| last_phase || t == *from))
            else {
                break;
            };
            tiers[i] = *to;
            text = render(&tiers);
            tokens = counter.count(&text);
        }
    }
    if !fits(&text, tokens) {
        // Even the framing alone is over budget: emit nothing.
        text = String::new();
        tokens = 0;
        tiers.fill(None);
    }
    Packed {
        tiers,
        text,
        tokens,
    }
}

/// Text as the packer shows it: no frontmatter block, no trailing whitespace
/// on a line, at most one blank line in a row, no leading or trailing blank
/// lines. Nothing inside a line is rewritten, so `path:line` citations and
/// code fences survive. Chunks never start with frontmatter, but a stale
/// line-window read of a changed file can.
#[must_use]
pub fn trim(text: &str) -> String {
    let (_, body_start) = split_frontmatter(text);
    let mut out = String::new();
    let mut blank_pending = false;
    for line in text[body_start..].lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank_pending = !out.is_empty();
            continue;
        }
        if blank_pending {
            out.push('\n');
            blank_pending = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// `s` cut to at most `max` characters, the last one an ellipsis when cut.
#[must_use]
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The first `n` non-empty lines of `text` that are not headings, for a
/// preview. Falls back to heading lines if nothing else exists.
#[must_use]
pub fn preview(text: &str, n: usize) -> Vec<&str> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let body: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !l.starts_with('#'))
        .take(n)
        .collect();
    if body.is_empty() {
        lines.into_iter().take(n).collect()
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_is_monotonic_and_nonzero() {
        let c = ByteHeuristic;
        assert_eq!(c.count(""), 0);
        assert!(c.count("a") >= 1);
        assert!(c.count("hello world, this is text") > c.count("hello"));
    }

    #[test]
    fn trim_drops_frontmatter_and_collapses_blank_runs() {
        let t = "---\ntitle: X\npinned: true\n---\n\n\n# X  \n\n\n\nbody line   \n\n\n\nmore\n\n\n";
        assert_eq!(trim(t), "# X\n\nbody line\n\nmore\n");
        assert_eq!(trim("\n\n\nonly\n"), "only\n");
        assert_eq!(trim(""), "");
        assert_eq!(
            trim("no frontmatter\n---\nrule"),
            "no frontmatter\n---\nrule\n"
        );
    }

    #[test]
    fn truncate_keeps_short_lines_and_marks_long_ones() {
        assert_eq!(truncate("short", 10), "short");
        let t = truncate(&"x".repeat(30), 10);
        assert_eq!(t.chars().count(), 10);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn preview_skips_headings_unless_nothing_else() {
        assert_eq!(
            preview("# H\n\nfirst\nsecond\nthird\n", 2),
            vec!["first", "second"]
        );
        assert_eq!(preview("# Only heading\n", 2), vec!["# Only heading"]);
    }

    /// Items render as `<tier letter> x width` so sizes are predictable:
    /// L0 = 10 bytes, L1 = 30, L2 = 100 (plus a newline each).
    fn render(tiers: &[Option<Tier>]) -> String {
        let mut out = String::from("HEAD\n");
        for t in tiers.iter().flatten() {
            let w = match t {
                Tier::L0 => 10,
                Tier::L1 => 30,
                Tier::L2 => 100,
            };
            out.push_str(&"x".repeat(w));
            out.push('\n');
        }
        out.push_str("FOOT\n");
        out
    }

    struct Bytes;
    impl TokenCounter for Bytes {
        fn count(&self, text: &str) -> usize {
            text.len()
        }
    }

    fn limits(tokens: usize) -> Limits {
        Limits {
            tokens,
            bytes: None,
        }
    }

    #[test]
    fn generous_start_when_everything_fits() {
        let p = pack(12, |_| true, limits(10_000), &Bytes, render);
        let want: Vec<Option<Tier>> = [Tier::L2; 3]
            .iter()
            .chain([Tier::L1; 7].iter())
            .chain([Tier::L0; 2].iter())
            .map(|t| Some(*t))
            .collect();
        assert_eq!(p.tiers, want);
        assert_eq!(p.tokens, render(&p.tiers).len());
        assert_eq!(p.omitted(), 0);
    }

    #[test]
    fn sheds_in_order_tail_first() {
        // Full: 10 + 3*101 + 7*31 + 2*11 = 552 bytes.
        let p = pack(12, |_| true, limits(540), &Bytes, render);
        assert!(p.tiers[10..].iter().all(Option::is_none), "L0 tail first");
        assert_eq!(p.tiers[9], Some(Tier::L1));
        assert_eq!(p.tokens, 530);

        let p = pack(12, |_| true, limits(500), &Bytes, render);
        assert_eq!(p.tiers[9], Some(Tier::L0), "then the last L1s downgrade");
        assert_eq!(p.tiers[8], Some(Tier::L0));
        assert_eq!(p.tiers[7], Some(Tier::L1));
        assert_eq!(p.tokens, 490);

        let p = pack(12, |_| true, limits(250), &Bytes, render);
        assert_eq!(
            p.tiers[0],
            Some(Tier::L2),
            "rank 1 keeps its full text longest"
        );
        assert_eq!(p.tiers[1], Some(Tier::L1));
        assert_eq!(p.tiers[3], Some(Tier::L0));
        assert_eq!(p.tokens, 250);

        // Below what three previews cost, the previews become references
        // before anything is dropped: 10 + 10*11 = 120, so 80 keeps six.
        let p = pack(12, |_| true, limits(80), &Bytes, render);
        assert_eq!(p.tiers[0], Some(Tier::L0));
        assert_eq!(p.tiers[5], Some(Tier::L0));
        assert!(p.tiers[6..].iter().all(Option::is_none));
        assert_eq!(p.tokens, 76);
    }

    #[test]
    fn never_over_budget_and_empty_when_framing_does_not_fit() {
        for budget in 0..700 {
            let p = pack(12, |i| i != 1, limits(budget), &Bytes, render);
            assert!(p.tokens <= budget, "budget {budget}: {}", p.tokens);
            assert_ne!(p.tiers[1], Some(Tier::L2), "has_full is honoured");
            if budget < 10 {
                assert_eq!(p.text, "");
                assert_eq!(p.omitted(), 12);
            }
        }
    }

    #[test]
    fn byte_cap_is_a_second_limit() {
        let p = pack(
            12,
            |_| true,
            Limits {
                tokens: 10_000,
                bytes: Some(200),
            },
            &Bytes,
            render,
        );
        assert!(p.text.len() <= 200);
        assert!(p.shown() > 0);
    }
}
