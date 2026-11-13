//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_analysis::{Analyzer, TokenFilter, Tokenizer};
use uqa_storage::key_value::{KeyValueCatalog, KeyValueStore};

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 1 << 20,
    maximum_value_bytes: 1 << 20,
};
fn catalog() -> (Arc<KeyValueCatalog>, Arc<dyn CatalogFacade>) {
    let storage = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory().unwrap();
    let catalog = Arc::new(storage.catalog());
    (catalog.clone(), catalog)
}
fn revision(resources: &AnalyzerResources) -> (Arc<CompiledAnalyzer>, String, String) {
    let compiled = resources
        .compile(&Analyzer::new(
            Tokenizer::Whitespace,
            vec![TokenFilter::Lowercase],
            vec![],
        ))
        .unwrap();
    let text = serde_json::to_string(&compiled.descriptor().configuration().unwrap()).unwrap();
    let descriptor = compiled.descriptor().canonical_json().to_owned();
    (compiled, text, descriptor)
}
fn finish(
    input: &mut NamedAnalyzerCatalogInput,
    steps: usize,
) -> StorageBackendResult<(AnalyzerCatalog, usize)> {
    for polls in 1..1_000_000 {
        if let Some(value) = input.advance(steps)? {
            return Ok((value, polls));
        }
    }
    panic!("named analyzer input did not progress")
}
fn compare(catalog: Arc<dyn CatalogFacade>, resources: Arc<AnalyzerResources>) {
    let expected = restore(catalog.as_ref(), &resources, false, |_, _| {
        panic!("load-only input never migrates")
    });
    let control = StorageReadControl::with_limit(16 << 20);
    let mutations = MemoryBudget::new(16 << 20);
    let mut input =
        NamedAnalyzerCatalogInput::new(catalog, resources, LIMITS, &mutations, &control).unwrap();
    match (expected, finish(&mut input, 1)) {
        (Ok(expected), Ok((actual, polls))) => {
            assert!(polls > 1);
            assert_eq!(
                actual.keys().collect::<Vec<_>>(),
                expected.current.keys().collect::<Vec<_>>()
            );
            for (name, compiled) in expected.current {
                assert!(Arc::ptr_eq(actual.get(&name).unwrap(), &compiled));
            }
            assert!(expected.pending.is_empty());
            drop(actual);
        }
        (Err(StorageBackendError::Serde(expected)), Err(StorageBackendError::Json(actual))) => {
            assert_eq!(actual.to_string(), expected.to_string());
            assert_eq!(actual.is_data(), expected.is_data());
            assert_eq!(actual.is_syntax(), expected.is_syntax());
            assert_eq!(actual.is_eof(), expected.is_eof());
        }
        (Err(expected), Err(actual)) => assert_eq!(actual.to_string(), expected.to_string()),
        _ => panic!("named analyzer result differs from ordinary restoration"),
    }
    assert!(input.advance(1).is_err());
    drop(input);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(mutations.used(), 0);
}
fn alter_value(store: &Arc<dyn KeyValueStore>, value: &str, duplicate: bool) {
    let key = store
        .scan_prefix(&[])
        .unwrap()
        .into_iter()
        .find(|(_, bytes)| bytes.as_slice() == value.as_bytes())
        .unwrap()
        .0;
    let mut batch = store.batch();
    if duplicate {
        let mut another = key;
        another.push(255);
        batch.put(&another, value.as_bytes()).unwrap();
    } else {
        batch.delete(&key).unwrap();
    }
    batch.commit().unwrap();
}

fn corrupt_later_physical_row(store: &Arc<dyn KeyValueStore>, value: &str) {
    let mut key = store
        .scan_prefix(&[])
        .unwrap()
        .into_iter()
        .find(|(_, bytes)| bytes.as_slice() == value.as_bytes())
        .unwrap()
        .0;
    key.push(254);
    let mut batch = store.batch();
    batch.put(&key, &[255]).unwrap();
    batch.commit().unwrap();
}

#[test]
fn named_analyzer_input_preserves_complete_family_order_and_native_revision_validation() {
    let resources = Arc::new(AnalyzerResources::default());
    let (_, configuration, descriptor) = revision(&resources);
    for scenario in [
        "valid",
        "whitespace",
        "builtin",
        "descriptor",
        "configuration",
        "mismatch",
        "legacy",
        "orphan",
        "duplicate_descriptor",
        "duplicate_definition",
        "late_descriptor",
        "late_definition",
    ] {
        let (raw, catalog) = catalog();
        let name = match scenario {
            "whitespace" | "late_definition" => " name ",
            "builtin" | "late_descriptor" => "keyword",
            _ => "custom",
        };
        catalog
            .save_analyzer_revision(name, &configuration, &descriptor)
            .unwrap();
        match scenario {
            "descriptor" => catalog
                .save_analyzer_revision(name, "invalid", "invalid")
                .unwrap(),
            "configuration" => catalog
                .save_analyzer_revision(name, "invalid", &descriptor)
                .unwrap(),
            "mismatch" => catalog
                .save_analyzer_revision(name, "{}", &descriptor)
                .unwrap(),
            "legacy" => catalog.save_analyzer(name, &configuration).unwrap(),
            "orphan" => alter_value(&raw.store(), &configuration, false),
            "late_descriptor" => corrupt_later_physical_row(&raw.store(), &descriptor),
            "late_definition" => corrupt_later_physical_row(&raw.store(), &configuration),
            "duplicate_descriptor" => alter_value(&raw.store(), &descriptor, true),
            "duplicate_definition" => alter_value(&raw.store(), &configuration, true),
            _ => (),
        }
        compare(catalog, resources.clone());
    }
}

