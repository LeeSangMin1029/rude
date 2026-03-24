//! Unit tests for `dupe_analyze` — Verdict labels.

use crate::dupe_analyze::Verdict;

#[test]
fn verdict_labels() {
    assert_eq!(Verdict::SafeToMerge.label(), "SAFE TO MERGE");
    assert_eq!(Verdict::ReviewNeeded.label(), "REVIEW NEEDED");
    assert_eq!(Verdict::DifferentLogic.label(), "DIFFERENT LOGIC");
}
