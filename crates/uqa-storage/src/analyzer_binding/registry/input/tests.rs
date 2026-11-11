//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    analyzer_binding::{capture::fields, AnalyzerBindingOwner},
    AnalyzerPhase,
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_analysis::AnalyzerResources;

mod retained;

fn key(table: &str, field: &str, input: &StorageReadControl) -> Budgeted<(String, String)> {
    let control =
        ProductionControl::new(input.memory(), input.cancellation(), input.cancellation());
    let (table, left) = control.copy_text(table).unwrap().into_parts();
    let (field, right) = control.copy_text(field).unwrap().into_parts();
    control
        .finish((table, field), control.combine(left, right))
        .unwrap()
        .into_budgeted()
        .unwrap()
}

fn wire() -> Vec<u8> {
    let index = uqa_analysis::whitespace_analyzer().compile().unwrap();
    let search = uqa_analysis::keyword_analyzer().compile().unwrap();
    let binding = FieldAnalyzerBinding::unassigned(index.clone(), search.clone())
        .assigned(
            "historical",
            index,
            AnalyzerPhase::Index,
            AnalyzerBindingOwner::Field,
        )
        .assigned(
            "historical",
            search,
            AnalyzerPhase::Search,
            AnalyzerBindingOwner::Field,
        );
    let source = BTreeMap::from([(("public.saved".into(), "body".into()), binding)]);
    let control = StorageReadControl::with_limit(1 << 20);
    let captured = fields(&source, &control).unwrap();
    let mut reader = captured[0].1.image_reader(&control).unwrap();
    let mut wire = Vec::new();
    let mut buffer = [0; 37];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        wire.extend_from_slice(&buffer[..count]);
    }
    wire
}

fn binding(wire: &[u8], input: &StorageReadControl) -> Budgeted<FieldAnalyzerBinding> {
    FieldAnalyzerBinding::read_image(&mut &*wire, &AnalyzerResources::default(), input).unwrap()
}

#[test]
fn admitted_field_catalog_preserves_names_revisions_and_independent_mutation_retirement() {
    let wire = wire();
    let input = StorageReadControl::with_limit(4 << 20);
    let key = key("public.saved", "body", &input);
    let key_pointer = key.0.as_ptr();
    let binding = binding(&wire, &input);
    let name_pointer = binding.index.name.as_ref().unwrap().as_ptr();
    let index = Arc::downgrade(&binding.index.compiled);
    let search = Arc::downgrade(&binding.search.compiled);
    let mut catalog = FieldAnalyzerCatalog::new(input.memory())
        .insert_image(key, binding, &input)
        .unwrap();
    let (key, binding) = catalog.iter().next().unwrap();
    assert_eq!(key.0.as_ptr(), key_pointer);
    assert_eq!(binding.index.name.as_ref().unwrap().as_ptr(), name_pointer);
    assert_eq!(binding.index.compiled.analyze("a b").unwrap(), ["a", "b"]);
    assert_eq!(binding.search.compiled.analyze("a b").unwrap(), ["a b"]);
    let retained = catalog.clone();
    let bytes = input.memory().used();
    let runtime = MemoryBudget::new(4 << 20);
    catalog = catalog.into_mutation_budget(&runtime);
    input.cancellation().cancel();
    let full = input
        .memory()
        .reserve(input.memory().limit() - bytes)
        .unwrap();
    catalog
        .remove(&("public.saved".into(), "body".into()))
        .unwrap();
    assert!(catalog.is_empty());
    assert_eq!(retained.len(), 1);
    let seek = StorageReadControl::with_limit(1024);
    let names: Vec<_> = retained
        .durable_fields_with_control("public.saved", &seek)
        .unwrap()
        .map(|(field, binding)| (field, binding.last_phase))
        .collect();
    assert_eq!(names, [("body", AnalyzerPhase::Search)]);
    assert_eq!(seek.memory().used(), 0);
    assert!(retained
        .durable_fields_with_control("public.saved", &StorageReadControl::with_limit(0))
        .is_err());
    drop((retained, catalog, full));
    assert!(index.upgrade().is_none());
    assert!(search.upgrade().is_none());
    assert_eq!(input.memory().used(), 0);
    assert_eq!(runtime.used(), 0);
}

#[test]
fn rejected_field_input_consumes_candidate_and_keeps_no_partial_image_or_runtime_root() {
    let wire = wire();
    let input = StorageReadControl::with_limit(4 << 20);
    let catalog = FieldAnalyzerCatalog::new(input.memory())
        .insert_image(
            key("public.saved", "body", &input),
            binding(&wire, &input),
            &input,
        )
        .unwrap();
    assert!(catalog
        .insert_image(
            key("public.saved", "body", &input),
            binding(&wire, &input),
            &input,
        )
        .is_err());
    assert_eq!(input.memory().used(), 0);
    let foreign = StorageReadControl::with_limit(4 << 20);
    assert!(FieldAnalyzerCatalog::new(input.memory())
        .insert_image(
            key("public.saved", "body", &foreign),
            binding(&wire, &input),
            &input,
        )
        .is_err());
    assert_eq!(input.memory().used(), 0);
    assert_eq!(foreign.memory().used(), 0);
    let key = key("public.saved", "body", &input);
    let binding = binding(&wire, &input);
    input.cancellation().cancel();
    assert!(FieldAnalyzerCatalog::new(input.memory())
        .insert_image(key, binding, &input)
        .is_err());
    assert_eq!(input.memory().used(), 0);
}
