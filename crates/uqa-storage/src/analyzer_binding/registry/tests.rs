//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{analyzer_image::AnalyzerImageSnapshot, AnalyzerBindingOwner, AnalyzerPhase};
use uqa_analysis::{Analyzer, AnalyzerLimits, AnalyzerResources};

fn bytes(image: &AnalyzerImageSnapshot) -> Vec<u8> {
    let mut output = Vec::new();
    image
        .write_image(&mut output, &StorageReadControl::with_limit(0))
        .unwrap();
    output
}

fn historical_binding(resources: &AnalyzerResources) -> FieldAnalyzerBinding {
    let words = resources.compile(&Analyzer::default()).unwrap();
    let whole = resources
        .compile(&uqa_analysis::keyword_analyzer())
        .unwrap();
    FieldAnalyzerBinding::unassigned(words.clone(), words.clone())
        .assigned(
            "historical",
            words,
            AnalyzerPhase::Index,
            AnalyzerBindingOwner::Field,
        )
        .assigned(
            "historical",
            whole,
            AnalyzerPhase::Search,
            AnalyzerBindingOwner::Field,
        )
}

#[test]
fn native_field_capture_excludes_temporary_roots_without_cardinality_work_or_pipeline_retention() {
    for count in [1, 64, 4096] {
        let resources = AnalyzerResources::new(AnalyzerLimits::default());
        let value = historical_binding(&resources);
        let weak = [
            Arc::downgrade(&value.index.compiled),
            Arc::downgrade(&value.search.compiled),
        ];
        let expected = [&value.index, &value.search].map(|side| {
            let mut output = Vec::new();
            crate::analyzer_image::write_analyzer_image(
                &side.compiled,
                &mut output,
                &StorageReadControl::with_limit(0),
            )
            .unwrap();
            output
        });
        assert_ne!(expected[0], expected[1]);
        let memory = MemoryBudget::new(32 << 20);
        let mut catalog = FieldAnalyzerCatalog::new(&memory);
        let cancellation = CancellationToken::new();
        for index in 0..count {
            catalog
                .insert(
                    &(format!("public.table{index:04}"), "body".into()),
                    value.clone(),
                    false,
                    &cancellation,
                )
                .unwrap();
        }
        let control = StorageReadControl::with_limit(64 << 20);
        let mut captured = None;
        let measured =
            allocation_counter::measure(|| captured = Some(catalog.capture(&control).unwrap()));
        assert_eq!(measured.count_total, 0);
        let captured = captured.unwrap();
        let required = control.memory().used();
        let mut excluded = value.clone();
        excluded.index.name = Some("x".repeat(1 << 20));
        catalog
            .insert(
                &("public.temporary".into(), "x".repeat(1 << 20)),
                excluded,
                true,
                &cancellation,
            )
            .unwrap();
        let exact = StorageReadControl::with_limit(required);
        let mut other = None;
        let measured =
            allocation_counter::measure(|| other = Some(catalog.capture(&exact).unwrap()));
        assert_eq!(measured.count_total, 0);
        assert_eq!(exact.memory().used(), required);
        assert_eq!(other.as_ref().unwrap().len(), count);
        let small = StorageReadControl::with_limit(required - 1);
        assert!(matches!(
            catalog.capture(&small),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(small.memory().used(), 0);
        drop((catalog, value, resources, other));
        assert!(weak.into_iter().all(|value| value.upgrade().is_none()));
        assert_eq!(exact.memory().used(), 0);
        let child = captured
            .binding(&("public.table0000".into(), "body".into()))
            .unwrap()
            .unwrap();
        assert_eq!(child.index.name.as_deref(), Some("historical"));
        assert_eq!(child.search.name.as_deref(), Some("historical"));
        assert_eq!(child.owner, AnalyzerBindingOwner::Field);
        assert_eq!(child.last_phase, AnalyzerPhase::Search);
        assert_eq!(
            [bytes(&child.index.image), bytes(&child.search.image)],
            expected
        );
        drop(captured);
        if count > 1 {
            assert!(control.memory().used() < required);
        }
        assert_eq!(
            [bytes(&child.index.image), bytes(&child.search.image)],
            expected
        );
        drop(child);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_field_publication_rename_and_removal_preserve_all_roots_on_failure() {
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let compiled = resources.compile(&Analyzer::default()).unwrap();
    let value = FieldAnalyzerBinding::unassigned(compiled.clone(), compiled);
    let memory = MemoryBudget::new(1 << 20);
    let mut catalog = FieldAnalyzerCatalog::new(&memory);
    let key = ("public.saved".into(), "body".into());
    let cancellation = CancellationToken::new();
    catalog
        .insert(&key, value.clone(), false, &cancellation)
        .unwrap();
    catalog
        .insert(
            &("public.temporary".into(), "body".into()),
            value.clone(),
            true,
            &cancellation,
        )
        .unwrap();
    for index in 0..16 {
        catalog
            .insert(
                &(format!("public.other{index:02}"), "body".into()),
                value.clone(),
                false,
                &cancellation,
            )
            .unwrap();
    }
    let original = catalog.clone();
    let used = memory.used();
    let exhausted = memory.reserve(memory.available()).unwrap();
    assert!(catalog
        .insert(&key, value.clone(), true, &cancellation)
        .is_err());
    assert!(catalog
        .with_renamed_field(&key.0, "body", "renamed")
        .is_err());
    assert!(catalog
        .with_renamed_table(&key.0, "public.renamed")
        .is_err());
    assert!(catalog.without_table(&key.0).is_err());
    assert!(catalog.durable.shares_root(&original.durable));
    assert!(catalog.temporary.shares_root(&original.temporary));
    assert!(catalog.images.shares_root(&original.images));
    assert_eq!(memory.used(), memory.limit());
    drop(exhausted);
    assert_eq!(memory.used(), used);
    let changed = catalog
        .with_renamed_field(&key.0, "body", "renamed")
        .unwrap()
        .with_renamed_table(&key.0, "public.moved")
        .unwrap();
    assert!(changed.get(&key).is_none());
    assert!(changed
        .get(&("public.moved".into(), "renamed".into()))
        .is_some());
    assert!(catalog.get(&key).is_some());
    let removed = changed.without_table("public.moved").unwrap();
    assert_eq!(removed.len(), 17);
    let mut cleared = catalog.clone();
    cleared.clear_durable();
    assert_eq!(cleared.len(), 1);
    assert!(cleared
        .capture(&StorageReadControl::with_limit(0))
        .unwrap()
        .is_empty());
    cancellation.cancel();
    assert!(matches!(
        catalog.insert(&key, value, false, &cancellation),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(catalog.images.shares_root(&original.images));
    drop((catalog, original, changed, removed, cleared, resources));
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_field_children_preserve_admission_and_original_cancellation_after_parent_retirement() {
    let compiled = Analyzer::default().compile().unwrap();
    let mut catalog = FieldAnalyzerCatalog::default();
    let key = ("public.saved".into(), "body".into());
    catalog
        .insert(
            &key,
            FieldAnalyzerBinding::unassigned(compiled.clone(), compiled),
            false,
            &CancellationToken::new(),
        )
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let captured = catalog.capture(&control).unwrap();
    let used = control.memory().used();
    let exhausted = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    assert!(captured.binding(&key).is_err());
    drop(exhausted);
    assert_eq!(control.memory().used(), used);
    let child = captured.binding(&key).unwrap().unwrap();
    let used = control.memory().used();
    let measured = allocation_counter::measure(|| {
        assert!(Arc::ptr_eq(
            &child,
            &captured.binding(&key).unwrap().unwrap()
        ));
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(control.memory().used(), used);
    drop((captured, catalog));
    let image = child.index.image.clone();
    drop(child);
    control.cancellation().cancel();
    let mut output = Vec::new();
    assert!(matches!(
        image.write_image(&mut output, &StorageReadControl::with_limit(0)),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(output.is_empty());
    drop(image);
    assert_eq!(control.memory().used(), 0);
}
