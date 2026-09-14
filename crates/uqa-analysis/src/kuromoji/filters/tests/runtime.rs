//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::kuromoji::tokenizer::tests::model;
use crate::kuromoji::{
    JapaneseAnalyzer, JapaneseFilter, KuromojiLimits, KuromojiMode, KuromojiOptions,
};
use crate::{AnalysisError, AnalysisResult};
use uqa_core::memory::MemoryBudget;

#[test]
fn japanese_analysis_and_normalization_unwind_partial_buffers_and_remain_reusable() {
    let analyzer = JapaneseAnalyzer::new(model(), None, KuromojiMode::Search).unwrap();
    let text = "ｶﾞｯﾂﾎﾟｰｽﾞで走りました ＵＱＡ İ";
    for normalize in [false, true] {
        let baseline = MemoryBudget::new(usize::MAX);
        let mut polls = 0;
        let run = |budget: &MemoryBudget,
                   limits: KuromojiLimits,
                   poll: &mut dyn FnMut() -> AnalysisResult<()>| {
            let mut poll = || poll();
            if normalize {
                analyzer
                    .normalize_budgeted(text, limits, budget, &mut poll)
                    .map(drop)
            } else {
                analyzer
                    .analyze_budgeted(text, limits, budget, &mut poll)
                    .map(drop)
            }
        };
        run(&baseline, KuromojiLimits::default(), &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(baseline.used(), 0);
        for stop in [1, 2, polls / 4, polls / 2, polls - 1, polls] {
            let budget = MemoryBudget::new(baseline.peak() + 7);
            let held = budget.reserve(7).unwrap();
            let mut calls = 0;
            let result = run(&budget, KuromojiLimits::default(), &mut || {
                calls += 1;
                if calls == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(result, Err(AnalysisError::Cancelled)),
                "normalization {normalize}: poll {stop}/{polls}"
            );
            assert_eq!(calls, stop);
            assert_eq!(budget.used(), 7);
            drop(held);
        }
        for allowance in [0, 1, baseline.peak() / 2, baseline.peak() - 1] {
            let budget = MemoryBudget::new(allowance + 7);
            let held = budget.reserve(7).unwrap();
            match run(&budget, KuromojiLimits::default(), &mut || Ok(())) {
                Ok(()) => assert!(budget.peak() <= allowance + 7),
                Err(AnalysisError::Memory(_)) => {}
                Err(error) => panic!("{error}"),
            }
            assert_eq!(budget.used(), 7);
            drop(held);
        }
        for limits in [
            KuromojiLimits {
                max_input_utf16: 0,
                ..KuromojiLimits::default()
            },
            KuromojiLimits {
                max_output_utf16: 0,
                ..KuromojiLimits::default()
            },
        ] {
            let budget = MemoryBudget::new(baseline.peak() + 7);
            let held = budget.reserve(7).unwrap();
            assert!(matches!(
                run(&budget, limits, &mut || Ok(())),
                Err(AnalysisError::KuromojiDictionary(_))
            ));
            assert_eq!(budget.used(), 7);
            drop(held);
        }
        run(&baseline, KuromojiLimits::default(), &mut || Ok(())).unwrap();
        assert_eq!(baseline.used(), 0);
    }
    let limits = KuromojiLimits {
        max_tokens: 0,
        max_lattice_positions: 0,
        max_lattice_candidates: 0,
        max_n_best_work: 0,
        ..KuromojiLimits::default()
    };
    assert_eq!(
        *analyzer
            .normalize_budgeted(
                "ＵＱＡです",
                limits,
                &MemoryBudget::new(1 << 20),
                &mut || Ok(())
            )
            .unwrap(),
        "uqaです"
    );
}

#[test]
fn japanese_filter_preparation_is_bounded_cancelled_and_separately_retained() {
    let model = model();
    let filters = [
        JapaneseFilter::PartOfSpeech { stop_tags: None },
        JapaneseFilter::Stop {
            words: Some(vec!["ABC".repeat(1024), "abc".repeat(1024), String::new()]),
            ignore_case: true,
        },
    ];
    let baseline = MemoryBudget::new(usize::MAX);
    let mut polls = 0;
    let analyzer = JapaneseAnalyzer::with_filters_budgeted(
        model.clone(),
        None,
        KuromojiOptions::default(),
        &filters,
        KuromojiLimits::default(),
        &baseline,
        &mut || {
            polls += 1;
            Ok(())
        },
    )
    .unwrap();
    assert!(baseline.used() > 0);
    let peak = baseline.peak();
    drop(analyzer);
    assert_eq!(baseline.used(), 0);
    for stop in [1, 2, polls / 4, polls / 2, polls - 1, polls] {
        let budget = MemoryBudget::new(peak + 7);
        let held = budget.reserve(7).unwrap();
        let mut calls = 0;
        let result = JapaneseAnalyzer::with_filters_budgeted(
            model.clone(),
            None,
            KuromojiOptions::default(),
            &filters,
            KuromojiLimits::default(),
            &budget,
            &mut || {
                calls += 1;
                if calls == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(result, Err(AnalysisError::Cancelled)));
        assert_eq!(calls, stop);
        assert_eq!(budget.used(), 7);
        drop(held);
    }
    for limits in [
        KuromojiLimits {
            max_filter_entries: 1,
            ..KuromojiLimits::default()
        },
        KuromojiLimits {
            max_filter_utf16: 1,
            ..KuromojiLimits::default()
        },
    ] {
        let budget = MemoryBudget::new(peak + 7);
        let held = budget.reserve(7).unwrap();
        assert!(matches!(
            JapaneseAnalyzer::with_filters_budgeted(
                model.clone(),
                None,
                KuromojiOptions::default(),
                &filters,
                limits,
                &budget,
                &mut || Ok(())
            ),
            Err(AnalysisError::KuromojiDictionary(_))
        ));
        assert_eq!(budget.used(), 7);
        drop(held);
    }
    let zero = MemoryBudget::new(0);
    assert!(matches!(
        JapaneseAnalyzer::with_filters_budgeted(
            model,
            None,
            KuromojiOptions::default(),
            &filters,
            KuromojiLimits::default(),
            &zero,
            &mut || Ok(())
        ),
        Err(AnalysisError::Memory(_))
    ));
    assert_eq!(zero.used(), 0);
}
