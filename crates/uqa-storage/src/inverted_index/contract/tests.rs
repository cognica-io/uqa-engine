//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::AnalyzerPhase;

#[test]
fn analyzer_assignment_normalization_preserves_aliases_case_and_untrimmed_errors() {
    for (input, name, phase) in [
        ("INDEX", "index", AnalyzerPhase::Index),
        ("query", "search", AnalyzerPhase::Search),
        ("QUERY", "search", AnalyzerPhase::Search),
        ("Search", "search", AnalyzerPhase::Search),
        ("both", "both", AnalyzerPhase::Both),
        ("BOTH", "both", AnalyzerPhase::Both),
    ] {
        assert_eq!(
            AnalyzerPhase::normalize(input).unwrap(),
            (name.into(), phase)
        );
    }
    assert_eq!(
        AnalyzerPhase::normalize(" BOTH ").unwrap_err(),
        "phase must be 'index'|'search'|'both', got ` both `"
    );
    assert_eq!(
        AnalyzerPhase::normalize("UNKNOWN").unwrap_err(),
        "phase must be 'index'|'search'|'both', got `unknown`"
    );
    assert!(AnalyzerPhase::parse("QUERY").is_err());
}
