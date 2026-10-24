//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{expr::observations, ResultRow};

#[test]
fn ordinary_result_values_are_inspected_once() {
    let rows = (0..8192)
        .map(|id| {
            ResultRow::from([
                ("id".into(), Value::Int(id)),
                ("amount".into(), Value::Int(id * 7)),
            ])
        })
        .collect();
    let mut result = SQLResult::from_rows(vec!["id".into(), "amount".into()], rows);
    let before = observations::visits();
    render_result_enum_labels(None, &mut result).unwrap();
    let inspected = observations::visits() - before;
    for (id, row) in result.rows.iter().enumerate() {
        let id = i64::try_from(id).unwrap();
        assert_eq!(row["id"], Value::Int(id));
        assert_eq!(row["amount"], Value::Int(id * 7));
    }
    assert_eq!(inspected, 8192 * 2, "one carrier inspection per scalar");
}

#[test]
fn ordinary_nested_output_retains_payloads_with_one_carrier_walk() {
    let values = (0..4096).map(Value::Int).collect::<Vec<_>>();
    let pointer = values.as_ptr();
    let mut result = SQLResult::from_rows(
        vec!["values".into()],
        vec![ResultRow::from([("values".into(), Value::List(values))])],
    );
    let before = observations::visits();
    let allocation = allocation_counter::measure(|| {
        render_result_enum_labels(None, &mut result).unwrap();
    });
    let inspected = observations::visits() - before;
    let Value::List(values) = &result.rows[0]["values"] else {
        panic!("list output must retain its carrier");
    };
    assert_eq!(values.as_ptr(), pointer);
    assert_eq!(values.len(), 4096);
    for (index, value) in values.iter().enumerate() {
        assert_eq!(*value, Value::Int(i64::try_from(index).unwrap()));
    }
    assert_eq!(allocation.count_total, 0);
    assert_eq!(
        inspected, 4097,
        "inspect the container and every child once"
    );
}
