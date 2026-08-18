//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeMap;
use std::sync::Arc;

#[test]
fn qualified_schema_lookup_reads_positional_values() {
    let schema = RowSchema::with_identities(
        vec!["id".into(), "id".into()],
        vec![
            ColumnIdentity::qualified("orders", "id"),
            ColumnIdentity::qualified("customer", "id"),
        ],
        vec![None, None],
    );
    let row = PhysicalRow::from_values(vec![Value::Int(1), Value::Int(2)]);
    let view = schema.view(&row);
    assert_eq!(view.qualified_column("orders", "id"), Some(&Value::Int(1)));
    assert_eq!(
        view.qualified_column("customer", "id"),
        Some(&Value::Int(2))
    );
}

#[test]
fn duplicate_qualified_identity_is_ambiguous_but_star_keeps_both_positions() {
    let schema = RowSchema::with_identities(
        vec!["id".into(), "id".into()],
        vec![
            ColumnIdentity::qualified("nested", "id"),
            ColumnIdentity::qualified("nested", "id"),
        ],
        vec![None, None],
    );
    let row = PhysicalRow::from_values(vec![Value::Int(1), Value::Int(2)]);
    let view = schema.view(&row);

    assert!(view.qualified_column_is_ambiguous("nested", "id"));
    assert_eq!(view.qualified_column("nested", "id"), None);
    assert_eq!(
        schema.qualified_star_layout("nested"),
        vec![("id".into(), 0, None), ("id".into(), 1, None)]
    );
}

#[test]
fn named_map_exact_key_precedes_qualified_metadata_suffixes() {
    let schema = RowSchema::from_named_columns(vec!["id".into(), "old.id".into(), "new.id".into()]);
    let row = PhysicalRow::from_values(vec![Value::Int(2), Value::Int(1), Value::Int(2)]);
    let view = schema.view(&row);
    assert!(!view.column_is_ambiguous("id"));
    assert_eq!(view.column("id"), Some(&Value::Int(2)));

    let relational = RowSchema::with_identities(
        vec!["id".into(), "id".into()],
        vec![
            ColumnIdentity::unqualified("id"),
            ColumnIdentity::qualified("other", "id"),
        ],
        vec![None, None],
    );
    assert!(relational.column_is_ambiguous("id"));
}

#[test]
fn join_composes_fragments_without_cloning_values() {
    let left_schema = RowSchema::new(vec!["l.value".into()]);
    let right_schema = RowSchema::new(vec!["r.value".into()]);
    let output_schema = RowSchema::join(&left_schema, &right_schema, Vec::new());
    let left = PhysicalRow::from_values(vec![Value::Str("left".repeat(128))]);
    let right = PhysicalRow::from_values(vec![Value::Str("right".repeat(128))]);
    let left_fragment = Arc::clone(&left.fragments[0].values);
    let right_fragment = Arc::clone(&right.fragments[0].values);

    let joined = PhysicalRow::concat(&left, &right);

    assert_eq!(joined.fragment_count(), 2);
    assert!(Arc::ptr_eq(&joined.fragments[0].values, &left_fragment));
    assert!(Arc::ptr_eq(&joined.fragments[1].values, &right_fragment));
    let view = output_schema.view(&joined);
    assert_eq!(view.get("l.value"), left_fragment.first());
    assert_eq!(view.get("r.value"), right_fragment.first());
}

#[test]
fn lock_rows_clone_keeps_shared_fragments_and_concatenated_origins() {
    let left = PhysicalRow::from_values(vec![Value::Str("left".repeat(128))])
        .with_lock_origin(RowLockOrigin::new("accounts", "public.accounts", 1));
    let right = PhysicalRow::from_values(vec![Value::Str("right".repeat(128))])
        .with_lock_origin(RowLockOrigin::new("owners", "public.owners", 2));
    let left_fragment = Arc::clone(&left.fragments[0].values);
    let right_fragment = Arc::clone(&right.fragments[0].values);
    let joined = PhysicalRow::concat(&left, &right);
    let locked = joined.clone();

    assert_eq!(locked.fragment_count(), 2);
    assert!(Arc::ptr_eq(&locked.fragments[0].values, &left_fragment));
    assert!(Arc::ptr_eq(&locked.fragments[1].values, &right_fragment));
    assert_eq!(locked.lock_origins().len(), 2);
    assert_eq!(locked.lock_origins()[0].doc_id, 1);
    assert_eq!(locked.lock_origins()[1].doc_id, 2);
}

