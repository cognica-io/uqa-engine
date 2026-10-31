//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::InternalRelationId;

fn source(name: &str) -> RowSchema {
    RowSchema::with_qualified_types(name, vec!["k".into()], vec![Some(ColumnType::Integer)])
}

#[test]
fn nullable_relations_keep_hidden_identity_through_join_projection_and_scope() {
    let marker = InternalRelationId::allocate().column(0);
    let input = source("r");
    let nullable = input.with_nullable_relations(marker).unwrap();
    assert_eq!(nullable.columns(), input.columns());
    assert_eq!(nullable.physical_width(), 2);
    assert_eq!(nullable.whole_row_presence("r"), Some(marker));
    assert!(nullable
        .with_nullable_relations(InternalRelationId::allocate().column(0))
        .is_none());
    let joined = RowSchema::join(&source("l"), &nullable, []);
    let (canonical, _) = joined.canonical_projection();
    assert_eq!(canonical.whole_row_presence("r"), Some(marker));
    assert_eq!(canonical.internal_slot(marker), Some(2));
    let outer = RowSchema::with_outer_schema(&source("local"), &nullable);
    assert_eq!(outer.whole_row_presence("r"), Some(marker));
    let shadowed = RowSchema::with_outer_schema(&source("r"), &nullable);
    assert_eq!(shadowed.whole_row_presence("r"), None);
    let alias = RowSchema::with_relation_qualifier(&canonical, "j");
    assert_eq!(alias.whole_row_presence("j"), None);
    assert!(!alias.has_qualifier("r"));
    assert!(alias
        .with_nullable_relations(InternalRelationId::allocate().column(0))
        .is_some());
    let selected = RowSchema::select(&nullable, &[("k".into(), "k".into())]);
    assert_eq!(selected.whole_row_presences().count(), 0);
}

#[test]
fn presence_relayout_uses_relation_identity_when_internal_ids_change() {
    let first = InternalRelationId::allocate().column(0);
    let second = InternalRelationId::allocate().column(0);
    let source = source("r");
    let input = source.with_nullable_relations(first).unwrap();
    let target = source.with_nullable_relations(second).unwrap();
    assert!(!input.has_same_physical_layout(&target));
    assert_eq!(input.relayout_slots(&target).unwrap(), vec![0, 1]);
}

#[test]
fn restored_presence_rejects_missing_wrong_type_and_duplicate_bindings() {
    let marker = InternalRelationId::allocate().column(0);
    let input = source("r");
    let binding = || [("r".into(), marker)];
    assert!(input.clone().with_whole_row_presences(binding()).is_err());
    let integer = RowSchema::append_internal_typed(&input, &[(marker, Some(ColumnType::Integer))]);
    assert!(integer.with_whole_row_presences(binding()).is_err());
    let boolean = RowSchema::append_internal_typed(&input, &[(marker, Some(ColumnType::Boolean))]);
    assert!(boolean
        .clone()
        .with_whole_row_presences(binding().into_iter().chain(binding()))
        .is_err());
    let restored = boolean.with_whole_row_presences(binding()).unwrap();
    assert_eq!(restored.whole_row_presence("r"), Some(marker));
}

#[test]
fn removing_internal_attributes_retires_only_their_presence_bindings() {
    let left_marker = InternalRelationId::allocate().column(0);
    let right_marker = InternalRelationId::allocate().column(0);
    let left = source("l").with_nullable_relations(left_marker).unwrap();
    let right = source("r").with_nullable_relations(right_marker).unwrap();
    let joined = RowSchema::join(&left, &right, []);
    let retained = RowSchema::without_internal_attributes(&joined, &[left_marker]);
    assert_eq!(retained.whole_row_presence("l"), None);
    assert_eq!(retained.whole_row_presence("r"), Some(right_marker));
    assert!(retained.internal_slot(left_marker).is_none());
    let (canonical, _) = retained.canonical_projection();
    assert_eq!(canonical.whole_row_presence("r"), Some(right_marker));
    assert!(canonical.internal_slot(right_marker).is_some());
    let removed = RowSchema::without_internal_attributes(&canonical, &[right_marker]);
    assert_eq!(removed.whole_row_presences().count(), 0);
    let unchanged = RowSchema::without_internal_attributes(&joined, &[]);
    assert!(joined.has_same_physical_layout(&unchanged));
}

#[test]
fn nullable_layout_admits_growth_and_keeps_shared_marker_owners() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    let memory = MemoryBudget::new(4 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let marker = InternalRelationId::allocate().column(0);
    let input = RowSchema::with_qualified_types(
        "r",
        vec!["x".repeat(1 << 20)],
        vec![Some(ColumnType::Integer)],
    );
    let occupied = memory.reserve(memory.available() - (32 << 10)).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(input.with_nullable_relations_with_control(marker, &control));
    });
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert!(allocation.bytes_total < 128 << 10, "{allocation:?}");
    assert_eq!(memory.used(), occupied.bytes());
    drop(occupied);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            input
                .with_nullable_relations_with_control(marker, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
        token.reset();
    }
    let input = source("r");
    let output = input
        .with_nullable_relations_with_control(marker, &control)
        .unwrap()
        .unwrap();
    let expected = RowSchema::append_internal_typed(&input, &[(marker, Some(ColumnType::Boolean))])
        .with_whole_row_presences([("r".into(), marker)])
        .unwrap();
    assert_eq!(output, expected);
    let held = memory.used();
    assert!(held > 0);
    let clone = output.clone();
    drop(output);
    assert_eq!(memory.used(), held);
    assert!(clone
        .with_nullable_relations_with_control(InternalRelationId::allocate().column(0), &control)
        .unwrap()
        .is_none());
    assert_eq!(memory.used(), held);
    let nested = RowSchema::join_with_control(&source("l"), &clone, [], &control).unwrap();
    let next_marker = InternalRelationId::allocate().column(0);
    let complete = nested
        .with_nullable_relations_with_control(next_marker, &control)
        .unwrap()
        .unwrap();
    drop((clone, nested));
    assert_eq!(complete.whole_row_presence("r"), Some(marker));
    assert_eq!(complete.whole_row_presence("l"), Some(next_marker));
    assert_eq!(complete.internal_slot(marker), Some(2));
    assert_eq!(complete.internal_slot(next_marker), Some(3));
    assert!(memory.used() > 0);
    drop(complete);
    assert_eq!(memory.used(), 0);
}
