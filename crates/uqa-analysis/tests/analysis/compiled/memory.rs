//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_analysis::{AnalysisResult, AnalyzedText};
use uqa_core::memory::{Budgeted, MemoryBudget, MemoryError};

fn pipeline() -> Analyzer {
    Analyzer::new(
        Tokenizer::Pattern {
            pattern: "\\s+".into(),
        },
        vec![
            TokenFilter::Lowercase,
            TokenFilter::ASCIIFolding,
            TokenFilter::Stop {
                language: "english".into(),
                custom_words: Vec::new(),
            },
            TokenFilter::PorterStem,
            TokenFilter::Synonym {
                synonyms: BTreeMap::from([("hello".into(), vec!["hi".into(), "greeting".into()])]),
                synonyms_path: None,
            },
        ],
        vec![
            CharFilter::HTMLStrip,
            CharFilter::PatternReplace {
                pattern: "猫".into(),
                replacement: "CATS".into(),
            },
        ],
    )
}

fn run(
    analyzer: &Analyzer,
    compiled: &CompiledAnalyzer,
    frozen: bool,
    source: &str,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<AnalyzedText>> {
    if frozen {
        compiled.analyze_tokens_budgeted(source, budget, poll)
    } else {
        analyzer.analyze_tokens_budgeted(source, budget, poll)
    }
}

#[test]
fn complete_pipeline_retains_offsets_gaps_and_shared_output_ownership() {
    let analyzer = pipeline();
    let compiled = analyzer.compile().unwrap();
    for frozen in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let source = "<b>HÉLLO 猫</b> and".to_owned();
        let output = run(&analyzer, &compiled, frozen, &source, &budget, &mut || {
            Ok(())
        })
        .unwrap();
        assert_eq!(
            output
                .tokens()
                .iter()
                .map(|token| token.term().as_str().unwrap())
                .collect::<Vec<_>>(),
            ["hello", "hi", "greeting", "cat"]
        );
        for (index, token) in output.tokens().iter().enumerate() {
            assert_eq!(
                token.offsets().unwrap().utf8,
                if index == 3 { 10..13 } else { 3..9 }
            );
            assert_eq!(
                token.position_increment(),
                u32::from(index == 0 || index == 3)
            );
            assert_eq!(token.position_length(), 1);
        }
        assert_eq!(output.final_offsets().utf8.end, source.len());
        assert_eq!(output.final_position_increment(), 1);
        assert!(budget.used() >= output.reserved_bytes());
        let first = output.into_shared().unwrap();
        let second = first.clone();
        let retained = budget.used();
        drop(source);
        drop(first);
        assert_eq!(budget.used(), retained);
        assert_eq!(second.tokens()[3].term(), "cat");
        drop(second);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn whole_pipeline_byte_limits_unwind_character_token_and_expansion_buffers() {
    let analyzer = pipeline();
    let compiled = analyzer.compile().unwrap();
    let source = "<b>HÉLLO 猫</b> and";
    for frozen in [false, true] {
        let baseline = MemoryBudget::new(1 << 20);
        let expected = run(&analyzer, &compiled, frozen, source, &baseline, &mut || {
            Ok(())
        })
        .unwrap();
        let peak = baseline.peak();
        let mut failures = 0;
        for allowance in (0..peak).step_by(113).chain([peak]) {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            match run(
                &analyzer,
                &compiled,
                frozen,
                source,
                &budget,
                &mut || Ok(()),
            ) {
                Ok(output) => assert_eq!(*output, *expected),
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => failures += 1,
                output => panic!("frozen={frozen}, allowance={allowance}: {output:?}"),
            }
            assert_eq!(budget.used(), 7);
            assert!(budget.peak() <= budget.limit());
            drop(other);
        }
        assert!(failures > 2);
        let exact = MemoryBudget::new(peak);
        let output = run(&analyzer, &compiled, frozen, source, &exact, &mut || Ok(())).unwrap();
        assert_eq!(*output, *expected);
        drop(output);
        assert_eq!(exact.used(), 0);
    }
}

#[test]
fn cancellation_at_every_pipeline_callback_leaves_only_unrelated_ownership() {
    let analyzer = pipeline();
    let compiled = analyzer.compile().unwrap();
    let source = "<b>HÉLLO 猫</b> and";
    for frozen in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let other = budget.reserve(7).unwrap();
        let mut polls = 0;
        let output = run(&analyzer, &compiled, frozen, source, &budget, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
        drop(output);
        assert!(polls > 20);
        for stop in 1..=polls {
            let mut calls = 0;
            let output = run(&analyzer, &compiled, frozen, source, &budget, &mut || {
                calls += 1;
                if calls == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(output, Err(AnalysisError::Cancelled)),
                "frozen={frozen}, callback={stop}: {output:?}"
            );
            assert_eq!(budget.used(), 7);
        }
        drop(other);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn budgeted_execution_keeps_compiled_synonyms_fixed_and_uncompiled_reload_failures_atomic() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synonyms.txt");
    std::fs::write(&path, "cat => feline\n").unwrap();
    let analyzer = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Synonym {
            synonyms: BTreeMap::new(),
            synonyms_path: Some(path.clone()),
        }],
        vec![CharFilter::HTMLStrip],
    );
    let compiled = analyzer.compile().unwrap();
    std::fs::write(&path, "cat => animal\n").unwrap();
    let budget = MemoryBudget::new(1 << 20);
    let other = budget.reserve(7).unwrap();
    for (frozen, expected) in [(false, "animal"), (true, "feline")] {
        let output = run(
            &analyzer,
            &compiled,
            frozen,
            "<b>cat</b>",
            &budget,
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(output.tokens()[1].term(), expected);
        drop(output);
        assert_eq!(budget.used(), 7);
    }
    std::fs::remove_file(path).unwrap();
    let output = analyzer.analyze_tokens_budgeted("<b>cat</b>", &budget, || Ok(()));
    assert!(matches!(output, Err(AnalysisError::SynonymFile(_))));
    assert_eq!(budget.used(), 7);
    let output = compiled
        .analyze_tokens_budgeted("<b>cat</b>", &budget, || Ok(()))
        .unwrap();
    assert_eq!(output.tokens()[1].term(), "feline");
    drop(output);
    assert_eq!(budget.used(), 7);
    drop(other);
}
