//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected schema collations survive physical spill boundaries and exact sizing.

use super::*;
use uqa_sql::ast::{ColumnType, InternalRelationId};

#[test]
fn record_field_collations_survive_spill_and_legacy_row_bytes() {
    let row = Value::Row(
        uqa_core::RowValue::typed(
            vec![Value::Str("B".into()), Value::Null, Value::Str("a".into())],
            [Some(950), Some(0), None]
                .into_iter()
                .map(|collation_oid| uqa_core::RecordFieldType {
                    oid: 25,
                    type_modifier: -1,
                    collation_oid,
                })
                .collect(),
        )
        .unwrap(),
    );
    let batch = Batch::from_physical_rows(
        RowSchema::new(vec!["record".into()]),
        vec![PhysicalRow::from_values(vec![row.clone()])],
    );
    let expected = format::encoded_batch_size(&batch).unwrap();
    let mut buffer = SpillBuffer::new(0);
    buffer.push(batch).unwrap();
    assert_eq!(buffer.spilled_bytes(), expected);
    let mut reader = format::open_spill_reader(buffer.spill_file.as_ref().unwrap(), None).unwrap();
    let record = format::read_bounded_spill_record(&mut reader, expected, "row collations")
        .unwrap()
        .unwrap();
    for end in 0..record.len() {
        assert!(
            format::decode_batch(&record[..end]).is_err(),
            "truncated at {end}"
        );
    }
    let restored = buffer.drain_all().unwrap();
    assert!(restored[0].rows[0]
        .value(0)
        .unwrap()
        .has_same_representation(&row));

    // These are the historical map/row bytes, independent of the current encoder.
    for tag in [13, 19] {
        let mut bytes = vec![9];
        bytes.extend_from_slice(&1_u64.to_le_bytes());
        bytes.extend_from_slice(&3_u64.to_le_bytes());
        bytes.extend_from_slice(b"row");
        bytes.push(tag);
        if tag == 19 {
            bytes.extend_from_slice(&1_u64.to_le_bytes());
            bytes.extend_from_slice(&25_u32.to_le_bytes());
            bytes.extend_from_slice(&(-1_i32).to_le_bytes());
        }
        bytes.extend_from_slice(&1_u64.to_le_bytes());
        bytes.push(0);
        let (document, remaining) = format::decode_document(&bytes).unwrap();
        assert!(remaining.is_empty());
        let Value::Row(row) = &document["row"] else {
            panic!("row");
        };
        assert_eq!(row.values(), &[Value::Null]);
        if tag == 19 {
            assert_eq!(row.field_types().unwrap()[0].oid, 25);
            assert_eq!(row.field_types().unwrap()[0].collation_oid, None);
        } else {
            assert!(row.field_types().is_none());
        }
    }
}

#[test]
fn record_field_spill_rejects_invalid_collation_presence() {
    let mut bytes = vec![9];
    bytes.extend_from_slice(&1_u64.to_le_bytes());
    bytes.extend_from_slice(&3_u64.to_le_bytes());
    bytes.extend_from_slice(b"row");
    bytes.push(25);
    bytes.extend_from_slice(&1_u64.to_le_bytes());
    bytes.extend_from_slice(&25_u32.to_le_bytes());
    bytes.extend_from_slice(&(-1_i32).to_le_bytes());
    bytes.push(2);
    bytes.extend_from_slice(&1_u64.to_le_bytes());
    bytes.push(0);
    assert!(format::decode_document(&bytes)
        .unwrap_err()
        .to_string()
        .contains("invalid row field collation presence"));
}

#[test]
fn selected_collations_survive_spill_with_exact_record_accounting() {
    let internal = InternalRelationId::allocate().column(0);
    let schema = RowSchema::with_types(
        vec!["c".into(), "p".into(), "conflict".into()],
        vec![Some(ColumnType::Text); 3],
    )
    .with_column_collations([Some(950), Some(951), Some(0)]);
    let schema = RowSchema::with_identity_aliases(
        &schema,
        &[(ColumnIdentity::qualified("original", "p"), 1)],
    );
    let schema = RowSchema::with_physical_internal_aliases(
        &schema,
        &[(internal, 0, Some(ColumnType::Text))],
    );
    let batch = Batch::from_physical_rows(
        schema.clone(),
        vec![PhysicalRow::from_values(vec![
            Value::Str("a".into()),
            Value::Str("b".into()),
            Value::Null,
        ])
        .with_lock_origin(crate::RowLockOrigin::new("items", "public.items", 1))],
    );
    let expected = format::encoded_batch_size(&batch).unwrap();
    let mut buffer = SpillBuffer::new(0);
    buffer.push(batch).unwrap();
    assert_eq!(buffer.spilled_bytes(), expected);
    let mut reader = format::open_spill_reader(buffer.spill_file.as_ref().unwrap(), None).unwrap();
    let record = format::read_bounded_spill_record(&mut reader, expected, "collated batch")
        .unwrap()
        .unwrap();
    assert_eq!(record.len() + format::RECORD_PREFIX_BYTES, expected);
    for end in 0..record.len() {
        assert!(
            format::decode_batch(&record[..end]).is_err(),
            "truncated at {end}"
        );
    }
    let restored = buffer.drain_all().unwrap();
    assert_eq!(restored[0].schema, schema);
    assert_eq!(restored[0].rows[0].lock_origins().len(), 1);
    assert_eq!(
        restored[0].rows[0].lock_origins()[0].storage_name.as_ref(),
        "public.items"
    );
    assert_eq!(
        restored[0].schema.qualified_collation("original", "p"),
        Some(951)
    );
    assert_eq!(restored[0].schema.internal_collation(internal), Some(950));
    assert_eq!(restored[0].rows[0].value(0), Some(&Value::Str("a".into())));
    assert_eq!(restored[0].rows[0].value(1), Some(&Value::Str("b".into())));
    assert_eq!(restored[0].rows[0].value(2), Some(&Value::Null));
}
