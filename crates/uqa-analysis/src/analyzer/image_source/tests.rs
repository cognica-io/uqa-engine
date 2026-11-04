//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    Analyzer, AnalyzerLimits, AnalyzerResourceKind, AnalyzerResources, TokenFilter, Tokenizer,
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn analyzer_native_image_capture_is_constant_and_retires_the_executable_owner() {
    for count in [1, 64, 4096] {
        let configuration = Analyzer::new(
            Tokenizer::Whitespace,
            vec![TokenFilter::Stop {
                language: "english".into(),
                custom_words: (0..count).map(|index| format!("word{index}")).collect(),
            }],
            Vec::new(),
        );
        let owner = AnalyzerResources::new(AnalyzerLimits::default());
        let compiled = owner.compile(&configuration).unwrap();
        let weak = Arc::downgrade(&compiled);
        let expected = compiled.descriptor().canonical_json().to_owned();
        let construction = MemoryBudget::new(1 << 24);
        let cancellation = CancellationToken::new();
        let prepare = ProductionControl::new(&construction, &cancellation, &cancellation);
        let source = compiled.image_source_with_control(&prepare).unwrap();
        let same = compiled.image_source_with_control(&prepare).unwrap();
        assert!(Arc::ptr_eq(&source.0, &same.0));
        let allowance = MemoryBudget::new(source.retained_bytes());
        let control = ProductionControl::new(&allowance, &cancellation, &cancellation);
        let mut captured = None;
        let measured = allocation_counter::measure(|| {
            captured = Some(source.capture(&control).unwrap());
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(allowance.used(), source.retained_bytes());
        let captured = captured.unwrap();
        drop((compiled, source, same, owner));
        assert!(weak.upgrade().is_none());
        assert_eq!(captured.canonical_json(), expected);
        assert!(construction.used() > 0);
        drop(captured);
        assert_eq!(allowance.used(), 0);
        assert_eq!(construction.used(), 0);
    }
}

#[test]
fn analyzer_native_image_failure_preserves_source_and_releases_admission() {
    let compiled = Analyzer::default().compile().unwrap();
    let empty = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let denied = ProductionControl::new(&empty, &cancellation, &cancellation);
    assert!(matches!(
        compiled.image_source_with_control(&denied),
        Err(ValueRetentionError::Memory(_))
    ));
    assert!(compiled.image_source.0.lock().upgrade().is_none());
    assert_eq!(empty.used(), 0);
    let source = compiled
        .image_source_with_control(&ProductionControl::uncontrolled())
        .unwrap();
    let small = MemoryBudget::new(source.retained_bytes() - 1);
    let limited = ProductionControl::new(&small, &cancellation, &cancellation);
    assert!(matches!(
        source.capture(&limited),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(small.used(), 0);
    cancellation.cancel();
    assert!(matches!(
        source.capture(&limited),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert!(matches!(
        compiled.image_source_with_control(&limited),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert_eq!(small.used(), 0);
    assert_eq!(compiled.analyze("text").unwrap(), ["text"]);
}

#[test]
fn analyzer_native_image_shares_enabled_dictionary_artifacts_without_retaining_models() {
    for (kind, tokenizer) in [
        (AnalyzerResourceKind::Nori, "nori_tokenizer"),
        (AnalyzerResourceKind::Kuromoji, "kuromoji_tokenizer"),
    ] {
        let owner = AnalyzerResources::new(AnalyzerLimits::default());
        if owner.image_resource_byte_limit(kind).is_err() {
            continue;
        }
        let configuration: Analyzer = serde_json::from_value(serde_json::json!({
            "tokenizer": {"type": tokenizer}, "normalization": {"type": "unavailable"}
        }))
        .unwrap();
        let first = owner.compile(&configuration).unwrap();
        let mut changed = configuration.clone();
        changed.token_filters.push(TokenFilter::Lowercase);
        let second = owner.compile(&changed).unwrap();
        let memory = MemoryBudget::new(128 << 20);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&memory, &cancellation, &cancellation);
        let a = first.image_source_with_control(&control).unwrap();
        let b = second.image_source_with_control(&control).unwrap();
        assert_ne!(a.canonical_json(), b.canonical_json());
        assert_eq!(a.dictionary_resources().len(), 1);
        assert_eq!(b.dictionary_resources().len(), 1);
        let left = a.dictionary_resources().next().unwrap();
        let right = b.dictionary_resources().next().unwrap();
        assert_eq!(left.kind(), kind);
        assert_eq!(left.sha256(), right.sha256());
        assert_eq!(left.bytes().as_ptr(), right.bytes().as_ptr());
        assert!(memory.used() < 64 << 10);
        let weak_first = Arc::downgrade(&first);
        let weak_second = Arc::downgrade(&second);
        drop((first, second, owner));
        assert!(weak_first.upgrade().is_none());
        assert!(weak_second.upgrade().is_none());
        assert_eq!(
            a.dictionary_resources().next().unwrap().bytes(),
            b.dictionary_resources().next().unwrap().bytes()
        );
        drop((a, b));
        assert_eq!(memory.used(), 0);
    }
}
