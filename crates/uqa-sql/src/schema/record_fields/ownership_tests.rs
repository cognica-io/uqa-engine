//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl, ProductionVec},
    CancellationToken,
};

#[test]
fn field_and_descriptor_leases_follow_their_last_independent_reader() {
    let budget = MemoryBudget::new(1 << 20);
    let cancel = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancel, &cancel);
    let ty = ColumnType::Array(Box::new(ColumnType::Text));
    let fields = RecordFields::from_types_with_control([Some(&ty)], &control).unwrap();
    let field_bytes = budget.used();
    let mut names = ProductionVec::new(control);
    names
        .push_produced(control.copy_text("field").unwrap())
        .unwrap();
    let mut metadata = ProductionVec::new(control);
    metadata
        .push_copy(RecordFieldMetadata {
            collation_oid: 0,
            sql_unknown: false,
        })
        .unwrap();
    let descriptor = RecordDescriptor::from_parts_with_control(
        fields.clone(),
        Some(names.finish().unwrap()),
        metadata.finish().unwrap(),
        &control,
    )
    .unwrap();
    let clone = descriptor.clone();
    let descriptor_bytes = budget.used();
    assert!(descriptor_bytes > field_bytes);
    drop(descriptor);
    assert_eq!(budget.used(), descriptor_bytes);
    let retained_fields = clone.types.clone();
    assert!(retained_fields.shares_storage(&fields));
    drop(clone);
    assert_eq!(budget.used(), field_bytes);
    drop(fields);
    assert_eq!(budget.used(), field_bytes);
    drop(retained_fields);
    assert_eq!(budget.used(), 0);
}

#[test]
fn rejected_record_production_releases_completed_fields_and_preserves_siblings() {
    let budget = MemoryBudget::new(4096);
    let cancel = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancel, &cancel);
    let sibling = budget.reserve(3072).unwrap();
    let baseline = budget.used();
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(RecordFields::from_types_with_control(
            std::iter::repeat_n(Some(&ColumnType::Text), 4096),
            &control,
        ));
    });
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert!(allocation.bytes_total < 16 * 1024, "{allocation:?}");
    assert_eq!(budget.used(), baseline);
    drop(sibling);
    assert_eq!(budget.used(), 0);
}
