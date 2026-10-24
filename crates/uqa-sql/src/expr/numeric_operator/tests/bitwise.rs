//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn integer_bitwise_values_types_and_diagnostics_match_postgresql() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("pg18_bitwise.json")).unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let sql = case["expression"].as_str().unwrap();
        let expression = expression(sql);
        let scalar = crate::plan::ExpressionPlan::lower(expression.clone()).scalar;
        let actual = crate::scalar_type(&scalar, &RowSchema::default(), &[]).and_then(|ty| {
            eval(&expression, &EvalContext::new(None, &[])).map(|value| (ty, value))
        });
        if let Some(sqlstate) = case["sqlstate"].as_str() {
            let error = actual.unwrap_err();
            assert_eq!(error.sqlstate(), Some(sqlstate), "{sql}: {error}");
            assert_eq!(error.to_string(), case["message"].as_str().unwrap(), "{sql}");
        } else {
            let (ty, value) = actual.unwrap_or_else(|error| panic!("{sql}: {error}"));
            assert_eq!(
                crate::catalog::type_metadata::pg_type_oid(&ty.expect("selected result")),
                case["type_oid"].as_i64().unwrap(),
                "{sql}"
            );
            let text = (!matches!(value, Value::Null))
                .then(|| crate::expr::value_to_string(&value).unwrap());
            assert_eq!(text.as_deref(), case["value"].as_str(), "{sql}");
        }
    }
}

#[test]
fn integer_bitwise_catalog_identities_match_postgresql() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("pg18_bitwise.json")).unwrap();
    let entries = reference["operator_catalog"].as_array().unwrap();
    assert_eq!(entries.len(), 18);
    for row in entries {
        let text = |position: usize| row[position].as_str().unwrap();
        let oid = text(4).parse().unwrap();
        let function_oid = text(5).parse().unwrap();
        assert_eq!(text(7), "t");
        if text(1).is_empty() {
            let entry = crate::type_resolution::operators::unary_operator_by_oid(oid)
                .unwrap_or_else(|| panic!("missing unary operator {oid}"));
            assert_eq!(entry.name, text(0));
            assert_eq!(entry.function_oid, function_oid);
            assert_eq!(entry.operand_type, ColumnType::from_sql_name(text(2)).unwrap());
        } else {
            let entry = crate::type_resolution::operators::binary_operator_by_oid(oid)
                .unwrap_or_else(|| panic!("missing binary operator {oid}"));
            assert_eq!(entry.name, text(0));
            assert_eq!(entry.function_oid, function_oid);
            assert_eq!(entry.operand_types, [text(1), text(2)]);
            assert_eq!(entry.result_type, text(3));
        }
    }
}
