//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::schema::{ColumnIdentity, SchemaBuildMetadata};
use crate::{ast::InternalRelationId, ColumnType};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn expected(schema: &RowSchema, expression: &ScalarExpr) -> Vec<usize> {
    let slots: Vec<_> = match expression {
        ScalarExpr::Column(name) => schema.column_slot(name).map_or_else(
            || schema.qualified_star_slots(name).collect(),
            |slot| vec![slot],
        ),
        ScalarExpr::QualifiedColumn { qualifier, column } => schema
            .qualified_slot(qualifier, column)
            .into_iter()
            .collect(),
        ScalarExpr::QualifiedStar(qualifier) => schema.qualified_star_slots(qualifier).collect(),
        ScalarExpr::InternalColumn(column) => schema.internal_slot(*column).into_iter().collect(),
        ScalarExpr::Position(position) => schema.slot(*position).into_iter().collect(),
        ScalarExpr::Star => (0..schema.len())
            .filter(|position| schema.wildcard_position_visible(*position))
            .filter_map(|position| schema.slot(position))
            .collect(),
        _ => Vec::new(),
    };
    normalize(slots)
}

fn normalize(mut slots: Vec<usize>) -> Vec<usize> {
    slots.sort_unstable();
    slots.dedup();
    slots
}

fn joined_layouts() -> (RowSchema, RowSchema, InternalRelationId, InternalRelationId) {
    let left_relation = InternalRelationId::allocate();
    let right_relation = InternalRelationId::allocate();
    let mut metadata = SchemaBuildMetadata {
        exact_unqualified_precedence: true,
        ..SchemaBuildMetadata::default()
    };
    metadata.aliases.extend([
        (ColumnIdentity::qualified("l", "hidden"), 2),
        (ColumnIdentity::qualified("shared", "overridden"), 7),
        (ColumnIdentity::unqualified("fallback"), 3),
    ]);
    metadata.internal.insert(left_relation.column(0), 7);
    metadata.wildcard_hidden.insert(1);
    let left = RowSchema::from_typed_parts_with_aliases_and_exact_precedence(
        vec![
            "dup".into(),
            "id".into(),
            "gone".into(),
            "a.b".into(),
            "dup".into(),
        ],
        vec![
            ColumnIdentity::qualified("q", "dup"),
            ColumnIdentity::qualified("l", "id"),
            ColumnIdentity::qualified("l", "gone"),
            ColumnIdentity::unqualified("a.b"),
            ColumnIdentity::unqualified("dup"),
        ],
        vec![Some(ColumnType::Integer); 5],
        vec![4, 1, NULL_SLOT, 6, 5],
        8,
        metadata,
    );
    let mut metadata = SchemaBuildMetadata::default();
    metadata.aliases.extend([
        (ColumnIdentity::qualified("shared", "overridden"), NULL_SLOT),
        (ColumnIdentity::qualified("l", "gone"), 3),
        (ColumnIdentity::unqualified("fallback"), 4),
    ]);
    metadata.internal.insert(right_relation.column(0), 4);
    metadata.wildcard_hidden.insert(2);
    let right = RowSchema::from_typed_parts_with_aliases_and_exact_precedence(
        vec!["id".into(), "z".into(), "dup".into()],
        vec![
            ColumnIdentity::qualified("r", "id"),
            ColumnIdentity::qualified("r", "z"),
            ColumnIdentity::qualified("q", "dup"),
        ],
        vec![Some(ColumnType::Integer); 3],
        vec![2, 0, 1],
        5,
        metadata,
    );
    (left, right, left_relation, right_relation)
}

