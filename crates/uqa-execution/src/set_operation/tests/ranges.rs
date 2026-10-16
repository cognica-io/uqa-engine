//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn scalar_range_set_multiplicities_match_postgresql_in_memory_and_spill() {
    for (name, first, equal, other) in [
        ("numrange", "[1.0,3.00)", "[1.00,3.0)", "[4,5)"),
        ("nummultirange", "{[1.0,3.00)}", "{[1.00,3.0)}", "{[4,5)}"),
    ] {
        let ty = uqa_sql::ColumnType::from_sql_name(name).unwrap();
        let first = Value::Str(first.into());
        let equal = Value::Str(equal.into());
        let other = Value::Str(other.into());
        for budget in [1, 64 * 1024] {
            for (kind, all, expected) in [
                (SetOpKind::Union, false, [1, 1, 1]),
                (SetOpKind::Union, true, [3, 1, 3]),
                (SetOpKind::Intersect, false, [1, 0, 1]),
                (SetOpKind::Intersect, true, [1, 0, 1]),
                (SetOpKind::Except, false, [0, 1, 0]),
                (SetOpKind::Except, true, [1, 1, 0]),
            ] {
                let scan = |values: &[Value]| {
                    Box::new(TableScan::from_typed_rows(
                        vec!["v".into()],
                        vec![Some(ty.clone())],
                        values
                            .iter()
                            .map(|value| ResultRow::from([("v".into(), value.clone())]))
                            .collect(),
                    ))
                };
                let mut set = ExternalSetOperation::new(
                    scan(&[first.clone(), first.clone(), other.clone(), Value::Null]),
                    scan(&[equal.clone(), Value::Null, Value::Null]),
                    kind,
                    all,
                    budget,
                )
                .unwrap();
                let (_, rows) = run_to_rows(&mut set).unwrap();
                let mut counts = [0; 3];
                for row in rows {
                    let value = &row["v"];
                    let index = if matches!(value, Value::Null) {
                        2
                    } else if value == &other {
                        1
                    } else {
                        assert!(
                            value.has_same_representation(&first)
                                || value.has_same_representation(&equal)
                        );
                        0
                    };
                    counts[index] += 1;
                }
                assert_eq!(
                    counts, expected,
                    "{name}: {kind:?}, all={all}, budget={budget}"
                );
            }
        }
    }
}