#[test]
fn named_analyzer_input_preserves_generic_diagnostic_fields_before_native_publication() {
    let resources = Arc::new(AnalyzerResources::default());
    let (_, configuration, descriptor) = revision(&resources);
    let (_, catalog) = catalog();
    let keys = uqa_core::json::JsonScalarProtocol::from_values(
        &serde_json::Number::from(1u8),
        serde_json::value::RawValue::NULL,
    )
    .unwrap();
    let raw = format!(
        "{{{}:{}}}",
        serde_json::to_string(keys.raw).unwrap(),
        serde_json::to_string(&configuration).unwrap()
    );
    for text in [
        &configuration,
        &raw,
        "null",
        "{}",
        "[]",
        "false",
        "1e99999",
        "{\"tokenizer\":null}",
        "{\"a\":{",
        "{\"a\":\"\\ud800\"}",
    ] {
        catalog
            .save_analyzer_revision("custom", text, &descriptor)
            .unwrap();
        compare(catalog.clone(), resources.clone());
    }
    for length in 0..configuration.len() {
        catalog
            .save_analyzer_revision("custom", &configuration[..length], &descriptor)
            .unwrap();
        compare(catalog.clone(), resources.clone());
    }
}

#[test]
fn named_analyzer_input_keeps_original_diagnostic_failures_before_publication() {
    let resources = Arc::new(AnalyzerResources::default());
    let (_, configuration, descriptor) = revision(&resources);
    let (_, catalog) = catalog();
    let keys = uqa_core::json::JsonScalarProtocol::from_values(
        &serde_json::Number::from(1u8),
        serde_json::value::RawValue::NULL,
    )
    .unwrap();
    for key in [keys.number, keys.raw] {
        for payload in [
            "null", "true", "1.2", "[]", "{}", "\"01\"", "\"[1,]\"", "\"1\"",
        ] {
            for suffix in ["}", ",\"later\":invalid}", "\n false}", ""] {
                let text =
                    format!("{{\n\"earlier\":\"한🙂\",\n\"value\":{{{key:?}:{payload}{suffix}}}");
                assert!(serde_json::from_str::<serde_json::Value>(&text).is_err() || suffix == "}");
                catalog
                    .save_analyzer_revision("custom", &text, &descriptor)
                    .unwrap();
                compare(catalog.clone(), resources.clone());
            }
        }
    }
    catalog
        .save_analyzer_revision("custom", &configuration, &descriptor)
        .unwrap();
    compare(catalog, resources);
}

#[test]
fn named_analyzer_input_adopts_original_names_and_independent_mutation_allowances() {
    let resources = Arc::new(AnalyzerResources::default());
    let (compiled, configuration, descriptor) = revision(&resources);
    let (_, catalog) = catalog();
    for index in 0..24 {
        catalog
            .save_analyzer_revision(
                &format!("{}_{index}", "long_name_".repeat(1024)),
                &configuration,
                &descriptor,
            )
            .unwrap();
    }
    let control = StorageReadControl::with_limit(16 << 20);
    let mutations = MemoryBudget::new(8 << 20);
    let mut input =
        NamedAnalyzerCatalogInput::new(catalog.clone(), resources, LIMITS, &mutations, &control)
            .unwrap();
    let (mut restored, polls) = finish(&mut input, 1).unwrap();
    assert!(polls > 100);
    assert_eq!(restored.len(), 24);
    let original = restored.clone();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    restored.insert("later", compiled).unwrap();
    assert_eq!(original.len(), 24);
    assert_eq!(restored.len(), 25);
    assert!(mutations.used() > 0);
    drop((restored, original, input, catalog));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(mutations.used(), 0);
}

#[test]
fn named_analyzer_input_retires_selected_rows_and_unpublished_revisions_on_terminal_paths() {
    let resources = Arc::new(AnalyzerResources::default());
    let (_, configuration, descriptor) = revision(&resources);
    let (_, catalog) = catalog();
    for index in 0..16 {
        catalog
            .save_analyzer_revision(&format!("custom_{index}"), &configuration, &descriptor)
            .unwrap();
    }
    for steps in [1, 17, 128] {
        for terminal in ["cancel", "memory", "zero", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(16 << 20);
            let mut input = NamedAnalyzerCatalogInput::new(
                catalog.clone(),
                resources.clone(),
                LIMITS,
                &MemoryBudget::new(1 << 20),
                &control,
            )
            .unwrap();
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
                        panic!("abandoned named catalog");
                    }))
                    .is_err()
                );
            } else {
                if terminal != "drop" {
                    let error = finish(&mut input, usize::from(terminal != "zero"))
                        .err()
                        .expect("terminal named analyzer input");
                    match terminal {
                        "cancel" => assert!(matches!(error, StorageBackendError::Cancelled(_))),
                        "memory" => assert!(matches!(error, StorageBackendError::Memory(_))),
                        _ => (),
                    }
                    assert!(input.advance(1).is_err());
                }
                drop(input);
            }
            drop(occupied);
            assert_eq!(control.memory().used(), 0);
        }
    }
}
