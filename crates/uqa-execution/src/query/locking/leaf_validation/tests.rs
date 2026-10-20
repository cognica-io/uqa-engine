//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::Value;
use uqa_sql::{
    ast::{InternalRelationId, RetainedRowSource},
    ColumnType,
};

#[test]
fn retained_row_rechecks_copy_internal_values_and_null_extensions_by_identity() {
    let relation = InternalRelationId::allocate();
    let schema = RowSchema::with_internal_relation_metadata(
        relation,
        [
            (Some(ColumnType::Text), Some(950)),
            (Some(ColumnType::Integer), None),
        ],
    );
    let descriptor = RetainedRowSource::new(relation, schema.clone(), 2).unwrap();
    let prefix = RowSchema::with_types(vec!["unrelated".into()], vec![Some(ColumnType::Text)]);
    let candidate_schema = RowSchema::join(&prefix, &schema, std::iter::empty::<String>());
    let row = PhysicalRow::from_values(vec![
        Value::Str("unrelated".into()),
        Value::Str("retained".into()),
        Value::Int(7),
    ]);
    let (copied_schema, copied) =
        copy_retained_source_row(&descriptor, &candidate_schema, &row).unwrap();
    assert!(copied_schema.shares_index(&schema));
    assert_eq!(
        copied_schema.internal_collation(relation.column(0)),
        Some(950)
    );
    assert_eq!(
        copied.into_physical_values(),
        vec![Value::Str("retained".into()), Value::Int(7)]
    );
    let null_extended = RowSchema::project_with_sources(
        &RowSchema::default(),
        vec![],
        vec![
            (
                relation.column(0),
                Some(ColumnType::Text),
                uqa_sql::schema::ProjectedSlot::Input(None),
            ),
            (
                relation.column(1),
                Some(ColumnType::Integer),
                uqa_sql::schema::ProjectedSlot::Input(None),
            ),
        ],
        0,
        false,
    );
    let empty = PhysicalRow::from_values(vec![]);
    let (_, copied) = copy_retained_source_row(&descriptor, &null_extended, &empty).unwrap();
    assert_eq!(
        copied.into_physical_values(),
        vec![Value::Null, Value::Null]
    );
    assert!(copy_retained_source_row(&descriptor, &RowSchema::default(), &empty).is_err());
}