#[test]
fn rebind_lock_origin_qualifiers_keeps_storage_names() {
    let row = PhysicalRow::from_values(vec![Value::Int(1)]).with_lock_origin(RowLockOrigin::new(
        "accounts",
        "public.accounts",
        7,
    ));
    let rebound = row.rebind_lock_origin_qualifiers(Arc::<str>::from("balances"));
    assert_eq!(rebound.lock_origins()[0].qualifier.as_ref(), "balances");
    assert_eq!(
        rebound.lock_origins()[0].storage_name.as_ref(),
        "public.accounts"
    );
    assert_eq!(rebound.lock_origins()[0].doc_id, 7);
}

#[test]
fn selection_renames_by_remapping_slots() {
    let input = RowSchema::new(vec!["source".into(), "value".into()]);
    let output = RowSchema::select(&input, &[("renamed".into(), "value".into())]);
    let row = PhysicalRow::from_values(vec![Value::Int(1), Value::Int(2)]);
    assert_eq!(output.view(&row).get("renamed"), Some(&Value::Int(2)));
    assert_eq!(output.physical_width(), 2);
}

#[test]
fn shared_storage_projection_remaps_without_cloning_values() {
    let stored = Arc::new(vec![Value::Str("alpha".repeat(64)), Value::Int(7)]);
    let row = PhysicalRow::from_shared_values(Arc::clone(&stored), Arc::from([1, NULL_SLOT, 0]));
    let schema = RowSchema::new(vec!["number".into(), "missing".into(), "text".into()]);
    let view = schema.view(&row);

    assert!(Arc::ptr_eq(&row.fragments[0].values, &stored));
    assert_eq!(view.get("number"), Some(&Value::Int(7)));
    assert_eq!(view.get("missing"), Some(&Value::Null));
    assert_eq!(view.get("text"), stored.first());
}

#[test]
fn lookup_aliases_share_a_slot_without_becoming_output_columns() {
    let input = RowSchema::new(vec!["id".into()]);
    let schema =
        RowSchema::with_identity_aliases(&input, &[(ColumnIdentity::qualified("orders", "id"), 0)]);
    assert!(schema.has_qualifier("orders"));
    assert!(!schema.has_qualifier("missing"));
    let row = PhysicalRow::from_values(vec![Value::Int(7)]);
    let view = schema.view(&row);

    assert_eq!(schema.columns(), ["id"]);
    assert_eq!(schema.physical_width(), 1);
    assert_eq!(view.get("orders.id"), None);
    assert_eq!(view.qualified_column("orders", "id"), Some(&Value::Int(7)));
    assert_eq!(
        view.to_result_row(),
        BTreeMap::from([("id".into(), Value::Int(7))])
    );
}

#[test]
fn canonical_projection_preserves_public_columns_and_hidden_alias_slots() {
    let input = RowSchema::new(vec!["value".into()]);
    let aliased = RowSchema::with_identity_aliases(
        &input,
        &[(ColumnIdentity::qualified("source", "value"), 0)],
    );
    let projected = RowSchema::append(&aliased, &["value".into()]);
    let source = PhysicalRow::from_values(vec![Value::Str("source".into())]);
    let source_values = Arc::clone(&source.fragments[0].values);
    let source = source.append_values(vec![Value::Str("projected".into())]);
    let projected_values = Arc::clone(&source.fragments[1].values);

    let (canonical, slots) = projected.canonical_projection();
    let row = source.project_slots(&slots);
    let view = canonical.view(&row);

    assert_eq!(canonical.columns(), ["value"]);
    assert_eq!(canonical.physical_width(), 2);
    assert_eq!(view.get("value"), Some(&Value::Str("projected".into())));
    assert_eq!(
        view.qualified_column("source", "value"),
        Some(&Value::Str("source".into()))
    );
    assert!(Arc::ptr_eq(&row.fragments[0].values, &projected_values));
    assert!(Arc::ptr_eq(&row.fragments[1].values, &source_values));
}

#[test]
fn result_materialization_happens_only_at_explicit_boundary() {
    let schema = RowSchema::new(vec!["a".into(), "b".into()]);
    let batch = Batch::from_physical_rows(
        schema,
        vec![PhysicalRow::from_values(vec![Value::Int(1), Value::Int(2)])],
    );
    assert_eq!(
        batch.into_result_rows(),
        vec![BTreeMap::from([
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Int(2)),
        ])]
    );
}

