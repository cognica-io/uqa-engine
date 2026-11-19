//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::AnalyzerPhase;

#[test]
fn field_installation_preserves_physical_failure_before_later_field_validation() {
    let resources = Arc::new(AnalyzerResources::default());
    let plain = binding(&resources);
    for first_fails in [true, false] {
        for ordinary in [true, false] {
            let mut table = table(&plain);
            table.fail = first_fails.then(|| "a".into());
            let log = table.log.clone();
            let missing = plain.assigned(
                "missing_installation_name",
                plain.index.compiled.clone(),
                AnalyzerPhase::Both,
                AnalyzerBindingOwner::Field,
            );
            let saved = saved([("a", plain.clone()), ("b", missing)]);
            let control = StorageReadControl::with_limit(16 << 20);
            let mutations = MemoryBudget::new(16 << 20);
            let names = uqa_storage::analyzer_catalog::AnalyzerCatalog::new(&mutations);
            let error = if ordinary {
                let state = Catalog {
                    fields: Arc::new(Mutex::new(FieldAnalyzerCatalog::new(&mutations))),
                    named: names,
                };
                let result = restore(
                    tables(table),
                    state.clone(),
                    saved,
                    &GinAnalyzerOwners::new(),
                    false,
                );
                assert_eq!(state.fields.lock().len(), usize::from(!first_fails));
                match result {
                    Ok(_) => panic!("invalid field installed"),
                    Err(error) => error,
                }
            } else {
                let saved = retained(saved, resources.clone(), &control);
                let mut input = FieldAnalyzerInstallation::new(
                    tables(table),
                    saved,
                    names,
                    &mutations,
                    &control,
                )
                .unwrap();
                let error = match finish(&mut input) {
                    Ok(_) => panic!("invalid field installed"),
                    Err(error) => error,
                };
                assert!(input.advance(1).is_err());
                error
            };
            assert_eq!(
                error.to_string(),
                if first_fails {
                    "forced field install a"
                } else {
                    "field analyzer binding references missing name `missing_installation_name`"
                }
            );
            assert_eq!(*log.lock(), ["a"]);
            assert_eq!(control.memory().used(), 0);
            assert_eq!(mutations.used(), 0);
        }
    }
}

#[test]
fn field_installation_keeps_load_only_migration_and_unmatched_record_boundaries() {
    let resources = Arc::new(AnalyzerResources::default());
    let plain = binding(&resources);
    for (scenario, expected, installed) in [
        (
            "missing",
            "field analyzer catalog requires an initial migration",
            1,
        ),
        (
            "source",
            "positional index requires an initial source migration",
            2,
        ),
        (
            "orphan",
            "table-field analyzer references a missing table, column, or physical FTS field",
            2,
        ),
    ] {
        for ordinary in [true, false] {
            let mut table = table(&plain);
            table.rebuild = scenario == "source";
            let log = table.log.clone();
            let mut saved = saved([("a", plain.clone()), ("b", plain.clone())]);
            if scenario == "missing" {
                saved.bindings.remove(&("public.docs".into(), "b".into()));
            }
            if scenario == "orphan" {
                saved.labels.insert(
                    ("public.orphan".into(), "body".into()),
                    vec![("both".into(), String::new())],
                );
            }
            let control = StorageReadControl::with_limit(16 << 20);
            let mutations = MemoryBudget::new(16 << 20);
            let names = uqa_storage::analyzer_catalog::AnalyzerCatalog::new(&mutations);
            let error = if ordinary {
                let state = Catalog {
                    fields: Arc::new(Mutex::new(FieldAnalyzerCatalog::new(&mutations))),
                    named: names,
                };
                let result = restore(
                    tables(table),
                    state.clone(),
                    saved,
                    &GinAnalyzerOwners::new(),
                    false,
                );
                assert_eq!(state.fields.lock().len(), installed);
                match result {
                    Ok(_) => panic!("incomplete fields installed"),
                    Err(error) => error,
                }
            } else {
                let saved = retained(saved, resources.clone(), &control);
                let mut input = FieldAnalyzerInstallation::new(
                    tables(table),
                    saved,
                    names,
                    &mutations,
                    &control,
                )
                .unwrap();
                match finish(&mut input) {
                    Ok(_) => panic!("incomplete fields installed"),
                    Err(error) => error,
                }
            };
            assert_eq!(error.to_string(), expected);
            assert_eq!(log.lock().len(), installed);
            assert_eq!(control.memory().used(), 0);
            assert_eq!(mutations.used(), 0);
        }
    }
}

#[test]
fn ordinary_field_migration_keeps_query_alias_phase_order_and_deferred_writes() {
    let resources = AnalyzerResources::default();
    let plain = binding(&resources);
    let mut table = table(&plain);
    table.fields = TableMetadata::new(vec!["a".into()]).snapshot();
    let mutations = MemoryBudget::new(16 << 20);
    let mut names = uqa_storage::analyzer_catalog::AnalyzerCatalog::new(&mutations);
    names
        .insert("installation_index", plain.index.compiled.clone())
        .unwrap();
    names
        .insert("installation_search", plain.search.compiled.clone())
        .unwrap();
    let state = Catalog {
        fields: Arc::new(Mutex::new(FieldAnalyzerCatalog::new(&mutations))),
        named: names,
    };
    let key = ("public.docs".into(), "a".into());
    let saved = CatalogFieldBindings {
        bindings: BTreeMap::new(),
        labels: BTreeMap::from([(
            key.clone(),
            vec![
                ("query".into(), "installation_search".into()),
                ("INDEX".into(), "installation_index".into()),
            ],
        )]),
    };
    let result = restore(
        tables(table),
        state.clone(),
        saved,
        &GinAnalyzerOwners::new(),
        true,
    )
    .unwrap();
    assert_eq!(result.bindings.len(), 1);
    assert_eq!(
        result.rebuild_tables.into_iter().collect::<Vec<_>>(),
        ["public.docs"]
    );
    let fields = state.fields.lock();
    let value = fields.get(&key).unwrap();
    assert_eq!(value.last_phase, AnalyzerPhase::Search);
    assert_eq!(value.index.name.as_deref(), Some("installation_index"));
    assert_eq!(value.search.name.as_deref(), Some("installation_search"));
    assert_eq!(value.index.compiled.analyze("A B").unwrap(), ["a", "b"]);
    assert_eq!(value.search.compiled.analyze("A B").unwrap(), ["A B"]);
}
