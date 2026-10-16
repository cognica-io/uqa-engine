//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{PhysicalScanDirection, ScrollMaterialize};
use uqa_sql::ColumnType;

#[test]
fn range_ties_preserve_selected_types_in_forward_and_backward_scans() {
    for kind in ["numrange", "nummultirange"] {
        let ty = ColumnType::from_sql_name(kind).unwrap();
        let values = ["[1.0,3.00)", "[1.00,3.0)", "[4,5)"].map(|input| {
            let input = if kind == "nummultirange" {
                format!("{{{input}}}")
            } else {
                input.into()
            };
            uqa_sql::expr::cast_value(&Value::Str(input), kind).unwrap()
        });
        let domain = ColumnType::Domain {
            schema: "public".into(),
            name: "range_domain".into(),
            oid: 50_001,
            array_oid: Some(50_002),
            collation_oid: None,
            base: Box::new(ty.clone()),
        };
        for (ty, expected) in [(ty, 2), (domain, 2), (ColumnType::Text, 1)] {
            for nulls in [false, true] {
                let values = if nulls {
                    vec![Value::Null, Value::Null, values[2].clone()]
                } else {
                    values.to_vec()
                };
                let count = if nulls { 2 } else { expected };
                for directional in [false, true] {
                    let scan = Box::new(TableScan::from_typed_rows(
                        vec!["r".into()],
                        vec![Some(ty.clone())],
                        values.iter().cloned().map(|v| row([("r", v)])).collect(),
                    ));
                    let input: Box<dyn PhysicalOperator> = if directional {
                        Box::new(ScrollMaterialize::new(scan))
                    } else {
                        scan
                    };
                    let mut limit = Limit::with_ties(
                        input,
                        0,
                        1,
                        vec![SortKey {
                            expr: col("r"),
                            descending: false,
                            nulls_first: Some(true),
                        }],
                        DefaultExpressionEvaluator::shared(vec![]),
                    );
                    if directional {
                        limit.open().unwrap();
                        let expected = values[..count].to_vec();
                        assert_eq!(drain(&mut limit, PhysicalScanDirection::Forward), expected);
                        assert_eq!(
                            drain(&mut limit, PhysicalScanDirection::Backward),
                            expected.iter().cloned().rev().collect::<Vec<_>>()
                        );
                        limit.rewind().unwrap();
                        assert_eq!(drain(&mut limit, PhysicalScanDirection::Forward), expected);
                        limit.close().unwrap();
                    } else {
                        let (_, rows) = run_to_rows(&mut limit).unwrap();
                        assert_eq!(rows.len(), count, "{kind}: {ty:?}");
                        for (row, input) in rows.iter().zip(&values) {
                            assert!(row["r"].has_same_representation(input));
                        }
                    }
                }
            }
        }
    }
}

fn drain(limit: &mut Limit<'_>, direction: PhysicalScanDirection) -> Vec<Value> {
    let mut output = Vec::new();
    while let Some(batch) = limit.next_direction(direction).unwrap() {
        output.extend(batch.rows.iter().map(|row| row.value(0).unwrap().clone()));
    }
    output
}
