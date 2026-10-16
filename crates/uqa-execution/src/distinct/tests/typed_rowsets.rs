//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ColumnType;

#[test]
fn scalar_range_rowsets_preserve_postgresql_classes_in_memory_and_spill() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../uqa-sql/src/expr/range/comparison/postgresql.json"
    ))
    .unwrap();
    for family in reference["families"].as_array().unwrap() {
        let kind = family["kind"].as_str().unwrap();
        let values = family["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| {
                uqa_sql::expr::cast_value(&Value::Str(value.as_str().unwrap().into()), kind)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let ty = ColumnType::from_sql_name(kind).unwrap();
        let domain = ColumnType::Domain {
            schema: "public".into(),
            name: "range_domain".into(),
            oid: 50_001,
            array_oid: Some(50_002),
            collation_oid: None,
            base: Box::new(ty.clone()),
        };
        for ty in [ty, domain] {
            for budget in [1, 64 * 1024] {
                let types = vec![Some(ty.clone())];
                let schema = RowSchema::with_types(vec!["r".into()], types.clone());
                let mut hash = CanonicalRowHashSet::with_sql_types(types.clone());
                let mut exact = ExactRowSet::with_sql_types(budget, types);
                for (index, value) in values.iter().enumerate() {
                    let first = !(0..index).any(|prior| family["comparisons"][index][prior] == 0);
                    let borrowed = [&values[index]];
                    let inserted = if index % 2 == 0 {
                        hash.insert_borrowed(&borrowed).unwrap()
                    } else {
                        hash.insert_values(std::slice::from_ref(value)).unwrap()
                    };
                    assert_eq!(inserted, first, "{kind}: hash {index}");
                    let inserted = if index % 2 == 0 {
                        exact.insert_sql_values(std::slice::from_ref(value), None)
                    } else {
                        exact.insert_sql_physical(
                            &PhysicalRow::from_values(vec![value.clone()]),
                            &schema,
                            None,
                        )
                    }
                    .unwrap();
                    assert_eq!(inserted, first, "{kind}: exact {index}");
                    assert!(hash.contains_borrowed(&borrowed).unwrap());
                    assert!(hash.contains_values(std::slice::from_ref(value)).unwrap());
                    assert!(exact
                        .contains_sql_values(std::slice::from_ref(value), None)
                        .unwrap());
                }
                assert!(hash.rows[0][0].has_same_representation(&values[0]));
                assert_eq!(exact.has_spilled(), budget == 1);
                assert!(exact.in_memory_key_bytes() <= budget);
            }
        }
    }
}

#[test]
fn typed_sql_rowsets_keep_raw_and_text_key_domains_separate() {
    let first = Value::Str("[1.0,3.00)".into());
    let equal = Value::Str("[1.00,3.0)".into());
    for ty in [None, Some(ColumnType::Text)] {
        let mut hash = CanonicalRowHashSet::with_sql_types(vec![ty.clone()]);
        let mut exact = ExactRowSet::with_sql_types(1, vec![ty]);
        for value in [&first, &equal] {
            assert!(hash.insert_values(std::slice::from_ref(value)).unwrap());
            assert!(exact
                .insert_sql_values(std::slice::from_ref(value), None)
                .unwrap());
        }
    }
    let mut raw = ExactRowSet::with_sql_types(
        1,
        vec![Some(ColumnType::from_sql_name("numrange").unwrap())],
    );
    for value in [&first, &equal] {
        assert!(raw.insert_values(std::slice::from_ref(value)).unwrap());
        assert!(raw.contains_values(std::slice::from_ref(value)).unwrap());
    }
}

#[test]
fn sql_rowset_physical_and_value_access_keep_one_selected_type_domain() {
    for kind in ["numrange", "nummultirange"] {
        let values = ["[1.0,3.00)", "[1.00,3.0)"].map(|input| {
            let input = if kind == "nummultirange" {
                format!("{{{input}}}")
            } else {
                input.into()
            };
            uqa_sql::expr::cast_value(&Value::Str(input), kind).unwrap()
        });
        let ty = ColumnType::from_sql_name(kind).unwrap();
        let selected = RowSchema::with_types(vec!["r".into()], vec![Some(ty.clone())]);
        let untyped = RowSchema::with_types(vec!["r".into()], vec![None]);
        let physical = PhysicalRow::from_values(vec![values[0].clone()]);
        for budget in [1, 64 * 1024] {
            for explicit_types in [false, true] {
                let mut rows = if explicit_types {
                    ExactRowSet::with_sql_types(budget, vec![Some(ty.clone())])
                } else {
                    ExactRowSet::new(budget)
                };
                assert!(!rows.contains_sql_values(&values[..1], None).unwrap());
                let schema = if explicit_types { &untyped } else { &selected };
                assert!(rows.insert_sql_physical(&physical, schema, None).unwrap());
                for value in &values {
                    assert!(rows
                        .contains_sql_values(std::slice::from_ref(value), None)
                        .unwrap());
                    assert!(!rows
                        .insert_sql_values(std::slice::from_ref(value), None)
                        .unwrap());
                }
                assert_eq!(rows.has_spilled(), budget == 1);
                assert!(rows.in_memory_key_bytes() <= budget);
            }

            let mut rows = ExactRowSet::new(budget);
            assert!(rows.insert_sql_values(&values[..1], None).unwrap());
            assert!(!rows
                .insert_sql_physical(&physical, &selected, None)
                .unwrap());
            assert!(rows.insert_sql_values(&values[1..], None).unwrap());
            for value in &values {
                assert!(rows
                    .contains_sql_values(std::slice::from_ref(value), None)
                    .unwrap());
            }
        }
    }
}
