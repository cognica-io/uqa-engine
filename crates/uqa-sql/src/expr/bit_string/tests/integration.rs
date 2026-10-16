//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ColumnType;
use uqa_core::Value;

#[test]
fn bit_string_assignment_and_cast_dispatch_keep_the_independent_scalar_contract() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("../postgresql.json")).unwrap();
    let control = ProductionControl::uncontrolled();
    let mut checked = 0;
    for case in reference["cases"].as_array().unwrap() {
        let op = case["op"].as_str().unwrap();
        let modifier = case["modifier"].as_i64().unwrap_or(-1);
        if op == "modifier" || modifier > i64::from(MAX_DECLARED_LENGTH) {
            continue;
        }
        let length = (modifier > 0).then_some(modifier as u32);
        let text = case["text"].as_str().unwrap_or("");
        let ty = if case["kind"] == "varbit" {
            ColumnType::VarBit(length)
        } else {
            ColumnType::Bit(length)
        };
        let input = match op {
            "input" | "text_cast" => Value::Str(text.into()),
            "from_integer" => Value::Int(case["integer"].as_str().unwrap().parse().unwrap()),
            _ => Value::BitString(
                input_with_control(text, BitStringKind::Fixed, -1, &control)
                    .unwrap()
                    .into_uncontrolled()
                    .unwrap(),
            ),
        };
        let actual = match op {
            "input" => crate::assignment::conversion::convert_value_to_column_type(input, &ty),
            "coerce" if !case["explicit"].as_bool().unwrap() => {
                crate::assignment::conversion::convert_value_to_column_type(input, &ty)
            }
            "to_integer" => crate::expr::cast_value_from(
                &input,
                if case["width"] == 32 {
                    "integer"
                } else {
                    "bigint"
                },
                Some("bit"),
            ),
            _ => crate::expr::cast_value_from(
                &input,
                &ty.sql_name(),
                match op {
                    "from_integer" => Some(if case["width"] == 32 {
                        "integer"
                    } else {
                        "bigint"
                    }),
                    "text_cast" => Some("text"),
                    _ => Some(if case["kind"] == "varbit" {
                        "varbit"
                    } else {
                        "bit"
                    }),
                },
            ),
        }
        .and_then(|value| crate::expr::value_to_string(&value));
        let expected = &case["expected"];
        if let Some(expected) = expected.as_str() {
            assert_eq!(
                actual.unwrap_or_else(|error| panic!("{}: {error}", case["sql"])),
                expected,
                "{}",
                case["sql"]
            );
        } else {
            let error = actual.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                expected["sqlstate"].as_str(),
                "{}",
                case["sql"]
            );
            assert_eq!(
                error.to_string(),
                expected["message"].as_str().unwrap(),
                "{}",
                case["sql"]
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 420);
}

#[test]
fn bit_string_keyword_and_catalog_declarations_preserve_type_identity_and_modifiers() {
    use crate::catalog::type_metadata::*;
    for (name, ty, oid, modifier) in [
        ("bit", ColumnType::Bit(Some(1)), 1560, 1),
        ("\"bit\"", ColumnType::Bit(None), 1560, -1),
        ("pg_catalog.bit", ColumnType::Bit(None), 1560, -1),
        ("bit(3)", ColumnType::Bit(Some(3)), 1560, 3),
        ("varbit", ColumnType::VarBit(None), 1562, -1),
        ("bit varying(3)", ColumnType::VarBit(Some(3)), 1562, 3),
    ] {
        let mut statements =
            crate::compiler::compile(&format!("CREATE TABLE bits (v {name})")).unwrap();
        let crate::ast::Statement::CreateTable(table) = statements.pop().unwrap() else {
            panic!("table declaration");
        };
        assert_eq!(table.columns[0].ty, ty, "{name}");
        assert_eq!(ColumnType::from_sql_name(name).unwrap(), ty, "{name}");
        assert_eq!(
            ColumnType::from_sql_name(&ty.sql_name()).unwrap(),
            ty,
            "{name}"
        );
        assert_eq!(pg_type_oid(&ty), oid);
        assert_eq!(pg_type_modifier(&ty), modifier);
        assert_eq!(
            (
                pg_type_len(&ty),
                pg_type_by_value(&ty),
                pg_type_align(&ty),
                pg_type_storage(&ty)
            ),
            (-1, false, "i", "x")
        );
        let array = ColumnType::Array(Box::new(ty.clone()));
        assert_eq!(pg_type_oid(&array), oid + 1);
        assert_eq!(pg_type_modifier(&array), modifier);
    }
    assert_eq!(
        ColumnType::from_sql_name("_bit").unwrap(),
        ColumnType::Array(Box::new(ColumnType::Bit(None)))
    );
    assert_eq!(
        crate::type_resolution::routine_type_category("bit varying"),
        'V'
    );
    assert!(crate::type_resolution::routine_type_is_preferred(
        "bit varying"
    ));
    assert!(!crate::type_resolution::routine_type_is_preferred("bit"));
}
