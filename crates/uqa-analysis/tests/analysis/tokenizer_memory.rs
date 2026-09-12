//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_analysis::{AnalysisError, CharFilter, Tokenizer};
use uqa_core::memory::{MemoryBudget, MemoryError};

fn tokenizers() -> Vec<Tokenizer> {
    vec![
        Tokenizer::Whitespace,
        Tokenizer::Standard,
        Tokenizer::Letter,
        Tokenizer::NGram {
            min_gram: 1,
            max_gram: 3,
        },
        Tokenizer::Pattern {
            pattern: "[ ,]+".into(),
        },
        Tokenizer::Keyword,
    ]
}

#[test]
fn budgeted_tokenizers_preserve_scalar_spans_and_emission_order() {
    let cases = [
        (
            Tokenizer::Whitespace,
            " ab\t韓🙂\ncd ",
            vec!["ab", "韓🙂", "cd"],
        ),
        (Tokenizer::Standard, "é😀A_b 42", vec!["é", "A_b", "42"]),
        (Tokenizer::Letter, "éRust42世界 ABC", vec!["Rust", "ABC"]),
        (
            Tokenizer::NGram {
                min_gram: 1,
                max_gram: 2,
            },
            "a🙂b",
            vec!["a", "🙂", "b", "a🙂", "🙂b"],
        ),
        (
            Tokenizer::Pattern {
                pattern: String::new(),
            },
            "a🙂b",
            vec!["a", "🙂", "b"],
        ),
        (Tokenizer::Keyword, "한 🙂", vec!["한 🙂"]),
    ];
    for (tokenizer, input, expected) in cases {
        let budget = MemoryBudget::new(1 << 20);
        let output = tokenizer
            .tokenize_with_offsets_budgeted(input, &budget, || Ok(()))
            .unwrap();
        assert_eq!(
            output
                .tokens()
                .iter()
                .map(|token| token.term().as_str().unwrap())
                .collect::<Vec<_>>(),
            expected
        );
        for token in output.tokens() {
            let offsets = token.offsets().unwrap();
            assert_eq!(&input[offsets.utf8.clone()], token.term().as_str().unwrap());
            assert_eq!(
                offsets.utf16.start,
                input[..offsets.utf8.start].encode_utf16().count()
            );
            assert_eq!(
                offsets.utf16.end,
                input[..offsets.utf8.end].encode_utf16().count()
            );
            assert_eq!(token.position_increment(), 1);
            assert_eq!(token.position_length(), 1);
        }
        assert_eq!(output.final_offsets().utf8.end, input.len());
        assert_eq!(
            output.final_offsets().utf16.end,
            input.encode_utf16().count()
        );
        assert!(budget.used() >= output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn token_allocation_failures_clean_up_at_different_stages_and_allow_reuse() {
    let input = "한🙂 abc, def";
    for tokenizer in tokenizers() {
        let baseline = MemoryBudget::new(1 << 20);
        let expected = tokenizer
            .tokenize_with_offsets_budgeted(input, &baseline, || Ok(()))
            .unwrap();
        let mut failures = 0;
        for allowance in (0..=baseline.peak()).step_by(64) {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            match tokenizer.tokenize_with_offsets_budgeted(input, &budget, || Ok(())) {
                Ok(actual) => assert_eq!(*actual, *expected),
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {
                    failures += 1;
                }
                other => panic!("{tokenizer:?}: {other:?}"),
            }
            assert_eq!(budget.used(), 7);
            assert!(budget.peak() <= budget.limit());
            drop(other);
            assert_eq!(budget.used(), 0);
        }
        assert!(failures > 2);
        let budget = MemoryBudget::new(baseline.peak());
        let actual = tokenizer
            .tokenize_with_offsets_budgeted(input, &budget, || Ok(()))
            .unwrap();
        assert_eq!(*actual, *expected);
        drop(actual);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn cancellation_interrupts_scans_emission_projection_and_final_position_validation() {
    let input = "ab 韓🙂 xyz";
    for tokenizer in tokenizers() {
        let baseline = MemoryBudget::new(1 << 20);
        let mut polls = 0;
        let expected = tokenizer
            .tokenize_with_offsets_budgeted(input, &baseline, || {
                polls += 1;
                Ok(())
            })
            .unwrap();
        assert!(polls > 5);
        for stop in 1..=polls {
            let budget = MemoryBudget::new(1 << 20);
            let other = budget.reserve(7).unwrap();
            let mut count = 0;
            let actual = tokenizer.tokenize_with_offsets_budgeted(input, &budget, || {
                count += 1;
                if count == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(actual, Err(AnalysisError::Cancelled)),
                "{tokenizer:?} poll {stop}"
            );
            assert_eq!(budget.used(), 7);
            drop(other);
            assert_eq!(budget.used(), 0);
        }
        assert!(!expected.tokens().is_empty());
    }
}

#[test]
fn mapped_tokens_retain_source_owners_and_budgeted_result_sharing_retains_all_leases() {
    let source_budget = MemoryBudget::new(1 << 20);
    let token_budget = MemoryBudget::new(1 << 20);
    let filtered = CharFilter::HTMLStrip
        .filter_with_offsets_budgeted("<b>韓🙂</b>", &source_budget, &mut || Ok(()))
        .unwrap();
    let output = Tokenizer::Whitespace
        .tokenize_mapped_budgeted(&filtered, &token_budget, || Ok(()))
        .unwrap();
    assert_eq!(output.tokens()[0].term(), "韓🙂");
    assert_eq!(output.tokens()[0].offsets().unwrap().utf8, 3..10);
    drop(filtered);
    #[cfg(feature = "nori")]
    assert!(source_budget.used() > 0);
    #[cfg(not(feature = "nori"))]
    assert_eq!(source_budget.used(), 0);
    let shared = output.into_shared().unwrap();
    let retained = token_budget.used();
    let other = shared.clone();
    drop(shared);
    assert_eq!(token_budget.used(), retained);
    assert_eq!(other.tokens()[0].term(), "韓🙂");
    drop(other);
    assert_eq!(source_budget.used(), 0);
    assert_eq!(token_budget.used(), 0);
}

#[test]
fn failed_borrowed_input_calls_do_not_publish_new_coordinate_reservations() {
    let filtered = uqa_analysis::FilteredText::new("한🙂 repeated source");
    let baseline = MemoryBudget::new(1 << 20);
    let mut polls = 0;
    let expected = Tokenizer::Keyword
        .tokenize_mapped_budgeted(&filtered, &baseline, || {
            polls += 1;
            Ok(())
        })
        .unwrap();
    let peak = baseline.peak();
    drop(expected);
    assert_eq!(baseline.used(), 0);
    for stop in 1..=polls {
        let budget = MemoryBudget::new(1 << 20);
        let mut count = 0;
        assert!(matches!(
            Tokenizer::Keyword.tokenize_mapped_budgeted(&filtered, &budget, || {
                count += 1;
                if count == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(AnalysisError::Cancelled)
        ));
        assert_eq!(budget.used(), 0, "poll {stop}");
    }
    for limit in (0..peak).step_by(32) {
        let budget = MemoryBudget::new(limit);
        let result = Tokenizer::Keyword.tokenize_mapped_budgeted(&filtered, &budget, || Ok(()));
        assert!(matches!(
            result,
            Err(AnalysisError::Memory(MemoryError::Limit { .. }))
        ));
        assert_eq!(budget.used(), 0, "limit {limit}");
    }
}

#[cfg(feature = "nori")]
#[path = "tokenizer_memory/korean.rs"]
mod korean;
