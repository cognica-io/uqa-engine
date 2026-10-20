//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::output::QueryRows;
use crate::spill::SpillBatchWriter;
use crate::{PhysicalRow, RowSchema, SpillBuffer};
use uqa_core::Value;

fn output(
    columns: Vec<String>,
    schema: RowSchema,
    rows: impl Iterator<Item = Vec<Value>>,
) -> QueryOutput {
    let mut buffer = SpillBuffer::new(64 * 1024);
    let mut writer = SpillBatchWriter::new(schema.clone()).unwrap();
    for row in rows {
        writer
            .push(&mut buffer, PhysicalRow::from_values(row))
            .unwrap();
    }
    writer.finish(&mut buffer).unwrap();
    QueryOutput {
        column_types: vec![None; columns.len()],
        columns,
        internal_schema: schema.clone(),
        rows: QueryRows::SharedSpill(buffer.into_shared(schema).unwrap()),
    }
}

#[test]
fn insert_source_input_bounds_live_payload_during_creation_and_replay() {
    let allocated = allocation_counter::measure(|| {
        let result = output(
            vec!["id".into(), "body".into()],
            RowSchema::new(vec!["id".into(), "body".into()]),
            (0..512).map(|id| vec![Value::Int(id), Value::Str("x".repeat(32768))]),
        );
        let mut count = 0;
        for (id, row) in InsertInputRows::query(result, CancellationToken::new())
            .unwrap()
            .enumerate()
        {
            let row = row.unwrap();
            assert_eq!(row[0], ScalarExpr::Literal(Value::Int(id as i64)));
            let ScalarExpr::Literal(Value::Str(body)) = &row[1] else {
                panic!("body")
            };
            assert_eq!(body.len(), 32768);
            assert!(body.bytes().all(|byte| byte == b'x'));
            count += 1;
        }
        assert_eq!(count, 512);
    });
    assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
}

#[test]
fn insert_source_input_preserves_positions_nulls_and_zero_column_multiplicity() {
    let result = output(
        vec!["same".into(), "same".into()],
        RowSchema::new(vec!["same".into(), "same".into(), "private".into()]),
        std::iter::once(vec![Value::Int(7), Value::Null, Value::Int(99)]),
    );
    let rows = InsertInputRows::query(result, CancellationToken::new())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows[0].as_ref(),
        &[
            ScalarExpr::Literal(Value::Int(7)),
            ScalarExpr::Literal(Value::Null)
        ]
    );
    let result = output(
        Vec::new(),
        RowSchema::default(),
        (0..4097).map(|_| Vec::new()),
    );
    let mut count = 0;
    for row in InsertInputRows::query(result, CancellationToken::new()).unwrap() {
        assert!(row.unwrap().is_empty());
        count += 1;
    }
    assert_eq!(count, 4097);
    let schema = RowSchema::remap_positions(
        &RowSchema::new(vec!["private".into(), "second".into(), "first".into()]),
        &[
            ("same".into(), 2),
            ("same".into(), 1),
            ("missing".into(), 9),
        ],
        &[],
    );
    let result = output(
        vec!["same".into(), "same".into(), "missing".into()],
        schema,
        std::iter::once(vec![Value::Int(99), Value::Int(2), Value::Int(1)]),
    );
    let row = InsertInputRows::query(result, CancellationToken::new())
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(
        row.as_ref(),
        &[
            ScalarExpr::Literal(Value::Int(1)),
            ScalarExpr::Literal(Value::Int(2)),
            ScalarExpr::Literal(Value::Null),
        ]
    );
    let result = output(
        vec!["third".into(), "second".into()],
        RowSchema::new(vec!["private".into(), "second".into(), "third".into()]),
        std::iter::once(vec![Value::Int(99), Value::Int(2), Value::Int(3)]),
    );
    let row = InsertInputRows::query(result, CancellationToken::new())
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(
        row.as_ref(),
        &[
            ScalarExpr::Literal(Value::Int(3)),
            ScalarExpr::Literal(Value::Int(2)),
        ]
    );
}

#[test]
fn insert_source_input_observes_cancellation_and_requires_bounded_query_output() {
    let token = CancellationToken::new();
    let result = output(
        vec!["x".into()],
        RowSchema::new(vec!["x".into()]),
        std::iter::once(vec![Value::Int(1)]),
    );
    let mut input = InsertInputRows::query(result, token.clone()).unwrap();
    token.cancel();
    assert_eq!(input.next().unwrap().unwrap_err().sqlstate(), Some("57014"));
    let result = QueryOutput {
        columns: Vec::new(),
        column_types: Vec::new(),
        internal_schema: RowSchema::default(),
        rows: QueryRows::Rows(Vec::new()),
    };
    assert!(InsertInputRows::query(result, CancellationToken::new()).is_err());
    let values = vec![vec![ScalarExpr::Default]];
    let row = InsertInputRows::values(&values).next().unwrap().unwrap();
    assert!(matches!(row, Cow::Borrowed(_)));
    assert!(matches!(row[0], ScalarExpr::Default));
}
