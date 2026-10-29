//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn admitted_layout_matches_names_ambiguity_selected_types_and_collations() {
    let columns = [
        ("same", Some(ColumnType::Integer), None),
        ("same", Some(ColumnType::Text), Some(951)),
        ("quoted.column", None, None),
    ];
    let expected = RowSchema::with_column_metadata(
        Some("q.dot"),
        columns
            .iter()
            .map(|(name, ty, oid)| (name.to_string(), ty.clone(), *oid)),
    );
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let schema = RowSchema::with_column_metadata_with_control(
        Some("q.dot"),
        columns
            .iter()
            .map(|(name, ty, oid)| (*name, ty.as_ref(), *oid)),
        &control,
    )
    .unwrap();
    assert_eq!(schema, expected);
    assert_eq!(schema.column_slot("quoted.column"), Some(2));
    assert_eq!(schema.qualified_slot("q.dot", "quoted.column"), Some(2));
    assert!(schema.column_is_ambiguous("same"));
    assert!(schema.qualified_column_is_ambiguous("q.dot", "same"));
    assert_eq!(schema.column_collation(1), Some(951));
    let held = budget.used();
    assert!(held > 0);
    let allocation = allocation_counter::measure(|| {
        let clone = schema.clone();
        assert!(schema.shares_index(&clone));
        assert_eq!(budget.used(), held);
        assert_eq!(clone.qualified_slot("q.dot", "quoted.column"), Some(2));
    });
    assert_eq!(allocation.count_total, 0);
    let clone = schema.clone();
    drop(schema);
    assert_eq!(budget.used(), held);
    drop(clone);
    assert_eq!(budget.used(), 0);
}

#[test]
fn parameter_layout_preserves_internal_aliases_and_empty_relation_namespace() {
    let relation = InternalRelationId::allocate();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for columns in [
        vec![],
        vec![
            ("value", Some(ColumnType::Text)),
            ("", Some(ColumnType::Integer)),
        ],
    ] {
        let names = columns
            .iter()
            .map(|(name, _)| name.to_string())
            .collect::<Vec<_>>();
        let types = columns.iter().map(|(_, ty)| ty.clone()).collect::<Vec<_>>();
        let visible = RowSchema::with_qualified_types("routine", names, types.clone());
        let marked = RowSchema::with_internal_relation_types(relation, types);
        let expected = RowSchema::with_trailing_internal_aliases(&visible, &marked);
        let schema = RowSchema::with_parameter_metadata_with_control(
            "routine",
            relation,
            columns.iter().map(|(name, ty)| (*name, ty.as_ref())),
            &control,
        )
        .unwrap();
        assert_eq!(schema, expected);
        assert!(schema.has_qualifier("routine"));
        for position in 0..columns.len() {
            assert_eq!(
                schema.internal_slot(relation.column(position)),
                Some(position)
            );
        }
        drop(schema);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn admitted_constructor_transfers_column_names_and_releases_each_failure_prefix() {
    let column = "wide".repeat(16 * 1024);
    let token = CancellationToken::new();
    token.check().unwrap();
    for limit in [0, 64, 512, 2048, 8192] {
        let parent = MemoryBudget::new(limit + 17);
        let sibling = parent.reserve(17).unwrap();
        let budget = parent.child(limit);
        let control = ProductionControl::new(&budget, &token, &token);
        let measured = allocation_counter::measure(|| {
            let error = RowSchema::with_column_metadata_with_control(
                None,
                [("small", None, None), (column.as_str(), None, None)],
                &control,
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("53200"));
        });
        assert!(measured.bytes_total < 16 * 1024, "{limit}: {measured:?}");
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), 17);
        drop(sibling);
    }
    let budget = MemoryBudget::new(1 << 20);
    let control = ProductionControl::new(&budget, &token, &token);
    let mut names = ProductionVec::new(control);
    names
        .push_produced(control.copy_text("constant").unwrap())
        .unwrap();
    let names = names.finish().unwrap();
    let text_pointer = names[0].as_ptr();
    let schema = RowSchema::new_with_control(names, &control).unwrap();
    assert_eq!(schema.columns()[0].as_ptr(), text_pointer);
    assert_eq!(schema, RowSchema::new(vec!["constant".into()]));
    drop(schema);
    assert_eq!(budget.used(), 0);
}

#[test]
fn both_cancellation_scopes_precede_schema_allocation_and_keep_retained_input() {
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    control.check().unwrap();
    let sibling = budget.reserve(23).unwrap();
    for token in [&original, &invoking] {
        token.cancel();
        // Initialize the token's platform cancellation bookkeeping outside the measured producer.
        assert!(control.check().is_err());
        let measured = allocation_counter::measure(|| {
            assert_eq!(
                RowSchema::with_column_metadata_with_control(
                    None,
                    [("value", None, None)],
                    &control
                )
                .unwrap_err()
                .sqlstate(),
                Some("57014")
            );
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(budget.used(), 23);
        token.reset();
    }
    drop(sibling);
    assert_eq!(budget.used(), 0);
}
