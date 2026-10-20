//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::output::QueryRows;
use crate::spill::SpillBatchWriter;
use crate::SpillBuffer;

#[test]
fn view_rule_inputs_bound_live_payload_and_retain_row_specific_types() {
    let allocated = allocation_counter::measure(|| {
        let mut buffer = ViewInputBuffer::new(16 * 1024, CancellationToken::new());
        for id in 0..512 {
            buffer
                .push(vec![
                    (Value::Int(id), Some(ColumnType::Integer)),
                    (
                        Value::Str("x".repeat(32768)),
                        Some(ColumnType::Varchar(Some(32768 + id as u32))),
                    ),
                    (Value::Null, None),
                ])
                .unwrap();
        }
        let mut count = 0;
        for (id, row) in buffer.finish().enumerate() {
            let row = row.unwrap();
            assert_eq!(row[0], (Value::Int(id as i64), Some(ColumnType::Integer)));
            assert_eq!(row[1].1, Some(ColumnType::Varchar(Some(32768 + id as u32))));
            let Value::Str(body) = &row[1].0 else {
                panic!("body")
            };
            assert_eq!(body.len(), 32768);
            assert!(body.bytes().all(|byte| byte == b'x'));
            assert_eq!(row[2], (Value::Null, None));
            count += 1;
        }
        assert_eq!(count, 512);
    });
    assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
}

#[test]
fn view_rule_query_input_preserves_duplicate_positions_and_excludes_hidden_slots() {
    let schema = RowSchema::new(vec!["same".into(), "same".into(), "hidden".into()]);
    let mut buffer = SpillBuffer::new(16 * 1024);
    let mut writer = SpillBatchWriter::new(schema.clone()).unwrap();
    writer
        .push(
            &mut buffer,
            PhysicalRow::from_values(vec![Value::Int(7), Value::Null, Value::Int(99)]),
        )
        .unwrap();
    writer.finish(&mut buffer).unwrap();
    let output = QueryOutput {
        columns: vec!["same".into(), "same".into()],
        column_types: vec![Some(ColumnType::Integer), Some(ColumnType::Text)],
        internal_schema: schema.clone(),
        rows: QueryRows::SharedSpill(buffer.into_shared(schema).unwrap()),
    };
    let rows = ViewInputRows::query(output, CancellationToken::new())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![vec![
            (Value::Int(7), Some(ColumnType::Integer)),
            (Value::Null, Some(ColumnType::Text))
        ]]
    );
}

#[test]
fn view_rule_inputs_preserve_empty_rows_and_reject_cancelled_or_invalid_input() {
    let token = CancellationToken::new();
    let mut buffer = ViewInputBuffer::new(1, token.clone());
    for _ in 0..33 {
        buffer.push(Vec::new()).unwrap();
    }
    let mut rows = buffer.finish();
    for _ in 0..32 {
        assert!(rows.next().unwrap().unwrap().is_empty());
    }
    token.cancel();
    assert_eq!(rows.next().unwrap().unwrap_err().sqlstate(), Some("57014"));
    let mut encoded = Vec::new();
    ciborium::ser::into_writer(&[Some(ColumnType::Text)], &mut encoded).unwrap();
    assert!(decode(PhysicalRow::from_values(vec![
        Value::List(Vec::new()),
        Value::Bytes(encoded)
    ]))
    .is_err());
    let output = QueryOutput {
        columns: Vec::new(),
        column_types: Vec::new(),
        internal_schema: RowSchema::default(),
        rows: QueryRows::Rows(Vec::new()),
    };
    assert!(ViewInputRows::query(output, CancellationToken::new()).is_err());
}
