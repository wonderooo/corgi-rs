//! Accuracy bookkeeping.
//!
//! Two numbers matter and they pull in opposite directions:
//!
//! - **coverage** — how often the decoder produced a value at all,
//! - **accuracy** — how often it was right *when both sides had something to
//!   say*.
//!
//! Reporting accuracy over all rows would let a decoder look good by staying
//! silent, so rows where the auction listing itself is blank are excluded from
//! the accuracy denominator and counted separately.

use std::collections::HashMap;

/// Running totals for one compared field.
#[derive(Debug, Default, Clone)]
pub struct FieldStats {
    pub rows: u64,
    /// Rows where the auction listing supplied a usable value.
    pub truth: u64,
    /// Rows where the decoder supplied a value.
    pub produced: u64,
    /// Rows where both did, and so could be compared.
    pub comparable: u64,
    pub agree: u64,
    /// Agreement once the vocabularies the auctions do not distinguish are
    /// merged. Never lower than [`FieldStats::agree`].
    pub agree_relaxed: u64,
    mismatches: HashMap<(String, String), u64>,
}

impl FieldStats {
    /// Record one row.
    ///
    /// `expected` is what the auction listed and `actual` what the decoder
    /// produced; either may be absent.
    pub fn observe(
        &mut self,
        expected: Option<&str>,
        actual: Option<&str>,
        agrees: bool,
        agrees_relaxed: bool,
    ) {
        self.rows += 1;
        self.truth += u64::from(expected.is_some());
        self.produced += u64::from(actual.is_some());

        let (Some(expected), Some(actual)) = (expected, actual) else {
            return;
        };

        self.comparable += 1;
        self.agree += u64::from(agrees);
        self.agree_relaxed += u64::from(agrees_relaxed);

        if !agrees_relaxed {
            *self
                .mismatches
                .entry((expected.to_string(), actual.to_string()))
                .or_default() += 1;
        }
    }

    /// Share of rows the decoder answered.
    pub fn coverage(&self) -> f64 {
        ratio(self.produced, self.rows)
    }

    /// Share of comparable rows the decoder got right.
    pub fn accuracy(&self) -> f64 {
        ratio(self.agree, self.comparable)
    }

    /// Accuracy once interchangeable vocabularies are merged.
    pub fn accuracy_relaxed(&self) -> f64 {
        ratio(self.agree_relaxed, self.comparable)
    }

    /// The most frequent disagreements, worst first.
    pub fn top_mismatches(&self, limit: usize) -> Vec<((String, String), u64)> {
        let mut entries: Vec<_> = self
            .mismatches
            .iter()
            .map(|(pair, count)| (pair.clone(), *count))
            .collect();
        // Sort by count, then by the pair itself so the output is stable.
        entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        entries.truncate(limit);
        entries
    }

    /// Fold another accumulator into this one, for parallel runs.
    pub fn merge(&mut self, other: &FieldStats) {
        self.rows += other.rows;
        self.truth += other.truth;
        self.produced += other.produced;
        self.comparable += other.comparable;
        self.agree += other.agree;
        self.agree_relaxed += other.agree_relaxed;
        for (pair, count) in &other.mismatches {
            *self.mismatches.entry(pair.clone()).or_default() += count;
        }
    }
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

/// The fields compared, in report order.
pub const FIELDS: [&str; 9] = [
    "make",
    "model",
    "year",
    "body style",
    "fuel",
    "drive",
    "transmission",
    "cylinders",
    "displacement",
];

/// Every field's statistics for one decoder.
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub label: String,
    pub stats: HashMap<&'static str, FieldStats>,
    /// VINs that produced no result at all.
    pub decode_failures: u64,
    /// Failures grouped by reason.
    pub failure_reasons: HashMap<String, u64>,
    pub rows: u64,
}

