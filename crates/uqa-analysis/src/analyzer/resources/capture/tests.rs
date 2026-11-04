//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{Analyzer, AnalyzerLimits, AnalyzerResources, TokenFilter, Tokenizer};
use std::sync::Arc;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn analyzer_capture_detaches_resolved_descriptor_without_reopening_files_or_retaining_pipeline() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synonyms.txt");
    std::fs::write(&path, "quick => fast, rapid\n").unwrap();
    let config = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::synonym_from_path(&path).unwrap()],
        Vec::new(),
    );
    let owner = AnalyzerResources::new(AnalyzerLimits::default());
    let source = owner.compile(&config).unwrap();
    let original = Arc::downgrade(&source);
    let expected = source.descriptor().canonical_json().to_owned();
    directory.close().unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let mut captured = None;
    let measured = allocation_counter::measure(|| {
        captured = Some(source.capture_image_with_control(&control).unwrap());
    });
    let captured = captured.unwrap();
    assert_eq!(
        measured.bytes_current,
        i64::try_from(memory.used()).unwrap()
    );
    drop((source, owner));
    assert!(original.upgrade().is_none());
    assert_eq!(captured.canonical_json(), expected);
    assert_eq!(captured.dictionary_resources().len(), 0);
    drop(captured);
    assert_eq!(memory.used(), 0);
}

#[test]
fn analyzer_capture_rejects_unfunded_and_cancelled_descriptor_copies_without_leases() {
    let source = crate::standard_analyzer("english").compile().unwrap();
    let memory = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    assert!(matches!(
        source.capture_image_with_control(&control),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(memory.used(), 0);
    cancellation.cancel();
    assert!(matches!(
        source.capture_image_with_control(&control),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}

#[test]
fn analyzer_capture_keeps_every_enabled_dictionary_byte_and_independent_allowance() {
    let owner = AnalyzerResources::new(AnalyzerLimits::default());
    for (kind, tokenizer) in [
        (AnalyzerResourceKind::Nori, "nori_tokenizer"),
        (AnalyzerResourceKind::Kuromoji, "kuromoji_tokenizer"),
    ] {
        if owner.image_resource_byte_limit(kind).is_err() {
            continue;
        }
        let config: Analyzer = serde_json::from_value(serde_json::json!({
            "tokenizer": {"type": tokenizer}, "normalization": {"type": "unavailable"}
        }))
        .unwrap();
        let source = owner.compile(&config).unwrap();
        let memory = MemoryBudget::new(128 << 20);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&memory, &cancellation, &cancellation);
        let captured = source.capture_image_with_control(&control).unwrap();
        assert_eq!(
            captured.canonical_json(),
            source.descriptor().canonical_json()
        );
        assert_eq!(captured.dictionary_resources().len(), 1);
        for (saved, original) in captured
            .dictionary_resources()
            .zip(source.dictionary_resources())
        {
            assert_eq!(saved.kind(), kind);
            assert_eq!(saved.sha256(), original.sha256());
            assert_eq!(saved.bytes(), original.bytes());
            assert_ne!(saved.bytes().as_ptr(), original.bytes().as_ptr());
            assert!(memory.used() >= original.bytes().len());
        }
        let small = MemoryBudget::new(source.descriptor().canonical_json().len() + 256);
        let limited = ProductionControl::new(&small, &cancellation, &cancellation);
        assert!(matches!(
            source.capture_image_with_control(&limited),
            Err(ValueRetentionError::Memory(_))
        ));
        assert_eq!(small.used(), 0);
        drop(source);
        assert!(memory.used() > 0);
        drop(captured);
        assert_eq!(memory.used(), 0);
    }
}
