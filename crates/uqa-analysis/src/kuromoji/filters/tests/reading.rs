//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::kuromoji::tokenizer::tests::model;
use crate::kuromoji::{JapaneseAnalyzer, JapaneseFilter, KuromojiLimits, KuromojiOptions};
use crate::{AnalysisError, AnalysisResult, CharFilter, Tokenizer};
use uqa_core::memory::MemoryBudget;

#[test]
fn japanese_readings_preserve_source_ownership_and_leave_normalization_independent() {
    let model = model();
    let default: JapaneseFilter =
        serde_json::from_str(r#"{"type":"kuromoji_readingform"}"#).unwrap();
    assert_eq!(default, JapaneseFilter::ReadingForm { use_romaji: false });
    for (use_romaji, expected) in [(false, "ヒラガナ"), (true, "hiragana")] {
        let filter = JapaneseFilter::ReadingForm { use_romaji };
        let encoded = serde_json::to_string(&filter).unwrap();
        assert_eq!(
            serde_json::from_str::<JapaneseFilter>(&encoded).unwrap(),
            filter
        );
        let source_budget = MemoryBudget::new(1 << 20);
        let source = CharFilter::HTMLStrip
            .filter_with_offsets_budgeted("<b>ひらがな</b>", &source_budget, &mut || Ok(()))
            .unwrap();
        let output = filter
            .filter_analyzed(
                Tokenizer::Whitespace.tokenize_mapped(&source).unwrap(),
                &model,
            )
            .unwrap();
        let token = &output.tokens()[0];
        assert_eq!(token.term(), expected);
        assert_eq!(token.offsets().unwrap().utf16, 3..7);
        assert_eq!(token.offsets().unwrap().utf8, 3..15);
        assert_eq!(token.position_increment(), 1);
        assert_eq!(token.position_length(), 1);
        assert!(token.japanese_morphology().is_none());
        drop(source);
        assert!(source_budget.used() > 0);
        drop(output);
        assert_eq!(source_budget.used(), 0);
        let analyzer = JapaneseAnalyzer::with_filters(
            model.clone(),
            None,
            KuromojiOptions::default(),
            &[filter],
        )
        .unwrap();
        assert_eq!(
            analyzer.normalize("ひらがな ＵＱＡ").unwrap(),
            "ひらがな uqa"
        );
    }
}

#[test]
fn japanese_reading_expansion_obeys_output_limits_and_unwinds_both_passes() {
    let model = model();
    for (use_romaji, replacement) in [(false, "キ"), (true, "ki")] {
        let filter = JapaneseFilter::ReadingForm { use_romaji };
        let text = "き".repeat(2049);
        let source = Tokenizer::Keyword.tokenize_with_offsets(&text).unwrap();
        let limits = KuromojiLimits {
            max_output_utf16: replacement.encode_utf16().count() * 2049,
            ..KuromojiLimits::default()
        };
        let run = |budget: &MemoryBudget,
                   limits: KuromojiLimits,
                   poll: &mut dyn FnMut() -> AnalysisResult<()>| {
            let input = source.clone_budgeted(budget, &mut *poll)?;
            filter.filter_analyzed_budgeted(input, &model, limits, &mut || poll())
        };
        let baseline = MemoryBudget::new(usize::MAX);
        let mut polls = 0;
        let expected = run(&baseline, limits, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(
            expected.tokens()[0].term(),
            replacement.repeat(2049).as_str()
        );
        assert_eq!(expected.tokens()[0].offsets().unwrap().utf16, 0..2049);
        assert_eq!(source.tokens()[0].term(), text.as_str());
        for stop in 1..=polls {
            let budget = MemoryBudget::new(baseline.peak() + 7);
            let held = budget.reserve(7).unwrap();
            let mut calls = 0;
            let result = run(&budget, limits, &mut || {
                calls += 1;
                if calls == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(result, Err(AnalysisError::Cancelled)),
                "{use_romaji} poll {stop}/{polls}"
            );
            assert_eq!(calls, stop);
            assert_eq!(budget.used(), 7);
            drop(held);
        }
        for allowance in [0, 1, baseline.peak() / 2, baseline.peak() - 1] {
            let budget = MemoryBudget::new(allowance + 7);
            let held = budget.reserve(7).unwrap();
            match run(&budget, limits, &mut || Ok(())) {
                Ok(actual) => {
                    assert_eq!(*actual, *expected);
                    assert!(budget.peak() <= allowance + 7);
                }
                Err(AnalysisError::Memory(_)) => {}
                Err(error) => panic!("{error}"),
            }
            assert_eq!(budget.used(), 7);
            drop(held);
        }
        let budget = MemoryBudget::new(baseline.peak() + 7);
        let held = budget.reserve(7).unwrap();
        assert!(matches!(
            run(
                &budget,
                KuromojiLimits {
                    max_output_utf16: limits.max_output_utf16 - 1,
                    ..limits
                },
                &mut || Ok(())
            ),
            Err(AnalysisError::KuromojiDictionary(_))
        ));
        assert_eq!(budget.used(), 7);
        drop(held);
        drop(expected);
        assert_eq!(baseline.used(), 0);
        drop(run(&baseline, limits, &mut || Ok(())).unwrap());
        assert_eq!(baseline.used(), 0);
    }
}
