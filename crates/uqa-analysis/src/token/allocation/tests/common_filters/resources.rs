//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn common_filter_limits_and_every_cancellation_release_only_the_consumed_owner() {
    let filters = [
        TokenFilter::Lowercase,
        TokenFilter::ASCIIFolding,
        TokenFilter::PorterStem,
        stop(&["the", "ab", "ÀΟΣ"]),
        TokenFilter::Length {
            min_length: 5,
            max_length: 0,
        },
        synonyms(),
        TokenFilter::Ngram {
            min_gram: 2,
            max_gram: 3,
            keep_short: false,
        },
        TokenFilter::EdgeNgram {
            min_gram: 4,
            max_gram: 5,
        },
    ];
    let original = input();
    for filter in &filters {
        let baseline = MemoryBudget::new(1 << 20);
        let input = original.clone_budgeted(&baseline, || Ok(())).unwrap();
        let mut calls = 0;
        let expected = apply(filter, input, &mut || {
            calls += 1;
            Ok(())
        })
        .unwrap();
        let peak = baseline.peak();
        let mut succeeded = false;
        for allowance in 0..=peak {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            let result = original
                .clone_budgeted(&budget, || Ok(()))
                .and_then(|input| apply(filter, input, &mut || Ok(())));
            match result {
                Ok(output) => {
                    assert_eq!(*output, *expected, "{filter:?}, allowance {allowance}");
                    assert_owned(output, &budget, 7);
                    succeeded = true;
                }
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {}
                result => panic!("{filter:?}, allowance {allowance}: {result:?}"),
            }
            assert_eq!(budget.used(), 7);
            assert!(budget.peak() <= budget.limit());
            drop(other);
        }
        assert!(succeeded, "{filter:?}");
        for stop in 1..=calls {
            let budget = MemoryBudget::new(1 << 20);
            let other = budget.reserve(7).unwrap();
            let input = original.clone_budgeted(&budget, || Ok(())).unwrap();
            let mut count = 0;
            let result = apply(filter, input, &mut || {
                count += 1;
                if count == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(result, Err(AnalysisError::Cancelled)),
                "{filter:?}, callback {stop}: {result:?}"
            );
            assert_eq!(budget.used(), 7);
            drop(other);
        }
    }
}

#[test]
fn removal_compacts_existing_capacity_and_preserves_upstream_terminal_precedence() {
    let mut original = input();
    original.batch.terminal = Some(Box::new(original.batch.tokens[2].clone()));
    let budget = MemoryBudget::new(1 << 20);
    let input = original.clone_budgeted(&budget, || Ok(())).unwrap();
    let vector = input.batch.tokens.as_ptr();
    let capacity = input.batch.tokens.capacity();
    let term = input.batch.tokens[2].term.as_str().unwrap().as_ptr();
    let terminal = input.batch.terminal.as_deref().unwrap() as *const AnalysisToken;
    let old_bytes = input.reserved_bytes();
    let blocker = budget.reserve(budget.limit() - budget.used()).unwrap();
    let output = apply(
        &stop(&["UQA", "the", "ab", "Àrunning", "ÀΟΣ"]),
        input,
        &mut || Ok(()),
    )
    .unwrap();
    assert_eq!(output.batch.tokens.as_ptr(), vector);
    assert_eq!(output.batch.tokens.capacity(), capacity);
    assert_eq!(output.batch.tokens[0].term.as_str().unwrap().as_ptr(), term);
    assert_eq!(
        std::ptr::from_ref(output.batch.terminal.as_deref().unwrap()),
        terminal
    );
    assert_eq!(output.batch.tokens[0].position_increment, 4);
    assert!(output.reserved_bytes() < old_bytes);
    assert_owned(output, &budget, blocker.bytes());
}

