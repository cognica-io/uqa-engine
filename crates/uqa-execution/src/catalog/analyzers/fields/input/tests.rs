//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::analyzers::fields::restore;
use uqa_analysis::{Analyzer, TokenFilter, Tokenizer};
use uqa_storage::{
    key_value::{KeyValueCatalog, KeyValueStore},
    AnalyzerBindingOwner, AnalyzerPhase,
};

mod errors;
mod gin;
mod retention;

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 1 << 20,
    maximum_value_bytes: 1 << 20,
};

fn catalog() -> Arc<KeyValueCatalog> {
    Arc::new(
        uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory()
            .unwrap()
            .catalog(),
    )
}

fn binding(resources: &AnalyzerResources) -> FieldAnalyzerBinding {
    let index = resources
        .compile(&Analyzer::new(
            Tokenizer::Whitespace,
            vec![TokenFilter::Lowercase],
            vec![],
        ))
        .unwrap();
    let search = resources
        .compile(&Analyzer::new(Tokenizer::Keyword, vec![], vec![]))
        .unwrap();
    FieldAnalyzerBinding::unassigned(index, search)
}

fn save(catalog: &dyn CatalogFacade, table: &str, field: &str, binding: &FieldAnalyzerBinding) {
    let (name, phase) = binding
        .last_assignment_ref()
        .unwrap_or(("", binding.phase_name()));
    catalog
        .replace_table_field_analyzer_binding(
            table,
            field,
            phase,
            name,
            &binding.to_json().unwrap(),
        )
        .unwrap();
}

fn finish(
    input: &mut FieldAnalyzerCatalogInput,
    steps: usize,
) -> StorageBackendResult<(RestoredFieldAnalyzers, usize)> {
    for polls in 1..1_000_000 {
        if let Some(value) = input.advance(steps)? {
            return Ok((value, polls));
        }
    }
    panic!("field analyzer input did not progress")
}

fn input(
    catalog: &dyn CatalogFacade,
    resources: &Arc<AnalyzerResources>,
    gin: &GinAnalyzerOwners,
    control: &StorageReadControl,
) -> FieldAnalyzerCatalogInput {
    FieldAnalyzerCatalogInput::new(
        catalog,
        resources.clone(),
        LIMITS,
        Arc::new(gin.clone()),
        control,
    )
    .unwrap()
}

fn compare(
    catalog: &dyn CatalogFacade,
    resources: &Arc<AnalyzerResources>,
    gin: &GinAnalyzerOwners,
) {
    let expected = restore(catalog, resources, gin).unwrap();
    let control = StorageReadControl::with_limit(16 << 20);
    let mut input = input(catalog, resources, gin, &control);
    let (mut actual, polls) = finish(&mut input, 1).unwrap();
    assert!(polls > 1);
    assert_eq!(actual.bindings.len(), expected.bindings.len());
    assert_eq!(actual.labels.len(), expected.labels.len());
    for (key, binding) in &expected.bindings {
        let (original_key, value) = actual.take_binding(key).unwrap();
        assert_eq!(*original_key, *key);
        assert_eq!(value.to_json().unwrap(), binding.to_json().unwrap());
        assert!(original_key.budget().shares_allowance(control.memory()));
        assert!(value.budget().shares_allowance(control.memory()));
    }
    actual.finish().unwrap();
    assert!(input.advance(1).is_err());
    drop(input);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn field_analyzer_input_preserves_native_phases_canonical_keys_and_gin_ownership() {
    let resources = Arc::new(AnalyzerResources::default());
    let original = binding(&resources);
    let catalog = catalog();
    let mut gin = GinAnalyzerOwners::new();
    for (index, (table, phase, owner)) in [
        ("docs", AnalyzerPhase::Both, AnalyzerBindingOwner::Field),
        (
            "public.docs",
            AnalyzerPhase::Index,
            AnalyzerBindingOwner::Field,
        ),
        (
            "\"app.schema\".\"Doc\"",
            AnalyzerPhase::Search,
            AnalyzerBindingOwner::Field,
        ),
        ("app.docs", AnalyzerPhase::Both, AnalyzerBindingOwner::Gin),
    ]
    .into_iter()
    .enumerate()
    {
        let field = format!("body_{index}");
        let binding = original.assigned("saved", original.index.compiled.clone(), phase, owner);
        assert_eq!(
            binding.last_assignment(),
            Some(("saved".into(), binding.phase_name().into()))
        );
        save(catalog.as_ref(), table, &field, &binding);
        if owner == AnalyzerBindingOwner::Gin {
            gin.insert(("app.docs".into(), field), "saved".into());
        }
    }
    assert_eq!(original.last_assignment_ref(), None);
    save(catalog.as_ref(), "docs", "unassigned", &original);
    compare(catalog.as_ref(), &resources, &gin);
    let expected = restore(catalog.as_ref(), &resources, &gin).unwrap();
    assert_eq!(expected.bindings.len(), 5);
    assert!(expected
        .bindings
        .contains_key(&("public.docs".into(), "body_0".into())));
    assert!(expected
        .bindings
        .contains_key(&("\"app.schema\".\"Doc\"".into(), "body_2".into())));
    let unassigned = &expected.bindings[&("public.docs".into(), "unassigned".into())];
    assert_eq!(
        unassigned.index.compiled.analyze("A B").unwrap(),
        ["a", "b"]
    );
    assert_eq!(unassigned.search.compiled.analyze("A B").unwrap(), ["A B"]);
}

#[test]
fn field_analyzer_input_keeps_orphans_until_table_consumption_finishes() {
    let resources = Arc::new(AnalyzerResources::default());
    for orphan in [false, true] {
        let catalog = catalog();
        save(catalog.as_ref(), "docs", "body", &binding(&resources));
        if orphan {
            catalog
                .save_table_field_analyzer("missing", "body", "both", "saved")
                .unwrap();
        }
        let control = StorageReadControl::with_limit(16 << 20);
        let mut input = input(
            catalog.as_ref(),
            &resources,
            &GinAnalyzerOwners::new(),
            &control,
        );
        let (mut actual, _) = finish(&mut input, 1).unwrap();
        assert!(actual
            .take_binding(&("public.other".into(), "body".into()))
            .is_none());
        if orphan {
            drop(
                actual
                    .take_binding(&("public.docs".into(), "body".into()))
                    .unwrap(),
            );
        }
        let error = actual.finish().unwrap_err();
        assert_eq!(
            error.to_string(),
            "table-field analyzer references a missing table, column, or physical FTS field"
        );
        drop(input);
        assert_eq!(control.memory().used(), 0);
    }
}
