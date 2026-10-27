//! Budget packer: fit ranked hits into a token budget using L0/L1/L2 tiers
//! and a measure-and-drop loop. Whole chunk or nothing.
//!
//! Implemented in M3.

/// Token counting is behind a trait so the BPE-backed counter can be swapped
/// for a byte heuristic when the tables are unavailable.
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
}
