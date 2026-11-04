//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn row(name: impl Into<String>) -> CatalogIndexRow {
    CatalogIndexRow {
        relation: RelationIdentity::new("Quoted.Schema", name),
        table_name: "\"Quoted.Schema\".\"Table.Name\"".into(),
        index_type: "btree".into(),
        columns_json: "[ \"id\" ]".into(),
        parameters_json: "{ \"x\": 1.00, \"x\": 2 }".into(),
        definition_json: Some("{ \"opaque\": true }".into()),
    }
}

#[test]
fn native_index_capture_has_constant_allocation_and_retains_exact_old_rows() {
    for count in [1, 64, 4096] {
        let memory = MemoryBudget::new(64 << 20);
        let mut source = CatalogIndexes::with_budget(&memory);
        for index in 0..count {
            source
                .insert(row(format!("i{index:05}")), RelationPersistence::Unlogged)
                .unwrap();
        }
        let first = RelationIdentity::new("Quoted.Schema", "i00000");
        let control = StorageReadControl::with_limit(64 << 20);
        let mut saved = None;
        let allocation =
            allocation_counter::measure(|| saved = Some(source.capture(&control).unwrap()));
        assert_eq!(allocation.count_total, 0);
        let saved = saved.unwrap();
        let required = control.memory().used();
        let mut temporary = row("x".repeat(1 << 20));
        temporary.table_name = "public.actual_temporary".into();
        temporary.parameters_json = "x".repeat(1 << 20);
        source
            .insert(temporary, RelationPersistence::Temporary)
            .unwrap();
        let exact = StorageReadControl::with_limit(required);
        let independent = source.capture(&exact).unwrap();
        assert_eq!(exact.memory().used(), required);
        assert_eq!(saved.len(), count);
        assert_eq!(independent.len(), count);
        for (key, original) in saved.iter() {
            assert!(std::ptr::eq(original, source.get(key).unwrap()));
            assert!(std::ptr::eq(original, independent.get(key).unwrap()));
        }
        let denied = StorageReadControl::with_limit(required - 1);
        assert!(source.capture(&denied).is_err());
        assert_eq!(denied.memory().used(), 0);
        let mut changed = row("i00000");
        changed.columns_json = "changed".into();
        source.replace(changed).unwrap();
        assert_eq!(saved.get(&first).unwrap().columns_json, "[ \"id\" ]");
        assert_eq!(
            saved.get(&first).unwrap().parameters_json,
            "{ \"x\": 1.00, \"x\": 2 }"
        );
        assert_eq!(
            saved.get(&first).unwrap().definition_json.as_deref(),
            Some("{ \"opaque\": true }")
        );
        drop((source, saved));
        assert_eq!(control.memory().used(), 0);
        assert!(memory.used() > 0);
        drop(independent);
        assert_eq!(exact.memory().used(), 0);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_index_mutations_share_unrelated_payloads_and_preserve_failed_generations() {
    let memory = MemoryBudget::new(16 << 20);
    let mut source = CatalogIndexes::with_budget(&memory);
    for index in 0..128 {
        source
            .insert(row(format!("i{index:03}")), RelationPersistence::Permanent)
            .unwrap();
    }
    let mut large = row("unrelated");
    large.definition_json = Some("x".repeat(4 << 20));
    let large_key = large.relation.clone();
    source
        .insert(large, RelationPersistence::Permanent)
        .unwrap();
    let original = source.clone();
    let selected = RelationIdentity::new("Quoted.Schema", "i064");
    let allocated = allocation_counter::measure(|| {
        source
            .insert(row("i064"), RelationPersistence::Temporary)
            .unwrap();
        source
            .remove(&RelationIdentity::new("Quoted.Schema", "i001"))
            .unwrap();
    });
    assert!(allocated.bytes_total < 64 << 10, "{allocated:?}");
    assert!(allocated.count_total < 256, "{allocated:?}");
    assert!(std::ptr::eq(
        source.get(&large_key).unwrap(),
        original.get(&large_key).unwrap()
    ));
    assert!(original.is_durable(&selected));
    assert!(!source.is_durable(&selected));
    let previous = source.clone();
    let used = memory.used();
    let blocked = memory.reserve(memory.available()).unwrap();
    assert!(source
        .insert(row("i064"), RelationPersistence::Permanent)
        .is_err());
    assert!(source
        .remove(&RelationIdentity::new("Quoted.Schema", "i002"))
        .is_err());
    assert!(source.rename(&selected, row("renamed")).is_err());
    assert!(source
        .without_table("\"Quoted.Schema\".\"Table.Name\"")
        .is_err());
    assert_eq!(source.len(), previous.len());
    for (key, row) in previous.iter() {
        assert!(std::ptr::eq(row, source.get(key).unwrap()));
        assert_eq!(source.is_durable(key), previous.is_durable(key));
    }
    drop(blocked);
    assert_eq!(memory.used(), used);
    source.rename(&selected, row("renamed")).unwrap();
    let renamed = RelationIdentity::new("Quoted.Schema", "renamed");
    assert!(!source.is_durable(&renamed));
    assert!(source.get(&selected).is_none());
    assert!(previous.get(&selected).is_some());
    drop((source, previous, original));
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_index_restoration_keeps_actual_temporary_membership_and_ordered_observers() {
    let memory = MemoryBudget::new(64 << 10);
    let mut current = CatalogIndexes::with_budget(&memory);
    let mut temporary = row("temporary");
    temporary.relation = RelationIdentity::new("public", "temporary");
    current
        .insert(temporary.clone(), RelationPersistence::Temporary)
        .unwrap();
    current
        .insert(row("obsolete"), RelationPersistence::Permanent)
        .unwrap();
    let mut restored = CatalogIndexes::with_budget(&memory);
    let mut durable = row("durable");
    durable.relation = RelationIdentity::new("pg_temp_literal", "durable");
    restored
        .insert(durable.clone(), RelationPersistence::Unlogged)
        .unwrap();
    let joined = restored.with_temporary_from(&current).unwrap();
    assert_eq!(joined.len(), 2);
    assert!(std::ptr::eq(
        joined.get(&temporary.relation).unwrap(),
        current.get(&temporary.relation).unwrap()
    ));
    let control = StorageReadControl::with_limit(64 << 10);
    let captured = joined.capture(&control).unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured.iter().next().unwrap().0, &durable.relation);
    let mut iter = joined.iter();
    assert_eq!(iter.len(), 2);
    assert_eq!(iter.next_back().unwrap().0, &temporary.relation);
    assert_eq!(iter.next().unwrap().0, &durable.relation);
    assert_eq!(iter.len(), 0);
    assert!(iter.next_back().is_none());
    restored
        .insert(temporary.clone(), RelationPersistence::Permanent)
        .unwrap();
    assert!(restored.with_temporary_from(&current).is_err());
    assert!(CatalogIndexes::default()
        .with_temporary_from(&current)
        .is_err());
    current.clear_durable();
    assert_eq!(current.len(), 1);
    assert!(current.get(&temporary.relation).is_some());
    control.cancellation().cancel();
    assert!(joined.capture(&control).is_err());
    drop((captured, joined, restored, current));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_index_generation_charges_owned_keys_and_complete_encoded_payloads() {
    let mut measurements = Vec::new();
    for padding in [0, 1024] {
        let memory = MemoryBudget::new(1 << 20);
        let mut source = CatalogIndexes::with_budget(&memory);
        let allocation = allocation_counter::measure(|| {
            for index in 0..16 {
                let mut value = row(format!("i{index:03}"));
                let suffix = "x".repeat(padding + index);
                for field in [
                    &mut value.relation.schema,
                    &mut value.relation.name,
                    &mut value.table_name,
                    &mut value.index_type,
                    &mut value.columns_json,
                    &mut value.parameters_json,
                    value.definition_json.as_mut().unwrap(),
                ] {
                    field.push_str(&suffix);
                }
                source
                    .insert(value, RelationPersistence::Permanent)
                    .unwrap();
            }
        });
        measurements.push((allocation.bytes_current, memory.used()));
        let control = StorageReadControl::with_limit(1 << 20);
        let captured = source.capture(&control).unwrap();
        assert_eq!(control.memory().used(), memory.used());
        drop(source);
        assert!(memory.used() > 0);
        drop(captured);
        assert_eq!(memory.used(), 0);
        assert_eq!(control.memory().used(), 0);
    }
    // Equal cardinality keeps node layouts and excluded reference-count bookkeeping fixed; every separately allocated key and row field must fund its entire growth.
    assert_eq!(
        measurements[1].0 - measurements[0].0,
        i64::try_from(measurements[1].1 - measurements[0].1).unwrap()
    );
}
