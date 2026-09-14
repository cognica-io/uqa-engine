//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{
    render::{render, Span},
    *,
};
use super::reference;
use crate::AnalysisError;
use uqa_core::memory::{BudgetedVec, MemoryError};

fn spans(text: &str, ranges: &[(usize, usize)], budget: &MemoryBudget) -> BudgetedVec<Span> {
    let points: Vec<_> = text
        .char_indices()
        .map(|(byte, _)| byte)
        .chain([text.len()])
        .collect();
    let mut spans = BudgetedVec::new(budget);
    spans.reserve(ranges.len()).unwrap();
    for &(start, end) in ranges {
        spans.push(Span::new(points[start], points[end])).unwrap();
    }
    spans
}

#[test]
fn source_windows_density_ties_and_markers_match_the_allocating_reference() {
    let mut checked = 0;
    for seed in 0..256usize {
        let alphabet = ['a', ' ', '한', '🙂', '\t', 'Σ', '\u{2003}', '\u{301}'];
        let mut state = seed as u64 + 1;
        let length = seed % 97;
        let mut text = String::new();
        let mut ranges = Vec::new();
        for index in 0..length {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            text.push(alphabet[(state >> 32) as usize % alphabet.len()]);
            if state.is_multiple_of(5) {
                ranges.push((index, index + 1));
            }
        }
        // Include long selected matches and clusters separated by overlapping context windows.
        if seed % 7 == 0 && length > 12 {
            ranges = vec![(1, 10), (11, 12), (length - 1, length)];
            if length == 13 {
                ranges.pop();
            }
        }
        for fragment_size in [0, 1, 2, 3, 10, 29, 30, 31, 60, usize::MAX] {
            for max_fragments in [0, 1, 2, 3, usize::MAX] {
                let opts = HighlightOptions {
                    start_tag: "〈🙂".into(),
                    end_tag: "〉".into(),
                    max_fragments,
                    fragment_size,
                };
                let expected = reference::render_highlights(&text, &ranges, &opts);
                let budget = MemoryBudget::new(1 << 20);
                let input = spans(&text, &ranges, &budget);
                let output = render(&text, input, &opts, &budget, &mut || Ok(())).unwrap();
                assert_eq!(
                    *output, expected,
                    "seed={seed}, fragments={max_fragments}, size={fragment_size}"
                );
                assert_eq!(budget.used(), output.reserved_bytes());
                assert_eq!(output.reserved_bytes(), output.capacity());
                drop(output);
                assert_eq!(budget.used(), 0);
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 12_800);
}

#[test]
fn window_rendering_needs_no_full_source_character_table_or_fragment_copies() {
    let text = format!("{}한🙂{}", "x".repeat(100_000), "y".repeat(100_000));
    let opts = HighlightOptions {
        max_fragments: 1,
        fragment_size: 2,
        ..Default::default()
    };
    let budget = MemoryBudget::new(512);
    let other = budget.reserve(7).unwrap();
    let input = spans(&text, &[(100_000, 100_002)], &budget);
    let output = render(&text, input, &opts, &budget, &mut || Ok(())).unwrap();
    assert_eq!(*output, "...<b>한🙂</b>...");
    assert_eq!(budget.used(), output.capacity() + 7);
    assert!(budget.peak() <= 512);
    drop(output);
    assert_eq!(budget.used(), 7);
    drop(other);
}

#[test]
fn renderer_limits_and_every_callback_failure_preserve_independent_owners() {
    let text = "앞 fox 뒤쪽 text more foo bar words 끝 fox 마지막";
    let ranges = [(2, 5), (34, 37)];
    for opts in [
        HighlightOptions::default(),
        HighlightOptions {
            max_fragments: 2,
            fragment_size: 10,
            ..Default::default()
        },
    ] {
        let baseline = MemoryBudget::new(1 << 20);
        let mut polls = 0;
        let expected = render(
            text,
            spans(text, &ranges, &baseline),
            &opts,
            &baseline,
            &mut || {
                polls += 1;
                Ok(())
            },
        )
        .unwrap();
        let peak = baseline.peak();
        let initial = ranges.len() * size_of::<Span>();
        for allowance in initial..=peak {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            match render(
                text,
                spans(text, &ranges, &budget),
                &opts,
                &budget,
                &mut || Ok(()),
            ) {
                Ok(output) => assert_eq!(*output, *expected),
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {}
                output => panic!("allowance={allowance}: {output:?}"),
            }
            assert_eq!(budget.used(), 7);
            assert!(budget.peak() <= budget.limit());
            drop(other);
        }
        for stop in 1..=polls {
            let budget = MemoryBudget::new(1 << 20);
            let other = budget.reserve(7).unwrap();
            let mut count = 0;
            let output = render(
                text,
                spans(text, &ranges, &budget),
                &opts,
                &budget,
                &mut || {
                    count += 1;
                    if count == stop {
                        Err(AnalysisError::Cancelled)
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(
                matches!(output, Err(AnalysisError::Cancelled)),
                "callback={stop}"
            );
            assert_eq!(budget.used(), 7);
            drop(other);
        }
    }
}

#[test]
fn complete_and_word_scanning_entry_points_preserve_reference_text() {
    let configurations = [
        None,
        Some(crate::standard_analyzer("english")),
        Some(Analyzer::new(
            crate::Tokenizer::Keyword,
            Vec::new(),
            Vec::new(),
        )),
        Some(Analyzer::new(
            crate::Tokenizer::NGram {
                min_gram: 1,
                max_gram: 3,
            },
            Vec::new(),
            vec![crate::CharFilter::HTMLStrip],
        )),
    ];
    for analyzer in &configurations {
        for text in [
            "",
            "c++",
            "running runs quickly",
            "ΣΟΣ AΣ’Α",
            "<b>한&amp;🙂</b> 끝",
            "a\u{200d}b\0a_b 42",
            "one two three four five six seven eight nine ten",
        ] {
            for query in [
                vec![],
                vec![String::new()],
                vec!["runs".into(), "RUNS".into()],
                vec![text.into()],
                vec!["a".into(), "b".into(), "five".into(), "eight".into()],
            ] {
                for opts in [
                    HighlightOptions::default(),
                    HighlightOptions {
                        max_fragments: 2,
                        fragment_size: 5,
                        ..Default::default()
                    },
                ] {
                    let expected =
                        reference::highlight(text, &query, analyzer.as_ref(), &opts).unwrap();
                    assert_eq!(
                        highlight(text, &query, analyzer.as_ref(), &opts).unwrap(),
                        expected
                    );
                    let expected =
                        reference::highlight_words(text, &query, analyzer.as_ref(), &opts).unwrap();
                    assert_eq!(
                        highlight_words(text, &query, analyzer.as_ref(), &opts).unwrap(),
                        expected
                    );
                }
            }
        }
    }
}
