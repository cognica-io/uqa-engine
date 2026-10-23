//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn batch_spill_preserves_present_and_absent_record_identity() {
    let marker = uqa_sql::ast::InternalRelationId::allocate().column(0);
    let schema = RowSchema::with_qualified_types(
        "r",
        vec!["k".into()],
        vec![Some(uqa_sql::ColumnType::Integer)],
    )
    .with_nullable_relations(marker)
    .unwrap();
    let batch = Batch::from_physical_rows(
        schema.clone(),
        vec![
            PhysicalRow::from_values(vec![Value::Null, Value::Bool(true)]),
            PhysicalRow::from_values(vec![Value::Null, Value::Null]),
        ],
    );
    let expected_size = format::encoded_batch_size(&batch).unwrap();
    let mut spill = SpillBuffer::new(1);
    spill.push(batch).unwrap();
    assert!(spill.has_spilled());
    assert_eq!(spill.spilled_bytes(), expected_size);
    let mut reader = format::open_spill_reader(spill.spill_file.as_ref().unwrap(), None).unwrap();
    let record = format::read_bounded_spill_record(&mut reader, expected_size, "row presence")
        .unwrap()
        .unwrap();
    for end in 0..record.len() {
        assert!(
            format::decode_batch(&record[..end]).is_err(),
            "truncated at {end}"
        );
    }
    let restored = spill.drain_all().unwrap();
    let mut actual = Vec::new();
    for batch in restored {
        assert!(schema.has_same_physical_layout(&batch.schema));
        for row in &batch.rows {
            let view = batch.schema.view(row);
            actual.push(
                crate::eval_scalar(
                    &crate::ScalarExpr::QualifiedStar("r".into()),
                    &crate::ScalarEvalContext::from_row_lookup(&view, &[]),
                )
                .unwrap(),
            );
        }
    }
    assert_eq!(
        actual,
        vec![
            Value::Record(vec![("k".into(), Value::Null)].into()),
            Value::Null
        ]
    );
}
