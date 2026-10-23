//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Spilled physical rows retain the original SQL binding scope and descriptors.

use super::*;
use uqa_sql::ast::{ColumnType, CompositeTypeReference};
use uqa_sql::schema::ScalarTypeSchema;

fn named_row_type() -> ColumnType {
    ColumnType::Composite(CompositeTypeReference {
        schema: "public".into(),
        name: "items".into(),
        oid: 20_000,
        array_oid: 20_001,
        relation_oid: 20_002,
    })
}

#[test]
fn spill_readers_retain_named_rows_and_record_field_descriptors() {
    let row_type = named_row_type();
    let fields = Arc::from([Some(ColumnType::SmallInteger), Some(ColumnType::Text)]);
    let schema = RowSchema::with_qualified_types(
        "items",
        vec!["value".into()],
        vec![Some(ColumnType::Record)],
    )
    .with_relation_row_type("items", row_type.clone())
    .unwrap()
    .with_record_fields([(0, Arc::clone(&fields))]);
    let values = vec![Value::Record(
        vec![("n".into(), Value::Int(42)), ("s".into(), Value::Null)].into(),
    )];
    for budget in [0, usize::MAX] {
        for mode in 0..3 {
            let mut buffer = SpillBuffer::new(budget);
            buffer
                .push(Batch::from_physical_rows(
                    schema.clone(),
                    vec![PhysicalRow::from_values(values.clone())],
                ))
                .unwrap();
            let batches = match mode {
                0 => buffer.drain_all().unwrap(),
                1 => buffer
                    .into_shared(schema.clone())
                    .unwrap()
                    .reader()
                    .unwrap()
                    .collect::<ExecResult<Vec<_>>>()
                    .unwrap(),
                _ => buffer
                    .into_shared(schema.clone())
                    .unwrap()
                    .into_reader()
                    .unwrap()
                    .collect::<ExecResult<Vec<_>>>()
                    .unwrap(),
            };
            assert_eq!(batches.len(), 1);
            assert_eq!(batches[0].schema, schema);
            assert_eq!(batches[0].schema.whole_row_type("items"), Some(&row_type));
            assert!(Arc::ptr_eq(
                batches[0].schema.record_fields(0).unwrap(),
                &fields
            ));
            assert_eq!(batches[0].rows[0].value(0), Some(&values[0]));
        }
    }
}

#[test]
fn empty_relation_identity_survives_spill_without_a_value_slot() {
    let schema = RowSchema::new(Vec::new())
        .with_relation_row_type("items", named_row_type())
        .unwrap();
    let row = PhysicalRow::from_values(Vec::new());
    assert!(validate_decoded_schema(
        Batch::from_physical_rows(RowSchema::new(Vec::new()), vec![row.clone()]),
        Some(&schema),
    )
    .is_err());
    let mut buffer = SpillBuffer::new(0);
    buffer
        .push(Batch::from_physical_rows(schema.clone(), vec![row]))
        .unwrap();
    let batches = buffer.drain_all().unwrap();
    assert_eq!(batches[0].schema, schema);
    assert_eq!(batches[0].rows.len(), 1);
    assert_eq!(batches[0].rows[0].value(0), None);
    assert!(batches[0].schema.has_qualifier("items"));
}

#[test]
fn anonymous_empty_namespace_survives_spill_without_a_named_type() {
    let schema = RowSchema::with_qualified_types("q", Vec::new(), Vec::new());
    for shared in [false, true] {
        let mut buffer = SpillBuffer::new(0);
        buffer
            .push(Batch::from_physical_rows(
                schema.clone(),
                vec![PhysicalRow::from_values(Vec::new())],
            ))
            .unwrap();
        let batches = if shared {
            buffer
                .into_shared(schema.clone())
                .unwrap()
                .reader()
                .unwrap()
                .collect::<ExecResult<Vec<_>>>()
                .unwrap()
        } else {
            buffer.drain_all().unwrap()
        };
        assert_eq!(batches.len(), 1);
        assert!(batches[0].schema.shares_index(&schema));
        assert!(batches[0].schema.has_qualifier("q"));
        assert_eq!(batches[0].schema.whole_row_type("q"), None);
        assert_eq!(batches[0].rows.len(), 1);
        assert_eq!(batches[0].rows[0].value(0), None);
    }
}

#[test]
fn restored_schema_rejects_different_physical_types_aliases_and_collations() {
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(ColumnType::Text)])
        .with_column_collations([Some(950)]);
    for other in [
        RowSchema::with_types(vec!["value".into()], vec![Some(ColumnType::Integer)]),
        schema.clone().with_column_collations([Some(951)]),
        RowSchema::with_identity_aliases(
            &schema,
            &[(ColumnIdentity::qualified("unexpected", "value"), 0)],
        ),
    ] {
        let error = restore_decoded_schema(
            Batch::from_physical_rows(other, vec![PhysicalRow::from_values(vec![Value::Null])]),
            Some(&schema),
        )
        .unwrap_err();
        assert!(error.to_string().contains("spill batch schema mismatch"));
    }
}
