//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::CompositeTypeReference, ColumnType};

#[test]
fn named_view_schemas_preserve_collations_and_shared_composite_fields() {
    let fields: crate::schema::RecordFields = vec![Some(ColumnType::Text)].into();
    let input = RowSchema::with_types(
        vec!["c".into(), "conflict".into(), "pair".into()],
        vec![
            Some(ColumnType::Text),
            Some(ColumnType::Text),
            Some(ColumnType::Composite(CompositeTypeReference {
                schema: "public".into(),
                name: "pair".into(),
                oid: 40_001,
                relation_oid: 40_002,
                array_oid: 40_003,
            })),
        ],
    )
    .with_column_collations([Some(950), Some(0), None])
    .with_record_fields([(2, fields.clone())]);
    let output = named_view_schema(
        &input,
        &["renamed".into(), "indeterminate".into(), "value".into()],
    )
    .unwrap();
    assert_eq!(output.column_types(), input.column_types());
    assert_eq!(output.column_collation(0), Some(950));
    assert_eq!(output.column_collation(1), Some(0));
    assert_eq!(output.column_collation(2), Some(0));
    assert!(crate::schema::RecordFields::shares_storage(
        output.record_fields(2).unwrap(),
        &fields
    ));
    assert!(named_view_schema(&input, &["short".into()]).is_err());
}
