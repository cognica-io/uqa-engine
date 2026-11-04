//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::StorageBackendError;
use uqa_analysis::{Analyzer, AnalyzerLimits, AnalyzerResources};

fn image_bytes(image: &AnalyzerImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    image
        .write_image(&mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    bytes
}

#[test]
fn native_analyzer_catalog_capture_is_constant_and_keeps_exact_revisions() {
    for count in [1, 64, 4096] {
        let resources = AnalyzerResources::new(AnalyzerLimits::default());
        let compiled = resources.compile(&Analyzer::default()).unwrap();
        let weak = Arc::downgrade(&compiled);
        let mut expected = Vec::new();
        crate::analyzer_image::write_analyzer_image(
            &compiled,
            &mut expected,
            &StorageReadControl::with_limit(0),
        )
        .unwrap();
        let memory = MemoryBudget::new(16 << 20);
        let mut catalog = AnalyzerCatalog::new(&memory);
        for index in 0..count {
            catalog
                .insert(&format!("analyzer{index:04}"), compiled.clone())
                .unwrap();
        }
        let control = StorageReadControl::with_limit(32 << 20);
        let mut captured = None;
        let measured = allocation_counter::measure(|| {
            captured = Some(catalog.capture(&control).unwrap());
        });
        assert_eq!(measured.count_total, 0);
        let captured = captured.unwrap();
        assert_eq!(captured.len(), count);
        assert_eq!(captured.names().count(), count);
        let required = control.memory().used();
        let exact = StorageReadControl::with_limit(required);
        let independently_captured = catalog.capture(&exact).unwrap();
        let small = StorageReadControl::with_limit(required - 1);
        assert!(matches!(
            catalog.capture(&small),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(small.memory().used(), 0);
        catalog
            .insert(
                "analyzer0000",
                uqa_analysis::keyword_analyzer().compile().unwrap(),
            )
            .unwrap();
        let changed = catalog
            .capture(&StorageReadControl::with_limit(32 << 20))
            .unwrap();
        assert_ne!(
            image_bytes(&changed.image("analyzer0000").unwrap().unwrap()),
            expected
        );
        drop((catalog, changed, compiled, resources));
        assert!(weak.upgrade().is_none());
        let child = captured.image("analyzer0000").unwrap().unwrap();
        assert_eq!(image_bytes(&child), expected);
        drop((captured, independently_captured));
        assert_eq!(exact.memory().used(), 0);
        assert!(control.memory().used() < required || count == 1);
        assert_eq!(image_bytes(&child), expected);
        drop(child);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_analyzer_catalog_denied_publication_preserves_both_roots_and_footprints() {
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let memory = MemoryBudget::new(1 << 20);
    let mut catalog = AnalyzerCatalog::new(&memory);
    catalog
        .insert("saved", resources.compile(&Analyzer::default()).unwrap())
        .unwrap();
    let original = catalog.clone();
    let footprint = catalog.images.retained_bytes().unwrap();
    let used = memory.used();
    let exhausted = memory.reserve(memory.limit() - used).unwrap();
    assert!(matches!(
        catalog.insert(
            "saved",
            resources
                .compile(&uqa_analysis::keyword_analyzer())
                .unwrap()
        ),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(catalog
        .extend([(
            "other".into(),
            resources.compile(&Analyzer::default()).unwrap()
        )])
        .is_err());
    assert!(catalog.revisions.shares_root(&original.revisions));
    assert!(catalog.images.shares_root(&original.images));
    assert_eq!(catalog.images.retained_bytes().unwrap(), footprint);
    assert_eq!(memory.used(), memory.limit());
    drop(exhausted);
    assert_eq!(memory.used(), used);
    assert!(catalog.remove("saved").unwrap());
    assert!(catalog.is_empty());
    assert!(original.contains_key("saved"));
    drop((catalog, original));
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_analyzer_children_share_original_admission_and_cancellation() {
    let mut catalog = AnalyzerCatalog::default();
    catalog
        .insert("saved", Analyzer::default().compile().unwrap())
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let captured = catalog.capture(&control).unwrap();
    let before = control.memory().used();
    let exhausted = control
        .memory()
        .reserve(control.memory().limit() - before)
        .unwrap();
    let measured = allocation_counter::measure(|| {
        assert!(matches!(
            captured.image("saved"),
            Err(StorageBackendError::Memory(_))
        ));
    });
    assert_eq!(measured.count_total, 0);
    drop(exhausted);
    assert_eq!(control.memory().used(), before);
    let first = captured.image("saved").unwrap().unwrap();
    let after = control.memory().used();
    let mut second = None;
    let measured = allocation_counter::measure(|| {
        second = Some(captured.image("saved").unwrap().unwrap());
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(control.memory().used(), after);
    assert_eq!(image_bytes(&first), image_bytes(second.as_ref().unwrap()));
    drop((catalog, captured));
    control.cancellation().cancel();
    let mut output = Vec::new();
    assert!(matches!(
        first.write_image(&mut output, &StorageReadControl::with_limit(0)),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(output.is_empty());
    drop((first, second));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_analyzer_publication_observes_cancellation_before_reusing_a_cached_image() {
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let compiled = resources.compile(&Analyzer::default()).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let mut catalog = AnalyzerCatalog::new(&memory);
    catalog.insert("saved", compiled.clone()).unwrap();
    let original = catalog.clone();
    let used = memory.used();
    let cancellation = uqa_core::CancellationToken::new();
    cancellation.cancel();
    // Establish the cancelled signal and its thread-local deferral bookkeeping before measuring native publication.
    assert_eq!(
        cancellation.check(),
        Err(uqa_core::QueryCancelled::USER_REQUEST)
    );
    let measured = allocation_counter::measure(|| {
        assert!(matches!(
            catalog.insert_with_cancellation("other", compiled.clone(), &cancellation),
            Err(StorageBackendError::Cancelled(_))
        ));
    });
    assert_eq!(measured.count_total, 0);
    assert!(catalog.revisions.shares_root(&original.revisions));
    assert!(catalog.images.shares_root(&original.images));
    assert_eq!(memory.used(), used);
    drop((catalog, original, compiled, resources));
    assert_eq!(memory.used(), 0);
}
