//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::scope::subqueries::CachedScalarSubquery;
use uqa_sql::ColumnType;

#[test]
fn scalar_range_membership_keeps_null_empty_and_streamed_semantics() {
    let services = Services::default();
    let catalog = uqa_sql::expr::EngineValueCatalog(&services);
    for kind in ["numrange", "nummultirange"] {
        let values = ["[1.0,3.00)", "[1.00,3.0)", "[4,5)"].map(|value| {
            let text = if kind == "nummultirange" {
                format!("{{{value}}}")
            } else {
                value.into()
            };
            uqa_sql::expr::cast_value(&Value::Str(text), kind).unwrap()
        });
        let [first, equal, other] = values;
        for budget in [1, 64 * 1024] {
            for (input, expected) in [
                (vec![], [Some(false), Some(false), Some(false)]),
                (vec![Value::Null], [None, None, None]),
                (vec![first.clone()], [Some(true), Some(false), None]),
                (vec![first.clone(), Value::Null], [Some(true), None, None]),
            ] {
                let schema = RowSchema::with_types(
                    vec!["r".into()],
                    vec![Some(ColumnType::from_sql_name(kind).unwrap())],
                );
                let mut spill = crate::SpillBuffer::new(budget);
                if !input.is_empty() {
                    spill
                        .push(crate::Batch::from_physical_rows(
                            schema.clone(),
                            input
                                .into_iter()
                                .map(|value| PhysicalRow::from_values(vec![value]))
                                .collect(),
                        ))
                        .unwrap();
                }
                let rows = CachedScalarSubquery {
                    columns: vec!["r".into()],
                    rows: spill.into_shared(schema).unwrap(),
                };
                let membership = rows.membership(budget, Some(&catalog)).unwrap();
                for (needle, expected) in [&equal, &other, &Value::Null].into_iter().zip(expected) {
                    assert_eq!(
                        membership.contains(needle, Some(&catalog)).unwrap(),
                        expected
                    );
                    assert_eq!(
                        rows.result()
                            .unwrap()
                            .contains_with_catalog(needle, &catalog)
                            .unwrap(),
                        expected
                    );
                }
            }
        }
    }
}