impl Report {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            ..Default::default()
        }
    }

    pub fn field(&mut self, name: &'static str) -> &mut FieldStats {
        self.stats.entry(name).or_default()
    }

    pub fn record_failure(&mut self, reason: String) {
        self.decode_failures += 1;
        *self.failure_reasons.entry(reason).or_default() += 1;
    }

    pub fn merge(&mut self, other: &Report) {
        self.rows += other.rows;
        self.decode_failures += other.decode_failures;
        for (name, stats) in &other.stats {
            self.stats.entry(name).or_default().merge(stats);
        }
        for (reason, count) in &other.failure_reasons {
            *self.failure_reasons.entry(reason.clone()).or_default() += count;
        }
    }

    /// Print the field table.
    pub fn print(&self) {
        println!("\n=== {} ===", self.label);
        println!(
            "rows {}, decode failures {} ({:.2}%)",
            self.rows,
            self.decode_failures,
            100.0 * ratio(self.decode_failures, self.rows)
        );

        println!(
            "\n  {:<14} {:>9} {:>10} {:>10} {:>10}",
            "field", "coverage", "compared", "accuracy", "relaxed"
        );
        for name in FIELDS {
            let Some(stats) = self.stats.get(name) else {
                continue;
            };
            println!(
                "  {:<14} {:>8.2}% {:>10} {:>9.2}% {:>9.2}%",
                name,
                100.0 * stats.coverage(),
                stats.comparable,
                100.0 * stats.accuracy(),
                100.0 * stats.accuracy_relaxed(),
            );
        }

        if !self.failure_reasons.is_empty() {
            let mut reasons: Vec<_> = self.failure_reasons.iter().collect();
            reasons.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            println!("\n  failure reasons:");
            for (reason, count) in reasons.iter().take(8) {
                println!("    {count:>8}  {reason}");
            }
        }
    }

    /// Print the worst disagreements per field.
    pub fn print_mismatches(&self, limit: usize) {
        if limit == 0 {
            return;
        }
        for name in FIELDS {
            let Some(stats) = self.stats.get(name) else {
                continue;
            };
            let top = stats.top_mismatches(limit);
            if top.is_empty() {
                continue;
            }
            println!("\n  top {name} mismatches (listing -> decoded):");
            for ((expected, actual), count) in top {
                println!("    {count:>8}  {expected}  ->  {actual}");
            }
        }
    }
}

/// Print two reports side by side, so a change is visible rather than merely
/// reported.
pub fn print_comparison(baseline: &Report, candidate: &Report) {
    println!("\n=== {} vs {} ===", baseline.label, candidate.label);
    println!(
        "\n  {:<14} {:>19} {:>19} {:>19}",
        "field", "coverage", "accuracy", "relaxed accuracy"
    );
    for name in FIELDS {
        let (Some(before), Some(after)) = (baseline.stats.get(name), candidate.stats.get(name))
        else {
            continue;
        };
        println!(
            "  {:<14} {:>8.2}% -> {:>6.2}% {:>8.2}% -> {:>6.2}% {:>8.2}% -> {:>6.2}%",
            name,
            100.0 * before.coverage(),
            100.0 * after.coverage(),
            100.0 * before.accuracy(),
            100.0 * after.accuracy(),
            100.0 * before.accuracy_relaxed(),
            100.0 * after.accuracy_relaxed(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_silent_decoder_scores_zero_coverage_not_perfect_accuracy() {
        let mut stats = FieldStats::default();
        for _ in 0..10 {
            stats.observe(Some("RAM"), None, false, false);
        }
        assert_eq!(stats.coverage(), 0.0);
        assert_eq!(stats.comparable, 0);
        // No comparable rows means no accuracy claim at all.
        assert_eq!(stats.accuracy(), 0.0);
    }

    #[test]
    fn rows_without_ground_truth_stay_out_of_the_accuracy_denominator() {
        let mut stats = FieldStats::default();
        stats.observe(Some("RAM"), Some("RAM"), true, true);
        stats.observe(None, Some("JEEP"), false, false);
        assert_eq!(stats.rows, 2);
        assert_eq!(stats.comparable, 1);
        assert_eq!(stats.accuracy(), 1.0);
        assert_eq!(stats.coverage(), 1.0);
    }

    #[test]
    fn relaxed_agreement_is_never_worse_than_strict() {
        let mut stats = FieldStats::default();
        stats.observe(Some("AWD"), Some("4WD"), false, true);
        assert_eq!(stats.accuracy(), 0.0);
        assert_eq!(stats.accuracy_relaxed(), 1.0);
        // A relaxed match is not a mismatch worth showing.
        assert!(stats.top_mismatches(5).is_empty());
    }

    #[test]
    fn mismatches_are_ranked_and_stable() {
        let mut stats = FieldStats::default();
        for _ in 0..3 {
            stats.observe(Some("JEEP"), Some("DODGE"), false, false);
        }
        stats.observe(Some("RAM"), Some("DODGE"), false, false);

        let top = stats.top_mismatches(5);
        assert_eq!(top[0].0, ("JEEP".to_string(), "DODGE".to_string()));
        assert_eq!(top[0].1, 3);
        assert_eq!(top.len(), 2);
    }

    #[test]
    fn merging_preserves_every_total() {
        let mut left = FieldStats::default();
        left.observe(Some("A"), Some("A"), true, true);
        let mut right = FieldStats::default();
        right.observe(Some("A"), Some("B"), false, false);

        left.merge(&right);
        assert_eq!(left.rows, 2);
        assert_eq!(left.comparable, 2);
        assert_eq!(left.agree, 1);
        assert_eq!(left.accuracy(), 0.5);
        assert_eq!(left.top_mismatches(1)[0].1, 1);
    }
}