#[test]
fn removing_every_token_releases_vector_before_allocating_the_terminal_box() {
    let mut original = Tokenizer::Whitespace
        .tokenize_with_offsets("a b c")
        .unwrap();
    original.batch.tokens[2].position_increment = 0;
    let expected_terminal = original.batch.tokens[2].clone();
    let budget = MemoryBudget::new(1 << 20);
    let input = original.clone_budgeted(&budget, || Ok(())).unwrap();
    let blocker = budget.reserve(budget.limit() - budget.used()).unwrap();
    let output = apply(&stop(&["a", "b", "c"]), input, &mut || Ok(())).unwrap();
    assert!(output.tokens().is_empty());
    assert_eq!(output.batch.tokens.capacity(), 0);
    assert_eq!(output.batch.terminal.as_deref(), Some(&expected_terminal));
    assert_eq!(output.final_position_increment(), 2);
    assert_eq!(output.reserved_bytes(), size_of::<AnalysisToken>() + 1);
    assert_owned(output, &budget, blocker.bytes());
}

#[test]
fn zero_increment_trailing_removal_remains_available_to_downstream_filters() {
    for filter in [
        stop(&["x"]),
        TokenFilter::EdgeNgram {
            min_gram: 3,
            max_gram: 3,
        },
    ] {
        let mut original = Tokenizer::Whitespace
            .tokenize_with_offsets("UQA x")
            .unwrap();
        original.batch.tokens[1].position_increment = 0;
        let terminal = original.batch.tokens[1].clone();
        let budget = MemoryBudget::new(1 << 20);
        let input = original.clone_budgeted(&budget, || Ok(())).unwrap();
        let output = apply(&filter, input, &mut || Ok(())).unwrap();
        assert_eq!(output.batch.terminal.as_deref(), Some(&terminal));
        assert_eq!(output.final_position_increment(), 0);
        assert_owned(output, &budget, 0);
    }
}

#[test]
fn no_expansion_and_whole_grams_transfer_buffers_with_a_full_allowance() {
    for filter in [
        TokenFilter::Synonym {
            synonyms: std::collections::BTreeMap::new(),
            synonyms_path: None,
        },
        TokenFilter::Ngram {
            min_gram: 3,
            max_gram: 8,
            keep_short: true,
        },
        TokenFilter::EdgeNgram {
            min_gram: 3,
            max_gram: 8,
        },
    ] {
        let original = Tokenizer::Whitespace
            .tokenize_with_offsets("UQA 韓🙂a")
            .unwrap();
        let budget = MemoryBudget::new(1 << 20);
        let input = original.clone_budgeted(&budget, || Ok(())).unwrap();
        let vector = input.batch.tokens.as_ptr();
        let term = input.batch.tokens[0].term.as_str().unwrap().as_ptr();
        let bytes = input.reserved_bytes();
        let blocker = budget.reserve(budget.limit() - budget.used()).unwrap();
        let output = apply(&filter, input, &mut || Ok(())).unwrap();
        assert_eq!(*output, original);
        assert_eq!(output.batch.tokens.as_ptr(), vector);
        assert_eq!(output.batch.tokens[0].term.as_str().unwrap().as_ptr(), term);
        assert_eq!(output.reserved_bytes(), bytes);
        assert_owned(output, &budget, blocker.bytes());
    }
}

#[test]
fn replacement_keeps_original_capacity_until_changed_bytes_are_dropped() {
    for (source, changes) in [("uqa", false), ("UQA", true)] {
        let mut original = Tokenizer::Keyword.tokenize_with_offsets(source).unwrap();
        let mut text = String::with_capacity(8192);
        text.push_str(source);
        original.batch.tokens[0].term = text.into();
        let budget = MemoryBudget::new(1 << 20);
        let bytes = original.batch.allocation_bytes(&mut || Ok(())).unwrap();
        let input = Budgeted::new(original, budget.reserve(bytes).unwrap());
        let pointer = input.batch.tokens[0].term.as_str().unwrap().as_ptr();
        let mut minimum_seen = usize::MAX;
        let output = apply(&TokenFilter::Lowercase, input, &mut || {
            minimum_seen = minimum_seen.min(budget.used());
            Ok(())
        })
        .unwrap();
        assert!(budget.peak() >= bytes + 3);
        assert_eq!(output.tokens()[0].term.as_str(), Some("uqa"));
        if changes {
            assert_ne!(
                output.batch.tokens[0].term.as_str().unwrap().as_ptr(),
                pointer
            );
            assert!(!output.batch.tokens[0].verbatim);
            assert!(output.reserved_bytes() < bytes);
        } else {
            assert_eq!(
                output.batch.tokens[0].term.as_str().unwrap().as_ptr(),
                pointer
            );
            assert!(output.batch.tokens[0].verbatim);
            assert_eq!(output.reserved_bytes(), bytes);
            assert!(minimum_seen >= bytes);
        }
        assert_owned(output, &budget, 0);
    }
}