#[test]
fn result_materialization_moves_uniquely_owned_values() {
    let payload = "owned payload".repeat(32);
    let payload_address = payload.as_ptr();
    let batch = Batch::from_physical_rows(
        RowSchema::new(vec!["value".into()]),
        vec![PhysicalRow::from_values(vec![Value::Str(payload)])],
    );

    let rows = batch.into_result_rows();
    let Value::Str(value) = &rows[0]["value"] else {
        panic!("materialized value must remain text");
    };
    assert_eq!(value.as_ptr(), payload_address);
}

#[test]
fn consuming_prefix_reuses_unique_fragment_and_drops_suffix() {
    let source = PhysicalRow::from_values(vec![
        Value::Str("payload".repeat(32)),
        Value::Int(7),
        Value::Int(11),
    ]);
    let storage_address = Arc::as_ptr(&source.fragments[0].values);

    let prefix = source.into_prefix(1);

    assert_eq!(Arc::as_ptr(&prefix.fragments[0].values), storage_address);
    assert_eq!(prefix.fragments[0].values.len(), 1);
    assert!(prefix.fragments[0].projection.is_none());
}

#[test]
fn result_materialization_moves_prefix_projected_values() {
    let payload = "sorted payload".repeat(32);
    let payload_address = payload.as_ptr();
    let source = PhysicalRow::from_values(vec![Value::Str(payload), Value::Int(7)]);
    let projected = source.project_slots(&[0]);
    drop(source);
    let batch = Batch::from_physical_rows(RowSchema::new(vec!["value".into()]), vec![projected]);

    let rows = batch.into_result_rows();
    let Value::Str(value) = &rows[0]["value"] else {
        panic!("materialized value must remain text");
    };
    assert_eq!(value.as_ptr(), payload_address);
}

#[test]
fn result_materialization_reads_shared_projection_without_changing_storage() {
    let stored = Arc::new(vec![
        Value::Str("unused".into()),
        Value::Str("selected".repeat(32)),
        Value::Int(7),
    ]);
    let row = PhysicalRow::from_shared_values(Arc::clone(&stored), Arc::from([2, 1]));
    let batch = Batch::from_physical_rows(
        RowSchema::new(vec!["number".into(), "text".into()]),
        vec![row],
    );

    assert_eq!(
        batch.into_result_rows(),
        vec![BTreeMap::from([
            ("number".into(), Value::Int(7)),
            ("text".into(), Value::Str("selected".repeat(32))),
        ])]
    );
    assert_eq!(stored[0], Value::Str("unused".into()));
}

#[test]
fn result_materialization_preserves_non_identity_schema_mapping() {
    let source = RowSchema::new(vec!["left".into()]);
    let source = RowSchema::append(&source, &["right".into()]);
    let selected = RowSchema::select(&source, &[("renamed".into(), "right".into())]);
    let payload = "remapped payload".repeat(32);
    let payload_address = payload.as_ptr();
    let batch = Batch::from_physical_rows(
        selected,
        vec![PhysicalRow::from_values(vec![Value::Int(1)]).append_values(vec![Value::Str(payload)])],
    );

    let rows = batch.into_result_rows();
    let Value::Str(value) = &rows[0]["renamed"] else {
        panic!("materialized value must remain text");
    };
    assert_eq!(value.as_ptr(), payload_address);
}

#[test]
fn result_materialization_clones_only_duplicate_physical_slots() {
    let source = RowSchema::new(vec!["value".into()]);
    let selected = RowSchema::select(
        &source,
        &[
            ("first".into(), "value".into()),
            ("second".into(), "value".into()),
        ],
    );
    let batch = Batch::from_physical_rows(
        selected,
        vec![PhysicalRow::from_values(vec![Value::Str("shared".into())])],
    );

    assert_eq!(
        batch.into_result_rows(),
        vec![BTreeMap::from([
            ("first".into(), Value::Str("shared".into())),
            ("second".into(), Value::Str("shared".into())),
        ])]
    );
}

#[test]
fn remapped_result_materialization_keeps_the_last_duplicate_label() {
    let source = RowSchema::new(vec!["first".into(), "unused".into(), "last".into()]);
    let selected = RowSchema::select(
        &source,
        &[
            ("value".into(), "first".into()),
            ("value".into(), "last".into()),
        ],
    );
    let batch = Batch::from_physical_rows(
        selected,
        vec![PhysicalRow::from_values(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ])],
    );

    assert_eq!(
        batch.into_result_rows(),
        vec![BTreeMap::from([("value".into(), Value::Int(3))])]
    );
}
