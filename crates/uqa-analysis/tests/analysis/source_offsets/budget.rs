//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{mapping, offsets};
use uqa_analysis::{AnalysisError, CharFilter, TextCoordinates};
use uqa_core::memory::{Budgeted, MemoryBudget, MemoryError};

#[test]
fn coordinate_limits_precede_allocation_and_ascii_needs_no_boundary_buffer() {
    let ascii_budget = MemoryBudget::new(0);
    let ascii = TextCoordinates::new_budgeted("ascii", &ascii_budget, &mut || Ok(())).unwrap();
    assert_eq!(ascii.reserved_bytes(), 0);
    assert_eq!(ascii.utf8_to_utf16(3).unwrap(), 3);
    let bytes = 4 * size_of::<(usize, usize)>();
    let small = MemoryBudget::new(bytes - 1);
    assert!(matches!(
        TextCoordinates::new_budgeted("A한🙂", &small, &mut || Ok(())),
        Err(AnalysisError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(small.peak(), 0);
    let exact = MemoryBudget::new(bytes);
    let coordinates = TextCoordinates::new_budgeted("A한🙂", &exact, &mut || Ok(())).unwrap();
    assert_eq!(
        coordinates.covering_offsets_utf16(2..3).unwrap(),
        offsets(4..8, 2..3)
    );
    assert_eq!(coordinates.reserved_bytes(), bytes);
    assert_eq!(exact.used(), bytes);
    drop(coordinates);
    assert_eq!(exact.used(), 0);
}

#[test]
fn cloning_filtered_text_shares_allocations_and_later_edits_keep_older_views_intact() {
    let original = "<b>韓&amp;🙂</b>";
    let first_budget = MemoryBudget::new(1 << 20);
    let first = CharFilter::HTMLStrip
        .filter_with_offsets_budgeted(original, &first_budget, &mut || Ok(()))
        .unwrap();
    let used = first_budget.used();
    let peak = first_budget.peak();
    let cloned = first.clone();
    assert_eq!(first_budget.used(), used);
    assert_eq!(first_budget.peak(), peak);
    let second_budget = MemoryBudget::new(1 << 20);
    let second = mapping(&[("韓", "한국")])
        .filter_mapped_budgeted(cloned, &second_budget, &mut || Ok(()))
        .unwrap();
    assert_eq!(first.as_str(), " 韓&🙂 ");
    assert_eq!(second.as_str(), " 한국&🙂 ");
    assert_eq!(first.source_offsets(1..4).unwrap().utf8, 3..6);
    assert_eq!(second.source_offsets(1..7).unwrap().utf8, 3..6);
    assert_eq!(first_budget.used(), used);
    drop(first);
    assert!(first_budget.used() > 0);
    drop(second);
    assert_eq!(first_budget.used(), 0);
    assert_eq!(second_budget.used(), 0);
}

#[test]
fn identity_capture_expansion_allocates_only_the_retained_coordinate_index() {
    let expected = size_of::<Budgeted<TextCoordinates>>() + 3 * size_of::<(usize, usize)>();
    let budget = MemoryBudget::new(expected);
    let filter = CharFilter::PatternReplace {
        pattern: "(?P<x>.*)".into(),
        replacement: "$x".into(),
    };
    let output = filter
        .filter_with_offsets_budgeted("韓🙂", &budget, &mut || Ok(()))
        .unwrap();
    assert_eq!(output.as_str(), "韓🙂");
    assert_eq!(budget.used(), expected);
    assert_eq!(output.source_offsets(3..7).unwrap().utf16, 1..3);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn allocation_failures_release_source_edits_and_keep_other_owners_reserved() {
    let filter = CharFilter::HTMLStrip;
    let input = "<b>韓&amp;🙂</b>";
    let baseline = MemoryBudget::new(1 << 20);
    let expected = filter
        .filter_with_offsets_budgeted(input, &baseline, &mut || Ok(()))
        .unwrap();
    let mut partial_failures = 0;
    for allowance in (0..=baseline.peak()).step_by(32) {
        let budget = MemoryBudget::new(allowance + 7);
        let other = budget.reserve(7).unwrap();
        match filter.filter_with_offsets_budgeted(input, &budget, &mut || Ok(())) {
            Ok(output) => {
                assert_eq!(output.as_str(), expected.as_str());
                assert_eq!(
                    output.source_covering_offsets_utf16(3..4).unwrap(),
                    expected.source_covering_offsets_utf16(3..4).unwrap()
                );
            }
            Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {
                if budget.peak() > 7 {
                    partial_failures += 1;
                }
            }
            other => panic!("unexpected result at {allowance}: {other:?}"),
        }
        assert_eq!(budget.used(), 7);
        drop(other);
        assert_eq!(budget.used(), 0);
    }
    assert!(partial_failures > 3);
}

#[test]
fn cancellation_during_edits_and_coordinate_building_discards_the_entire_result() {
    let input = "<b>韓&amp;🙂</b>".repeat(40);
    let baseline = MemoryBudget::new(1 << 20);
    let mut polls = 0;
    let expected = CharFilter::HTMLStrip
        .filter_with_offsets_budgeted(&input, &baseline, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
    assert!(polls > 100);
    for stop in [1, 2, 12, polls / 3, polls / 2, polls - 1, polls] {
        let budget = MemoryBudget::new(1 << 20);
        let mut count = 0;
        let result =
            CharFilter::HTMLStrip.filter_with_offsets_budgeted(&input, &budget, &mut || {
                count += 1;
                if count == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
        assert!(
            matches!(result, Err(AnalysisError::Cancelled)),
            "stop {stop}: {result:?}"
        );
        assert_eq!(budget.used(), 0);
    }
    let repeated = CharFilter::HTMLStrip
        .filter_with_offsets_budgeted(&input, &MemoryBudget::new(1 << 20), &mut || Ok(()))
        .unwrap();
    assert_eq!(repeated.as_str(), expected.as_str());
}

#[test]
fn coordinate_cancellation_covers_ascii_scans_and_reserved_unicode_indexes() {
    for input in ["a".repeat(8192), "a🙂".repeat(2048)] {
        let baseline = MemoryBudget::new(1 << 20);
        let mut polls = 0;
        let coordinates = TextCoordinates::new_budgeted(&input, &baseline, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
        assert!(polls > 5);
        for stop in [1, 2, polls / 2, polls] {
            let budget = MemoryBudget::new(1 << 20);
            let mut count = 0;
            let result = TextCoordinates::new_budgeted(&input, &budget, &mut || {
                count += 1;
                if count == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(matches!(result, Err(AnalysisError::Cancelled)));
            assert_eq!(budget.used(), 0);
        }
        assert_eq!(coordinates.utf16_len(), input.encode_utf16().count());
    }
}

#[test]
fn capture_offsets_reserve_slot_capacity_before_search() {
    let required = size_of::<[(usize, usize); 4]>();
    let budget = MemoryBudget::new(required - 1);
    let filter = CharFilter::PatternReplace {
        pattern: "(a)(b)(c)".into(),
        replacement: "$1$2$3".into(),
    };
    assert!(
        matches!(filter.filter_with_offsets_budgeted("abc", &budget, &mut || Ok(())), Err(AnalysisError::Memory(MemoryError::Limit { required: bytes, limit })) if bytes == required && limit == required - 1)
    );
    assert_eq!(budget.peak(), 0);
}

#[cfg(feature = "nori")]
#[test]
fn retained_korean_token_contexts_keep_source_reservations_after_the_input_view_is_dropped() {
    use uqa_analysis::nori::{KoreanFilter, KoreanTokenizer, NoriOptions};
    let model = crate::nori_resources::model();
    let tokenizer = KoreanTokenizer::new(model.clone(), None, NoriOptions::default()).unwrap();
    let budget = MemoryBudget::new(1 << 20);
    let filtered = CharFilter::HTMLStrip
        .filter_with_offsets_budgeted("<b>韓國</b>", &budget, &mut || Ok(()))
        .unwrap();
    let output = tokenizer.tokenize(filtered.as_str()).unwrap();
    let analyzed = output.into_analyzed(&filtered).unwrap();
    drop(filtered);
    assert!(budget.used() > 0);
    let analyzed = KoreanFilter::ReadingForm
        .filter_analyzed(analyzed, model)
        .unwrap();
    assert_eq!(analyzed.tokens()[0].term(), "한국");
    assert_eq!(analyzed.tokens()[0].offsets().unwrap().utf8, 3..9);
    let retained = budget.used();
    let cloned = analyzed.clone();
    drop(analyzed);
    assert_eq!(budget.used(), retained);
    drop(cloned);
    assert_eq!(budget.used(), 0);
}
