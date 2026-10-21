//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_analysis::AnalysisError;

const FIXTURES: [&str; 5] = [
    include_str!("../../fixtures/unicode-17/keyword.json"),
    include_str!("../../fixtures/unicode-17/whitespace.json"),
    include_str!("../../fixtures/unicode-17/standard.json"),
    include_str!("../../fixtures/unicode-17/ngram.json"),
    include_str!("../../fixtures/unicode-17/english.json"),
];

#[test]
fn historical_unicode_17_descriptors_restore_without_rewriting_or_cache_aliasing() {
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    for wire in FIXTURES {
        let restored = resources.restore_json(wire).unwrap();
        assert_eq!(restored.descriptor().canonical_json(), wire);
        assert!(Arc::ptr_eq(
            &restored,
            &resources.restore_json(wire).unwrap()
        ));
        let current = resources.compile(restored.configuration()).unwrap();
        assert_ne!(
            current.descriptor().fingerprint(),
            restored.descriptor().fingerprint()
        );
        assert!(!Arc::ptr_eq(&current, &restored));
        assert_eq!(
            restored.analyze("ΟΣ").unwrap(),
            current.analyze("ΟΣ").unwrap()
        );
    }
    let old = resources.restore_json(FIXTURES[0]).unwrap();
    let current = resources.compile(old.configuration()).unwrap();
    // New Unicode 17 letters were uncased in the retained regex context table.
    assert_eq!(
        old.analyze("\u{a7ce}Σ AΣ\u{a7ce} İ \u{16ea0}").unwrap(),
        ["\u{a7cf}σ aς\u{a7cf} i\u{307} \u{16ebb}"]
    );
    assert_eq!(
        current.analyze("\u{a7ce}Σ AΣ\u{a7ce} İ \u{16ea0}").unwrap(),
        ["\u{a7ce}σ aς\u{a7ce} i\u{307} \u{16ea0}"]
    );
}

#[test]
fn historical_unicode_profiles_still_reject_unknown_or_mismatched_tables() {
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let source: Value = serde_json::from_str(FIXTURES[0]).unwrap();
    for (key, value) in [
        ("rust_unicode", json!([18, 0, 0])),
        ("rust_unicode", Value::Null),
        ("rust_unicode", json!([16, 0, 0])),
        ("expressions", json!([])),
        ("expressions", json!(["unknown", "unknown"])),
        ("normalization_unicode", json!([17, 0, 0])),
    ] {
        let mut wire = source.clone();
        wire["descriptor"]["runtime_profiles"][key] = value;
        assert!(
            matches!(
                resources.restore_json(&super::validation::rehash(&mut wire)),
                Err(AnalysisError::Descriptor(_))
            ),
            "{wire}"
        );
    }
    assert_eq!(resources.cache_stats().analyzers, 0);
}
