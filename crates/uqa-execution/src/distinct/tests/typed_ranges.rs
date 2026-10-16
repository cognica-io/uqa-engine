//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn scalar_range_distinct_preserves_postgresql_classes_in_memory_and_spill() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-sql/src/expr/range/comparison/postgresql.json"
    )))
    .unwrap();
    for family in reference["families"].as_array().unwrap() {
        let kind = family["kind"].as_str().unwrap();
        let inputs = family["values"].as_array().unwrap();
        let mut representatives = Vec::<usize>::new();
        for index in 0..inputs.len() {
            if !representatives
                .iter()
                .any(|&prior| family["comparisons"][index][prior] == 0)
            {
                representatives.push(index);
            }
        }
        let values = inputs
            .iter()
            .map(|input| {
                uqa_sql::expr::cast_value(&Value::Str(input.as_str().unwrap().into()), kind)
                    .unwrap()
            })
            .chain([Value::Null, Value::Null])
            .collect::<Vec<_>>();
        let expected = representatives
            .iter()
            .map(|&index| values[index].clone())
            .chain(std::iter::once(Value::Null))
            .collect::<Vec<_>>();
        let ty = uqa_sql::ColumnType::from_sql_name(kind).unwrap();
        let domain = uqa_sql::ColumnType::Domain {
            schema: "public".into(),
            name: "range_domain".into(),
            oid: 50_001,
            array_oid: Some(50_002),
            collation_oid: None,
            base: Box::new(ty.clone()),
        };
        for ty in [&ty, &domain] {
            for on in [false, true] {
                for budget in [1, 64 * 1024] {
                    let actual = distinct_values(Some(ty), &values, budget, on);
                    assert_eq!(actual.len(), expected.len(), "{kind}, work_mem={budget}");
                    for (actual, expected) in actual.iter().zip(&expected) {
                        assert!(actual.has_same_representation(expected), "{kind}");
                    }
                }
            }
        }
    }
}

#[test]
fn text_distinct_does_not_infer_ranges_from_spelling() {
    let first = Value::Str("[1.0,3.00)".into());
    let second = Value::Str("[1.00,3.0)".into());
    let values = [
        first.clone(),
        second.clone(),
        first.clone(),
        Value::Null,
        Value::Null,
    ];
    for ty in [None, Some(&uqa_sql::ColumnType::Text)] {
        for on in [false, true] {
            for budget in [1, 64 * 1024] {
                assert_eq!(
                    distinct_values(ty, &values, budget, on),
                    [first.clone(), second.clone(), Value::Null]
                );
            }
        }
    }
}

fn distinct_values(
    ty: Option<&uqa_sql::ColumnType>,
    values: &[Value],
    budget: usize,
    on: bool,
) -> Vec<Value> {
    let directory = tempfile::tempdir().unwrap();
    let scan = Box::new(TableScan::from_typed_rows(
        vec!["v".into()],
        vec![ty.cloned()],
        values.iter().cloned().map(value_row).collect(),
    ));
    let mut distinct = if on {
        Distinct::on_with_work_mem(
            scan,
            vec![ScalarExpr::Column("v".into())],
            Arc::new(Evaluator),
            budget,
        )
    } else {
        Distinct::all_with_work_mem(scan, budget)
    }
    .with_spill_directory(directory.path().to_path_buf());
    distinct.open().unwrap();
    let mut actual = Vec::new();
    while let Some(batch) = distinct.next().unwrap() {
        actual.extend(
            batch
                .rows
                .into_iter()
                .map(|row| row.value(0).unwrap().clone()),
        );
    }
    assert_eq!(distinct.has_spilled(), budget == 1);
    distinct.close().unwrap();
    drop(distinct);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    actual
}
