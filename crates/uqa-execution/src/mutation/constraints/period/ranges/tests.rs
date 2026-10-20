//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{ParentRanges, RangeObservation};
use uqa_core::CancellationToken;
use uqa_sql::ast::RangeSubtype;
use uqa_sql::expr::{multirange_from_ranges, parse_multirange, parse_range};

#[test]
fn temporal_parent_union_preserves_coverage_gaps_and_overlap() {
    #[derive(serde::Deserialize)]
    struct Case {
        subtype: RangeSubtype,
        parents: String,
        child: String,
        covered: bool,
        overlaps: bool,
    }
    #[derive(serde::Deserialize)]
    struct Reference {
        cases: Vec<Case>,
    }
    let reference: Reference = serde_json::from_str(include_str!("tests/union.json")).unwrap();
    for Case {
        subtype,
        parents,
        child,
        covered,
        overlaps,
    } in reference.cases
    {
        let parents = parse_multirange(&parents, subtype).unwrap();
        let child = parse_multirange(&child, subtype).unwrap();
        for bytes in [0, 64 * 1024] {
            let mut ranges = ParentRanges::new(subtype, bytes, CancellationToken::new());
            for range in parents.ranges().iter().rev() {
                ranges.push(range).unwrap();
                ranges.push(range).unwrap();
            }
            assert_eq!(
                ranges.observe(child.ranges()).unwrap(),
                RangeObservation { covered, overlaps },
                "{subtype:?}, budget {bytes}"
            );
        }
    }
    let empty = ParentRanges::new(RangeSubtype::Integer, 1, CancellationToken::new());
    let child = parse_range("[1,2)", RangeSubtype::Integer).unwrap();
    assert_eq!(
        empty.observe(&[child]).unwrap(),
        RangeObservation {
            covered: false,
            overlaps: false
        }
    );
}

#[test]
fn temporal_parent_union_spills_without_retaining_all_ranges() {
    let count = 16_384;
    let parents = (0..count)
        .map(|i| parse_range(&format!("[{i},{})", i + 1), RangeSubtype::Integer).unwrap())
        .collect::<Vec<_>>();
    let child = parse_range(&format!("[0,{count})"), RangeSubtype::Integer).unwrap();
    // Retain the former accumulator as a negative control, using the same parsed input and exact union observation.
    let baseline = allocation_counter::measure(|| {
        let mut accumulated = Vec::new();
        for parent in &parents {
            accumulated.push(parent.clone());
        }
        let union = multirange_from_ranges(RangeSubtype::Integer, accumulated);
        assert!(union.contains_range(&child));
    });
    assert!(baseline.bytes_max > 512 * 1024, "{baseline:?}");
    let bounded = allocation_counter::measure(|| {
        let mut ranges =
            ParentRanges::new(RangeSubtype::Integer, 64 * 1024, CancellationToken::new());
        for range in parents.iter().rev() {
            ranges.push(range).unwrap();
        }
        assert_eq!(
            ranges.observe(std::slice::from_ref(&child)).unwrap(),
            RangeObservation {
                covered: true,
                overlaps: true
            }
        );
    });
    assert!(bounded.bytes_max <= 512 * 1024, "{bounded:?}");
}

#[test]
fn temporal_parent_union_retains_cancellation_and_subtype_errors() {
    let token = CancellationToken::new();
    let mut ranges = ParentRanges::new(RangeSubtype::Integer, 1, token.clone());
    let wrong = parse_range("[1,2)", RangeSubtype::Numeric).unwrap();
    assert!(matches!(
        ranges.push(&wrong),
        Err(uqa_sql::SQLError::TypeMismatch(_))
    ));
    let child = parse_range("[1,2)", RangeSubtype::Integer).unwrap();
    ranges.push(&child).unwrap();
    token.cancel();
    let error = ranges.observe(&[child]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
}
