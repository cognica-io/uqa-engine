//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::MemoryBudget;
use uqa_storage::analyzer_binding::registry::FieldAnalyzerCatalog;

#[test]
fn field_analyzer_input_retains_both_selected_families_and_original_native_payloads() {
    let resources = Arc::new(AnalyzerResources::default());
    let original = binding(&resources);
    let catalog = catalog();
    let table = format!("\"{}\".docs", "Schema".repeat(2048));
    let key = (table.clone(), "body".repeat(2048));
    save(catalog.as_ref(), &table, &key.1, &original);
    let control = StorageReadControl::with_limit(16 << 20);
    let mut input = input(
        catalog.as_ref(),
        &resources,
        &GinAnalyzerOwners::new(),
        &control,
    );
    catalog.drop_table_field_analyzers(&table).unwrap();
    drop(catalog);
    let (mut restored, polls) = finish(&mut input, 1).unwrap();
    assert!(polls > 32);
    let (original_key, original_value) = restored.bindings.iter().next().unwrap();
    let key_pointer = original_key.0 .0.as_ptr();
    let field_pointer = original_key.0 .1.as_ptr();
    let revision = original_value.index.compiled.clone();
    let (key, value) = restored.take_binding(&key).unwrap();
    assert_eq!(key.0.as_ptr(), key_pointer);
    assert_eq!(key.1.as_ptr(), field_pointer);
    assert!(Arc::ptr_eq(&value.index.compiled, &revision));
    restored.finish().unwrap();
    let mut adoption = ImageDecoder::<_, StorageBackendError>::new(
        control.memory(),
        control.cancellation(),
        |mut reader| async move {
            let mut work = reader.input();
            FieldAnalyzerCatalog::new(work.memory)
                .insert_restored_from(key, value, &mut work)
                .await
        },
    )
    .unwrap();
    let mut adopted = loop {
        if let Some(value) = adoption
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            )
            .unwrap()
        {
            break value;
        }
    };
    let (key, binding) = adopted.iter().next().unwrap();
    assert_eq!(key.0.as_ptr(), key_pointer);
    assert_eq!(key.1.as_ptr(), field_pointer);
    assert!(Arc::ptr_eq(&binding.index.compiled, &revision));
    assert_eq!(binding.search.compiled.analyze("A B").unwrap(), ["A B"]);
    let key = key.clone();
    let retained = adopted.clone();
    let mutations = MemoryBudget::new(16 << 20);
    adopted = adopted.into_mutation_budget(&mutations);
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    adopted.remove(&key).unwrap();
    assert!(adopted.is_empty());
    assert_eq!(retained.len(), 1);
    drop((adopted, retained, adoption, input, revision, occupied));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(mutations.used(), 0);
}

#[test]
fn field_analyzer_input_retires_unpublished_families_on_every_terminal_path() {
    let resources = Arc::new(AnalyzerResources::default());
    let original = binding(&resources);
    let catalog = catalog();
    for index in 0..16 {
        save(
            catalog.as_ref(),
            "docs",
            &format!("body_{index}"),
            &original,
        );
    }
    for steps in [1, 17, 128] {
        for terminal in ["cancel", "memory", "zero", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(16 << 20);
            let mut input = input(
                catalog.as_ref(),
                &resources,
                &GinAnalyzerOwners::new(),
                &control,
            );
            assert!(input.advance(steps).unwrap().is_none());
            let occupied = (terminal == "memory").then(|| {
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap()
            });
            if terminal == "cancel" {
                control.cancellation().cancel();
            }
            if terminal == "unwind" {
                assert!(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _owner = input;
                        panic!("abandoned field catalog");
                    }))
                    .is_err()
                );
            } else {
                if terminal != "drop" {
                    assert!(finish(&mut input, usize::from(terminal != "zero")).is_err());
                    assert!(input.advance(1).is_err());
                }
                drop(input);
            }
            drop(occupied);
            assert_eq!(control.memory().used(), 0);
        }
    }
}
