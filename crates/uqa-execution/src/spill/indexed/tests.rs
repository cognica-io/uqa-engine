//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::{Seek as _, SeekFrom, Write as _};

use uqa_core::Value;
use uqa_sql::expr::RowLookup as _;

use super::*;
use crate::ColumnIdentity;

fn indexed_id_schema() -> RowSchema {
    RowSchema::new(vec!["id".into()])
}

fn indexed_id_row(value: i64) -> PhysicalRow {
    PhysicalRow::from_values(vec![Value::Int(value)])
}

#[test]
fn indexed_spill_reads_large_partitions_without_an_offset_vector() {
    let schema = RowSchema::new(vec!["id".into(), "payload".into()]);
    let mut spill = IndexedSpill::new(schema).unwrap();
    for value in 0..4096_i64 {
        spill
            .push(&PhysicalRow::from_values(vec![
                Value::Int(value),
                Value::List(vec![Value::Str(format!("row-{value}"))]),
            ]))
            .unwrap();
    }
    assert_eq!(spill.len(), 4096);
    assert!(spill.encoded_bytes() > 4096 * 8);
    for index in [4095_u64, 0, 2048, 17] {
        let row = spill.get(index).unwrap();
        assert_eq!(
            spill.row_schema().view(&row).get("id"),
            Some(&Value::Int(index as i64))
        );
    }
    assert!(spill.get(4096).unwrap_err().to_string().contains("outside"));
}

#[test]
fn indexed_spill_preserves_hidden_aliases_without_named_rows() {
    let input = RowSchema::new(vec!["value".into()]);
    let aliased = RowSchema::with_identity_aliases(
        &input,
        &[(ColumnIdentity::qualified("source", "value"), 0)],
    );
    let schema = RowSchema::append(&aliased, &["value".into()]);
    let row = PhysicalRow::from_values(vec![Value::Str("source".into())])
        .append_values(vec![Value::Str("projected".into())]);
    let mut spill = IndexedSpill::new(schema).unwrap();
    spill.push(&row).unwrap();

    let restored = spill.get(0).unwrap();
    let view = spill.row_schema().view(&restored);
    assert_eq!(view.get("value"), Some(&Value::Str("projected".into())));
    assert_eq!(
        view.qualified_column("source", "value"),
        Some(&Value::Str("source".into()))
    );
}

#[test]
fn indexed_spill_preserves_structural_internal_attributes() {
    let relation = uqa_sql::ast::InternalRelationId::allocate();
    let internal = relation.column(0);
    let schema = RowSchema::with_internal_relation_types(
        relation,
        vec![Some(uqa_sql::ast::ColumnType::BigInteger)],
    );
    let mut spill = IndexedSpill::new(schema).unwrap();
    spill
        .push(&PhysicalRow::from_values(vec![Value::Int(7)]))
        .unwrap();

    let restored = spill.get(0).unwrap();
    assert!(spill.row_schema().columns().is_empty());
    assert_eq!(spill.row_schema().physical_width(), 1);
    let slot = spill.row_schema().internal_slot(internal).unwrap();
    assert_eq!(restored.value(slot), Some(&Value::Int(7)));
}

#[test]
fn indexed_spill_retains_the_exact_input_physical_layout() {
    let input = RowSchema::new(vec!["discarded".into(), "id".into()]);
    let schema = RowSchema::select(&input, &[("id".into(), "id".into())]);
    let row = PhysicalRow::from_values(vec![Value::Str("unused".into()), Value::Int(7)]);
    let mut spill = IndexedSpill::new(schema.clone()).unwrap();
    spill.push(&row).unwrap();

    let restored = spill.get(0).unwrap();
    assert_eq!(spill.row_schema(), &schema);
    assert_eq!(spill.row_schema().physical_width(), 2);
    assert_eq!(
        spill.row_schema().view(&restored).get("id"),
        Some(&Value::Int(7))
    );
}

#[test]
fn indexed_spill_rejects_corrupt_length_before_payload_allocation() {
    let mut spill = IndexedSpill::new(indexed_id_schema()).unwrap();
    spill.push(&indexed_id_row(1)).unwrap();
    spill.data.as_file_mut().seek(SeekFrom::Start(0)).unwrap();
    spill
        .data
        .as_file_mut()
        .write_all(&u64::MAX.to_le_bytes())
        .unwrap();
    spill.data.as_file_mut().flush().unwrap();

    let error = spill.get(0).unwrap_err();
    assert!(error.to_string().contains("does not match record payload"));
}

#[test]
fn indexed_spill_rejects_corrupt_record_offsets() {
    let mut spill = IndexedSpill::new(indexed_id_schema()).unwrap();
    spill.push(&indexed_id_row(1)).unwrap();
    spill.push(&indexed_id_row(2)).unwrap();
    spill
        .offsets
        .as_file_mut()
        .seek(SeekFrom::Start(8))
        .unwrap();
    spill
        .offsets
        .as_file_mut()
        .write_all(&0_u64.to_le_bytes())
        .unwrap();
    spill.offsets.as_file_mut().flush().unwrap();

    let error = spill.get(0).unwrap_err();
    assert!(error.to_string().contains("record bounds"));
}

#[test]
fn indexed_spill_rejects_metadata_overflow_before_writing() {
    let row = indexed_id_row(1);

    let mut row_overflow = IndexedSpill::new(indexed_id_schema()).unwrap();
    row_overflow.rows = u64::MAX;
    let error = row_overflow.push(&row).unwrap_err();
    assert!(error.to_string().contains("row count overflow"));
    assert_eq!(row_overflow.data.as_file().metadata().unwrap().len(), 0);
    assert_eq!(row_overflow.offsets.as_file().metadata().unwrap().len(), 0);

    let mut byte_overflow = IndexedSpill::new(indexed_id_schema()).unwrap();
    byte_overflow.encoded_bytes = u64::MAX;
    let error = byte_overflow.push(&row).unwrap_err();
    assert!(error.to_string().contains("byte count overflow"));
    assert_eq!(byte_overflow.data.as_file().metadata().unwrap().len(), 0);
    assert_eq!(byte_overflow.offsets.as_file().metadata().unwrap().len(), 0);
}