#[test]
fn consuming_input_releases_exhausted_capacity_while_moved_tokens_keep_their_leases() {
    let original = attributes();
    let budget = MemoryBudget::new(1 << 20);
    let input = original.clone_budgeted(&budget, || Ok(())).unwrap();
    let bytes = input.reserved_bytes();
    let vector_bytes = input.batch.tokens.capacity() * size_of::<AnalysisToken>();
    let terminal = input.batch.terminal.as_deref().unwrap() as *const AnalysisToken;
    let (input, memory) = input.into_parts();
    let input =
        TokenBatchAllocation::from_budgeted(Budgeted::new(input.batch, memory)).into_input();
    let (first, input) = input.next(&mut || Ok(())).unwrap();
    let first = first.unwrap();
    assert_eq!(budget.used(), bytes);
    let (last, input) = input.next(&mut || Ok(())).unwrap();
    let last = last.unwrap();
    assert_eq!(budget.used(), bytes - vector_bytes);
    let (end, input) = input.next(&mut || Ok(())).unwrap();
    assert!(end.is_none());
    let (output_terminal, increment) = input.finish();
    let output_terminal = output_terminal.unwrap();
    assert_eq!(std::ptr::from_ref(&**output_terminal), terminal);
    assert_eq!(increment, original.final_position_increment());
    assert_eq!(
        budget.used(),
        first.reserved_bytes() + last.reserved_bytes() + output_terminal.reserved_bytes()
    );
    drop((first, last, output_terminal));
    assert_eq!(budget.used(), 0);
}

#[test]
fn position_overflow_unwinds_transferred_and_retained_buffers() {
    for filter in [
        stop(&["a"]),
        TokenFilter::Ngram {
            min_gram: 3,
            max_gram: 4,
            keep_short: false,
        },
    ] {
        let mut original = Tokenizer::Whitespace
            .tokenize_with_offsets("a word")
            .unwrap();
        original.batch.tokens[0].position_increment = u32::MAX;
        original.batch.tokens[1].position_increment = 2;
        let budget = MemoryBudget::new(1 << 20);
        let bytes = original.batch.allocation_bytes(&mut || Ok(())).unwrap();
        let input = Budgeted::new(original, budget.reserve(bytes).unwrap());
        let result = apply(&filter, input, &mut || Ok(()));
        assert!(matches!(result, Err(AnalysisError::TokenPositionOverflow)));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn long_common_filter_loops_poll_and_release_the_owned_input_on_cancellation() {
    let term = "韓🙂a".repeat(4096);
    let original = Tokenizer::Keyword.tokenize_with_offsets(&term).unwrap();
    for filter in [
        stop(&[&term]),
        TokenFilter::Length {
            min_length: 1,
            max_length: 0,
        },
        TokenFilter::Synonym {
            synonyms: [(term.clone(), vec!["UQA".into()])].into(),
            synonyms_path: None,
        },
        TokenFilter::EdgeNgram {
            min_gram: 1,
            max_gram: 2,
        },
    ] {
        let budget = MemoryBudget::new(1 << 22);
        let input = original.clone_budgeted(&budget, || Ok(())).unwrap();
        let mut calls = 0;
        let output = apply(&filter, input, &mut || {
            calls += 1;
            Ok(())
        })
        .unwrap();
        assert!(calls >= 12, "{filter:?}");
        drop(output);
        for stop in 1..=calls {
            let input = original.clone_budgeted(&budget, || Ok(())).unwrap();
            let mut count = 0;
            let result = apply(&filter, input, &mut || {
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
    }
}
