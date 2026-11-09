//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{TableMetadata, TableMetadataValue};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

mod image;

fn capture_bytes<T: TableMetadataValue>(value: &TableMetadata<T>, bytes: usize) {
    let memory = MemoryBudget::new(bytes);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let captured = value.capture_with_control(&control).unwrap();
    assert_eq!(memory.used(), bytes);
    assert!(value.snapshot().shares_definition(&captured.snapshot()));
    drop(captured);
    assert_eq!(memory.used(), 0);
    let denied = MemoryBudget::new(bytes - 1);
    assert!(value
        .capture_with_control(&ProductionControl::new(&denied, &original, &invoking))
        .is_err());
    assert_eq!(denied.used(), 0);
    invoking.cancel();
    assert!(value.capture_with_control(&control).is_err());
    let invoking = CancellationToken::new();
    original.cancel();
    assert!(value
        .capture_with_control(&ProductionControl::new(&memory, &original, &invoking))
        .is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn table_metadata_capture_shares_native_capacities_without_population_allocation() {
    let mut names = Vec::with_capacity(4096);
    for _ in 0..2048 {
        names.push(String::with_capacity(512));
    }
    let bytes = size_of::<Vec<String>>()
        + names.capacity() * size_of::<String>()
        + names.iter().map(String::capacity).sum::<usize>();
    let value = TableMetadata::new(names);
    capture_bytes(&value, bytes);
    let memory = MemoryBudget::new(bytes);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut captured = None;
    let allocation = allocation_counter::measure(|| {
        captured = Some(value.capture_with_control(&control).unwrap());
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    drop(value);
    assert_eq!(captured.as_ref().unwrap().len(), 2048);
    assert_eq!(memory.used(), bytes);
    drop(captured);
    assert_eq!(memory.used(), 0);
}

#[test]
fn table_metadata_publication_keeps_original_snapshots_and_recounts_detached_capacities() {
    let mut text = String::with_capacity(1024);
    text.push_str("original");
    let mut value = TableMetadata::new(Some(text));
    let original = value.clone();
    let first = value.snapshot();
    {
        let mut write = TableMetadata::write(&mut value);
        write.as_mut().unwrap().push_str(" replacement");
    }
    assert_eq!(original.as_deref(), Some("original"));
    assert_eq!(value.as_deref(), Some("original replacement"));
    assert!(!first.shares_definition(&value.snapshot()));
    capture_bytes(&original, size_of::<Option<String>>() + 1024);
    let bytes = size_of::<Option<String>>() + value.as_ref().unwrap().capacity();
    capture_bytes(&value, bytes);
    let retained = value.snapshot();
    {
        let write = TableMetadata::write(&mut value);
        assert!(write.is_some());
    }
    assert!(retained.shares_definition(&value.snapshot()));
    value = original.clone();
    assert!(first.shares_definition(&value.snapshot()));
}

#[test]
fn table_metadata_write_publishes_capacities_during_unwind_and_fails_closed_when_forgotten() {
    let mut value = TableMetadata::new(None::<String>);
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut write = TableMetadata::write(&mut value);
        *write = Some(String::with_capacity(4096));
        panic!("writer unwind");
    }));
    assert!(unwind.is_err());
    capture_bytes(&value, size_of::<Option<String>>() + 4096);
    let mut write = TableMetadata::write(&mut value);
    *write = Some(String::with_capacity(8192));
    std::mem::forget(write);
    assert!(value
        .capture_with_control(&ProductionControl::uncontrolled())
        .is_err());
    *TableMetadata::write(&mut value) = None;
    capture_bytes(&value, size_of::<Option<String>>());
}

#[test]
fn table_metadata_constraints_charge_all_nested_names_and_vector_capacities() {
    use crate::ast::{
        ForeignKey, ReferencedPartitionConstraint, TableKeyConstraint, TableKeyConstraintKind,
    };
    let key = TableKeyConstraint {
        catalog_identity: None,
        index_identity: None,
        name: Some("key".into()),
        kind: TableKeyConstraintKind::Unique,
        columns: vec!["id".into()],
        included_columns: vec!["included".into()],
        nulls_not_distinct: true,
        without_overlaps: false,
    };
    let mut keys = Vec::with_capacity(4);
    let nested = key.name.as_ref().unwrap().capacity()
        + key.columns.capacity() * size_of::<String>()
        + key.columns[0].capacity()
        + key.included_columns.capacity() * size_of::<String>()
        + key.included_columns[0].capacity();
    keys.push(key);
    let bytes = size_of::<Vec<TableKeyConstraint>>()
        + keys.capacity() * size_of::<TableKeyConstraint>()
        + nested;
    capture_bytes(&TableMetadata::new(keys), bytes);
    let foreign: ForeignKey = serde_json::from_str(r#"{"referenced_key":"pk","name":"fk","local_columns":["local"],"ref_table":"public.parent","ref_columns":["id"],"on_delete_set_columns":["local"],"referenced_partitions":[]}"#).unwrap();
    let mut foreign = foreign;
    foreign.referenced_partitions = Vec::<ReferencedPartitionConstraint>::with_capacity(7);
    foreign
        .referenced_partitions
        .push(ReferencedPartitionConstraint {
            partition: [7; 16],
            parent: Some([3; 16]),
            name: "derived_partition_constraint".into(),
            catalog_identity: crate::ast::ConstraintCatalogIdentity {
                object_id: [9; 16],
                oid: 1234,
            },
            validated: false,
        });
    let nested = foreign.referenced_key.as_ref().unwrap().capacity()
        + foreign.name.as_ref().unwrap().capacity()
        + foreign.ref_table.capacity()
        + [
            &foreign.local_columns,
            &foreign.ref_columns,
            &foreign.on_delete_set_columns,
        ]
        .iter()
        .map(|names| {
            names.capacity() * size_of::<String>()
                + names.iter().map(String::capacity).sum::<usize>()
        })
        .sum::<usize>()
        + foreign.referenced_partitions.capacity() * size_of::<ReferencedPartitionConstraint>()
        + foreign.referenced_partitions[0].name.capacity();
    let foreign = vec![foreign];
    let bytes =
        size_of::<Vec<ForeignKey>>() + foreign.capacity() * size_of::<ForeignKey>() + nested;
    capture_bytes(&TableMetadata::new(foreign), bytes);
}

#[test]
fn table_metadata_dropped_attributes_preserve_original_owned_payloads() {
    use crate::catalog::relation_attributes::DroppedAttribute;
    let mut alignment = String::with_capacity(128);
    alignment.push('i');
    let attributes = vec![DroppedAttribute {
        number: 2,
        type_length: -1,
        type_modifier: -1,
        dimensions: 1,
        by_value: false,
        alignment,
        storage: "x".into(),
        collation: 100,
        identity: "d".into(),
        is_local: false,
        inheritance_count: 3,
    }];
    let bytes = size_of::<Vec<DroppedAttribute>>()
        + attributes.capacity() * size_of::<DroppedAttribute>()
        + attributes[0].alignment.capacity()
        + attributes[0].storage.capacity()
        + attributes[0].identity.capacity();
    let value = TableMetadata::new(attributes);
    capture_bytes(&value, bytes);
    assert_eq!(value[0].number, 2);
    assert_eq!(value[0].identity, "d");
    assert_eq!(
        serde_json::to_value(&value).unwrap(),
        serde_json::to_value(&*value).unwrap()
    );
}

mod authority;
