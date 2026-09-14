//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn diagnostics_preserve_the_complete_typed_result_and_revision() {
    let mut analyzers = vec![
        crate::keyword_analyzer(),
        crate::standard_analyzer("english"),
    ];
    if let Ok(nori) = crate::get_analyzer("nori") {
        analyzers.push(nori);
    }
    for analyzer in analyzers {
        let analyzer = analyzer.compile().unwrap();
        for input in ["", "The cats and", "😀나물은 漢字 \"\\\n\0"] {
            let mut expected =
                serde_json::to_value(analyzer.analyze_tokens(input).unwrap()).unwrap();
            expected["analyzer_fingerprint"] =
                serde_json::to_value(analyzer.descriptor().fingerprint()).unwrap();
            let budget = MemoryBudget::new(16 * 1024 * 1024);
            let actual = analyzer
                .analyze_diagnostic_budgeted(input, &budget, || Ok(()))
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&actual).unwrap(),
                expected
            );
            assert_eq!(budget.used(), actual.capacity());
            assert_eq!(actual.reserved_bytes(), actual.capacity());
            drop(actual);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn encoding_retains_analysis_and_releases_all_reservations_on_failure() {
    let analyzer = crate::keyword_analyzer().compile().unwrap();
    let input = "\0\"\\\n".repeat(1024);
    let budget = MemoryBudget::new(16 * 1024 * 1024);
    let mut analysis_polls = 0_usize;
    let analysis = analyzer
        .analyze_tokens_budgeted(&input, &budget, || {
            analysis_polls += 1;
            Ok(())
        })
        .unwrap();
    let analysis_peak = budget.peak();
    drop(analysis);
    let mut total_polls = 0_usize;
    let diagnostic = analyzer
        .analyze_diagnostic_budgeted(&input, &budget, || {
            total_polls += 1;
            Ok(())
        })
        .unwrap();
    let peak = budget.peak();
    assert!(
        peak > analysis_peak,
        "encoding must reserve its output alongside the token stream"
    );
    assert!(total_polls > analysis_polls + 2);
    drop(diagnostic);
    assert_eq!(budget.used(), 0);

    let limited = MemoryBudget::new(peak - 1);
    let error = analyzer
        .analyze_diagnostic_budgeted(&input, &limited, || Ok(()))
        .unwrap_err();
    assert!(matches!(error, AnalysisError::Memory(_)));
    assert_eq!(limited.used(), 0);

    for cancel_at in [
        analysis_polls + 1,
        analysis_polls.midpoint(total_polls),
        total_polls,
    ] {
        let mut polls = 0;
        let error = analyzer
            .analyze_diagnostic_budgeted(&input, &budget, || {
                polls += 1;
                if polls == cancel_at {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(matches!(error, AnalysisError::Cancelled));
        assert_eq!(polls, cancel_at);
        assert_eq!(budget.used(), 0);
    }
    drop(
        analyzer
            .analyze_diagnostic_budgeted(&input, &budget, || Ok(()))
            .unwrap(),
    );
    assert_eq!(budget.used(), 0);
}
