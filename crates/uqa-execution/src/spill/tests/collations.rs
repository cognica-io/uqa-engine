//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected schema collations survive physical spill boundaries and exact sizing.

use super::*;
use uqa_sql::ast::{ColumnType, InternalRelationId};

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
