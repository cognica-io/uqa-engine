//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn cancellation_covers_analysis_and_complete_occurrence_projection() {
    let analyzer = uqa_analysis::whitespace_analyzer().compile().unwrap();
    let input = "한국 😀 repeated repeated ".repeat(256);
    let mut total = 0;
    let expected = analyze_index_field_with_poll(&analyzer, &input, || {
        total += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(expected, analyze_index_field(&analyzer, &input).unwrap());
    assert!(total > expected.terms.len());
    for target in [1, total / 2, total] {
        let mut visited = 0;
        let error = analyze_index_field_with_poll(&analyzer, &input, || {
            visited += 1;
            if visited == target {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert!(matches!(
            error,
            StorageBackendError::Analysis(AnalysisError::Cancelled)
        ));
        assert_eq!(visited, target);
        assert_eq!(expected, analyze_index_field(&analyzer, &input).unwrap());
    }
    let cancellation = uqa_core::CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        analyze_index_field_cancellable(&analyzer, &input, &cancellation),
        Err(StorageBackendError::Cancelled(_))
    ));
    cancellation.reset();
    assert_eq!(
        expected,
        analyze_index_field_cancellable(&analyzer, &input, &cancellation).unwrap()
    );
}
