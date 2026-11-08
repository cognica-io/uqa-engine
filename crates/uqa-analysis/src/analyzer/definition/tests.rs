//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn published_analyzer_images_share_original_bytes_without_configuration_retention() {
    let mut definition = AnalyzerDefinition::new(Analyzer {
        tokenizer: crate::Tokenizer::Pattern {
            pattern: "x".repeat(1 << 20),
        },
        ..Analyzer::default()
    });
    let original = Arc::downgrade(&definition.configuration);
    let first = definition.image.as_ref().unwrap().as_ptr();
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut captured = None;
    let allocation = allocation_counter::measure(|| {
        captured = Some(definition.capture_image(&control).unwrap());
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    let captured = captured.unwrap();
    assert_eq!(captured.as_ptr(), first);
    assert!(memory.used() >= 1 << 20);
    definition = AnalyzerDefinition::new(crate::keyword_analyzer());
    assert!(original.upgrade().is_none());
    assert_ne!(definition.image.as_ref().unwrap().as_ptr(), first);
    drop(definition);
    let decoded: Analyzer = serde_json::from_slice(&captured).unwrap();
    assert!(
        matches!(decoded.tokenizer, crate::Tokenizer::Pattern { ref pattern } if pattern == &"x".repeat(1 << 20))
    );
    drop(captured);
    assert_eq!(memory.used(), 0);
}

#[test]
fn published_analyzer_images_preserve_unresolved_paths_and_deferred_validation() {
    let definition = AnalyzerDefinition::new(Analyzer {
        tokenizer: crate::Tokenizer::Pattern {
            pattern: "[".into(),
        },
        token_filters: vec![crate::TokenFilter::Synonym {
            synonyms: std::collections::BTreeMap::new(),
            synonyms_path: Some("unavailable-during-capture/synonyms.txt".into()),
        }],
        ..Analyzer::default()
    });
    let captured = definition
        .capture_image(&ProductionControl::uncontrolled())
        .unwrap();
    let expected = br#"{"tokenizer":{"type":"pattern","pattern":"["},"token_filters":[{"type":"synonym","synonyms_path":"unavailable-during-capture/synonyms.txt"}],"char_filters":[]}"#;
    assert_eq!(captured.as_bytes(), expected);
    assert_eq!(serde_json::to_vec(&definition).unwrap(), expected);
    assert!(matches!(
        definition.configuration().clone().compile(),
        Err(AnalysisError::InvalidRegex { .. })
    ));
    drop(definition);
    let restored: Analyzer = serde_json::from_slice(&captured).unwrap();
    assert!(matches!(
        restored.compile(),
        Err(AnalysisError::InvalidRegex { .. })
    ));
}

#[test]
fn published_analyzer_images_admit_exact_capacity_and_both_cancellation_scopes() {
    let definition = AnalyzerDefinition::new(crate::standard_cjk_analyzer("english"));
    let bytes = definition.image.as_ref().unwrap().retained_bytes;
    let token = CancellationToken::new();
    let denied = MemoryBudget::new(bytes - 1);
    assert!(matches!(
        definition
            .capture_image(&ProductionControl::new(&denied, &token, &token))
            .unwrap_err(),
        AnalyzerConfigurationError::Admission(ValueRetentionError::Memory(_))
    ));
    assert_eq!(denied.used(), 0);
    let memory = MemoryBudget::new(bytes);
    let captured = definition
        .capture_image(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    assert_eq!(memory.used(), bytes);
    drop(captured);
    assert_eq!(memory.used(), 0);
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(matches!(
            definition
                .capture_image(&ProductionControl::new(&memory, &original, &invoking))
                .unwrap_err(),
            AnalyzerConfigurationError::Admission(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(memory.used(), 0);
    }
}

#[cfg(unix)]
#[test]
fn published_analyzer_encoding_errors_leave_the_original_configuration_usable() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let path = std::ffi::OsString::from_vec(vec![0xff]).into();
    let definition = AnalyzerDefinition::new(Analyzer {
        token_filters: vec![crate::TokenFilter::Synonym {
            synonyms: std::collections::BTreeMap::new(),
            synonyms_path: Some(path),
        }],
        ..Analyzer::default()
    });
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let first = definition.capture_image(&control).unwrap_err();
    let second = definition.capture_image(&control).unwrap_err();
    let (AnalyzerConfigurationError::Encoding(first), AnalyzerConfigurationError::Encoding(second)) =
        (first, second)
    else {
        panic!("cached encoding errors")
    };
    assert!(matches!(first.as_ref(), AnalysisError::Json(_)));
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(memory.used(), 0);
    let crate::TokenFilter::Synonym {
        synonyms_path: Some(path),
        ..
    } = &definition.configuration().token_filters[0]
    else {
        panic!("original path")
    };
    assert_eq!(path.as_os_str().as_bytes(), [0xff]);
}