#[test]
fn joined_input_slots_preserve_ambiguity_hidden_aliases_and_physical_offsets() {
    let (left, right, left_relation, right_relation) = joined_layouts();
    let owned = RowSchema::join(&left, &right, []);
    assert_eq!(owned.column_slot("dup"), None);
    assert_eq!(owned.column_slot("fallback"), Some(12));
    assert_eq!(owned.qualified_slot("l", "gone"), Some(11));
    assert_eq!(owned.qualified_slot("shared", "overridden"), None);
    let memory = MemoryBudget::new(16384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let input = InputSlots::join(&left, &right, control).unwrap();
    assert_eq!(input.physical_width(), 13);
    let expressions = [
        ScalarExpr::Star,
        ScalarExpr::InternalColumn(left_relation.column(0)),
        ScalarExpr::InternalColumn(right_relation.column(0)),
    ]
    .into_iter()
    .chain((0..11).map(ScalarExpr::Position))
    .chain(["id", "dup", "a.b", "fallback", "l", "absent"].map(|s| ScalarExpr::Column(s.into())))
    .chain(["l", "r", "q", "shared"].map(|s| ScalarExpr::QualifiedStar(s.into())))
    .chain(
        [
            ("l", "id"),
            ("l", "gone"),
            ("q", "dup"),
            ("shared", "overridden"),
        ]
        .map(|(q, c)| ScalarExpr::QualifiedColumn {
            qualifier: q.into(),
            column: c.into(),
        }),
    );
    for expression in expressions {
        let mut actual = Vec::new();
        input
            .visit_reference(&expression, &mut |slot| {
                actual.push(slot);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            normalize(actual),
            expected(&owned, &expression),
            "{expression:?}"
        );
    }
    assert!(memory.used() > 0);
    drop(input);
    assert_eq!(memory.used(), 0);
}

#[test]
fn joined_input_indexes_borrow_payloads_and_admit_each_node_before_allocation() {
    let wide = "x".repeat(64 * 1024);
    let left = RowSchema::with_qualified_types("l", vec![wide.clone()], vec![None]);
    let right = RowSchema::with_qualified_types("r", vec![wide], vec![None]);
    let parent = MemoryBudget::new(8192);
    let sibling = parent.reserve(512).unwrap();
    let memory = parent.child(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(InputSlots::join(&left, &right, control).unwrap());
    });
    assert!(allocation.bytes_total < 8192, "{allocation:?}");
    let input = output.unwrap();
    let Layout::Join { visible, .. } = &input.layout else {
        panic!("joined")
    };
    for ((qualifier, name), _) in visible {
        assert!(
            std::ptr::eq(name.as_ptr(), left.identities()[0].column().as_ptr())
                || std::ptr::eq(name.as_ptr(), right.identities()[0].column().as_ptr())
        );
        assert!(qualifier.is_none() || *qualifier == Some("l") || *qualifier == Some("r"));
    }
    let retained = memory.used();
    drop(input);
    assert_eq!(parent.used(), sibling.bytes());
    let limited = parent.child(retained - 1);
    assert_eq!(
        InputSlots::join(
            &left,
            &right,
            ProductionControl::new(&limited, &token, &token)
        )
        .err()
        .unwrap()
        .sqlstate(),
        Some("53200")
    );
    assert_eq!(limited.used(), 0);
    assert_eq!(parent.used(), sibling.bytes());
}

#[test]
fn joined_input_observation_stops_on_failure_cancellation_and_unwind() {
    let left = RowSchema::with_qualified_types("l", vec!["a".into(), "b".into()], vec![None; 2]);
    let right = RowSchema::default();
    let parent = MemoryBudget::new(16384);
    let sibling = parent.reserve(512).unwrap();
    let memory = parent.child(8192);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let input = InputSlots::join(&left, &right, control).unwrap();
    let mut calls = 0;
    let error = input
        .visit_reference(&ScalarExpr::Star, &mut |_| {
            calls += 1;
            Err(SQLError::Routine {
                sqlstate: "P0001".into(),
                message: "stop".into(),
            })
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("P0001"));
    assert_eq!(calls, 1);
    drop(input);
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let input = InputSlots::join(&left, &right, control).unwrap();
        input
            .visit_reference(&ScalarExpr::Star, &mut |_| panic!("observer unwind"))
            .unwrap();
    }));
    assert!(unwind.is_err());
    assert_eq!(parent.used(), sibling.bytes());
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let input = InputSlots::join(&left, &right, control).unwrap();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            input
                .visit_reference(&ScalarExpr::Star, &mut |_| panic!("cancelled callback"))
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        drop(input);
        assert_eq!(
            InputSlots::join(&left, &right, control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(parent.used(), sibling.bytes());
    }
}
