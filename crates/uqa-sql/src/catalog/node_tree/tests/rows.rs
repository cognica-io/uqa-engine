//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row catalog diagnostics and physical representation boundaries.

use super::{deparse, expressions, parse, values, Field, Node, Routines};
use crate::{ColumnType, RowSchema, Statement};
use uqa_core::Value;

#[test]
fn row_selection_errors_match_postgresql() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("row_errors.json")).unwrap();
    for case in cases {
        let base = case["base"].as_str().unwrap();
        let check = case["check"].as_str().unwrap();
        let ty = ColumnType::from_sql_name(base).unwrap();
        let Statement::CreateDomain(domain) =
            crate::compile(&format!("CREATE DOMAIN d AS {base} CHECK({check})"))
                .unwrap()
                .remove(0)
        else {
            panic!("domain declaration")
        };
        let schema = RowSchema::with_types(vec!["value".into()], vec![Some(ty.clone())]);
        let context = expressions::ExpressionContext {
            schema: &schema,
            domain_value: Some(&ty),
            types: None,
            routines: &Routines,
        };
        let error = context.check(&domain.checks[0].expression).unwrap_err();
        assert_eq!(error.sqlstate(), case["sqlstate"].as_str(), "{check}");
        assert_eq!(
            error.to_string(),
            case["message"].as_str().unwrap(),
            "{check}"
        );
        assert_eq!(error.detail(), case["detail"].as_str(), "{check}");
        assert_eq!(error.hint(), case["hint"].as_str(), "{check}");
    }
}

#[test]
fn string_values_keep_postgresql_list_quoting_separate_from_field_tokens() {
    for text in [
        "",
        "f1",
        "123",
        "<>",
        "a b",
        "a\nb",
        "a\\b",
        "(한){글}",
        "\"quoted",
    ] {
        let list = Field::List(vec![Field::StringValue(text.into())]);
        assert_eq!(parse(&list.to_string()).unwrap(), list);
    }
    assert_eq!(
        parse("(\"f1\" \"f2\")").unwrap(),
        Field::List(vec![
            Field::StringValue("f1".into()),
            Field::StringValue("f2".into())
        ])
    );
    assert!(parse("(\"unfinished)").is_err());
}

fn replace(node: &mut Node, name: &str, value: Field) {
    node.fields
        .iter_mut()
        .find(|(field, _)| field == name)
        .unwrap()
        .1 = value;
}

#[test]
fn unknown_row_fields_validate_cstring_bounds_without_reading_null_payloads() {
    let valid = values::unknown_constant(&Value::Str("한글".into())).unwrap();
    for (length, bytes) in [
        (0, vec![]),
        (2, vec![b'x']),
        (1, vec![b'x']),
        (3, vec![0, b'x', 0]),
        (2, vec![255, 0]),
    ] {
        let mut node = valid.clone();
        replace(&mut node, "constvalue", Field::Datum { length, bytes });
        for pretty in [false, true] {
            assert!(deparse::expression(&node.clone().into(), &Routines, pretty).is_err());
        }
        replace(&mut node, "constisnull", true.into());
        assert_eq!(
            deparse::expression(&node.into(), &Routines, false).unwrap(),
            "NULL::unknown"
        );
    }
}

#[test]
fn row_comparison_reconstruction_rejects_inconsistent_lists() {
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!("rows.json")).unwrap();
    let node = cases
        .iter()
        .find(|case| {
            case["conbin"]
                .as_str()
                .unwrap()
                .starts_with("{ROWCOMPAREEXPR ")
        })
        .unwrap()["conbin"]
        .as_str()
        .unwrap();
    let Field::Node(original) = parse(node).unwrap() else {
        panic!("comparison node")
    };
    for (field, value) in [
        ("cmptype", 3.into()),
        ("largs", Field::Null),
        ("rargs", Field::Null),
        (
            "opnos",
            Field::List(vec![Field::Atom("o".into()), 97.into()]),
        ),
        (
            "opfamilies",
            Field::List(vec![Field::Atom("i".into()), 1976.into(), 1976.into()]),
        ),
        (
            "inputcollids",
            Field::List(vec![Field::Atom("o".into()), (-1).into(), 0.into()]),
        ),
    ] {
        let mut node = original.clone();
        replace(&mut node, field, value);
        assert!(
            deparse::expression(&node.into(), &Routines, false).is_err(),
            "{field}"
        );
    }
}
